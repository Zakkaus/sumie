#!/usr/bin/env python3

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
BASE = frozenset({"sumie-core", "sumie-config"})
ADAPTERS = {"sumie-vt", "sumie-font", "sumie-render", "sumie-wayland", "sumie-macos", "sumie-pty"}
EDGES = {name: BASE for name in ADAPTERS}
EDGES.update({
    "sumie-core": frozenset(),
    "sumie-config": frozenset(),
    "sumie-ui": BASE | {"sumie-font", "sumie-render"},
    "sumie": BASE | ADAPTERS | {"sumie-ui"},
})
UNSAFE_CRATES = {"sumie-vt", "sumie-font", "sumie-render"}
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
CARGO_RECORDS = ("compiler-artifact", "compiler-message", "build-script-executed", "build-finished")

RUST_TOKEN = re.compile(
    r'(?P<line>//[^\n]*)|(?P<block>/\*)|'
    r'(?:r|br|cr)(?P<hashes>\#*)"[\s\S]*?"(?P=hashes)|'
    r'(?:b|c)?"(?:[^"\\]|\\[\s\S])*"|'
    r"b?'(?:[^'\\\n]|\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|[^\n]))'|"
    r'(?:r\#)?\w+|\S'
)
BLOCK_DELIMITER = re.compile(r'/\*|\*/')


class CannotCheck(Exception):
    pass


def run(*args):
    return subprocess.run(args, cwd=ROOT, capture_output=True)


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


def cargo_record(line):
    if not line.startswith("{"):
        return None
    try:
        record = json.loads(line)
    except json.JSONDecodeError:
        return None
    if isinstance(record, dict) and record.get("reason") in CARGO_RECORDS:
        return record
    return None


def check_cargo(gate, args):
    result = run("cargo", *args)
    violation = False
    passed = 0
    for line in result.stdout.decode(errors="replace").splitlines():
        message = cargo_record(line) if gate != "fmt" else None
        if message is not None:
            if message["reason"] == "compiler-message" and isinstance(message.get("message"), dict):
                diagnostic = message["message"]
                if "rendered" in diagnostic and "level" in diagnostic:
                    print(diagnostic["rendered"] or "", end="")
                    violation |= diagnostic["level"] == "error"
        else:
            print(line)
            if gate == "test" and (summary := TEST_SUMMARY.match(line)):
                violation |= summary[1] == "FAILED"
                passed += int(summary[2])
            elif gate == "fmt":
                violation |= line.startswith("Diff in ")
    print(result.stderr.decode(errors="replace"), end="", file=sys.stderr)
    if result.returncode == 0:
        if gate == "test" and passed == 0:
            print("test: no passing workspace tests", file=sys.stderr)
            return 1
        if gate == "test":
            print(f"test: {passed} workspace tests passed")
        return 0
    if violation:
        return 1
    raise CannotCheck(f"cargo {args[0]} exited {result.returncode} without a check diagnostic")


def main():
    commands = {
        "fmt": ("fmt", "--all", "--check"),
        "clippy": ("clippy", "--locked", "--workspace", "--all-targets", "--message-format=json", "--", "-D", "warnings"),
        "test": ("test", "--locked", "--workspace", "--message-format=json"),
    }
    parser = argparse.ArgumentParser()
    parser.add_argument("gate", choices=("structure", *commands))
    gate = parser.parse_args().gate
    os.environ["GIT_OPTIONAL_LOCKS"] = "0"
    os.environ["CARGO_TERM_COLOR"] = "never"
    cache = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
    os.environ.setdefault("CARGO_TARGET_DIR", str(cache / "sumie" / "target"))
    print(f"== {gate}: {ROOT}", flush=True)
    try:
        return check_structure() if gate == "structure" else check_cargo(gate, commands[gate])
    except (CannotCheck, OSError, KeyError, ValueError) as error:
        print(f"{gate}: could not run: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
