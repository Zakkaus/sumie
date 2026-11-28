use sumie_core::{
    KeyAction, KeyCode as K, Pane, PaneCommand, PaneEffect, PhysicalKey, Platform, Shortcuts,
};

use super::{Routing, Scope, release, route};
use crate::{
    Error, Result,
    platform::{Event, LogicalKey, Modifiers},
};

/// Records what reaches the PTY; `reports` answers presses like Kitty's event mode.
#[derive(Default)]
struct Recorder {
    reports: bool,
    alternate: bool,
    screen_fails: bool,
    keys: Vec<(K, &'static str)>,
    pages: Vec<bool>,
    focus: Vec<bool>,
}

impl Pane for Recorder {
    type Snapshot = ();
    type Error = Error;

    fn command(&mut self, command: PaneCommand<'_>) -> Result<PaneEffect> {
        match command {
            PaneCommand::Key {
                physical: PhysicalKey::Code(code),
                action,
                ..
            } => {
                let (label, press) = match action {
                    KeyAction::Press => ("press", true),
                    KeyAction::Repeat => ("repeat", false),
                    KeyAction::Release => ("release", false),
                };
                self.keys.push((code, label));
                if press && self.reports {
                    return Ok(PaneEffect::ReportedPress {
                        viewport_changed: false,
                    });
                }
            }
            PaneCommand::Focus(focused) => self.focus.push(focused),
            PaneCommand::ScrollPage { up } => {
                self.pages.push(up);
                return Ok(PaneEffect::ViewportChanged);
            }
            _ => (),
        }
        Ok(PaneEffect::None)
    }

    fn snapshot(&mut self) -> Result<()> {
        Ok(())
    }

    fn primary_screen(&self) -> Result<bool> {
        if self.screen_fails {
            return Err("screen query failed".into());
        }
        Ok(!self.alternate)
    }
}

fn key(code: K, ch: char, action: KeyAction) -> Event {
    Event::Key {
        physical: PhysicalKey::Code(code),
        logical: LogicalKey::Character(ch),
        utf8: (!matches!(action, KeyAction::Release)).then(|| ch.into()),
        modifiers: Modifiers::default(),
        action,
    }
}

#[test]
fn losing_the_keyboard_releases_reported_keys_once_in_their_own_pane() {
    use KeyAction::{Press, Release, Repeat};
    let mut reporting = Recorder {
        reports: true,
        ..Recorder::default()
    };
    let mut legacy = Recorder::default();
    let mut routing = Routing::default();
    route(
        (1, 0),
        &mut reporting,
        &mut routing,
        false,
        key(K::KeyA, 'a', Press),
        Scope::Single,
    )
    .unwrap();
    // Pane focus moves; the press keeps its owner.
    let generation = routing.change_recipient();
    let b = |action| key(K::KeyB, 'b', action);
    route(
        (2, generation),
        &mut legacy,
        &mut routing,
        false,
        b(Press),
        Scope::Single,
    )
    .unwrap();

    let held = routing.keyboard_left();
    assert_eq!(held.iter().map(|key| key.pane).collect::<Vec<_>>(), [1]);
    for key in &held {
        release(&mut reporting, key, Modifiers::default()).unwrap();
    }
    assert!(routing.keyboard_left().is_empty());

    // After refocus, AppKit's late repeats and releases belong to no press.
    for event in [
        key(K::KeyA, 'a', Repeat),
        key(K::KeyA, 'a', Release),
        b(Repeat),
        b(Release),
    ] {
        assert_eq!(routing.release_owner(&event), None);
        route(
            (2, generation),
            &mut legacy,
            &mut routing,
            false,
            event,
            Scope::Single,
        )
        .unwrap();
    }
    assert_eq!(reporting.keys, [(K::KeyA, "press"), (K::KeyA, "release")]);
    assert_eq!(legacy.keys, [(K::KeyB, "press")]);
    route(
        (2, generation),
        &mut legacy,
        &mut routing,
        false,
        b(Press),
        Scope::Single,
    )
    .unwrap();
    assert_eq!(legacy.keys, [(K::KeyB, "press"), (K::KeyB, "press")]);
    assert!(reporting.focus.is_empty() && legacy.focus.is_empty());
}

#[test]
fn shift_page_keys_scroll_the_primary_screen_and_reach_the_alternate_one() {
    use KeyAction::{Press, Release, Repeat};
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let ctrl_shift = Modifiers {
        control: true,
        ..shift
    };
    for (alternate, code, modifiers, page) in [
        (false, K::PageUp, shift, Some(true)),
        (false, K::PageDown, shift, Some(false)),
        (true, K::PageUp, shift, None),
        (true, K::PageDown, shift, None),
        (false, K::PageUp, Modifiers::default(), None),
        (false, K::Home, shift, None),
    ] {
        let mut pane = Recorder {
            alternate,
            ..Recorder::default()
        };
        let mut routing = Routing::default();
        let chord = if modifiers.shift { "Shift+" } else { "" };
        let screen = if alternate { "alternate" } else { "primary" };
        for action in [Press, Repeat, Release] {
            let event = Event::Key {
                physical: PhysicalKey::Code(code),
                logical: LogicalKey::Other(None),
                utf8: None,
                modifiers,
                action,
            };
            let effect =
                route((1, 0), &mut pane, &mut routing, false, event, Scope::Single).unwrap();
            let scrolled = page.is_some() && matches!(action, Press);
            assert_eq!(
                effect == super::Effect::ViewportChanged,
                scrolled,
                "{chord}{code:?} on the {screen} screen"
            );
        }
        if let Some(up) = page {
            assert_eq!(pane.pages, [up], "{code:?}");
            assert!(pane.keys.is_empty(), "{code:?} reached the PTY");
        } else {
            assert!(pane.pages.is_empty(), "{code:?} scrolled");
            assert_eq!(
                pane.keys,
                [(code, "press"), (code, "repeat"), (code, "release")]
            );
        }
    }
    // Ctrl+Shift+PageUp stays a tab binding.
    let mut pane = Recorder::default();
    let event = Event::Key {
        physical: PhysicalKey::Code(K::PageUp),
        logical: LogicalKey::Other(None),
        utf8: None,
        modifiers: ctrl_shift,
        action: Press,
    };
    let effect = route(
        (1, 0),
        &mut pane,
        &mut Routing::default(),
        false,
        event,
        Scope::Single,
    );
    assert!(matches!(effect.unwrap(), super::Effect::Tab(_)));
}

#[test]
fn a_search_key_held_into_command_repeats_only_without_it_on_macos() {
    use super::{Effect, SearchAction};
    use KeyAction::{Press, Release, Repeat};
    let backspace = |action, super_key| Event::Key {
        physical: PhysicalKey::Code(K::Backspace),
        logical: LogicalKey::Backspace,
        utf8: None,
        modifiers: Modifiers {
            super_key,
            ..Modifiers::default()
        },
        action,
    };
    let command = |action| Event::Key {
        physical: PhysicalKey::Code(K::MetaLeft),
        logical: LogicalKey::Other(None),
        utf8: None,
        modifiers: Modifiers {
            super_key: !matches!(action, Release),
            ..Modifiers::default()
        },
        action,
    };
    let deleted = || Effect::Search(SearchAction::Backspace { word: false });
    for platform in [Platform::MacOS, Platform::Linux] {
        let mut pane = Recorder::default();
        let mut routing = Routing {
            shortcuts: Shortcuts::new(platform),
            ..Routing::default()
        };
        // Linux keeps Super out of the table, not out of the search box.
        let with_command = if platform == Platform::MacOS {
            Effect::None
        } else {
            deleted()
        };
        for (step, (event, expected)) in [
            (backspace(Press, false), deleted()),
            (command(Press), Effect::None),
            (backspace(Repeat, true), with_command),
            (command(Release), Effect::None),
            (backspace(Repeat, false), deleted()),
            (backspace(Release, false), Effect::None),
        ]
        .into_iter()
        .enumerate()
        {
            let effect =
                route((1, 0), &mut pane, &mut routing, true, event, Scope::Single).unwrap();
            assert_eq!(effect, expected, "{platform:?} step {step}");
        }
        assert!(
            pane.keys.is_empty(),
            "{platform:?}: a search key reached the PTY"
        );
    }
}

#[test]
fn only_shift_page_keys_ask_which_screen_is_active() {
    let mut pane = Recorder {
        screen_fails: true,
        ..Recorder::default()
    };
    let mut routing = Routing::default();
    let event = key(K::KeyA, 'a', KeyAction::Press);
    route((1, 0), &mut pane, &mut routing, false, event, Scope::Single).unwrap();
    assert_eq!(pane.keys, [(K::KeyA, "press")]);
    let event = Event::Key {
        physical: PhysicalKey::Code(K::PageUp),
        logical: LogicalKey::Other(None),
        utf8: None,
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        action: KeyAction::Press,
    };
    assert!(route((1, 0), &mut pane, &mut routing, false, event, Scope::Single).is_err());
}

#[test]
fn a_focus_report_is_one_pane_command() {
    let mut pane = Recorder::default();
    for focused in [false, true] {
        super::report_focus(&mut pane, focused).unwrap();
    }
    assert_eq!(pane.focus, [false, true]);
}

#[test]
fn settings_chord_opens_a_tab_unless_it_is_being_recorded() {
    for platform in [Platform::Linux, Platform::MacOS] {
        let mut pane = Recorder::default();
        let mut routing = Routing {
            shortcuts: Shortcuts::new(platform),
            ..Routing::default()
        };
        let event = |action| Event::Key {
            physical: PhysicalKey::Code(K::Comma),
            logical: LogicalKey::Character(','),
            utf8: None,
            modifiers: Modifiers {
                control: platform == Platform::Linux,
                super_key: platform == Platform::MacOS,
                ..Modifiers::default()
            },
            action,
        };
        assert_eq!(
            route(
                (1, 0),
                &mut pane,
                &mut routing,
                false,
                event(KeyAction::Press),
                Scope::Single
            )
            .unwrap(),
            super::Effect::Settings
        );
        route(
            (1, 0),
            &mut pane,
            &mut routing,
            false,
            event(KeyAction::Release),
            Scope::Recording,
        )
        .unwrap();
        assert_eq!(pane.keys, [] as [(K, &str); 0]);
        assert_eq!(
            route(
                (1, 0),
                &mut pane,
                &mut routing,
                false,
                event(KeyAction::Press),
                Scope::Recording
            )
            .unwrap(),
            super::Effect::None
        );
        assert_eq!(pane.keys, [(K::Comma, "press")]);
    }
}

#[test]
fn reloaded_binding_replaces_the_default_without_losing_a_held_release() {
    let config = sumie_config::Config::from_json(br#"{"keybinds":{"copy":"Ctrl+Alt+X"}}"#).unwrap();
    let mut pane = Recorder::default();
    let mut routing = Routing {
        shortcuts: Shortcuts::new(Platform::Linux),
        ..Routing::default()
    };
    let press = |code, ch, shift, action| Event::Key {
        physical: PhysicalKey::Code(code),
        logical: LogicalKey::Character(ch),
        utf8: None,
        modifiers: Modifiers {
            control: true,
            shift,
            alt: !shift,
            ..Modifiers::default()
        },
        action,
    };
    let mut send = |routing: &mut Routing, event| {
        route((1, 0), &mut pane, routing, false, event, Scope::Single).unwrap()
    };
    assert_eq!(
        send(&mut routing, press(K::KeyC, 'C', true, KeyAction::Press)),
        super::Effect::Copy
    );
    routing.set_bindings(&config.keybinds);
    send(&mut routing, press(K::KeyC, 'C', true, KeyAction::Release));
    assert_eq!(
        send(&mut routing, press(K::KeyX, 'x', false, KeyAction::Press)),
        super::Effect::Copy
    );
    assert_eq!(
        send(&mut routing, press(K::KeyC, 'C', true, KeyAction::Press)),
        super::Effect::None
    );
    assert_eq!(pane.keys, [(K::KeyC, "press")]);
}
