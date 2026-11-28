"""The negative controls for key encoding, page scrolling, search repeats, focusing clicks and query replies."""
import re
import sys

from negative.probe import probe, replace_once

BUILD = "crates/sumie-vt/build.rs"
LEVEL = re.compile(r"const GHOSTTY_PATCH_LEVEL: u32 = (\d+);")


def without_key_patch(call, offset):
    """Skip one Ghostty key patch on a patch level of its own, so no patched extraction is reused."""
    def change(source):
        level = LEVEL.search(source)
        if level is None:
            raise RuntimeError("expected GHOSTTY_PATCH_LEVEL in build.rs")
        source = replace_once(f"    keys_patch::{call}(&source);\n", "")(source)
        probe_level = int(level.group(1)) * 1000 + offset
        return source.replace(level.group(0), f"const GHOSTTY_PATCH_LEVEL: u32 = {probe_level};", 1)
    return change


def input_controls():
    probe("legacy Shift+Enter sends CSI 27", "test", BUILD, without_key_patch("install_plain_enter", 1),
          "test tests::keys::modified_enter_is_cr_until_a_mode_asks_for_more ... FAILED")
    probe("legacy Ctrl+[ sends nothing", "test", BUILD, without_key_patch("install_control_bytes", 2),
          "test tests::keys::legacy_control_keys_send_their_c0_byte ... FAILED")
    probe("modifyOtherKeys 2 sends Ctrl+[ as ESC", "test", BUILD,
          without_key_patch("install_other_keys_without_text", 3),
          "test tests::keys::modify_other_keys_2_encodes_control_keys_without_text ... FAILED")
    probe("size and color-scheme queries go unanswered", "test", "crates/sumie-vt/src/lib.rs",
          replace_once("            check(reports::register(terminal.raw))?;\n", ""),
          "test tests::reports::size_and_color_scheme_queries_get_replies ... FAILED")
    probe("a search key held into Command repeats with it", "test", "src/input.rs",
          replace_once("\n                    && !self.shortcuts.command_repeat(modifiers, action) =>", " =>"),
          "test input::tests::a_search_key_held_into_command_repeats_only_without_it_on_macos ... FAILED")
    probe("paste without a recipient panics", "test", "src/app/pane_input.rs",
          replace_once("window.read_clipboard(*pane.text_recipient.get_or_insert(routing.generation));",
                       'window.read_clipboard(pane.text_recipient.expect("focused text recipient"));'),
          "test app::tabs::tests::paste_shortcut_restores_a_missing_recipient ... FAILED")
    probe("close ignores a running foreground job", "test", "src/app/window_events.rs",
          replace_once("None => local.pty.has_foreground_job(),", "None => false,"),
          "test app::tabs::tests::window_close_uses_osc133_before_foreground_group ... FAILED")
    probe("Enter confirms a close", "test", "src/app/window_events.rs",
          replace_once(" || (enter && !paste)", " || (enter && paste)"),
          "test app::tabs::tests::window_close_uses_osc133_before_foreground_group ... FAILED")
    probe("ignore exec in the original shell group", "test", "crates/sumie-pty/src/lib.rs",
          replace_once("|shell| !is_shell(child, shell)", "|_| false"),
          "test detects_commands_and_shells_replaced_by_exec ... FAILED")
    probe("unbracketed multiline paste skips confirmation", "test", "src/app/pane_input.rs",
          replace_once("text.contains(['\\n', '\\r'])", "text.contains('\\0')"),
          "test app::tabs::tests::multiline_paste_waits_unless_bracketed ... FAILED")
    if sys.platform.startswith("linux"):
        probe("old Wayland IME recipients never expire without done", "test", "crates/sumie-wayland/src/ime.rs",
              replace_once("        if self.previous_recipients.len() == 4 {\n"
                           "            self.previous_recipients.pop_front();\n        }\n", ""),
              "test ime::tests::old_done_keeps_only_the_last_four_recipients ... FAILED")
        probe("Shift+PageUp leaves the history in place", "test", "src/input.rs",
              replace_once("return send_input(pane, routing, id, PaneCommand::ScrollPage { up });",
                           "return Ok(Effect::None);"),
              "test input::tests::shift_page_keys_scroll_the_primary_screen_and_reach_the_alternate_one"
              " ... FAILED")
        # The primary screen still scrolls, so only the alternate screen's rows can fail.
        probe("Shift+PageUp scrolls the alternate screen", "test", "crates/sumie-core/src/shortcuts.rs",
              replace_once("Some(Shortcut::ScrollPage { up }) if primary_screen()? =>",
                           "Some(Shortcut::ScrollPage { up }) if primary_screen().map(|_| true)? =>"),
              "Shift+PageUp on the alternate screen")
        probe("a focusing click reaches the pane", "test", "src/app/pane_input.rs",
              replace_once("        !(button == Button::Left && moved_focus)\n", "        true\n"),
              "test app::pane_input::tests::a_left_click_that_moves_focus_stays_out_of_the_pane ... FAILED")
