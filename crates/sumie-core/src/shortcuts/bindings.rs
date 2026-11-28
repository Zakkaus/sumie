use super::{Chord, FontZoom, PaneAction, Shortcut, Shortcuts, default_shortcut};
use crate::{
    KeyModifiers, PhysicalKey, Platform, TabAction,
    splits::{Axis, Direction},
};

/// One configurable application action and its primary chord on each platform.
#[derive(Debug, PartialEq, Eq)]
pub struct Binding {
    pub id: &'static str,
    pub label: &'static str,
    pub action: Shortcut,
    defaults: [&'static str; 2],
}

impl Binding {
    #[must_use]
    pub const fn default_on(&self, platform: Platform) -> &'static str {
        self.defaults[match platform {
            Platform::Linux => 0,
            Platform::MacOS => 1,
        }]
    }
}

macro_rules! bindings {
    ($(($id:literal, $label:literal, $action:expr, $linux:literal $(, $mac:literal)?)),+ $(,)?) => {
        &[$(Binding { id: $id, label: $label, action: $action, defaults: [$linux, last!($linux $(, $mac)?)] }),+]
    };
}
macro_rules! last {
    ($linux:literal, $mac:literal) => {
        $mac
    };
    ($linux:literal) => {
        $linux
    };
}

/// Bindings in settings display order. Additional built-in aliases remain active until the
/// action is explicitly rebound.
#[rustfmt::skip]
pub const BINDINGS: &[Binding] = {
    use Shortcut::{Copy, Pane, Paste, ScrollPage, Search, Settings, Tab, Zoom};
    bindings![
        ("copy", "Copy", Copy, "Ctrl+Shift+C", "Super+C"),
        ("paste", "Paste", Paste, "Ctrl+Shift+V", "Super+V"),
        ("search", "Search", Search, "Ctrl+Shift+F", "Super+F"),
        ("settings", "Open settings", Settings, "Ctrl+,", "Super+,"),
        ("new-tab", "New tab", Tab(TabAction::Open), "Ctrl+Shift+T", "Super+T"),
        ("close-pane", "Close pane", Tab(TabAction::Close), "Ctrl+Shift+W", "Super+W"),
        ("next-tab", "Next tab", Tab(TabAction::Cycle(1)), "Ctrl+Tab"),
        ("previous-tab", "Previous tab", Tab(TabAction::Cycle(-1)), "Ctrl+Shift+Tab"),
        ("move-tab-left", "Move tab left", Tab(TabAction::MoveLeft), "Ctrl+Shift+PageUp"),
        ("move-tab-right", "Move tab right", Tab(TabAction::MoveRight), "Ctrl+Shift+PageDown"),
        ("tab-1", "Tab 1", Tab(TabAction::Select(0)), "Alt+1", "Super+1"),
        ("tab-2", "Tab 2", Tab(TabAction::Select(1)), "Alt+2", "Super+2"),
        ("tab-3", "Tab 3", Tab(TabAction::Select(2)), "Alt+3", "Super+3"),
        ("tab-4", "Tab 4", Tab(TabAction::Select(3)), "Alt+4", "Super+4"),
        ("tab-5", "Tab 5", Tab(TabAction::Select(4)), "Alt+5", "Super+5"),
        ("tab-6", "Tab 6", Tab(TabAction::Select(5)), "Alt+6", "Super+6"),
        ("tab-7", "Tab 7", Tab(TabAction::Select(6)), "Alt+7", "Super+7"),
        ("tab-8", "Tab 8", Tab(TabAction::Select(7)), "Alt+8", "Super+8"),
        ("last-tab", "Last tab", Tab(TabAction::Last), "Alt+9", "Super+9"),
        ("split-right", "Split right", Pane(PaneAction::Split(Axis::Horizontal)), "Ctrl+Shift+E", "Super+E"),
        ("split-down", "Split down", Pane(PaneAction::Split(Axis::Vertical)), "Ctrl+Shift+O", "Super+O"),
        ("focus-left", "Focus pane left", Pane(PaneAction::Focus(Direction::Left)), "Ctrl+Alt+Left", "Super+Left"),
        ("focus-right", "Focus pane right", Pane(PaneAction::Focus(Direction::Right)), "Ctrl+Alt+Right", "Super+Right"),
        ("focus-up", "Focus pane above", Pane(PaneAction::Focus(Direction::Up)), "Ctrl+Alt+Up", "Super+Up"),
        ("focus-down", "Focus pane below", Pane(PaneAction::Focus(Direction::Down)), "Ctrl+Alt+Down", "Super+Down"),
        ("resize-left", "Move divider left", Pane(PaneAction::Resize(Direction::Left)), "Ctrl+Alt+Shift+Left"),
        ("resize-right", "Move divider right", Pane(PaneAction::Resize(Direction::Right)), "Ctrl+Alt+Shift+Right"),
        ("resize-up", "Move divider up", Pane(PaneAction::Resize(Direction::Up)), "Ctrl+Alt+Shift+Up"),
        ("resize-down", "Move divider down", Pane(PaneAction::Resize(Direction::Down)), "Ctrl+Alt+Shift+Down"),
        ("zoom-pane", "Zoom pane", Pane(PaneAction::Zoom), "Ctrl+Shift+Z", "Super+Shift+Enter"),
        ("font-increase", "Increase font size", Zoom(FontZoom::Increase), "Ctrl+=", "Super+="),
        ("font-decrease", "Decrease font size", Zoom(FontZoom::Decrease), "Ctrl+-", "Super+-"),
        ("font-reset", "Reset font size", Zoom(FontZoom::Reset), "Ctrl+0", "Super+0"),
        ("page-up", "Scroll up a page", ScrollPage { up: true }, "Shift+PageUp"),
        ("page-down", "Scroll down a page", ScrollPage { up: false }, "Shift+PageDown"),
    ]
};

impl Shortcuts {
    /// Replace overrides without changing ownership of keys already held down.
    pub fn set_bindings(&mut self, bindings: impl IntoIterator<Item = (Chord, Shortcut)>) {
        self.bindings.clear();
        self.bindings.extend(bindings);
    }

    pub(super) fn configured_shortcut(
        &self,
        physical: PhysicalKey,
        symbol: Option<char>,
        mods: KeyModifiers,
        has_splits: bool,
    ) -> Option<Shortcut> {
        if !self.bindings.is_empty()
            && let Some(chord) = Chord::from_event(physical, symbol, mods)
            && let Some(&(_, action)) = self.bindings.iter().find(|&&(key, _)| key == chord)
            && (has_splits
                || !matches!(
                    action,
                    Shortcut::Pane(PaneAction::Focus(_) | PaneAction::Resize(_))
                ))
        {
            return Some(action);
        }
        let default = default_shortcut(self.platform, physical, symbol, mods, has_splits)?;
        if self.bindings.iter().any(|&(_, action)| action == default) {
            (self.platform == Platform::MacOS && mods.super_key).then_some(Shortcut::Consume)
        } else {
            Some(default)
        }
    }
}

/// Find the action whose override, or default when it has none, is already the chord.
#[must_use]
pub fn binding_conflict(
    platform: Platform,
    chord: Chord,
    target: &Binding,
    override_for: impl Fn(&Binding) -> Option<Chord>,
) -> Option<&'static Binding> {
    BINDINGS
        .iter()
        .filter(|binding| binding.action != target.action)
        .find(|binding| {
            override_for(binding).or_else(|| Chord::parse(binding.default_on(platform)))
                == Some(chord)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_displayed_default_matches_its_application_action() {
        for binding in BINDINGS {
            for platform in [Platform::Linux, Platform::MacOS] {
                let chord = Chord::parse(binding.default_on(platform)).unwrap();
                let (key, symbol, mods) = chord.event();
                assert_eq!(
                    default_shortcut(platform, key, symbol, mods, true),
                    Some(binding.action)
                );
            }
        }
    }
}
