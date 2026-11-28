use std::{cell::Cell, io, os::unix::net::UnixStream, panic, sync::Once};

use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::Errno,
    termios::{OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr},
};
use signal_hook::{
    consts::{SIGHUP, SIGINT, SIGTERM, SIGWINCH},
    iterator::{backend::SignalDelivery, exfiltrator::SignalOnly},
};

use crate::{
    input::{Decoder, Key},
    model::{Mode, Model},
    screen::{Layout, Screen, frame},
};

const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[>0u\x1b[2J";
const LEAVE: &[u8] = b"\x1b[<u\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[?1049l\x1b[?25h";
const RECORD: &[u8] = b"\x1b[=3u";
const BROWSE: &[u8] = b"\x1b[=0u";
// A lone Esc must be distinguishable from an arrow sequence split by SSH jitter.
const ESC_TIMEOUT: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 100_000_000,
};

thread_local! {
    static ORIGINAL: Cell<Option<Termios>> = const { Cell::new(None) };
}

struct Terminal;

impl Terminal {
    fn enter() -> io::Result<Self> {
        install_panic_hook();
        let stdin = io::stdin();
        let original = tcgetattr(&stdin)?;
        let mut raw = original.clone();
        raw.make_raw();
        ORIGINAL.set(Some(original));
        let terminal = Self;
        tcsetattr(&stdin, OptionalActions::Now, &raw)?;
        write(ENTER)?;
        Ok(terminal)
    }

    fn size() -> io::Result<(usize, usize)> {
        let size = tcgetwinsize(io::stdout())?;
        let width = if size.ws_col == 0 {
            80
        } else {
            size.ws_col.into()
        };
        let height = if size.ws_row == 0 {
            24
        } else {
            size.ws_row.into()
        };
        Ok((width, height))
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        restore();
    }
}

fn install_panic_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            restore();
            previous(info);
        }));
    });
}

fn restore() {
    if let Ok(Some(original)) = ORIGINAL.try_with(Cell::take) {
        let _ = tcsetattr(io::stdin(), OptionalActions::Now, &original);
        let _ = write(LEAVE);
    }
}

// Bypass stdout's buffered lock, which may already be borrowed when the panic hook runs.
fn write(mut bytes: &[u8]) -> io::Result<()> {
    let stdout = io::stdout();
    while !bytes.is_empty() {
        match rustix::io::write(&stdout, bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

type Signals = SignalDelivery<UnixStream, SignalOnly>;

pub(crate) fn run(model: &mut Model) -> io::Result<u8> {
    let (reader, writer) = UnixStream::pair()?;
    let mut signals = Signals::with_pipe(
        reader,
        writer,
        SignalOnly,
        [SIGINT, SIGTERM, SIGHUP, SIGWINCH],
    )?;
    let _terminal = Terminal::enter()?;
    event_loop(model, &mut signals)
}

fn signal_status(signal: i32) -> u8 {
    128 + u8::try_from(signal).expect("Unix terminal signal fits in one byte")
}

fn event_loop(model: &mut Model, signals: &mut Signals) -> io::Result<u8> {
    let stdin = io::stdin();
    let mut decoder = Decoder::default();
    let mut screen = Screen::default();
    let mut output = String::new();
    let mut bytes = [0; 512];
    let mut size = Terminal::size()?;
    let mut dirty = true;
    loop {
        let (width, height) = size;
        if dirty {
            let recording = matches!(model.mode, Mode::Record);
            if recording != decoder.recording {
                write(if recording { RECORD } else { BROWSE })?;
                decoder.recording = recording;
            }
            screen.render(model, width, height, &Layout::new(model, width, height));
            frame(&mut output, &screen);
            write(output.as_bytes())?;
            dirty = false;
        }
        let mut fds = [
            PollFd::new(signals.get_read(), PollFlags::IN),
            PollFd::new(&stdin, PollFlags::IN),
        ];
        match poll(
            &mut fds,
            decoder.needs_escape_timeout().then_some(&ESC_TIMEOUT),
        ) {
            Ok(0) => {
                if let Some(key) = decoder.escape_timeout() {
                    if !model.key(key, Layout::new(model, width, height).page()) {
                        return Ok(0);
                    }
                    dirty = true;
                }
                continue;
            }
            Err(Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let input = fds[1].revents();
        if !fds[0].revents().is_empty() {
            let mut resize = false;
            for signal in signals.pending() {
                if signal != SIGWINCH {
                    return Ok(signal_status(signal));
                }
                resize = true;
            }
            if resize {
                size = Terminal::size()?;
                dirty = true;
            }
        }
        if input.contains(PollFlags::IN) {
            let n = match rustix::io::read(&stdin, &mut bytes) {
                Ok(0) => return Ok(0),
                Ok(n) => n,
                Err(Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            };
            for key in bytes[..n].iter().filter_map(|&byte| decoder.push(byte)) {
                if key == Key::Interrupt {
                    return Ok(signal_status(SIGINT));
                }
                let layout = Layout::new(model, size.0, size.1);
                if !layout.mouse(model, size.0, key) && !model.key(key, layout.page()) {
                    return Ok(0);
                }
                dirty = true;
            }
        } else if !input.is_empty() {
            return Ok(0);
        }
    }
}

#[cfg(test)]
mod tests;
