//! The hint on the banner row of the first launch. A marker file in the state directory
//! records that it was shown, so no later start shows it.

use std::{
    env,
    fs::{self, File},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

use sumie_config::Language;
use sumie_core::{
    KeyAction, PaneAction, Shortcut, TabAction, letter_chord, mouse::MousePhase, splits::Axis,
};
use sumie_locale::Locale;

use crate::platform::{Event, POLICY, PointerEvent};

/// The hint for this start, if it is the first one that finds `state_dir()` without a marker.
pub(super) fn hint(language: Language) -> Option<String> {
    let dir = state_dir()?;
    match claim(&dir) {
        Ok(()) => Some(text(Locale::from_language(language))),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => None,
        Err(error) => {
            tracing::warn!(%error, dir = %dir.display(), "cannot record the first-run hint");
            None
        }
    }
}

/// Create the marker; only the call that creates it succeeds.
fn claim(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    File::create_new(dir.join("first-run")).map(drop)
}

/// `$XDG_STATE_HOME/sumie` on Linux, `~/Library/Application Support/sumie` on macOS.
fn state_dir() -> Option<PathBuf> {
    let absolute = |name| {
        env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let base = if cfg!(target_os = "macos") {
        absolute("HOME")?.join("Library/Application Support")
    } else {
        absolute("XDG_STATE_HOME").or_else(|| absolute("HOME").map(|h| h.join(".local/state")))?
    };
    Some(base.join("sumie"))
}

/// New tab, split, copy and paste, and search, labelled from the platform's shortcut table.
fn text(locale: Locale) -> String {
    let chord = |shortcut| letter_chord(POLICY, shortcut).expect("the table binds it");
    let (prefix, tab) = chord(Shortcut::Tab(TabAction::Open));
    let split = chord(Shortcut::Pane(PaneAction::Split(Axis::Horizontal))).1;
    let (copy, paste) = (chord(Shortcut::Copy).1, chord(Shortcut::Paste).1);
    let find = chord(Shortcut::Search).1;
    let settings = sumie_config::user_config_path().unwrap_or_default();
    format!(
        "{prefix}{tab} {} · {prefix}{split} {} · {prefix}{copy}/{paste} {} · {prefix}{find} {} · {}: {}",
        locale.text("hint.tab"),
        locale.text("hint.split"),
        locale.text("hint.copy-paste"),
        locale.text("hint.find"),
        locale.text("hint.settings"),
        settings.display(),
    )
}

/// Take the hint away on a key or mouse press. Returns whether that press is consumed.
pub(super) fn consume(hint: &mut Option<String>, event: &Event) -> bool {
    let pressed = matches!(
        event,
        Event::Key {
            action: KeyAction::Press,
            ..
        } | Event::Pointer(PointerEvent {
            phase: MousePhase::Press(..),
            ..
        })
    );
    pressed && hint.take().is_some()
}

#[cfg(test)]
mod tests;
