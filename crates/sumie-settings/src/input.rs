use sumie_core::Chord;
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
    Chord(Chord),
    Click { column: usize, row: usize },
}

#[derive(Default)]
pub(crate) struct Decoder {
    bytes: Vec<u8>,
    pub(crate) recording: bool,
}

impl Decoder {
    pub(crate) fn push(&mut self, byte: u8) -> Option<Key> {
        match (self.bytes.first(), byte) {
            (None, 27) => {
                self.bytes.push(byte);
                None
            }
            (None, _) if self.recording => legacy_key(byte),
            (None, 3) => Some(Key::Interrupt),
            (None, b'\t') => Some(Key::Tab),
            (None, b'\r' | b'\n') => Some(Key::Enter),
            (None, 8 | 127) => Some(Key::Backspace),
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
            self.bytes.clear();
            return if self.recording {
                Chord::from_terminal(byte.into(), 2).map(Key::Chord)
            } else {
                None
            };
        }
        self.bytes.push(byte);
        if self.bytes.len() < 3 || (byte < 0x40 && self.bytes.len() < 64) {
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
            sequence => {
                let key = sgr_mouse(sequence)
                    .or_else(|| self.recording.then(|| reported_chord(sequence)).flatten());
                self.bytes.clear();
                return key;
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

fn legacy_key(byte: u8) -> Option<Key> {
    match byte {
        // Legacy terminals cannot distinguish these control bytes from the corresponding keys.
        8 | b'\t' | b'\r' | 127 => None,
        1..=26 => Chord::from_terminal(u32::from(b'a' + byte - 1), 4).map(Key::Chord),
        32..=126 => Some(Key::Char(char::from(byte))),
        _ => None,
    }
}

fn sgr_mouse(sequence: &[u8]) -> Option<Key> {
    let fields = std::str::from_utf8(sequence.strip_prefix(b"\x1b[<")?.strip_suffix(b"M")?).ok()?;
    let (button, coordinates) = fields.split_once(';')?;
    let (column, row) = coordinates.split_once(';')?;
    let column = column.parse::<usize>().ok()?.checked_sub(1)?;
    let row = row.parse::<usize>().ok()?.checked_sub(1)?;
    (button.parse::<u8>().ok()? & !0b0001_1100 == 0).then_some(Key::Click { column, row })
}

fn reported_chord(sequence: &[u8]) -> Option<Key> {
    let (final_byte, body) = sequence
        .strip_prefix(b"\x1b[")
        .or_else(|| sequence.strip_prefix(b"\x1bO"))?
        .split_last()?;
    let body = std::str::from_utf8(body).ok()?;
    let mut parts = body.split(';');
    let key = parts.next()?;
    let modifiers = parts.next().unwrap_or("1");
    let (mods, event) = modifiers.split_once(':').unwrap_or((modifiers, "1"));
    if event != "1" {
        return None;
    }
    let mods = mods.parse::<u8>().ok()?.checked_sub(1)?;
    let key = match final_byte {
        b'u' => key.split(':').next()?.parse().ok()?,
        b'A' => 57352,
        b'B' => 57353,
        b'C' => 57351,
        b'D' => 57350,
        b'H' => 57356,
        b'F' => 57357,
        b'P'..=b'S' => 57364 + u32::from(final_byte - b'P'),
        b'Z' => return Chord::from_terminal(9, 1).map(Key::Chord),
        b'~' => match key {
            "1" | "7" => 57356,
            "2" => 57348,
            "3" => 57349,
            "4" | "8" => 57357,
            "5" => 57354,
            "6" => 57355,
            "15" => 57368,
            "17" => 57369,
            "18" => 57370,
            "19" => 57371,
            "20" => 57372,
            "21" => 57373,
            "23" => 57374,
            "24" => 57375,
            _ => return None,
        },
        _ => return None,
    };
    if key == 27 {
        return Some(Key::Esc);
    }
    Chord::from_terminal(key, mods).map(Key::Chord)
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
