//! The few D-Bus messages sumie sends, in little-endian wire format: SASL EXTERNAL, `Hello` and
//! `org.freedesktop.Notifications.Notify`. Not a general client.

use std::fmt::Write as _;

const METHOD_CALL: u8 = 1;
const FIELD_PATH: u8 = 1;
const FIELD_INTERFACE: u8 = 2;
const FIELD_MEMBER: u8 = 3;
const FIELD_DESTINATION: u8 = 6;
const FIELD_REPLY_SERIAL: u8 = 5;
const FIELD_SIGNATURE: u8 = 8;
pub(super) const ERROR: u8 = 3;
pub(super) const SIGNAL: u8 = 4;

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";

/// `AUTH EXTERNAL` carries the decimal uid as hex, after the mandatory NUL byte.
pub(super) fn auth_line(uid: u32) -> Vec<u8> {
    let mut hex = String::new();
    for byte in uid.to_string().bytes() {
        write!(hex, "{byte:02x}").expect("writing to a string");
    }
    format!("\0AUTH EXTERNAL {hex}\r\n").into_bytes()
}

pub(super) const BEGIN: &[u8] = b"BEGIN\r\n";

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn align(&mut self, to: usize) {
        self.0.resize(self.0.len().next_multiple_of(to), 0);
    }

    fn byte(&mut self, value: u8) {
        self.0.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.align(4);
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn string(&mut self, value: &str) {
        self.u32(u32::try_from(value.len()).expect("bounded text"));
        self.0.extend_from_slice(value.as_bytes());
        self.byte(0);
    }

    fn signature(&mut self, value: &str) {
        self.byte(u8::try_from(value.len()).expect("short signature"));
        self.0.extend_from_slice(value.as_bytes());
        self.byte(0);
    }

    /// One header field: `(byte, variant)` with a string-like value of `kind` (`s`, `o` or `g`).
    fn field(&mut self, code: u8, kind: char, value: &str) {
        self.align(8);
        self.byte(code);
        self.signature(kind.encode_utf8(&mut [0; 4]));
        if kind == 'g' {
            self.signature(value);
        } else {
            self.string(value);
        }
    }
}

struct Call<'a> {
    destination: &'a str,
    path: &'a str,
    member: &'a str,
    signature: &'a str,
}

/// A method call on the interface named like its destination. `body` was marshaled from an
/// 8-byte boundary, which is where it starts in the message.
fn call(serial: u32, target: &Call, body: &[u8]) -> Vec<u8> {
    let mut out = Writer::default();
    out.0.extend_from_slice(&[b'l', METHOD_CALL, 0, 1]);
    out.u32(u32::try_from(body.len()).expect("bounded body"));
    out.u32(serial);
    out.u32(0);
    out.field(FIELD_PATH, 'o', target.path);
    out.field(FIELD_INTERFACE, 's', target.destination);
    out.field(FIELD_MEMBER, 's', target.member);
    out.field(FIELD_DESTINATION, 's', target.destination);
    if !target.signature.is_empty() {
        out.field(FIELD_SIGNATURE, 'g', target.signature);
    }
    let fields = u32::try_from(out.0.len() - 16).expect("short header");
    out.0[12..16].copy_from_slice(&fields.to_le_bytes());
    out.align(8);
    out.0.extend_from_slice(body);
    out.0
}

pub(super) fn hello(serial: u32) -> Vec<u8> {
    let target = Call {
        destination: "org.freedesktop.DBus",
        path: "/org/freedesktop/DBus",
        member: "Hello",
        signature: "",
    };
    call(serial, &target, &[])
}

/// Notification servers parse a markup subset in the summary and body (dunst, mako, GNOME), and
/// the text comes from the program in the pane.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// `Notify(app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout)`
/// with no actions or hints and the daemon's default timeout.
pub(super) fn notify(serial: u32, summary: &str, body: &str) -> Vec<u8> {
    let mut args = Writer::default();
    args.string("sumie");
    args.u32(0);
    args.string("");
    args.string(&escape(summary));
    args.string(&escape(body));
    args.u32(0); // actions: an empty `as`
    args.u32(0); // hints: an empty `a{sv}` ...
    args.align(8); // ... still pads to its 8-byte element alignment
    args.u32(u32::MAX); // expire_timeout -1
    let target = Call {
        destination: NOTIFICATIONS,
        path: "/org/freedesktop/Notifications",
        member: "Notify",
        signature: "susssasa{sv}i",
    };
    call(serial, &target, &args.0)
}

/// The `reply_serial` header field of a complete message, if it has one.
pub(super) fn reply_serial(message: &[u8]) -> Option<u32> {
    let word = |at: usize| {
        Some(u32::from_le_bytes(
            message.get(at..at + 4)?.try_into().ok()?,
        ))
    };
    let end = 16 + usize::try_from(word(12)?).ok()?;
    let mut at = 16;
    while at < end {
        let code = *message.get(at)?;
        let kind_length = usize::from(*message.get(at + 1)?);
        let kind = message.get(at + 2..at + 2 + kind_length)?;
        at += 3 + kind_length;
        let size = match kind {
            b"u" => {
                at = at.next_multiple_of(4);
                if code == FIELD_REPLY_SERIAL {
                    return word(at);
                }
                4
            }
            b"s" | b"o" => {
                at = at.next_multiple_of(4);
                usize::try_from(word(at)?).ok()? + 5
            }
            b"g" => usize::from(*message.get(at)?) + 2,
            _ => return None,
        };
        at = (at + size).next_multiple_of(8);
    }
    None
}

/// Length of the first complete message in `buffer` and its type, if all of it has arrived.
pub(super) fn frame(buffer: &[u8]) -> Option<(usize, u8)> {
    let header: &[u8; 16] = buffer.get(..16)?.try_into().ok()?;
    let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().expect("four bytes"));
    let total =
        16 + usize::try_from(word(12)).ok()?.next_multiple_of(8) + usize::try_from(word(4)).ok()?;
    (header[0] == b'l' && buffer.len() >= total).then_some((total, header[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_message_is_framed_and_aligned() {
        let message = notify(7, "Build", "done é");
        assert_eq!(frame(&message), Some((message.len(), METHOD_CALL)));
        assert_eq!(&message[8..12], &7u32.to_le_bytes());
        // The body matches GDBus marshaling the same arguments: sumie, replaces_id, icon,
        // summary, body, actions, hints padded to their 8-byte elements, expire_timeout -1.
        let expected: &[u8] = b"\x05\0\0\0sumie\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x05\0\0\0Build\0\0\0\x07\0\0\0done \xc3\xa9\0\0\0\0\0\0\0\0\0\xff\xff\xff\xff";
        let body = &message[message.len() - expected.len()..];
        assert_eq!(body, expected);
    }

    #[test]
    fn escape_neutralises_markup() {
        let table = [
            ("plain é", "plain é"),
            ("<b>bold</b>", "&lt;b&gt;bold&lt;/b&gt;"),
            ("a & b", "a &amp; b"),
            ("&lt;", "&amp;lt;"),
            ("<a href=\"x\">y</a>", "&lt;a href=\"x\"&gt;y&lt;/a&gt;"),
        ];
        for (text, expected) in table {
            assert_eq!(escape(text), expected, "{text}");
            let message = notify(1, text, text);
            let window = |needle: &str| {
                message
                    .windows(needle.len())
                    .any(|at| at == needle.as_bytes())
            };
            assert!(window(expected), "{text}");
            assert!(!window("<b>"), "{text}");
        }
    }

    /// A reply or signal as the bus sends it: a destination, then `reply_serial` when given.
    fn incoming(kind: u8, reply_serial: Option<u32>) -> Vec<u8> {
        let mut out = Writer::default();
        out.0.extend_from_slice(&[b'l', kind, 0, 1]);
        out.u32(0);
        out.u32(9);
        out.u32(0);
        out.field(FIELD_DESTINATION, 's', ":1.5");
        if let Some(serial) = reply_serial {
            out.align(8);
            out.byte(FIELD_REPLY_SERIAL);
            out.signature("u");
            out.u32(serial);
        }
        let fields = u32::try_from(out.0.len() - 16).expect("short header");
        out.0[12..16].copy_from_slice(&fields.to_le_bytes());
        out.align(8);
        out.0
    }

    #[test]
    fn replies_name_the_call_they_answer_and_signals_do_not() {
        let table = [
            (2, Some(7), Some(7)),
            (3, Some(1), Some(1)),
            (SIGNAL, None, None),
        ];
        for (kind, serial, expected) in table {
            let message = incoming(kind, serial);
            assert_eq!(frame(&message), Some((message.len(), kind)));
            assert_eq!(reply_serial(&message), expected, "type {kind}");
        }
        assert_eq!(reply_serial(&incoming(2, Some(7))[..20]), None);
    }

    #[test]
    fn auth_hex_encodes_the_decimal_uid() {
        assert_eq!(auth_line(1000), b"\0AUTH EXTERNAL 31303030\r\n");
    }
}
