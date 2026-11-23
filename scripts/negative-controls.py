from contextlib import ExitStack
import os
from pathlib import Path
import signal
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
GATE = ROOT / "scripts/check-gate.py"
CORE = "crates/sumie-core/src/lib.rs"
VT = "crates/sumie-vt/src/lib.rs"
WAYLAND = "crates/sumie-wayland/src/lib.rs"
CORE_MANIFEST = "crates/sumie-core/Cargo.toml"
RENDER_MANIFEST = "crates/sumie-render/Cargo.toml"
UI_MANIFEST = "crates/sumie-ui/Cargo.toml"
UNSAFE_READ = ("#[must_use]\npub fn read_probe() -> u8 {\n"
               "    let bytes = [0_u8; 1];\n"
               "    unsafe { std::ptr::read(bytes.as_ptr()) }\n}\n")


class GateUnavailable(Exception):
    pass


def git_status():
    return subprocess.check_output(
        ["git", "-C", ROOT, "status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )


def append(text):
    return lambda original: original + "\n" + text


def replace_once(old, new):
    def change(original):
        if original.count(old) != 1:
            raise RuntimeError(f"expected one anchor {old!r}")
        return original.replace(old, new, 1)
    return change


def restore(path, original):
    path.write_bytes(original)
    if path.read_bytes() != original:
        raise RuntimeError(f"could not restore {path}")


def probe(name, gate, file, change, marker, expected=1, *, new=False):
    path = ROOT / file
    original = b"" if new else path.read_bytes()
    edited = change(original.decode()).encode()
    if edited == original:
        raise RuntimeError(f"{name}: probe did not change {file}")
    print(f"\n== probe {name}: {file} ({gate})", flush=True)
    with ExitStack() as cleanup:
        if new:
            if not path.parent.is_dir():
                path.parent.mkdir()
                cleanup.callback(path.parent.rmdir)
            with path.open("xb") as handle:
                cleanup.callback(path.unlink)
                handle.write(edited)
        else:
            cleanup.callback(restore, path, original)
            path.write_bytes(edited)
        if path.read_bytes() != edited:
            raise RuntimeError(f"{name}: probe did not land in {file}")
        result = subprocess.run([sys.executable, GATE, gate], cwd=ROOT, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        print(result.stdout, end="")
        if result.returncode == 2:
            raise GateUnavailable(f"{name}: {gate} could not run (exit 2)")
        if result.returncode != expected or marker not in result.stdout:
            raise RuntimeError(f"{name}: expected exit {expected} and {marker!r}, got exit {result.returncode}")
    print(f"{name}: {'RED' if expected else 'GREEN'} as expected (exit {result.returncode}); restored {file}")


def controls():
    probe("shell drops command status", "shell", "shell-integration/bash/sumie.bash",
          replace_once("133;D;%d", "133;D"), "bash/sumie.bash: missing 133;D;%d")
    probe("terminfo replaces Tc with RGB", "terminfo", "terminfo/xterm-sumie.terminfo",
          replace_once("\tTc,", "\tRGB,"),
          "xterm-sumie: missing or invalid capability Tc\nxterm-sumie: RGB must be query-only")
    for name, manifest, dep, expected in (
        ("sumie-render", RENDER_MANIFEST, "sumie-font", 1),
        ("sumie-core", CORE_MANIFEST, "sumie-config", 1),
        ("sumie-core", CORE_MANIFEST, "sumie-macos", 1),
        ("sumie-ui", UI_MANIFEST, "sumie-font", 0),
    ):
        # Exercise renamed and inactive dev-edges, not just the host's resolved graph.
        edit = append(f'''[target.'cfg(target_os = "freebsd")'.dev-dependencies]
edge_probe = {{ package = "{dep}", path = "../{dep}" }}
''')
        edge = f"{name} -> {dep}"
        marker = f"dependencies: forbidden edge {edge}" if expected else "dependencies: scanned"
        probe(edge, "structure", manifest, edit, marker, expected)
    probe("missing unsafe forbid", "structure", CORE,
          replace_once("#![forbid(unsafe_code)]", "#![deny(unsafe_code)]"),
          f"unsafe: {CORE}: missing crate-level #![forbid(unsafe_code)]")
    for name, path in (("VT", VT), ("Wayland", WAYLAND)):
        probe(f"{name} unsafe exemption", "structure", path, append(UNSAFE_READ),
              "exempt", expected=0)
    probe("crate attributes after doc comment with BOM and CRLF", "structure", CORE,
          lambda source: ("\ufeff//! Core crate.\n#![doc(hidden)]\n" + source).replace("\n", "\r\n"),
          "unsafe: scanned", expected=0)
    for name, attribute, lint in (
        ("allow", "allow(clippy::too_many_lines)", "too_many_lines"),
        ("expect", 'cfg_attr(unix, expect(clippy::pedantic, reason = "probe"))', "pedantic"),
    ):
        probe(f"lint suppression ({name})", "structure", CORE,
              append(f"#[{attribute}]\npub fn lint_probe() {{}}\n"),
              f"lints: {CORE}: forbidden suppression of clippy::{lint}")
    for name, path, edit, marker, new in (
        ("crate-local Clippy config", "crates/sumie-core/clippy.toml",
         append("too-many-lines-threshold = 100\n"), "only the root clippy.toml is allowed", True),
        ("missing lint inheritance", CORE_MANIFEST, replace_once("[lints]\nworkspace = true\n", ""),
         f"lints: {CORE_MANIFEST}: expected [lints] workspace = true", False),
        ("unlisted crate directory", "crates/sumie-orphan/Cargo.toml",
         append('[package]\nname = "sumie-orphan"\nversion = "0.0.0"\n'),
         "workspace: crates/sumie-orphan: directory is not a workspace member", True),
    ):
        probe(name, "structure", path, edit, marker, new=new)
    probe("file length", "structure", CORE, append("// Deliberate line-limit probe.\n" * 601),
          "lines exceeds 600")
    probe("forbidden module", "structure", CORE, append("mod /* nested /* comment */ */ r#helper {}\n"),
          f"names: {CORE}: forbidden module name helper")
    probe("forbidden crate name", "structure", RENDER_MANIFEST, append('[lib]\nname = "common"\n'),
          "names: forbidden crate name common")
    probe("legal lookalike names", "structure", CORE,
          append('/* outer /* mod misc {} */ mod common; */\n'
                 'const _: &str = r#"mod helper {}"#;\n'
                 'pub mod helpful {}\n'), "names: scanned", expected=0)
    probe("formatting", "fmt", CORE, append("pub struct FormatProbe{pub value:u8}\n"), "Diff in ")
    probe("Clippy pedantic", "clippy", CORE,
          append("#[must_use]\npub fn narrow_probe(value: u16) -> u8 {\n    value as u8\n}\n"),
          "clippy::cast_possible_truncation")

    long_body = "    value = value.wrapping_add(1);\n" * 80 + "    value\n"
    probe("function length", "clippy", CORE,
          append("#[must_use]\npub fn line_probe(mut value: u8) -> u8 {\n" + long_body + "}\n"),
          "(81/80)")

    branches = "".join(
        f"    if flags & {1 << bit} != 0 {{\n        count += 1;\n    }}\n"
        for bit in range(15)
    )
    probe("cognitive complexity", "clippy", CORE,
          append("#[must_use]\npub fn complexity_probe(flags: u16) -> u8 {\n"
                 "    let mut count = 0;\n" + branches + "    count\n}\n"),
          "cognitive complexity of (16/15)")
    unsafe_path = WAYLAND if sys.platform.startswith("linux") else VT
    probe("undocumented unsafe block", "clippy", unsafe_path, append(UNSAFE_READ),
          "unsafe block missing a safety comment")
    probe("unit test failure", "test", CORE,
          append("#[test]\nfn test_probe() -> Result<(), &'static str> {\n"
                 '    Err("deliberate unit test failure")\n}\n'),
          "test test_probe ... FAILED")
    probe("contrast above the supported range", "test", "crates/sumie-config/src/lib.rs",
          replace_once("(1.0..=21.0).contains(&ratio)", "(1.0..=22.0).contains(&ratio)"),
          "test tests::load_configuration ... FAILED")
    probe("wide cell split", "test", VT,
          replace_once("ffi::CELL_WIDE_WIDE => 2,", "ffi::CELL_WIDE_WIDE => 1,"),
          "test tests::snapshot_keeps_wide_cells_whole ... FAILED")
    probe("oversized OSC 52 clipboard write", "test", "crates/sumie-vt/src/effects.rs",
          replace_once("if len > OUTPUT_LIMIT - self.clipboard_bytes {",
                       "if len > OUTPUT_LIMIT + 1 - self.clipboard_bytes {"),
          "test effects::tests::osc52_queues_only_bounded_standard_writes ... FAILED")
    probe("missing title event", "test", "crates/sumie-vt/src/effects.rs",
          replace_once(".chain(self.title.take().map(Event::Title))",
                       ".chain(self.title.take().filter(|_| false).map(Event::Title))"),
          "test effects::tests::osc2_queues_latest_title ... FAILED")
    probe("selection splits a wide glyph", "test", "crates/sumie-core/src/selection.rs",
          replace_once("end.column += 1;", "end.column += 0;"),
          "test selection::tests::drag_selects_whole_cell_groups ... FAILED")
    probe("alternate scroll on primary screen", "test", "crates/sumie-core/src/mouse.rs",
          replace_once("modes.alternate && modes.alternate_scroll", "modes.alternate_scroll"),
          "test mouse::tests::routes_mouse_by_mode_and_gesture ... FAILED")
    probe("unchanged settings request a contrast redraw", "test", "crates/sumie-config/src/changes.rs",
          replace_once("!= next.minimum_contrast().to_bits()",
                       "== next.minimum_contrast().to_bits()"),
          "test changes::tests::applies_only_changed_settings ... FAILED")
    if sys.platform.startswith("linux"):
        probe("sprite shifted off cell origin", "test", "crates/sumie-font/src/sprite.rs",
              replace_once("top: 0,", "top: 1,"),
              "test tests::linux::sprites_fill_their_grid_cells ... FAILED")
        probe("preedit written to PTY", "test", "src/input.rs",
              replace_once("Event::ImePreedit { text, .. } => return Ok(Effect::Preedit(text)),",
                           "Event::ImePreedit { ref text, .. } => PaneCommand::Text(text.as_bytes()),"),
              "test tests::local_pane_executes_committed_input_without_preedit ... FAILED")
        probe("paste queued without framing", "test", "src/pane.rs",
              replace_once("self.terminal.encode_paste(text)?", "text.into_bytes()"),
              "test tests::bracketed_paste_fits_or_leaves_the_queue_unchanged ... FAILED")
        probe("matched shortcut falls through to encoding", "test", "src/input.rs",
              replace_once("Some(Shortcut::Copy) => return Ok(Effect::Copy(serial)),",
                           "Some(Shortcut::Copy) => (),"),
              "test tests::matched_shortcuts_never_reach_the_pty ... FAILED")
    if sys.platform == "darwin":
        probe("Core Text bypasses the fallback cascade", "test", "crates/sumie-font/src/discovery/core_text.rs",
              replace_once("let fallback = ch.map(", "let fallback = ch.filter(|_| false).map("),
              "test tests::core_text_loads_the_named_collection_face_and_cjk_fallback ... FAILED")


def interrupted(_signum, _frame):
    raise KeyboardInterrupt


def main():
    os.environ["GIT_OPTIONAL_LOCKS"] = "0"
    # Restoring a probed file can keep its old mtime, so Cargo would reuse the probe's
    # artifacts in the next normal run. Probes build in their own target directory.
    target = os.environ.get("CARGO_TARGET_DIR") or str(ROOT / "target")
    os.environ["CARGO_TARGET_DIR"] = os.path.join(target, "negative-controls")
    signal.signal(signal.SIGHUP, interrupted)
    signal.signal(signal.SIGTERM, interrupted)
    print(f"== negative controls: {ROOT}", flush=True)
    try:
        before = git_status()
        lock = ROOT / "Cargo.lock"
        lock_before = lock.read_bytes()
        try:
            controls()
        finally:
            if git_status() != before or lock.read_bytes() != lock_before:
                raise RuntimeError("Git porcelain status or Cargo.lock changed during probes")
            print("Restored: Git porcelain status and Cargo.lock match before/after.")
    except RuntimeError as error:
        print(f"negative controls: {error}", file=sys.stderr)
        return 1
    except (GateUnavailable, OSError, UnicodeError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        print(f"negative controls: could not run: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
