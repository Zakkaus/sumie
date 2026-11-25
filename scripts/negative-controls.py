from contextlib import ExitStack
import os
from pathlib import Path
import re
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
# A default-mode-only performSelector wake. objc2 marks performSelector unsafe, so the
# probe also lifts sumie-macos's unsafe forbid; only this temporary build sees either.
DEFAULT_MODE_WAKE = """    let send = move || {
        let _ = proxy.send_event(event.clone());
    };
    let _ = DEFAULT_MODE_WAKE.set(std::sync::Mutex::new(Box::new(send)));
    || {
        let target: objc2::rc::Retained<DefaultModeWake> =
            unsafe { objc2::msg_send![<DefaultModeWake as objc2::ClassType>::class(), new] };
        let modes = objc2_foundation::NSArray::from_slice(&[objc2_foundation::ns_string!(
            "kCFRunLoopDefaultMode"
        )]);
        let () = unsafe {
            objc2::msg_send![&*target, performSelectorOnMainThread: objc2::sel!(wake),
                withObject: None::<&objc2::runtime::AnyObject>, waitUntilDone: false,
                modes: &*modes]
        };
    }
}

static DEFAULT_MODE_WAKE: std::sync::OnceLock<std::sync::Mutex<Box<dyn Fn() + Send>>> =
    std::sync::OnceLock::new();

objc2::define_class!(
    #[unsafe(super(objc2::runtime::NSObject))]
    #[thread_kind = objc2::AnyThread]
    #[name = "SumieDefaultModeWake"]
    struct DefaultModeWake;

    impl DefaultModeWake {
        #[unsafe(method(wake))]
        fn wake(&self) {
            (DEFAULT_MODE_WAKE.get().unwrap().lock().unwrap())();
        }
    }
);
"""
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
    probe("spawn ignores descriptor creation", "test", "crates/sumie-pty/src/spawn.rs",
          replace_once("    let _spawning = spawning();\n    command.spawn()", "    command.spawn()"),
          "test spawn::tests::a_spawn_waits_for_a_descriptor_being_created ... FAILED")
    probe("Bash 3.2 lacks [[ -v", "shell", "shell-integration/bash/sumie.bash",
          replace_once("[[ -n ${SUMIE_ORIG_ENV+set} ]]", "[[ -v SUMIE_ORIG_ENV ]]"),
          "bash/sumie.bash: [[ -v needs Bash 4.2; macOS has 3.2")
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
    probe("git patch pinned to a branch", "structure", "Cargo.toml",
          lambda manifest: re.sub(r'(?m)^(winit = \{ git = "[^"]+", )rev = "[0-9a-f]{40}" \}$',
                                  r'\1branch = "sumie/0.30.13" }', manifest),
          "patch: [patch.crates-io] winit: pin a full rev, not a branch or tag")
    quit_patch = "third_party/winit/0001-quit.patch"
    # `append` adds a blank line, then the hunk; its added line is the fifth.
    added = len((ROOT / quit_patch).read_text(encoding="utf-8").splitlines()) + 6
    probe("debug print in the winit fork patch", "structure", quit_patch,
          append('--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1,2 @@\n fn probe() {}\n+eprintln!("probe");\n'),
          f"patch: {quit_patch}:{added}: debug print in an added line")
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
    probe("Super matches a shortcut", "test", CORE,
          replace_once("if !matches!(action, KeyAction::Press) || modifiers.super_key {",
                       "if !matches!(action, KeyAction::Press) {"),
          "test tests::super_never_matches_a_shortcut ... FAILED")
    probe("closing an earlier tab loses the active pane", "test", "crates/sumie-core/src/tabs.rs",
          replace_once("self.active -= 1;", "self.active = 0;"),
          "test tabs::tests::close_preserves_active_identity_or_selects_a_neighbor ... FAILED")
    probe("three pixels lose their split divider", "test", "crates/sumie-core/src/splits/geometry.rs",
          replace_once("slack != 0 && !rect.is_empty()", "slack > 1 && !rect.is_empty()"),
          "test splits::tests::tiny_windows_partition_every_pixel ... FAILED")
    probe("contrast above the supported range", "test", "crates/sumie-config/src/lib.rs",
          replace_once("(1.0..=21.0).contains(&ratio)", "(1.0..=22.0).contains(&ratio)"),
          "test tests::load_configuration ... FAILED")
    probe("wide cell split", "test", VT,
          replace_once("ffi::CELL_WIDE_WIDE => 2,", "ffi::CELL_WIDE_WIDE => 1,"),
          "test tests::snapshot_keeps_wide_cells_whole ... FAILED")
    probe("snapshot ignores bold style", "test", "crates/sumie-vt/src/style.rs",
          replace_once("let flags = native.flags;", "let flags = native.flags & !ffi::STYLE_BOLD;"),
          "test style::tests::snapshot_keeps_cell_style ... FAILED")
    probe("DECSCUSR underline read as bar", "test", "crates/sumie-vt/src/cursor.rs",
          replace_once("ffi::CURSOR_STYLE_UNDERLINE => CursorStyle::Underline,",
                       "ffi::CURSOR_STYLE_UNDERLINE => CursorStyle::Bar,"),
          "test cursor::tests::decscusr_controls_shape_and_blink_until_reset ... FAILED")
    probe("hollow cursor border widened", "test", "crates/sumie-render/src/grid.rs",
          replace_once("solid([x, y + 1], [1, h - 2]);", "solid([x, y + 1], [2, h - 2]);"),
          "test grid_tests::grid_on_selected_backend ... FAILED")
    probe("block cursor covers cell glyphs", "test", "crates/sumie-render/src/grid.rs",
          lambda source: replace_once("if block.is_none()\n            && let Some(cursor)",
                                      "if let Some(cursor)")(
              replace_once("block.cursor.write(|instance| write(self.place(instance)));",
                           "let _ = block;")(source)),
          "test grid_tests::grid_on_selected_backend ... FAILED")
    probe("oversized OSC 52 clipboard write", "test", "crates/sumie-vt/src/effects.rs",
          replace_once("if len > OUTPUT_LIMIT - self.clipboard_bytes {",
                       "if len > OUTPUT_LIMIT + 1 - self.clipboard_bytes {"),
          "test effects::tests::osc52_queues_only_bounded_standard_writes ... FAILED")
    probe("VT ignores the Kitty thumbnail row cap", "test", "crates/sumie-vt/src/graphics.rs",
          replace_once("sumie_set_thumbnail_rows(self.raw, rows)",
                       "sumie_set_thumbnail_rows(self.raw, rows.min(0))"),
          "test tests::kitty_thumbnails_size_and_advance_before_rendering ... FAILED")
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
    probe("mouse tracking bypass ignores Shift", "test", "crates/sumie-core/src/mouse.rs",
          replace_once("modes.tracking && !event.modifiers.shift", "modes.tracking"),
          "test mouse::tests::routes_mouse_by_mode_and_gesture ... FAILED")
    probe("double click remains cell-wise", "test", "crates/sumie-core/src/mouse.rs",
          replace_once("Some((_, _, _, SelectionUnit::Cell)) => SelectionUnit::Word,",
                       "Some((_, _, _, SelectionUnit::Cell)) => SelectionUnit::Cell,"),
          "test mouse::tests::routes_mouse_by_mode_and_gesture ... FAILED")
    probe("logical line selection clips the final wide cell", "test", "crates/sumie-vt/src/selection.rs",
          replace_once("end.column += 1;", "end.column += 0;"),
          "test selection::tests::selects_words_and_wrapped_lines ... FAILED")
    probe("unchanged settings request a contrast redraw", "test", "crates/sumie-config/src/changes.rs",
          replace_once("!= next.minimum_contrast().to_bits()",
                       "== next.minimum_contrast().to_bits()"),
          "test changes::tests::applies_only_changed_settings ... FAILED")
    if sys.platform.startswith("linux"):
        probe("IME caret loses the right edge at fractional scale", "test", "src/view/geometry.rs",
              replace_once("bounds.end[i] * logical + device - 1", "bounds.end[i] * logical"),
              "test view::geometry::tests::caret_covers_the_cell_on_a_rounded_fractional_buffer ... FAILED")
        probe("unchanged animation deadline rearms the timer", "test", "src/app/animation.rs",
              replace_once("if next == self.timer.map(|(at, _)| at) {", "if next.is_none() && self.timer.is_none() {"),
              "test app::animation::tests::wake_keeps_an_unchanged_deadline_and_cancels_a_stopped_animation ... FAILED")
        probe("explicit cell backgrounds become translucent", "test", "src/view/paint.rs",
              replace_once("if default {", "if !highlighted && !covered {"),
              "test view::tests::background_opacity_respects_vt_colors_and_opaque_overlays ... FAILED")
        probe("idle OSC 8 underline disappears", "test", "src/view/attributes.rs",
              replace_once("style.hyperlink || hovered.is_some()", "hovered.is_some()"),
              "test view::attributes::tests::link_underline_precedence ... FAILED")
        probe("Ctrl+click link opens on press", "test", "crates/sumie-core/src/mouse.rs",
              replace_once("Some(event.pixel.map(f32::to_bits))));\n                Dispatch::Ignore",
                           "Some(event.pixel.map(f32::to_bits))));\n                Dispatch::OpenLink"),
              "test tests::ctrl_click_a_link_bypasses_mouse_tracking ... FAILED")
        probe("pane ink crosses the divider", "test", "crates/sumie-render/src/renderer.rs",
              replace_once("let scissor = pane.rect;", "let scissor = full;"),
              "test grid_tests::splits::gpu_split_pixels ... FAILED")
        probe("VT image dropped from the frame", "test", "src/view/paint.rs",
              replace_once("self.pane.images = (!snapshot.images.is_empty()).then(|| Arc::clone(&snapshot.images));",
                           "self.pane.images = None;"),
              "test grid_tests::grid_on_selected_backend ... FAILED")
        probe("terminal grid starts at the pane edge", "test", "src/view.rs",
              replace_once("pane.glyphs.offset = [grid.x.cast_signed(), grid.y.cast_signed()];",
                           "pane.glyphs.offset = [rect.x.cast_signed(), rect.y.cast_signed()];"),
              "test grid_tests::grid_on_selected_backend ... FAILED")
        probe("two tabs lose their bar", "test", "crates/sumie-ui/src/tabs.rs",
              replace_once("if count < 2 {", "if count < 3 {"),
              "test grid_tests::grid_on_selected_backend ... FAILED")
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
              replace_once("Some(Shortcut::Copy) => return Ok(Effect::Copy),",
                           "Some(Shortcut::Copy) => (),"),
              "test tests::matched_shortcuts_never_reach_the_pty ... FAILED")
        probe("search commit leaks to PTY", "test", "src/input.rs",
              replace_once("Event::ImeCommit { text, .. } | Event::ClipboardText { text, .. } if searching",
                           "Event::ClipboardText { text, .. } if searching"),
              "test tests::matched_shortcuts_never_reach_the_pty ... FAILED")
        probe("paste reply uses the current recipient", "test", "src/input.rs",
              replace_once("| Event::ClipboardText { generation, .. }", ""),
              "test tests::matched_shortcuts_never_reach_the_pty ... FAILED")
        probe("IME preedit applied before its commit", "test", "crates/sumie-wayland/src/ime.rs",
              replace_once("|| has_preedit {\n            events.push_back(Event::ImePreedit {",
                           "|| has_preedit {\n            events.push_front(Event::ImePreedit {"),
              "test ime::tests::done_applies_text_even_when_serial_defers_outgoing_state ... FAILED")
        probe("IME caret loses the left edge at fractional scale", "test", "src/view/geometry.rs",
              replace_once("let left = i64::from(bounds.start[i]) * logical / device;",
                           "let left = (i64::from(bounds.start[i]) * logical + device - 1) / device;"),
              "test view::geometry::tests::caret_covers_the_cell_on_a_rounded_fractional_buffer ... FAILED")
        probe("pointer hits use the nominal scale", "test", "src/view/geometry.rs",
              replace_once("[x, y][i] * f64::from(self.device[i]) / f64::from(self.logical[i])",
                           "[x, y][i] * f64::from(self.scale) / 120.0"),
              "test view::tests::odd_fractional_sizes_hit_by_the_per_axis_viewporter_ratio ... FAILED")
        probe("device pixels pass through the rounded point extent", "test", "src/view/geometry.rs",
              replace_once("Position::DevicePx(x, y) => [x, y],",
                           "Position::DevicePx(x, y) => {\n"
                           "                let points = |px: f64| px * 120.0 / f64::from(self.scale);\n"
                           "                self.window_pixel(Position::SurfaceLogical(points(x), points(y)))\n"
                           "            }"),
              "test view::tests::device_pixel_hits_ignore_the_rounded_point_extent ... FAILED")
        probe("a batch retires exited panes before its effects", "test", "src/app/frame_steps.rs",
              replace_once("    if batch.effects()? {\n        return Ok(true);\n    }\n"
                           "    Ok(exited && batch.retire()?)",
                           "    if exited && batch.retire()? {\n        return Ok(true);\n    }\n"
                           "    batch.effects()"),
              "test app::frame_steps::tests::a_batch_runs_ready_ptys_then_effects_then_retirement ... FAILED")
        probe("IME accepts the prior recipient's serial", "test", "crates/sumie-wayland/src/ime.rs",
              replace_once("self.enabled_at = Some(self.serial.wrapping_add(1));",
                           "self.enabled_at = Some(self.serial);"),
              "test ime::tests::reenable_discards_pending_and_old_serials_across_wrap ... FAILED")
        probe("search reserves a terminal row", "test", "src/view/geometry.rs",
              replace_once("Size {\n            cols,\n            rows,",
                           "Size {\n            cols,\n            rows: rows - u16::from(self.search_bar.is_some()),"),
              "test grid_tests::search_keeps_terminal_dimensions ... FAILED")
        probe("zoom leaves the font at its configured size", "test", "src/view.rs",
              replace_once("self.replace_font(self.font_settings.at_scale(self.scale, points)?);",
                           "self.replace_font(self.font_settings.at_scale(self.scale, None)?);"),
              "test grid_tests::font_zoom_reflows_grid_and_reset_restores_config_size ... FAILED")
        probe("search wheel reaches terminal mouse routing", "test", "src/input.rs",
              replace_once("return pane.search_pointer(event);",
                           "if !matches!(event.phase, sumie_core::mouse::MousePhase::Wheel(_)) {\n"
                           "            return pane.search_pointer(event);\n"
                           "        }"),
              "test tests::search_pointer_only_scrolls_history ... FAILED")
    if sys.platform == "darwin":
        probe("pedantic lint in macos-ci code", "clippy-macos-ci", "crates/sumie-macos/src/runner.rs",
              append("#[must_use]\npub fn narrow_probe(value: u16) -> u8 {\n    value as u8\n}\n"),
              "clippy::cast_possible_truncation")
        probe("default-mode-only performSelector wake", "macos-wake", "crates/sumie-macos/src/lib.rs",
              lambda source: replace_once("#![forbid(unsafe_code)]\n", "")(
                  replace_once("    move || {\n        let _ = proxy.send_event(event.clone());\n    }\n}\n",
                               DEFAULT_MODE_WAKE)(source)),
              "macos-wake: event-tracking: no wake inside the 300 ms interval")
        probe("exit watch removed on the first NOTE_EXIT", "test", "src/macos/sources.rs",
              replace_once("        self.take_ready(id).exit\n",
                           "        let exit = self.take_ready(id).exit;\n"
                           "        if exit {\n"
                           "            self.readiness.remove(Key::new(id, Interest::Exit));\n"
                           "        }\n"
                           "        exit\n"),
              "test macos::sources::tests::an_unreaped_exit_keeps_reporting_until_unregistered ... FAILED")
        probe("wake for a removed key delivered", "test", "crates/sumie-macos/src/readiness.rs",
              replace_once("        let live = !state.ready.is_empty();\n", "        let live = true;\n"),
              "test macos::sources::tests::a_report_queued_before_unregister_never_surfaces_after_it ... FAILED")
        probe("no re-watch after an early NOTE_EXIT", "test", "crates/sumie-pty/src/child/macos.rs",
              replace_once("            self.rewatch()?;\n        }\n        Ok(status",
                           "        }\n        Ok(status"),
              "test child::platform::tests::early_note_exit_is_watched_until_reaped ... FAILED")
        probe("frame cap checked after allocation", "test", "src/reload/frame.rs",
              lambda source: replace_once(
                  "        if size > MAX_BODY {\n            return Err(FrameError::TooLong(length));\n"
                  "        }\n        Ok((kind, size))", "        Ok((kind, size))")(
                  replace_once("            self.body = Some((kind, Vec::with_capacity(length), length));\n",
                               "            self.body = Some((kind, Vec::with_capacity(length), length));\n"
                               "            if length > MAX_BODY {\n"
                               "                return Err(FrameError::TooLong(u32::try_from(length).unwrap()));\n"
                               "            }\n")(source)),
              "test reload::frame::tests::a_body_over_the_cap_is_rejected_before_allocation ... FAILED")
        probe("config child waited for in Drop", "test", "src/macos/reload.rs",
              replace_once("    fn drop(&mut self) {\n        if let Some((_, key)) = self.kill.take() {",
                           "    fn drop(&mut self) {\n        let _ = self.child.wait();\n"
                           "        if let Some((_, key)) = self.kill.take() {"),
              "test macos::reload::tests::dropping_a_blocked_child_does_not_wait ... FAILED")
        probe("Core Text bypasses every fallback step", "test", "crates/sumie-font/src/discovery/core_text.rs",
              replace_once("let Some(ch) = ch else {", "let Some(ch) = ch.filter(|_| false) else {"),
              "test tests::core_text_loads_the_named_collection_face_and_cjk_fallback ... FAILED")
        probe("Core Text system-wide fallback queries an uncovered character", "test",
              "crates/sumie-font/src/discovery/core_text.rs",
              replace_once("location: u32::from(ch).try_into().ok()?,", "location: 0x378,"),
              "test tests::core_text_system_wide_fallback_covers_cjk ... FAILED")
        probe("Core Text system-wide fallback runs for undrawable characters", "font-test",
              "crates/sumie-font/src/discovery/core_text.rs",
              replace_once("        if !drawable(ch) {\n", "        if !drawable(ch) && false {\n"),
              "test discovery::core_text::tests::undrawable_misses_skip_the_system_wide_step ... FAILED")
        probe("Core Text system-wide fallback waits for the worker without a bound", "test",
              "crates/sumie-font/src/discovery/core_text.rs",
              replace_once("self.answers.recv_timeout(SYSTEM_WIDE_WAIT)", "self.answers.recv()"),
              "test discovery::core_text::tests::a_slow_system_wide_lookup_is_pending_after_the_wait ... FAILED")
        login = "test macos::login::tests::"
        lsof = login + "a_login_shell_inherits_no_parent_descriptor ... "
        probe("bridge duplicate inherited across exec", "test", "crates/sumie-macos/src/readiness.rs",
              replace_once("        let fd = fd.try_clone_to_owned()?;\n",
                           "        let fd = fd.try_clone_to_owned()?;\n"
                           "        fcntl(&fd, FcntlArg::F_SETFD(FdFlag::empty()))?;\n"),
              lsof + "FAILED")
        # XNU never copies a kqueue into a child, so the lsof test stays green here; the flag
        # test catches it, and the duplicates above are what the bridge could leak.
        probe("bridge kqueue without FD_CLOEXEC", "test", "crates/sumie-macos/src/readiness.rs",
              replace_once("fcntl(&queue, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;",
                           "fcntl(&queue, FcntlArg::F_SETFD(FdFlag::empty()))?;"),
              "test readiness::tests::bridge_descriptors_close_on_exec ... FAILED")
        probe("PS0 exported to Bash 3.2", "test", "shell-integration/bash/sumie.bash",
              replace_once("\n_sumie_ran=0\n", "\n_sumie_ran=0\nexport PS0='\\e]133;C\\a'\n"),
              login + "bash_3_2_login_shell_reads_its_profile_once_and_marks_prompts ... FAILED")
        probe("Bash 3.2 bootstrap no-op resets $?", "test", "shell-integration/bash/sumie.bash",
              replace_once("""//"$_sumie_boot"/'${_sumie_ran+_sumie_keep}'}""", """//"$_sumie_boot"/:}"""),
              login + "bash::bash_3_2_profile_appends_after_the_bootstrap ... FAILED")
        probe("Bash 3.2 DEBUG trap not reinstalled", "test", "shell-integration/bash/sumie.bash",
              replace_once('|| eval "${_sumie_trap-}"', "|| :"),
              login + "bash::bash_3_2_reinstalls_its_trap_after_one_set_at_the_prompt ... FAILED")
        probe("stop forwarding keys", "macos-keys", "src/macos/window.rs",
              replace_once("            Native::Key(key) => self.key(key),\n", "            Native::Key(_) => (),\n"),
              "keys: plain: recorded nothing instead of")
        probe("report focus on ModifiersChanged", "macos-keys", "src/app/pane_input.rs",
              replace_once("            pane.pane.pointer_modifiers_changed(modifiers);\n",
                           "            pane.pane.pointer_modifiers_changed(modifiers);\n"
                           "            if modifiers == KeyModifiers::default() {\n"
                           "                input::report_focus(&mut pane.pane, false)?;\n"
                           "            }\n"),
              "keys: focus: 2 focus-out reports for one focus loss")
        probe("configure on Resized", "macos-resize", "src/app/window_events.rs",
              replace_once("session.view.request_resize(scale, logical, device);\n",
                           "session.view.request_resize(scale, logical, device);\n"
                           "                    self.gpu.resize(device);\n"),
              "configures ran while GPU work was pending")
        probe("present every turn even when clean", "macos-idle", "src/macos/window.rs",
              replace_once("    fn background_commit_needed(&self) -> bool {\n        false",
                           "    fn background_commit_needed(&self) -> bool {\n        true"),
              "presents within")
        probe("retry 50 ms after a drawable timeout", "macos-timeout", "src/macos/surface.rs",
              replace_once("        self.hold(now + wait)\n",
                           "        self.hold(now + wait.min(Duration::from_millis(50)))\n"),
              "timeout: acquired again")
        probe("free the frame slot on every wake", "macos-flood", "src/macos/driver.rs",
              replace_once("                open.window.watches.dispatch()?;\n",
                           "                open.window.watches.dispatch()?;\n"
                           "                crate::platform::WindowOps::recover_frame_slot(&mut open.window);\n"),
              "flood: more than one present per refresh")
        probe("wait for the first settings without a deadline", "macos-loading", "src/macos/driver.rs",
              replace_once("None if Instant::now() >= *first_by =>", "None if false && Instant::now() >= *first_by =>"),
              "loading: no present within")
        # Its marker is a rate, which hosted runners only record.
        if os.environ.get("SUMIE_TIMING") != "record":
            probe("an 8-byte PTY read buffer", "macos-throughput", "crates/sumie-pty/src/reader.rs",
                  replace_once("const CHUNK_SIZE: usize = 8192;", "const CHUNK_SIZE: usize = 8;"),
                  "(below 20 MB/s)")


def interrupted(_signum, _frame):
    raise KeyboardInterrupt


def main():
    os.environ["GIT_OPTIONAL_LOCKS"] = "0"
    if sys.argv[1:] == ["macos-quit"]:
        evidence = os.environ.get("SUMIE_SPIKE_EVIDENCE")
        if not evidence:
            sys.exit("macos-quit: set SUMIE_SPIKE_EVIDENCE to an evidence directory")
        return int(any(subprocess.call(["bash", ROOT / "scripts/macos-spike/quit.sh", evidence, case])
                          for case in ("synchronous", "nested-queued", "system-cancel")))
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
