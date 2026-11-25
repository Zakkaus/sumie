use std::ffi::{c_int, c_void};
use std::mem::{size_of, take};
use std::ptr::{from_mut, null};
use std::slice;

use sumie_core::notify::{Notification, Progress, ProgressState};

use crate::ffi;

pub(super) const OUTPUT_LIMIT: usize = 1024 * 1024;
const NOTIFICATION_LIMIT: usize = 8;

/// Effects to apply after the terminal call returns. Payloads must not be logged.
pub enum Event {
    /// Decoded text bytes, or `None` for an explicit clipboard clear.
    Clipboard(Option<Vec<u8>>),
    Title(String),
    /// A bell character arrived since the last drain.
    Bell,
    Notification(Notification),
    /// The latest OSC 9;4 report since the last drain; `None` removes the indicator.
    Progress(Option<Progress>),
}

#[derive(Default)]
pub(super) struct Effects {
    pub(super) replies: Vec<u8>,
    clipboard: Vec<Option<Vec<u8>>>,
    clipboard_bytes: usize,
    title: Option<String>,
    bell: bool,
    notifications: Vec<Notification>,
    progress: Option<Progress>,
    progress_pending: bool,
    pub(super) version: &'static str,
}

impl Effects {
    pub(super) fn drain_events(&mut self) -> impl Iterator<Item = Event> + '_ {
        self.clipboard_bytes = 0;
        self.clipboard
            .drain(..)
            .map(Event::Clipboard)
            .chain(self.title.take().map(Event::Title))
            .chain(self.notifications.drain(..).map(Event::Notification))
            .chain(take(&mut self.progress_pending).then_some(Event::Progress(self.progress)))
            .chain(take(&mut self.bell).then_some(Event::Bell))
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

/// Install every effect callback. Returns the first failing result code.
///
/// # Safety
/// `terminal` is live, and its userdata is the boxed `Effects` that outlives it.
pub(super) unsafe fn register(terminal: ffi::Terminal) -> c_int {
    for (option, callback) in [
        (ffi::TERMINAL_OPT_WRITE_PTY, queue_reply as *const c_void),
        (
            ffi::TERMINAL_OPT_CLIPBOARD_WRITE,
            queue_clipboard as *const c_void,
        ),
        (
            ffi::TERMINAL_OPT_CLIPBOARD_READ,
            deny_clipboard_read as *const c_void,
        ),
        (
            ffi::TERMINAL_OPT_TITLE_CHANGED,
            queue_title as *const c_void,
        ),
        (ffi::TERMINAL_OPT_BELL, queue_bell as *const c_void),
        (
            ffi::TERMINAL_OPT_DESKTOP_NOTIFICATION,
            queue_notification as *const c_void,
        ),
        (
            ffi::TERMINAL_OPT_PROGRESS_REPORT,
            queue_progress as *const c_void,
        ),
    ] {
        // SAFETY: The caller guarantees a live terminal; each callback matches its option's type.
        let result = unsafe { ffi::ghostty_terminal_set(terminal, option, callback) };
        if result != ffi::SUCCESS {
            return result;
        }
    }
    ffi::SUCCESS
}

pub(super) unsafe extern "C" fn queue_bell(_: ffi::Terminal, data: *mut c_void) {
    // SAFETY: Userdata is the boxed Effects, exclusively accessible in this callback.
    unsafe { &mut *data.cast::<Effects>() }.bell = true;
}

pub(super) unsafe extern "C" fn queue_notification(
    _: ffi::Terminal,
    data: *mut c_void,
    request: *const ffi::DesktopNotification,
) {
    // SAFETY: Userdata is the boxed Effects, exclusively accessible in this callback.
    let queue = &mut unsafe { &mut *data.cast::<Effects>() }.notifications;
    if queue.len() == NOTIFICATION_LIMIT {
        return;
    }
    // SAFETY: Ghostty lends the request and both strings until the callback returns.
    let (title, body) = unsafe {
        let request = &*request;
        (
            borrowed_bytes(&request.title),
            borrowed_bytes(&request.body),
        )
    };
    queue.push(Notification::from_bytes(title, body));
}

pub(super) unsafe extern "C" fn queue_progress(
    _: ffi::Terminal,
    data: *mut c_void,
    report: *const ffi::ProgressReport,
) {
    // SAFETY: Ghostty lends the report until the callback returns.
    let report = unsafe { &*report };
    let state = match report.state {
        ffi::PROGRESS_SET => Some(ProgressState::Set),
        ffi::PROGRESS_ERROR => Some(ProgressState::Error),
        ffi::PROGRESS_INDETERMINATE => Some(ProgressState::Indeterminate),
        ffi::PROGRESS_PAUSE => Some(ProgressState::Paused),
        _ => None,
    };
    let percent = u8::try_from(report.progress)
        .ok()
        .map(|value| value.min(100));
    // SAFETY: Userdata is the boxed Effects, exclusively accessible in this callback.
    let effects = unsafe { &mut *data.cast::<Effects>() };
    effects.progress = state.map(|state| Progress { state, percent });
    effects.progress_pending = true;
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
    fn notifications_progress_and_bell_queue_in_order() {
        let table: [(&[u8], &str); 12] = [
            (b"\x1b]9;plain body\x07", "n:|plain body"),
            (b"\x1b]777;notify;Build;done\x1b\\", "n:Build|done"),
            (b"\x1b]9;4;1;40\x07", "p:Set 40"),
            (b"\x1b]9;4;3\x07", "p:Indeterminate -"),
            (b"\x1b]9;4;2;200\x07", "p:Error 100"),
            (b"\x1b]9;4;4;7\x07", "p:Paused 7"),
            (b"\x1b]9;4;0\x07", "p:none"),
            (b"\x07", "bell"),
            // The parser splits OSC 9 on a leading ConEmu command: 9;5 is one and is dropped,
            // 9;45 is plain text, and a malformed 9;4 is text too instead of a report.
            (b"\x1b]9;5 done\x07", "none"),
            (b"\x1b]9;45 done\x07", "n:|45 done"),
            (b"\x1b]9;4;\x07", "n:|4;"),
            (b"\x1b]9;4;1;\x07", "p:Set -"),
        ];
        for (bytes, expected) in table {
            let mut terminal = Terminal::new(5, 2).unwrap();
            terminal.write(bytes);
            let seen = terminal.drain_events().next().map(|event| match event {
                Event::Notification(n) => format!("n:{}|{}", n.title, n.body),
                Event::Progress(None) => "p:none".to_owned(),
                Event::Progress(Some(p)) => format!(
                    "p:{:?} {}",
                    p.state,
                    p.percent.map_or("-".to_owned(), |v| v.to_string())
                ),
                Event::Bell => "bell".to_owned(),
                _ => "other".to_owned(),
            });
            assert_eq!(seen.as_deref().unwrap_or("none"), expected, "{bytes:?}");
        }
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
