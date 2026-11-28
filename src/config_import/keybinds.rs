//! Translate a Ghostty `keybind` value to the sumie binding for the same action.

use sumie_config::Value;
use sumie_core::Chord;

use super::ghostty::Target;

/// Ghostty action to the id of the sumie action in `sumie_core::BINDINGS`.
const ACTIONS: &[(&str, &str)] = &[
    ("copy_to_clipboard", "copy"),
    ("paste_from_clipboard", "paste"),
    ("start_search", "search"),
    ("new_tab", "new-tab"),
    ("close_surface", "close-pane"),
    ("previous_tab", "previous-tab"),
    ("next_tab", "next-tab"),
    ("last_tab", "last-tab"),
    ("move_tab:-1", "move-tab-left"),
    ("move_tab:1", "move-tab-right"),
    ("new_split:right", "split-right"),
    ("new_split:down", "split-down"),
    ("goto_split:left", "focus-left"),
    ("goto_split:right", "focus-right"),
    ("goto_split:up", "focus-up"),
    ("goto_split:down", "focus-down"),
    ("resize_split:left,8", "resize-left"),
    ("resize_split:right,8", "resize-right"),
    ("resize_split:up,8", "resize-up"),
    ("resize_split:down,8", "resize-down"),
    ("toggle_split_zoom", "zoom-pane"),
    ("increase_font_size:1", "font-increase"),
    ("decrease_font_size:1", "font-decrease"),
    ("reset_font_size", "font-reset"),
    ("scroll_page_up", "page-up"),
    ("scroll_page_down", "page-down"),
];

pub(super) fn translate(value: &str) -> Option<(Target, Value)> {
    // The action has no `=`, but the trigger may end in one.
    let (trigger, action) = value.rsplit_once('=')?;
    let id = match ACTIONS.iter().find(|(name, _)| *name == action) {
        Some((_, id)) => (*id).to_owned(),
        None => match action.strip_prefix("goto_tab:")?.parse::<u8>().ok()? {
            number @ 1..=8 => format!("tab-{number}"),
            _ => return None,
        },
    };
    let chord = chord(trigger)?;
    Some((
        Target::Key(format!("keybinds.{id}")),
        Value::String(chord.to_string()),
    ))
}

/// Spell the trigger the way `Chord::parse` reads it; a sequence, prefix or unknown key fails.
fn chord(trigger: &str) -> Option<Chord> {
    let (modifiers, key) = trigger.rsplit_once('+').unwrap_or(("", trigger));
    let mut text = String::new();
    for modifier in modifiers.split('+').filter(|name| !name.is_empty()) {
        text += match modifier {
            "ctrl" | "control" => "Ctrl+",
            "shift" => "Shift+",
            "alt" | "opt" | "option" => "Alt+",
            "super" | "cmd" | "command" => "Super+",
            _ => return None,
        };
    }
    // `page_up` is `PageUp`, `plus` is `Plus`.
    for word in key.split('_') {
        let mut letters = word.chars();
        text.extend(letters.next().map(|first| first.to_ascii_uppercase()));
        text += letters.as_str();
    }
    Chord::parse(&text)
}
