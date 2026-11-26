//! Legacy key encoding as xterm sends it.

use std::fs;
use std::path::Path;

use super::replace_once;

/// Ghostty encodes Ctrl+[, Ctrl+I and Ctrl+M as CSI u from printable text, which sumie never
/// passes with Control, so they sent nothing. Send ESC, Tab and CR instead.
pub(super) fn install_control_bytes(source: &Path) {
    replace_once(
        &source.join("src/input/key_encode.zig"),
        &[(
            concat!(
                "        // These are purposely NOT handled here because of the fixterms\n",
                "        // specification: https://www.leonerd.org.uk/hacks/fixterms/\n",
                "        // These are processed as CSI u.\n",
                "        // 'i' => 0x09,\n",
                "        // 'm' => 0x0D,\n",
                "        // '[' => 0x1B,\n",
            ),
            concat!(
                "        // sumie: legacy Ctrl+I, Ctrl+M and Ctrl+[ are Tab, CR and ESC, as in xterm.\n",
                "        'i' => 0x09,\n",
                "        'm' => 0x0D,\n",
                "        '[' => 0x1B,\n",
            ),
        )],
    );
}

/// Ghostty sends CSI 27 for a modified Enter without modifyOtherKeys. Restrict those forms to
/// modifyOtherKeys state 2; otherwise Shift, Control and Super fall through to the table's CR,
/// and Alt keeps its ESC CR.
pub(super) fn install_plain_enter(source: &Path) {
    const START: &str = "    result.set(.enter, &.{\n";
    const ANY_MODE: &str = "}, .sequence = \"\\x1b[27;";
    const STATE_2: &str = "}, .modify_other_keys = .set_other, .sequence = \"\\x1b[27;";
    let path = source.join("src/input/function_keys.zig");
    let mut text = fs::read_to_string(&path).expect("read pinned Ghostty function keys");
    let start = text.find(START).expect("find pinned Ghostty Enter keys") + START.len();
    let len = text[start..]
        .find("    });\n")
        .expect("find pinned Ghostty Enter keys end");
    let table = &text[start..start + len];
    if table.contains(ANY_MODE) {
        let patched = table.replace(ANY_MODE, STATE_2);
        text.replace_range(start..start + len, &patched);
        fs::write(path, text).expect("patch pinned Ghostty Enter keys");
    }
}
