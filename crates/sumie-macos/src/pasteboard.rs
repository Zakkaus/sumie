//! The pasteboard's string API: the general board in production, a named board in tests.
//!
//! Only the pasteboard helper child (the binary's `macos::pasteboard`) calls this, never the
//! app process: a read can wait without bound for the board owner's promised data. Nothing
//! here logs or returns contents anywhere but to the caller.

use std::{error::Error, fmt};

use objc2::rc::Retained;
use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSString, ns_string};

/// Which board a helper serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Board {
    General,
    /// A board only its creator and tests know by name; [`Board::named`] makes one.
    Named(Name),
}

/// A board name other than the general board's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name(String);

/// The name would not make a board apart from the general one.
#[derive(Debug)]
pub struct NotNamed;

impl fmt::Display for NotNamed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a named pasteboard needs a name other than the general board's")
    }
}

impl Error for NotNamed {}

/// `NSPasteboardNameGeneral`, spelled out: the extern static needs `unsafe` to read.
const GENERAL_NAME: &str = "Apple CFPasteboard general";

impl Board {
    /// The board called `name`; never the general board, by its name or an empty one.
    ///
    /// # Errors
    ///
    /// [`NotNamed`] for an empty name or the general board's.
    pub fn named(name: &str) -> Result<Self, NotNamed> {
        if name.is_empty() || name == GENERAL_NAME {
            return Err(NotNamed);
        }
        Ok(Self::Named(Name(name.to_owned())))
    }

    fn open(&self) -> Retained<NSPasteboard> {
        match self {
            Self::General => NSPasteboard::generalPasteboard(),
            Self::Named(Name(name)) => NSPasteboard::pasteboardWithName(&NSString::from_str(name)),
        }
    }
}

/// `NSPasteboardTypeString`, spelled out: the extern static needs `unsafe` to read.
fn string_type() -> &'static NSString {
    ns_string!("public.utf8-plain-text")
}

/// The board's string, or `None` when it holds none. Can block on a lazy provider.
#[must_use]
pub fn read(board: &Board) -> Option<String> {
    board
        .open()
        .stringForType(string_type())
        .map(|text| text.to_string())
}

/// Replace the board's contents with `text`, or clear them for `None`; returns whether the
/// board took the string. The string goes to the pasteboard server at once, so it outlives
/// the calling process.
#[must_use]
pub fn write(board: &Board, text: Option<&str>) -> bool {
    let board = board.open();
    board.clearContents();
    text.is_none_or(|text| board.setString_forType(&NSString::from_str(text), string_type()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh board that nothing else writes.
    fn unique() -> Board {
        Board::named(&NSPasteboard::pasteboardWithUniqueName().name().to_string()).unwrap()
    }

    #[test]
    fn a_named_board_is_never_the_general_one() {
        // Only the general board's name is read, never its contents.
        let general = NSPasteboard::generalPasteboard().name().to_string();
        assert_eq!(general, GENERAL_NAME);
        assert!(Board::named(&general).is_err());
        assert!(Board::named("").is_err());
        assert_eq!(
            Board::named("org.sumie.test").unwrap(),
            Board::Named(Name("org.sumie.test".to_owned()))
        );
    }

    #[test]
    fn a_named_board_round_trips_and_clears() {
        let board = unique();
        assert_eq!(read(&board), None);
        assert!(write(&board, Some("sumie 墨\n")));
        assert_eq!(read(&board).as_deref(), Some("sumie 墨\n"));
        assert!(write(&board, None));
        assert_eq!(read(&board), None);
    }
}
