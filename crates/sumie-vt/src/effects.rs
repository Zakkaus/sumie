use std::ffi::{c_int, c_void};
use std::mem::size_of;
use std::ptr::{from_mut, null};
use std::slice;

use crate::ffi;

pub(super) const OUTPUT_LIMIT: usize = 1024 * 1024;

/// Effects to apply after the terminal call returns. Payloads must not be logged.
pub enum Event {
    /// Decoded text bytes, or `None` for an explicit clipboard clear.
    Clipboard(Option<Vec<u8>>),
    Title(String),
}

#[derive(Default)]
pub(super) struct Effects {
    pub(super) replies: Vec<u8>,
    clipboard: Vec<Option<Vec<u8>>>,
    clipboard_bytes: usize,
    title: Option<String>,
    pub(super) version: &'static str,
}

impl Effects {
    pub(super) fn drain_events(&mut self) -> impl Iterator<Item = Event> + '_ {
        self.clipboard_bytes = 0;
        self.clipboard
            .drain(..)
            .map(Event::Clipboard)
            .chain(self.title.take().map(Event::Title))
    }

    unsafe fn clipboard_write(&mut self, request: &ffi::ClipboardWrite) -> c_int {
        if request.location != ffi::CLIPBOARD_LOCATION_STANDARD {
            return ffi::CLIPBOARD_WRITE_UNSUPPORTED;
        }
        let bytes = match request.contents_len {
            0 => None,
            1 => {
                // SAFETY: The callback lends one initialized representation and its strings.
                let content = unsafe { &*request.contents };
                // SAFETY: The MIME string is valid for the lifetime of the representation.
                if unsafe { borrowed_bytes(&content.mime) } != b"text/plain" {
                    return ffi::CLIPBOARD_WRITE_UNSUPPORTED;
                }
                Some(&content.data)
            }
            _ => return ffi::CLIPBOARD_WRITE_UNSUPPORTED,
        };
        // Charge at least one byte so empty writes and clears cannot grow the queue unbounded.
        let len = bytes.map_or(0, |data| data.len).max(1);
        if len > OUTPUT_LIMIT - self.clipboard_bytes {
            return ffi::CLIPBOARD_WRITE_DENIED;
        }
        let bytes = bytes.map(|data| {
            // SAFETY: Ghostty lends the decoded bytes until the callback returns.
            unsafe { borrowed_bytes(data) }.to_vec()
        });
        self.clipboard.push(bytes);
        self.clipboard_bytes += len;
        ffi::CLIPBOARD_WRITE_SUCCESS
    }
}

// `GhosttyString` may have a null pointer when empty. Otherwise callers must
// ensure that it describes initialized bytes valid for the borrow.
unsafe fn borrowed_bytes(value: &ffi::Str) -> &[u8] {
    if value.len == 0 {
        return &[];
    }
    // SAFETY: Guaranteed by the caller for the nonempty case.
    unsafe { slice::from_raw_parts(value.ptr, value.len) }
}

pub(super) unsafe extern "C" fn queue_reply(
    _: ffi::Terminal,
    data: *mut c_void,
    bytes: *const u8,
    len: usize,
) {
    if len == 0 {
        return;
    }
    // SAFETY: Userdata is the boxed Effects; callbacks run within an exclusive terminal call.
    let queue = unsafe { &mut (*data.cast::<Effects>()).replies };
    if len <= OUTPUT_LIMIT - queue.len() {
        // SAFETY: Ghostty lends `len` initialized bytes for the duration of the callback.
        queue.extend_from_slice(unsafe { slice::from_raw_parts(bytes, len) });
    }
}

pub(super) unsafe extern "C" fn queue_clipboard(
    _: ffi::Terminal,
    data: *mut c_void,
    request: *const ffi::ClipboardWrite,
) {
    // SAFETY: Ghostty lends the request and exclusively passes the boxed Effects as userdata.
    let result = unsafe { (&mut *data.cast::<Effects>()).clipboard_write(&*request) };
    let reply = ffi::ClipboardWriteReply {
        size: size_of::<ffi::ClipboardWriteReply>(),
        result,
        remember: false,
    };
    // SAFETY: The original request and the sized reply are live until the synchronous call returns.
    unsafe { ((*request).reply)(request, &raw const reply) };
}

pub(super) unsafe extern "C" fn deny_clipboard_read(
    _: ffi::Terminal,
    _: *mut c_void,
    request: *const ffi::ClipboardRead,
) {
    // TODO(P1): If enabled in config, answer only from a complete, current clipboard cache.
    let reply = ffi::ClipboardReadReply {
        size: size_of::<ffi::ClipboardReadReply>(),
        result: ffi::CLIPBOARD_READ_DENIED,
        contents: null(),
        contents_len: 0,
        available: null(),
        available_len: 0,
        remember: false,
    };
    // SAFETY: The original request and the sized reply are live until the synchronous call returns.
    unsafe { ((*request).reply)(request, &raw const reply) };
}

pub(super) unsafe extern "C" fn queue_title(terminal: ffi::Terminal, data: *mut c_void) {
    let mut title = ffi::Str {
        ptr: null(),
        len: 0,
    };
    // SAFETY: The title callback expressly permits this read into a GhosttyString.
    if unsafe {
        ffi::ghostty_terminal_get(
            terminal,
            ffi::TERMINAL_DATA_TITLE,
            from_mut(&mut title).cast(),
        )
    } != ffi::SUCCESS
    {
        return;
    }
    // SAFETY: Ghostty lends the title until the next terminal mutation.
    let bytes = unsafe { borrowed_bytes(&title) };
    // SAFETY: Userdata is the boxed Effects, exclusively accessible in this callback.
    unsafe { &mut *data.cast::<Effects>() }.title =
        Some(String::from_utf8_lossy(bytes).into_owned());
}

#[cfg(test)]
mod tests {
    use super::{Event, OUTPUT_LIMIT};
    use crate::Terminal;

    #[test]
    fn osc52_queues_only_bounded_standard_writes() {
        let mut terminal = Terminal::new(5, 2).unwrap();
        terminal.write(b"\x1b]52;c;b3NjNTItb2s=\x07");
        {
            let mut events = terminal.drain_events();
            assert!(
                matches!(events.next(), Some(Event::Clipboard(Some(bytes))) if bytes == b"osc52-ok")
            );
            assert!(events.next().is_none());
        }

        terminal.write(b"\x1b]52;c;");
        terminal.write(&b"YWFh".repeat(OUTPUT_LIMIT / 3));
        terminal.write(b"YWE=\x07"); // 1 MiB + 1 decoded bytes.
        terminal.write(b"\x1b]52;p;YQ==\x07\x1b]52;s;YQ==\x07\x1b]52;c;?\x07");
        assert!(terminal.drain_events().next().is_none());
        let mut refused = false;
        terminal.drain_replies(|reply| refused = reply == b"\x1b]52;c;\x07");
        assert!(refused);
    }

    #[test]
    fn osc2_queues_latest_title() {
        let mut terminal = Terminal::new(5, 2).unwrap();
        terminal.write(b"\x1b]0;earlier\x07\x1b]2;title-ok\x07");
        let mut events = terminal.drain_events();
        assert!(matches!(events.next(), Some(Event::Title(title)) if title == "title-ok"));
        assert!(events.next().is_none());
    }
}
