"""The negative controls for key encoding and query replies."""
import re

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
    probe("size and color-scheme queries go unanswered", "test", "crates/sumie-vt/src/lib.rs",
          replace_once("            check(reports::register(terminal.raw))?;\n", ""),
          "test tests::reports::size_and_color_scheme_queries_get_replies ... FAILED")
