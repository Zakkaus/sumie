use std::{
    io::{self, Write as _},
    os::{fd::AsFd, unix::net::UnixStream},
};

use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::Errno,
    termios::{OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr},
};
use signal_hook::{
    SigId,
    consts::{SIGINT, SIGTERM, SIGWINCH},
    low_level::{pipe, unregister},
};

use crate::{
    input::Decoder,
    model::Model,
    screen::{Layout, Screen, frame},
};

const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[2J";
const LEAVE: &[u8] = b"\x1b[?1049l\x1b[?25h";
const ESC_TIMEOUT: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 35_000_000,
};

struct Terminal {
    original: Termios,
    output: io::StdoutLock<'static>,
}

impl Terminal {
    fn enter() -> io::Result<Self> {
        let stdin = io::stdin();
        let original = tcgetattr(&stdin)?;
        let mut raw = original.clone();
        raw.make_raw();
        let mut terminal = Self {
            original,
            output: io::stdout().lock(),
        };
        tcsetattr(&stdin, OptionalActions::Now, &raw)?;
        terminal.write(ENTER)?;
        Ok(terminal)
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.output.write_all(bytes)?;
        self.output.flush()
    }

    fn size(&self) -> io::Result<(usize, usize)> {
        let size = tcgetwinsize(self.output.as_fd())?;
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
        let _ = tcsetattr(io::stdin(), OptionalActions::Now, &self.original);
        let _ = self.write(LEAVE);
    }
}

struct SignalPipe {
    reader: UnixStream,
    registrations: Vec<SigId>,
}

impl SignalPipe {
    fn new(signals: &[i32]) -> io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        let mut wake = Self {
            reader,
            registrations: Vec::with_capacity(signals.len()),
        };
        for signal in signals {
            wake.registrations
                .push(pipe::register(*signal, writer.try_clone()?)?);
        }
        Ok(wake)
    }
}

impl Drop for SignalPipe {
    fn drop(&mut self) {
        for id in self.registrations.drain(..) {
            unregister(id);
        }
    }
}

pub(crate) fn run(model: &mut Model) -> io::Result<()> {
    let quit = SignalPipe::new(&[SIGINT, SIGTERM])?;
    let resize = SignalPipe::new(&[SIGWINCH])?;
    let mut terminal = Terminal::enter()?;
    let stdin = io::stdin();
    let mut poll_fds = [
        PollFd::new(&quit.reader, PollFlags::IN),
        PollFd::new(&resize.reader, PollFlags::IN),
        PollFd::new(&stdin, PollFlags::IN),
    ];
    let mut decoder = Decoder::default();
    let mut screen = Screen::default();
    let mut output = String::new();
    let mut bytes = [0; 512];
    let mut size = terminal.size()?;
    let mut dirty = true;
    loop {
        let (width, height) = size;
        if dirty {
            screen.render(model, width, height, &Layout::new(model, width, height));
            frame(&mut output, &screen);
            terminal.write(output.as_bytes())?;
            dirty = false;
        }
        match poll(
            &mut poll_fds,
            decoder.needs_escape_timeout().then_some(&ESC_TIMEOUT),
        ) {
            Ok(0) => {
                if let Some(key) = decoder.escape_timeout() {
                    if !model.key(key, Layout::new(model, width, height).page()) {
                        return Ok(());
                    }
                    dirty = true;
                }
                continue;
            }
            Err(Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        if !poll_fds[0].revents().is_empty() {
            return Ok(());
        }
        if !poll_fds[1].revents().is_empty() {
            rustix::io::read(&resize.reader, &mut bytes)?;
            size = terminal.size()?;
            dirty = true;
        }
        if poll_fds[2].revents().contains(PollFlags::IN) {
            let n = match rustix::io::read(&stdin, &mut bytes) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            };
            for key in bytes[..n].iter().filter_map(|&byte| decoder.push(byte)) {
                let page = Layout::new(model, size.0, size.1).page();
                if !model.key(key, page) {
                    return Ok(());
                }
                dirty = true;
            }
        } else if !poll_fds[2].revents().is_empty() {
            return Ok(());
        }
    }
}
