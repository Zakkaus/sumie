import os
import re
import shutil
import signal
import subprocess
import sys

# Importing the control modules must not leave bytecode in the worktree: the controls compare
# its status before and after.
sys.dont_write_bytecode = True
from negative.input import input_controls  # noqa: E402
from negative.macos import macos_controls  # noqa: E402
from negative.probe import ROOT, GateUnavailable, append, git_status, probe, replace_once  # noqa: E402

CORE = "crates/sumie-core/src/lib.rs"
VT = "crates/sumie-vt/src/lib.rs"
WAYLAND = "crates/sumie-wayland/src/lib.rs"
CORE_MANIFEST = "crates/sumie-core/Cargo.toml"
RENDER_MANIFEST = "crates/sumie-render/Cargo.toml"
UI_MANIFEST = "crates/sumie-ui/Cargo.toml"
# Converts packed pixels on the main thread and uploads RGBA, skipping the GPU expansion.
CPU_EXPANSION = """        let rgba: Vec<u8>;
        let pixels = if image.format == PixelFormat::Rgba {
            pixels
        } else {
            rgba = pixels
                .chunks_exact(image.format.channels())
                .flat_map(|pixel| match *pixel {
                    [gray] => [gray, gray, gray, 255],
                    [gray, alpha] => [gray, gray, gray, alpha],
                    [r, g, b] => [r, g, b, 255],
                    _ => unreachable!(),
                })
                .collect();
            &rgba
        };
        if pixels.len() != image.size[0] as usize * image.size[1] as usize * 4 {
"""
UNSAFE_READ = ("#[must_use]\npub fn read_probe() -> u8 {\n"
               "    let bytes = [0_u8; 1];\n"
               "    unsafe { std::ptr::read(bytes.as_ptr()) }\n}\n")


def controls():
    probe("shell drops command status", "shell", "shell-integration/bash/sumie.bash",
          replace_once("133;D;%d", "133;D"), "bash/sumie.bash: missing 133;D;%d")
    probe("spawn ignores descriptor creation", "test", "crates/sumie-pty/src/spawn.rs",
          replace_once("    let _spawning = spawning();\n    command.spawn()", "    command.spawn()"),
          "test spawn::tests::a_spawn_waits_for_a_descriptor_being_created ... FAILED")
    probe("first-run hint lets the dismissing press through", "test", "src/app/first_run.rs",
          replace_once("pressed && hint.take().is_some()", "pressed && hint.take().is_some() && false"),
          "test app::first_run::tests::a_press_dismisses_the_hint_and_is_consumed ... FAILED")
    if sys.platform in ("linux", "darwin"):
        watcher = "watch.rs" if sys.platform == "linux" else "watch_macos.rs"
        probe("ignore the dangling target", "test", f"crates/sumie-config/src/{watcher}",
              replace_once("filter(|target| target != path)", "filter(|target| target != path && target.exists())"),
              "test tests::creating_a_dangling_config_target_reloads_settings ... FAILED")
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
    probe("VT unsafe exemption", "structure", VT, append(UNSAFE_READ), "exempt", expected=0)
    for path in (WAYLAND, "crates/sumie-wayland/src/platform.rs"):
        probe("Wayland unsafe outside the handle borrow", "structure", path, append(UNSAFE_READ),
              f"unsafe: {path}: only the raw window handle borrow may use unsafe")
    if sys.platform.startswith("linux"):
        probe("cursor surface resizes the window", "test", "crates/sumie-wayland/src/scale.rs",
              replace_once("if !is_window || self.viewport.is_some()",
                           "let _ = is_window;\n        if self.viewport.is_some()"),
              "test scale::tests::cursor_surface_scale_does_not_resize_the_window ... FAILED")
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
    quit_patch = "third_party/winit/0001-macos.patch"
    # `append` adds a blank line, then the hunk; its added line is the fifth.
    added = len((ROOT / quit_patch).read_text(encoding="utf-8").splitlines()) + 6
    probe("debug print in the winit fork patch", "structure", quit_patch,
          append('--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1,2 @@\n fn probe() {}\n+eprintln!("probe");\n'),
          f"patch: {quit_patch}:{added}: debug print in an added line")
    probe("file length", "structure", CORE, append("// Deliberate line-limit probe.\n" * 601),
          "lines exceeds 600")
    probe("forbidden module", "structure", CORE, append("mod /* nested /* comment */ */ r#helper {}\n"),
          f"names: {CORE}: forbidden module name helper")
    probe("literal tab tint", "structure", "crates/sumie-ui/src/palette.rs",
          replace_once("black(u8::MAX), alpha", "black(u8::MAX), if is_light(luminance) { 10 } else { 56 }"),
          "ui tokens: crates/sumie-ui/src/palette.rs: numeric styling argument to tint_over")
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
    probe("undocumented unsafe block", "clippy", VT, append(UNSAFE_READ),
          "unsafe block missing a safety comment")
    probe("unit test failure", "test", CORE,
          append("#[test]\nfn test_probe() -> Result<(), &'static str> {\n"
                 '    Err("deliberate unit test failure")\n}\n'),
          "test test_probe ... FAILED")
    probe("Super matches a shortcut", "test", "crates/sumie-core/src/shortcuts.rs",
          replace_once("            Platform::Linux if modifiers.super_key => return Ok(None),\n", ""),
          "test shortcuts::tests::super_never_matches_a_shortcut ... FAILED")
    probe("Cmd+Z zooms the pane on macOS", "test", "crates/sumie-core/src/shortcuts.rs",
          replace_once("        'w' => Shortcut::Tab(TabAction::Close),\n",
                       "        'w' => Shortcut::Tab(TabAction::Close),\n        'z' => Shortcut::Pane(PaneAction::Zoom),\n"),
          "test shortcuts::tests::macos_binds_the_linux_actions_to_command_by_position_then_character ... FAILED")
    probe("closing an earlier tab loses the active pane", "test", "crates/sumie-core/src/tabs.rs",
          replace_once("self.active -= 1;", "self.active = 0;"),
          "test tabs::tests::close_preserves_active_identity_or_selects_a_neighbor ... FAILED")
    probe("three pixels lose their split divider", "test", "crates/sumie-core/src/splits/geometry.rs",
          replace_once("slack != 0 && !rect.is_empty()", "slack > 1 && !rect.is_empty()"),
          "test splits::tests::tiny_windows_partition_every_pixel ... FAILED")
    probe("contrast above the supported range", "test", "crates/sumie-config/src/lib.rs",
          replace_once("(1.0..=21.0).contains(&ratio)", "(1.0..=22.0).contains(&ratio)"),
          "minimum-contrast = 21.00000001:")
    if sys.platform.startswith("linux"):
        probe("config load opens a FIFO with no writer", "test", "crates/sumie-config/src/load.rs",
              replace_once("check_with(path, visit, &mut read_file)",
                           "check_with(path, visit, &mut |path| fs::read_to_string(path))"),
              "test tests::fifo_include_is_rejected_without_a_writer ... FAILED")
    probe("font size above the supported range", "test", "crates/sumie-config/src/lib.rs",
          replace_once("size > 0.0 && size <= MAX_FONT_SIZE", "size > 0.0 && (size * POINT_TO_PX).is_finite()"),
          "test tests::oversized_font_sizes_are_rejected ... FAILED")
    probe("settings key without a title", "test", "crates/sumie-config/src/images.rs",
          replace_once(',\n        title = "Thumbnail rows"\n', '\n'),
          "test settings::tests::every_leaf_has_title_and_category ... FAILED")
    probe("settings problem reworded", "test", "crates/sumie-config/src/problem.rs",
          replace_once('"font size must be greater than 0 and at most {MAX_FONT_SIZE} points"',
                       '"font size must be positive and at most {MAX_FONT_SIZE} points"'),
          "test problem::tests::each_problem_is_typed_located_and_worded_as_before ... FAILED")
    # A full re-serialize loses comments; dropping them stands in for toml::to_string, which
    # this toml build does not compile.
    probe("settings write re-serializes without comments", "test", "crates/sumie-config/src/edit.rs",
          replace_once("    Ok(document.to_string())\n",
                       "    Ok(document.to_string().lines().filter(|line| !line.contains('#'))"
                       ".map(|line| format!(\"{line}\\n\")).collect())\n"),
          "test edit::tests::edit_keeps_comments_and_order ... FAILED")
    probe("settings write skips the identity check", "test", "crates/sumie-config/src/write.rs",
          replace_once("discard(failed(error)))? != current.stat {",
                       "discard(failed(error)))? != current.stat && false {"),
          "test edit::tests::rebase_on_foreign_write ... FAILED")
    probe("wide cell split", "test", VT,
          replace_once("ffi::CELL_WIDE_WIDE => 2,", "ffi::CELL_WIDE_WIDE => 1,"),
          "test tests::snapshot_keeps_wide_cells_whole ... FAILED")
    probe("snapshot updates during synchronized output", "test", "crates/sumie-vt/src/effects.rs",
          replace_once("if self.render_hold_deadline().is_some() {", "if false {"),
          "test tests::synchronized_output_holds_consecutive_frames_on_the_same_row ... FAILED")
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
    if sys.platform.startswith("linux"):
        probe("Kitty file transfer accepts hard links", "test", "crates/sumie-vt/src/image_file.zig",
              replace_once(" or stat.nlink != 1", ""),
              "Kitty hard-link query returned OK")
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
        probe("queued IME commit uses the focused pane", "test", "src/app/tabs.rs",
              replace_once("        | Event::ImeCommit {\n            recipient: generation,\n            ..\n        } = event",
                           "        = event"),
              "test app::tabs::tests::queued_ime_commit_reaches_the_original_pane ... FAILED")
        probe("old IME serial drops committed text", "test", "crates/sumie-wayland/src/ime.rs",
              replace_once("        let recipient = if current {",
                           "        if !current { return; }\n        let recipient = if current {"),
              "test ime::tests::late_done_delivers_commit_to_its_serial_owner_without_preedit ... FAILED")
        probe("reaping a child skips ready output", "test", "src/app/tabs.rs",
              replace_once("            serviced.exited = self.pane.pty.exit_fd().is_none();",
                           "            serviced.exited = self.pane.pty.exit_fd().is_none();\n"
                           "            if serviced.exited { return Ok(serviced); }"),
              "test app::tabs::tests::exit_readiness_keeps_final_terminal_clipboard_write ... FAILED")
        probe("Wayland drops keyboard focus changes", "test", "crates/sumie-wayland/src/keyboard.rs",
              replace_once("(focused != was_focused).then_some(Event::KeyboardFocus(focused))",
                           "let _ = (focused, was_focused);\n        None"),
              "test keyboard::tests::reports_focus_loss_and_regain_without_repeating_unchanged_focus ... FAILED")
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
              replace_once("Some(event.pixel.map(f32::to_bits))));\n            return Dispatch::Ignore;",
                           "Some(event.pixel.map(f32::to_bits))));\n            return Dispatch::OpenLink;"),
              "test tests::ctrl_click_a_link_bypasses_mouse_tracking ... FAILED")
        probe("pane ink crosses the divider", "test", "crates/sumie-render/src/renderer.rs",
              replace_once("let scissor = pane.rect;", "let scissor = full;"),
              "test grid_tests::splits::gpu_split_pixels ... FAILED")
        probe("VT image dropped from the frame", "test", "src/view/paint.rs",
              replace_once("self.pane.images = (!snapshot.images.is_empty()).then(|| Arc::clone(&snapshot.images));",
                           "self.pane.images = None;"),
              "test grid_tests::grid_on_selected_backend ... FAILED")
        probe("RGB frames expanded on the CPU", "test", "crates/sumie-render/src/images/upload.rs",
              replace_once("        if image.format != PixelFormat::Rgba {\n", CPU_EXPANSION),
              "test grid_tests::images::a_4k_kitty_frame_expands_on_the_gpu ... FAILED")
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
              replace_once("Event::ImePreedit { text, cursor, .. } => return Ok(Effect::Preedit(text, cursor)),",
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
    if sys.platform == "linux" and shutil.which("kwin_wayland"):
        linux_clipboard_controls()
        close_pane = ("    pub(super) fn close_pane(\n        &mut self,\n        index: usize,\n"
                      "        id: u64,\n        window: &mut impl WindowOps,\n    ) -> Result<()> {\n")
        for name, leak, marker in (
            ("closing a pane leaks a descriptor", 'std::mem::forget(std::fs::File::open("/dev/null")?);',
             "fd count above baseline"),
            ("closing a pane leaks a thread",
             "std::thread::spawn(|| loop { std::thread::park(); });", "thread count above baseline"),
        ):
            probe(name, "linux-panes", "src/app/tabs.rs",
                  replace_once(close_pane, f"{close_pane}        {leak}\n"), marker)
        probe("SIGKILL escalation removed", "linux-panes", "crates/sumie-pty/src/child.rs",
              replace_once("            signal_group(self.pid, Signal::KILL)?;\n", ""),
              "the shell was not reaped within 6 s of the close")
    input_controls()
    macos_controls()
    if sys.platform.startswith("linux") and shutil.which("kwin_wayland"):
        pane_controls()
    else:
        print("\n== skipped: the pane budget controls need kwin_wayland", flush=True)


def linux_clipboard_controls():
    probe("drag does not set primary selection", "linux-clipboard",
          "crates/sumie-wayland/src/clipboard.rs",
          replace_once("        if on_release && let Some(bytes) = &bytes {\n"
                       "            self.write_primary(serial, Rc::clone(bytes));\n"
                       "        }\n", "        let _ = on_release;\n"),
          "FAIL: drag release did not publish the primary selection")
    probe("middle click pastes clipboard instead of primary", "linux-clipboard", "src/linux.rs",
          replace_once(".read_primary(sumie_pty::INPUT_LIMIT, recipient)",
                       ".read_clipboard(sumie_pty::INPUT_LIMIT, recipient)"),
          "FAIL: middle click did not paste primary text only")
    probe("middle click pastes under mouse tracking", "linux-clipboard", "crates/sumie-core/src/mouse.rs",
          replace_once("            && !modes.tracking\n        {\n            self.reset_clicks();",
                       "        {\n            self.reset_clicks();"),
          "FAIL: tracked middle click did not reach the program")


def pane_controls():
    probe("150 ms busy wait in Session::draw", "panes", "src/app.rs",
          replace_once("    ) -> Result<bool> {\n        self.frame_retry = None;\n",
                       "    ) -> Result<bool> {\n        let busy = Instant::now();\n"
                       "        while busy.elapsed() < Duration::from_millis(150) {}\n"
                       "        self.frame_retry = None;\n"),
          "(stall gap over 100 ms)")
    probe("one writer never prints", "panes", "scripts/perf/stall.py",
          replace_once("        if os.path.exists(stop):\n", "        if os.path.exists(stop) or index == 7:\n"),
          "FAIL: 19 of 20 panes advanced")
    probe("only the first ten visible panes refresh", "panes", "src/app/pane_layout.rs",
          replace_once("            pane.rebuild_snapshot()?;\n",
                       "            if i < 10 || pane.snapshot.is_none() {\n"
                       "                pane.rebuild_snapshot()?;\n            }\n"),
          "starved: a line waited")
    probe("frame log hook removed", "panes", "src/app.rs",
          replace_once("                self.frame_log\n"
                       "                    .presented(self.view.repainted_panes().filter(|_| dirty));\n", ""),
          "FAIL: no frame log")


def interrupted(_signum, _frame):
    raise KeyboardInterrupt


def main():
    os.environ["GIT_OPTIONAL_LOCKS"] = "0"
    if sys.argv[1:] == ["macos-quit"]:
        evidence = os.environ.get("SUMIE_SPIKE_EVIDENCE")
        if not evidence:
            print("macos-quit: set SUMIE_SPIKE_EVIDENCE to an evidence directory", file=sys.stderr)
            return 2
        for case in ("synchronous", "nested-queued", "system-cancel"):
            status = subprocess.call(["bash", ROOT / "scripts/macos-spike/quit.sh", evidence, case])
            if status:
                return 1 if status == 1 else 2
        return 0
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
            if sys.argv[1:] == ["linux-clipboard"]:
                linux_clipboard_controls()
            else:
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
