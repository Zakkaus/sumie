use unicode_segmentation::UnicodeSegmentation as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Delete,
    Backspace,
    Tab,
    Enter,
    Esc,
    Interrupt,
    Char(char),
}

#[derive(Default)]
pub(crate) struct Decoder {
    bytes: Vec<u8>,
}

impl Decoder {
    pub(crate) fn push(&mut self, byte: u8) -> Option<Key> {
        if byte == 3 {
            self.bytes.clear();
            return Some(Key::Interrupt);
        }
        match (self.bytes.first(), byte) {
            (None, b'\t') => Some(Key::Tab),
            (None, b'\r' | b'\n') => Some(Key::Enter),
            (None, 8 | 127) => Some(Key::Backspace),
            (None, 27) => {
                self.bytes.push(byte);
                None
            }
            (None, 0..=31) => None,
            (Some(27), _) => self.escape_byte(byte),
            _ => self.text_byte(byte),
        }
    }

    fn text_byte(&mut self, byte: u8) -> Option<Key> {
        if self.bytes.is_empty() && byte.is_ascii() {
            return Some(Key::Char(char::from(byte)));
        }
        self.bytes.push(byte);
        match std::str::from_utf8(&self.bytes) {
            Ok(text) => {
                let ch = text.chars().next().filter(|ch| !ch.is_control());
                self.bytes.clear();
                ch.map(Key::Char)
            }
            Err(error) if error.error_len().is_some() => {
                let retry = self.bytes.len() > 1;
                self.bytes.clear();
                if retry { self.push(byte) } else { None }
            }
            Err(_) => None,
        }
    }

    fn escape_byte(&mut self, byte: u8) -> Option<Key> {
        if self.bytes.len() == 1 && !matches!(byte, b'[' | b'O') {
            // Ignore Alt chords rather than binding them to the unmodified key.
            self.bytes.clear();
            return None;
        }
        self.bytes.push(byte);
        if self.bytes.len() < 3 || (byte < 0x40 && self.bytes.len() < 32) {
            return None;
        }
        let key = match self.bytes.as_slice() {
            b"\x1b[A" | b"\x1bOA" => Key::Up,
            b"\x1b[B" | b"\x1bOB" => Key::Down,
            b"\x1b[C" | b"\x1bOC" => Key::Right,
            b"\x1b[D" | b"\x1bOD" => Key::Left,
            b"\x1b[H" | b"\x1bOH" | b"\x1b[1~" | b"\x1b[7~" => Key::Home,
            b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" | b"\x1b[8~" => Key::End,
            b"\x1b[3~" => Key::Delete,
            b"\x1b[5~" => Key::PageUp,
            b"\x1b[6~" => Key::PageDown,
            _ => {
                self.bytes.clear();
                return None;
            }
        };
        self.bytes.clear();
        Some(key)
    }

    pub(crate) fn needs_escape_timeout(&self) -> bool {
        self.bytes.first() == Some(&27)
    }

    pub(crate) fn escape_timeout(&mut self) -> Option<Key> {
        let standalone = self.bytes == [27];
        self.bytes.clear();
        standalone.then_some(Key::Esc)
    }
}

#[derive(Default)]
pub(crate) struct Line {
    pub(crate) text: String,
    pub(crate) cursor: usize,
}

impl Line {
    pub(crate) fn new(text: String) -> Self {
        Self {
            cursor: text.len(),
            text,
        }
    }

    /// Update the line and report whether its text (not just its cursor) changed.
    pub(crate) fn key(&mut self, key: Key) -> bool {
        let prev = || {
            self.text[..self.cursor]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(at, _)| at)
        };
        let next = || {
            self.cursor
                + self.text[self.cursor..]
                    .graphemes(true)
                    .next()
                    .map_or(0, str::len)
        };
        match key {
            Key::Left => self.cursor = prev(),
            Key::Right => self.cursor = next(),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = self.text.len(),
            Key::Char(ch) if !ch.is_control() => {
                self.text.insert(self.cursor, ch);
                self.cursor += ch.len_utf8();
                return true;
            }
            Key::Backspace if self.cursor != 0 => {
                let start = prev();
                self.text.drain(start..self.cursor);
                self.cursor = start;
                return true;
            }
            Key::Delete if self.cursor < self.text.len() => {
                let end = next();
                self.text.drain(self.cursor..end);
                return true;
            }
            _ => {}
        }
        false
    }
}
