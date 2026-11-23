use std::ffi::c_int;
use std::ptr::from_ref;

use crate::{Error, Rgb, Terminal, check, ffi};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum CursorStyle {
    Bar = ffi::CURSOR_STYLE_BAR,
    Block = ffi::CURSOR_STYLE_BLOCK,
    Underline = ffi::CURSOR_STYLE_UNDERLINE,
    Hollow = ffi::CURSOR_STYLE_BLOCK_HOLLOW,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub column: u16,
    pub row: u16,
    pub visible: bool,
    pub color: Rgb,
    pub style: CursorStyle,
    pub blink: bool,
}

impl Terminal {
    /// Update the defaults without replacing an explicit DECSCUSR style.
    ///
    /// # Errors
    /// Returns an error if Ghostty rejects either option.
    pub fn set_cursor_defaults(&mut self, style: CursorStyle, blink: bool) -> Result<(), Error> {
        let style = style as c_int;
        // SAFETY: The terminal is live and each option reads its documented value type.
        unsafe {
            check(ffi::ghostty_terminal_set(
                self.raw,
                ffi::TERMINAL_OPT_DEFAULT_CURSOR_STYLE,
                from_ref(&style).cast(),
            ))?;
            check(ffi::ghostty_terminal_set(
                self.raw,
                ffi::TERMINAL_OPT_DEFAULT_CURSOR_BLINK,
                from_ref(&blink).cast(),
            ))
        }
    }

    pub(super) fn cursor(&self, color: Rgb) -> Result<Option<Cursor>, Error> {
        // SAFETY: Each key is paired with its documented bool, u16, or C enum output layout.
        unsafe {
            if !self.render_value::<bool>(ffi::RENDER_STATE_DATA_CURSOR_VIEWPORT_HAS_VALUE)? {
                return Ok(None);
            }
            let column: u16 = self.render_value(ffi::RENDER_STATE_DATA_CURSOR_VIEWPORT_X)?;
            let wide_tail: bool =
                self.render_value(ffi::RENDER_STATE_DATA_CURSOR_VIEWPORT_WIDE_TAIL)?;
            let style = match self.render_value(ffi::RENDER_STATE_DATA_CURSOR_VISUAL_STYLE)? {
                ffi::CURSOR_STYLE_BAR => CursorStyle::Bar,
                ffi::CURSOR_STYLE_BLOCK => CursorStyle::Block,
                ffi::CURSOR_STYLE_UNDERLINE => CursorStyle::Underline,
                ffi::CURSOR_STYLE_BLOCK_HOLLOW => CursorStyle::Hollow,
                _ => return Err(Error(ffi::INVALID_VALUE)),
            };
            Ok(Some(Cursor {
                column: column - u16::from(wide_tail),
                row: self.render_value(ffi::RENDER_STATE_DATA_CURSOR_VIEWPORT_Y)?,
                visible: self.render_value(ffi::RENDER_STATE_DATA_CURSOR_VISIBLE)?,
                color,
                style,
                blink: self.render_value(ffi::RENDER_STATE_DATA_CURSOR_BLINKING)?,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CursorStyle::*, Terminal};

    #[test]
    fn decscusr_controls_shape_and_blink_until_reset() {
        let mut terminal = Terminal::new(2, 2).unwrap();
        let mut appearance = |sequence: &[u8], defaults| {
            if let Some((style, blink)) = defaults {
                terminal.set_cursor_defaults(style, blink).unwrap();
            }
            terminal.write(sequence);
            let cursor = terminal.render_snapshot().unwrap().cursor.unwrap();
            (cursor.style, cursor.blink)
        };
        for (sequence, defaults, expected) in [
            (b"".as_slice(), Some((Underline, false)), (Underline, false)),
            (b"\x1b[1 q", None, (Block, true)),
            (b"\x1b[2 q", None, (Block, false)),
            (b"\x1b[3 q", None, (Underline, true)),
            (b"\x1b[4 q", None, (Underline, false)),
            (b"\x1b[5 q", None, (Bar, true)),
            (b"\x1b[6 q", None, (Bar, false)),
            (b"", Some((Underline, true)), (Bar, false)),
            (b"\x1b[0 q", None, (Underline, true)),
            (b"\x1b[2 q", None, (Block, false)),
            (b"\x1bc", None, (Underline, true)),
            (b"\x1b[?1049h", Some((Block, true)), (Block, true)),
            (b"", Some((Bar, false)), (Bar, false)),
            (b"\x1b[?1049l", None, (Bar, false)),
            (b"\x1b[?12h\x1b[?1049h", None, (Bar, true)),
            (b"\x1b[?1049l", None, (Bar, true)),
            (b"\x1b[?47h", None, (Bar, true)),
            (b"", Some((Underline, false)), (Underline, false)),
            (b"\x1b[?47l", None, (Underline, false)),
        ] {
            assert_eq!(appearance(sequence, defaults), expected, "{sequence:?}");
        }
    }
}
