//! Legacy key encoding as xterm sends it.

use std::fs;
use std::path::Path;

use super::replace_once;

/// Ghostty encodes Ctrl+[, Ctrl+I and Ctrl+M as CSI u from printable text, which sumie never
/// passes with Control, so they sent nothing; it also keeps Ctrl+Shift+<letter> from its C0 byte.
/// Send ESC, Tab, CR and the letter's C0 byte instead, as xterm does.
pub(super) fn install_control_bytes(source: &Path) {
    replace_once(
        &source.join("src/input/key_encode.zig"),
        &[
            (
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
            ),
            (
                concat!(
                    "                    // to encode shift as CSIu.\n",
                    "                    if (unset_mods.int() != ctrl_only) return null;\n",
                ),
                concat!(
                    "                    // to encode shift as CSIu.\n",
                    "                    // sumie: Shift does not change a letter's C0 byte, as in xterm.\n",
                    "                    if (byte >= 'a' and byte <= 'z') unset_mods.shift = false;\n",
                    "                    if (unset_mods.int() != ctrl_only) return null;\n",
                ),
            ),
        ],
    );
}

/// Ghostty's modifyOtherKeys state 2 encodes the event's text, which sumie never passes with
/// Control, so Ctrl+<char> fell through to its C0 byte. Encode the key's own character instead,
/// uppercased by Shift as xterm reports it; other shifted keys keep their legacy encoding.
pub(super) fn install_other_keys_without_text(source: &Path) {
    replace_once(
        &source.join("src/input/key_encode.zig"),
        &[(
            concat!(
                "        var it = view.iterator();\n",
                "        const codepoint = it.nextCodepoint() orelse break :modify_other;\n",
            ),
            concat!(
                "        var it = view.iterator();\n",
                "        // sumie: without text, use the key's character as xterm does.\n",
                "        const codepoint = it.nextCodepoint() orelse key: {\n",
                "            const cp = if (event.unshifted_codepoint > 0)\n",
                "                event.unshifted_codepoint\n",
                "            else\n",
                "                event.key.codepoint() orelse break :modify_other;\n",
                "            if (!event.mods.shift) break :key cp;\n",
                "            if (cp >= 'a' and cp <= 'z') break :key cp - 0x20;\n",
                "            break :modify_other;\n",
                "        };\n",
            ),
        )],
    );
}

/// Ghostty sends CSI 27 for a modified Enter without modifyOtherKeys. Restrict those forms to
/// modifyOtherKeys state 2; otherwise Shift, Control and Super fall through to the table's CR,
/// and Alt with any other modifier gets its own ESC CR row, as Alt alone already has.
pub(super) fn install_plain_enter(source: &Path) {
    const START: &str = "    result.set(.enter, &.{\n";
    const ANY_MODE: &str = "}, .sequence = \"\\x1b[27;";
    const STATE_2: &str = "}, .modify_other_keys = .set_other, .sequence = \"\\x1b[27;";
    const LEGACY_ALT: &str = "}, .modify_other_keys = .set, .sequence = \"\\x1b\\r\" },";
    let path = source.join("src/input/function_keys.zig");
    let mut text = fs::read_to_string(&path).expect("read pinned Ghostty function keys");
    let start = text.find(START).expect("find pinned Ghostty Enter keys") + START.len();
    let len = text[start..]
        .find("    });\n")
        .expect("find pinned Ghostty Enter keys end");
    let table = &text[start..start + len];
    let mut patched = String::with_capacity(table.len() * 2);
    for line in table.split_inclusive('\n') {
        let Some(at) = line.find(ANY_MODE) else {
            patched.push_str(line);
            continue;
        };
        patched.push_str(&line.replacen(ANY_MODE, STATE_2, 1));
        if line.contains(".alt = true") {
            patched.push_str(&line[..at]);
            patched.push_str(LEGACY_ALT);
            patched.push('\n');
        }
    }
    // One CSI 27 row for each of the 15 modifier sets, and ESC CR for the 8 that hold Alt.
    let rows = |tail: &str| patched.lines().filter(|line| line.contains(tail)).count();
    assert!(
        !patched.contains(ANY_MODE) && rows(STATE_2) == 15 && rows(LEGACY_ALT) == 8,
        "pinned Ghostty Enter keys no longer match the patch:\n{patched}"
    );
    if patched != table {
        text.replace_range(start..start + len, &patched);
        fs::write(path, text).expect("patch pinned Ghostty Enter keys");
    }
}
