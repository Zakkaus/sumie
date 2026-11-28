#!/usr/bin/env python3

import argparse
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tomllib

# Importing the gate modules must not leave bytecode in the worktree: the negative controls
# compare its status before and after.
sys.dont_write_bytecode = True
from gates.build import ROOT, CannotCheck, cargo_record, run  # noqa: E402
from gates.macos import (FRAME_GATES, LIFETIME_GATES, check_macos_frames,  # noqa: E402
                         check_macos_ime, check_macos_keys, check_macos_lifetime,
                         check_macos_pasteboard, check_macos_routes, check_macos_shell,
                         check_macos_wake)
from gates.panes import check_linux_panes, check_panes  # noqa: E402

BASE = frozenset({"sumie-core", "sumie-config"})
ADAPTERS = {"sumie-vt", "sumie-font", "sumie-render", "sumie-wayland", "sumie-macos", "sumie-pty"}
EDGES = {name: BASE for name in ADAPTERS}
EDGES.update({
    "sumie-core": frozenset(),
    "sumie-config": frozenset(),
    "sumie-ui": BASE | {"sumie-font", "sumie-render"},
    "sumie": BASE | ADAPTERS | {"sumie-ui"},
})
UNSAFE_CRATES = {"sumie-vt", "sumie-font", "sumie-render", "sumie-wayland"}
FORBIDDEN_NAMES = {"util", "common", "helper", "misc"}
FILE_LIMIT = 600
GATED_LINTS = {
    ("warnings",),
    *(("clippy", ":", ":", name) for name in (
        "all", "pedantic", "too_many_lines", "cognitive_complexity", "undocumented_unsafe_blocks",
    )),
}
CLIPPY_CONFIGS = {"clippy.toml", ".clippy.toml"}
TEST_SUMMARY = re.compile(r"^test result: (ok|FAILED)\. ([0-9]+) passed;")
FULL_REV = re.compile(r"[0-9a-f]{40}")
DEBUG_PRINT = re.compile(r"\b(?:e?print(?:ln)?|dbg)!")
# The test gate builds first, so this bounds only the test binaries: a hung test fails the
# gate in minutes instead of running until the CI job limit.
TEST_TIMEOUT = 600
# A probe that breaks sumie-font runs only its tests, so dependants cannot hang the gate.
TEST_GATES = ("test", "font-test")

RUST_TOKEN = re.compile(
    r'(?P<line>//[^\n]*)|(?P<block>/\*)|'
    r'(?:r|br|cr)(?P<hashes>\#*)"[\s\S]*?"(?P=hashes)|'
    r'(?:b|c)?"(?:[^"\\]|\\[\s\S])*"|'
    r"b?'(?:[^'\\\n]|\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|[^\n]))'|"
    r'(?:r\#)?\w+|\S'
)
BLOCK_DELIMITER = re.compile(r'/\*|\*/')


def read_command(*args):
    result = run(*args)
    if result.returncode:
        raise CannotCheck(f"{' '.join(args)}: {result.stderr.decode(errors='replace').strip()}")
    return result.stdout


def rust_tokens(source):
    cursor = 0
    while match := RUST_TOKEN.search(source, cursor):
        cursor = match.end()
        if match.group("line"):
            continue
        if match.group("block"):
            depth = 1
            while depth:
                delimiter = BLOCK_DELIMITER.search(source, cursor)
                if delimiter is None:
                    raise CannotCheck("unterminated Rust block comment")
                depth += 1 if delimiter[0] == "/*" else -1
                cursor = delimiter.end()
            continue
        yield match[0].removeprefix("r#")


def module_names(tokens):
    for keyword, name, end in zip(tokens, tokens[1:], tokens[2:]):
        if keyword == "mod" and end in (";", "{"):
            yield name


def attributes(tokens):
    for start, token in enumerate(tokens):
        if token != "#":
            continue
        inner = tokens[start + 1:start + 2] == ["!"]
        opening = start + 1 + inner
        if tokens[opening:opening + 1] != ["["]:
            continue
        depth = 1
        for end in range(opening + 1, len(tokens)):
            depth += (tokens[end] == "[") - (tokens[end] == "]")
            if depth == 0:
                yield start, end + 1, inner, tokens[opening + 1:end]
                break


def meta_args(tokens):
    if tokens[1:2] != ["("] or tokens[-1:] != [")"]:
        return
    depth = 0
    start = 2
    end = len(tokens) - 1
    for pos in range(start, end):
        token = tokens[pos]
        if token == "," and depth == 0:
            yield tokens[start:pos]
            start = pos + 1
        depth += (token in ("(", "[", "{")) - (token in (")", "]", "}"))
    if start < end:
        yield tokens[start:end]


def suppressed_lints(meta):
    if meta[:1] == ["cfg_attr"]:
        args = meta_args(meta)
        next(args, None)  # The condition is not an attribute.
        for nested in args:
            yield from suppressed_lints(nested)
    elif meta[:1] in (["allow"], ["expect"]):
        for lint in meta_args(meta):
            if tuple(lint) in GATED_LINTS:
                yield "".join(lint)


def check_rust(path, source, safe):
    tokens = list(rust_tokens(source))
    header_end = 0
    forbid = False
    errors = []
    for start, end, inner, meta in attributes(tokens):
        if inner and start == header_end:
            header_end = end
            if meta[:1] == ["forbid"] and ["unsafe_code"] in meta_args(meta):
                forbid = True
        for lint in suppressed_lints(meta):
            errors.append(f"lints: {path}: forbidden suppression of {lint}")
    if safe and not forbid:
        errors.append(f"unsafe: {path}: missing crate-level #![forbid(unsafe_code)]")
    names = set(path.with_suffix("").parts) | set(module_names(tokens))
    for name in sorted(names & FORBIDDEN_NAMES):
        errors.append(f"names: {path}: forbidden module name {name}")
    return errors


def source_files(target_roots):
    listed = read_command("git", "ls-files", "-z", "--cached", "--others", "--exclude-standard")
    deleted = read_command("git", "ls-files", "-z", "--deleted")
    paths = (set(listed.split(b"\0")) - set(deleted.split(b"\0"))) - {b""}
    paths = {Path(os.fsdecode(path)) for path in paths}
    rust_sources = target_roots | {p for p in paths if p.suffix == ".rs"}
    scripts = {p for p in paths if p.parts[0] == "scripts" and (
        p.suffix in (".sh", ".py") or p.parent == Path("scripts/hooks")
    )}
    return rust_sources, rust_sources | scripts


def check_edges(packages):
    errors = []
    members = {pkg["name"] for pkg in packages}
    for name in sorted(EDGES.keys() - members):
        errors.append(f"dependencies: missing workspace package {name}")
    paths = {str(Path(pkg["manifest_path"]).parent): pkg["name"] for pkg in packages}
    count = 0
    for pkg in packages:
        name = pkg["name"]
        if name not in EDGES:
            errors.append(f"dependencies: unlisted workspace package {name}")
            continue
        for dep in pkg["dependencies"]:
            other = paths.get(dep.get("path"), dep["name"])
            if other in members or other == "sumie" or other.startswith("sumie-"):
                count += 1
                if other not in EDGES[name]:
                    errors.append(f"dependencies: forbidden edge {name} -> {other}")
    print(f"dependencies: scanned {len(packages)} workspace packages, {count} sumie edges")
    return errors


def check_members(packages):
    errors = []
    manifests = [Path(pkg["manifest_path"]).relative_to(ROOT) for pkg in packages]
    members = {manifest.parent for manifest in manifests if manifest != Path("Cargo.toml")}
    dirs = {path.relative_to(ROOT) for path in (ROOT / "crates").iterdir() if path.is_dir()}
    for path in sorted(dirs - members):
        errors.append(f"workspace: {path}: directory is not a workspace member")
    for path in sorted(members - dirs):
        errors.append(f"workspace: {path}: member is not directly under crates/")
    for path in sorted(manifests):
        try:
            with (ROOT / path).open("rb") as manifest:
                lints = tomllib.load(manifest).get("lints", {})
        except (OSError, ValueError) as error:
            raise CannotCheck(f"{path}: {error}") from error
        if lints.get("workspace") is not True:
            errors.append(f"lints: {path}: expected [lints] workspace = true")
    print(f"workspace: compared {len(dirs)} crate directories with {len(members)} library members")
    print(f"lints: scanned {len(manifests)} manifests for lint inheritance")
    return errors


def check_clippy_configs():
    def fail(error):
        raise error

    errors = []
    for directory, dirs, files in os.walk(ROOT, onerror=fail):
        if ".git" in dirs:
            dirs.remove(".git")
        for name in sorted(CLIPPY_CONFIGS.intersection(files)):
            path = (Path(directory) / name).relative_to(ROOT)
            if path != Path("clippy.toml"):
                errors.append(f"lints: {path}: only the root clippy.toml is allowed")
    return errors


def check_patches():
    """Git patches pin the locked commit; vendored diffs add no debug prints."""
    try:
        with (ROOT / "Cargo.toml").open("rb") as manifest:
            patches = tomllib.load(manifest).get("patch", {})
        with (ROOT / "Cargo.lock").open("rb") as lock:
            locked = tomllib.load(lock)
        vendored = ROOT / "third_party"
        files = sorted(vendored.rglob("*.patch")) if vendored.is_dir() else []
        diffs = {path.relative_to(ROOT): path.read_text(encoding="utf-8") for path in files}
    except (OSError, UnicodeError, ValueError) as error:
        raise CannotCheck(f"patches: {error}") from error
    packages = locked.get("package", []) + locked.get("patch", {}).get("unused", [])
    sources = {}
    for pkg in packages:
        sources.setdefault(pkg.get("name"), set()).add(pkg.get("source", ""))
    errors = []
    entries = 0
    for registry, table in patches.items():
        for name, spec in table.items():
            if not isinstance(spec, dict) or "git" not in spec:
                continue
            entries += 1
            entry = f"patch: [patch.{registry}] {name}"
            rev = spec.get("rev", "")
            if "branch" in spec or "tag" in spec:
                errors.append(f"{entry}: pin a full rev, not a branch or tag")
            elif not FULL_REV.fullmatch(rev):
                errors.append(f"{entry}: rev must be a full 40-hex commit")
            elif not any(src.startswith("git+") and src.endswith(f"#{rev}")
                         for src in sources.get(spec.get("package", name), ())):
                errors.append(f"{entry}: Cargo.lock does not take {name} from rev {rev}")
    for path, diff in diffs.items():
        for number, line in enumerate(diff.splitlines(), 1):
            if line.startswith("+") and not line.startswith("+++") and DEBUG_PRINT.search(line):
                errors.append(f"patch: {path}:{number}: debug print in an added line")
    print(f"patches: scanned {entries} git patch entries and {len(diffs)} third_party patch files")
    return errors


def reserved_name(name):
    return name.replace("_", "-").removeprefix("sumie-") in FORBIDDEN_NAMES


def check_structure():
    data = json.loads(read_command("cargo", "metadata", "--no-deps", "--locked", "--format-version", "1"))
    packages = [pkg for pkg in data["packages"] if pkg["id"] in data["workspace_members"]]
    if not packages or any(not pkg["targets"] for pkg in packages):
        raise CannotCheck("cargo metadata did not supply Cargo targets for every workspace package")

    errors = check_edges(packages)
    errors.extend(check_members(packages))
    errors.extend(check_clippy_configs())
    errors.extend(check_patches())
    roots = set()
    safe_roots = set()
    crate_names = set()
    for pkg in packages:
        crate_names.add(pkg["name"])
        for target in pkg["targets"]:
            path = Path(target["src_path"]).relative_to(ROOT)
            roots.add(path)
            crate_names.add(target["name"])
            if pkg["name"] not in UNSAFE_CRATES:
                safe_roots.add(path)
    rust_sources, code_files = source_files(roots)
    if not safe_roots or not rust_sources or not code_files or not crate_names:
        raise CannotCheck("structural checks have no crate roots, source files, or names to scan")

    for name in sorted(crate_names):
        if reserved_name(name):
            errors.append(f"names: forbidden crate name {name}")
    for path in sorted(code_files):
        try:
            source = (ROOT / path).read_text(encoding="utf-8-sig")
            lines = source.count("\n") + bool(source and not source.endswith("\n"))
            if lines > FILE_LIMIT:
                errors.append(f"file length: {path}: {lines} lines exceeds {FILE_LIMIT}")
            if path in rust_sources:
                errors.extend(check_rust(path, source, path in safe_roots))
        except (OSError, UnicodeError, CannotCheck) as error:
            raise CannotCheck(f"{path}: {error}") from error
    print(f"unsafe: scanned {len(safe_roots)} safe crate roots; {len(roots - safe_roots)} exempt")
    print(f"file length: scanned {len(code_files)} code files (limit: {FILE_LIMIT})")
    print(f"names: scanned {len(crate_names)} crate/target names and {len(rust_sources)} Rust files")
    print(f"lints: scanned {len(rust_sources)} Rust files for suppressions and the tree for Clippy configs")
    for error in errors:
        print(error, file=sys.stderr)
    return int(bool(errors))


def run_cargo(args, timeout):
    """Runs cargo in its own process group, so a timeout also stops the test binaries."""
    with subprocess.Popen(["cargo", *args], cwd=ROOT, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, start_new_session=True) as process:
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            stdout, stderr = process.communicate()
            return stdout, stderr + f"cargo {args[0]} did not finish within {timeout} s\n".encode(), None
    return stdout, stderr, process.returncode


def check_cargo(gate, args, timeout=None):
    stdout, stderr, returncode = run_cargo(args, timeout)
    violation = returncode is None
    passed = 0
    for line in stdout.decode(errors="replace").splitlines():
        message = cargo_record(line) if gate != "fmt" else None
        if message is not None:
            if message["reason"] == "compiler-message" and isinstance(message.get("message"), dict):
                diagnostic = message["message"]
                if "rendered" in diagnostic and "level" in diagnostic:
                    print(diagnostic["rendered"] or "", end="")
                    violation |= diagnostic["level"] == "error"
        else:
            print(line)
            if gate in TEST_GATES and (summary := TEST_SUMMARY.match(line)):
                violation |= summary[1] == "FAILED"
                passed += int(summary[2])
            elif gate == "fmt":
                violation |= line.startswith("Diff in ")
    print(stderr.decode(errors="replace"), end="", file=sys.stderr)
    if returncode == 0:
        if gate in TEST_GATES and passed == 0:
            print("test: no passing workspace tests", file=sys.stderr)
            return 1
        if gate in TEST_GATES:
            print(f"test: {passed} workspace tests passed")
        return 0
    if violation:
        return 1
    raise CannotCheck(f"cargo {args[0]} exited {returncode} without a check diagnostic")


def check_terminfo():
    source = (ROOT / "terminfo/xterm-sumie.terminfo").read_text(encoding="utf-8")
    source = "\n".join(line for line in source.splitlines() if not line.lstrip().startswith("#"))
    fields = re.findall(r"(?:\\.|[^\\,])+", source, flags=re.DOTALL)
    capabilities = dict((re.split(r"([=#@])", field.strip(), maxsplit=1) + [""])[:2]
                        for field in fields[1:])
    errors = []
    for name, kind in (("Tc", ""), ("Smulx", "="), ("Setulc", "="),
                       ("Ms", "="), ("Sync", "="), ("Hls", "=")):
        if capabilities.get(name) != kind:
            errors.append(f"xterm-sumie: missing or invalid capability {name}")
    if "RGB" in capabilities:
        errors.append("xterm-sumie: RGB must be query-only")
    for error in errors:
        print(error, file=sys.stderr)
    return int(bool(errors))


def check_shell():
    scripts = ROOT / "shell-integration"
    for file in ("bash/sumie.bash", "zsh/.zshenv", "fish/vendor_conf.d/sumie.fish"):
        source = (scripts / file).read_text()
        for marker in ("133;A", "133;B", "133;C", "133;D;%d", "TERM=xterm-256color",
                       "SendEnv=COLORTERM TERM_PROGRAM"):
            if marker not in source:
                print(f"{file}: missing {marker}", file=sys.stderr)
                return 1
    for file in ("cwd.sh", "fish/vendor_conf.d/sumie.fish"):
        if "7;file://" not in (scripts / file).read_text():
            print(f"{file}: missing OSC 7 file URI", file=sys.stderr)
            return 1
    # The macOS login test runs /bin/bash 3.2; this also holds where it cannot run.
    for file in ("bash/sumie.bash", "cwd.sh"):
        lines = (scripts / file).read_text().splitlines()
        if any("[[ -v" in line for line in lines if not line.lstrip().startswith("#")):
            print(f"{file}: [[ -v needs Bash 4.2; macOS has 3.2", file=sys.stderr)
            return 1
    return 0


def main():
    commands = {
        "fmt": ("fmt", "--all", "--check"),
        "clippy": ("clippy", "--locked", "--workspace", "--all-targets", "--message-format=json", "--", "-D", "warnings"),
        "clippy-macos-ci": ("clippy", "--locked", "--workspace", "--all-targets", "--features", "macos-ci",
                            "--message-format=json", "--", "-D", "warnings"),
        "test": ("test", "--locked", "--workspace", "--message-format=json"),
        "font-test": ("test", "--locked", "-p", "sumie-font", "--message-format=json"),
    }
    parser = argparse.ArgumentParser()
    parser.add_argument("gate", choices=("structure", "terminfo", "shell", "macos-quit", "macos-wake",
                                         "macos-keys", "macos-shell", "macos-pasteboard", "macos-ime", "linux-panes",
                                         "macos-routes", "panes", *FRAME_GATES,
                                         *LIFETIME_GATES, *commands))
    gate = parser.parse_args().gate
    os.environ["GIT_OPTIONAL_LOCKS"] = "0"
    os.environ["CARGO_TERM_COLOR"] = "never"
    cache = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
    os.environ.setdefault("CARGO_TARGET_DIR", str(cache / "sumie" / "target"))
    print(f"== {gate}: {ROOT}", flush=True)
    try:
        if gate == "macos-quit":
            evidence = os.environ.get("SUMIE_SPIKE_EVIDENCE")
            if not evidence:
                sys.exit("macos-quit: set SUMIE_SPIKE_EVIDENCE to an evidence directory")
            return int(any(subprocess.call(["bash", ROOT / "scripts/macos-spike/quit.sh", evidence, case])
                              for case in ("menu", "key", "nested", "logout", "shutdown",
                                           "menu-during-later", "quit-all", "unknown-reason",
                                           "upgrade")))
        if gate == "clippy-macos-ci" and sys.platform != "darwin":
            raise CannotCheck("the macos-ci feature builds only on macOS")
        if gate == "macos-wake":
            return check_macos_wake()
        if gate == "macos-keys":
            return check_macos_keys()
        if gate == "macos-shell":
            return check_macos_shell()
        if gate == "macos-routes":
            return check_macos_routes()
        if gate == "macos-pasteboard":
            return check_macos_pasteboard()
        if gate == "macos-ime":
            return check_macos_ime()
        if gate in FRAME_GATES:
            return check_macos_frames(FRAME_GATES[gate])
        if gate in LIFETIME_GATES:
            return check_macos_lifetime(LIFETIME_GATES[gate])
        if gate == "linux-panes":
            return check_linux_panes()
        if gate == "panes":
            return check_panes()
        if gate == "structure":
            return check_structure()
        if gate == "terminfo":
            return check_terminfo()
        if gate == "shell":
            return check_shell()
        if gate in TEST_GATES:
            return check_cargo("test-build", (*commands[gate], "--no-run")) or check_cargo(
                gate, commands[gate], TEST_TIMEOUT)
        return check_cargo(gate, commands[gate])
    except (CannotCheck, OSError, KeyError, ValueError, subprocess.SubprocessError) as error:
        print(f"{gate}: could not run: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
