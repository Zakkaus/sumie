//! Serves the shared clipboard operations from the pasteboard through helper children, so no
//! pasteboard call can block the main thread.
//!
//! Each request runs in its own child: this executable in [`CHILD_ARG`] mode, its stdin one
//! end of a socket pair, as for the configuration child (`macos::reload`). The app writes one
//! request frame (`reload::frame`) without blocking and reads one reply. Requests run one at a
//! time, in the order they were made, so a paste after a copy reads that copy. A child that
//! has not replied within [`TIMEOUT`] is cancelled: the app closes its end, the child exits on
//! that EOF even while a lazy provider blocks it, and SIGKILL follows at a deadline. The app
//! reaps every child on exit readiness with `try_wait` and never waits or joins.
//!
//! A read's reply carries the recipient generation of its request, and the session drops it
//! when that recipient is gone; a read for a recipient that already has one pending joins it.
//! Contents never reach argv or the logs.
//!
//! Release builds serve the general board. A `macos-ci` build serves only the board that
//! `SUMIE_MACOS_CI_PASTEBOARD` names and refuses every request without one, so no check
//! touches the user's pasteboard.

use std::{
    collections::VecDeque,
    io::{self, ErrorKind, Read as _, Write as _},
    os::{
        fd::{AsFd as _, OwnedFd},
        unix::net::UnixStream,
    },
    process::{self, Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use sumie_macos::{
    exit::ExitWatch,
    pasteboard::{self, Board},
    readiness::{Interest, Key},
};

use super::sources::{Timers, Watches};
use crate::{
    Result,
    platform::TimerSource as _,
    reload::frame::{self, Decoder, Frame, Kind},
};

/// Selects the child mode on the command line.
pub(crate) const CHILD_ARG: &str = "--internal-pasteboard";
/// Names the check's board for a `macos-ci` build's helpers.
#[cfg(any(test, feature = "macos-ci"))]
const BOARD_ENV: &str = "SUMIE_MACOS_CI_PASTEBOARD";
const TIMEOUT: Duration = Duration::from_secs(5);
/// A cancelled child exits on EOF at once unless it is stopped; then SIGKILL ends it.
const KILL_AFTER: Duration = Duration::from_millis(500);
/// Requests beyond this many waiting are dropped, so a stalled board cannot queue without bound.
const QUEUE: usize = 16;
/// The longest string a read delivers or a copy sends: the PTY's whole input FIFO.
const LIMIT: usize = sumie_pty::INPUT_LIMIT;
const _: () = assert!(LIMIT <= frame::MAX_BODY);

pub(super) enum Request {
    /// A paste for the recipient generation the session stamped.
    Read { recipient: u64 },
    /// Replace the string, or clear the board for `None`.
    Write(Option<String>),
}

/// A finished read for the session.
pub(super) struct Pasted {
    pub(super) text: String,
    pub(super) recipient: u64,
}

type Launch = Box<dyn Fn() -> io::Result<Command>>;

/// The app's side of the helper children. Call [`Pasteboard::service`] every turn.
pub(super) struct Pasteboard {
    launch: Launch,
    timeout: Duration,
    timers: Timers,
    queue: VecDeque<Request>,
    running: Option<Running>,
    /// Finished or cancelled children that are not yet reaped.
    closing: Vec<Helper>,
    next_id: u64,
    closed: bool,
}

/// The request in flight.
struct Running {
    helper: Helper,
    /// The paste's recipient; `None` for a copy.
    recipient: Option<u64>,
    output: Vec<u8>,
    written: usize,
    decoder: Decoder,
    deadline: (Instant, Key),
}

/// One child. Its socket stays open until the reply or a cancel.
struct Helper {
    id: u64,
    child: Child,
    exit: ExitWatch,
    socket: Option<UnixStream>,
    kill: Option<(Instant, Key)>,
    reaped: bool,
}

enum Outcome {
    Pending,
    Reply(Frame),
    Failed(String),
}

const fn socket_key(id: u64) -> Key {
    Key::new(id * 2, Interest::Pasteboard)
}

const fn exit_key(id: u64) -> Key {
    Key::new(id * 2 + 1, Interest::Pasteboard)
}

const fn write_key(id: u64) -> Key {
    Key::new(id, Interest::PasteboardWrite)
}

impl Pasteboard {
    /// The production helpers: this executable in [`CHILD_ARG`] mode.
    pub(super) fn new(watches: &Watches) -> Result<Self> {
        let program = std::env::current_exe()?;
        let launch = move || {
            let mut command = Command::new(&program);
            command.arg(CHILD_ARG);
            Ok(command)
        };
        Ok(Self::with_launch(Box::new(launch), TIMEOUT, watches))
    }

    fn with_launch(launch: Launch, timeout: Duration, watches: &Watches) -> Self {
        Self {
            launch,
            timeout,
            timers: watches.animation_timer(),
            queue: VecDeque::new(),
            running: None,
            closing: Vec::new(),
            next_id: 0,
            closed: false,
        }
    }

    /// Queue a request and start it if none is in flight. Never blocks on the board.
    pub(super) fn request(&mut self, request: Request, watches: &mut Watches) {
        if self.closed {
            return;
        }
        if let Request::Write(Some(text)) = &request
            && text.len() > LIMIT
        {
            tracing::warn!(
                bytes = text.len(),
                limit = LIMIT,
                "the copy exceeds the pasteboard limit; the pasteboard is unchanged"
            );
            return;
        }
        if let Request::Read { recipient } = request
            && self.reading(recipient)
        {
            tracing::debug!("a paste for this recipient is already pending");
            return;
        }
        if self.queue.len() >= QUEUE {
            tracing::warn!("too many pasteboard requests are waiting; dropped one");
            return;
        }
        self.queue.push_back(request);
        self.start_next(watches);
    }

    /// Take a finished read, enforce the deadlines, reap children and start the next
    /// request. Never blocks.
    pub(super) fn service(&mut self, watches: &mut Watches) -> Result<Vec<Pasted>> {
        watches.take_pasteboard_ready()?;
        let mut pasted = Vec::new();
        if let Some(running) = &mut self.running {
            let outcome = match running.progress(watches, ExitWatch::try_reap)? {
                Outcome::Pending if running.deadline.0 <= Instant::now() => {
                    Outcome::Failed(format!("no reply within {:?}", self.timeout))
                }
                outcome => outcome,
            };
            if !matches!(outcome, Outcome::Pending) {
                let running = self.running.take().expect("a running request");
                self.timers.cancel(running.deadline.1);
                if let Some(text) = finish(running.recipient, outcome) {
                    pasted.push(text);
                }
                self.retire(running.helper, watches)?;
            }
        }
        let mut index = 0;
        while index < self.closing.len() {
            if self.closing[index].service(&self.timers)? {
                let helper = self.closing.swap_remove(index);
                watches.stop_pasteboard(exit_key(helper.id));
            } else {
                index += 1;
            }
        }
        self.start_next(watches);
        Ok(pasted)
    }

    /// Cancel the request in flight and drop the waiting ones; keep calling `service` until
    /// [`Pasteboard::children`] is zero.
    pub(super) fn close(&mut self, watches: &mut Watches) -> Result<()> {
        self.closed = true;
        self.queue.clear();
        if let Some(running) = self.running.take() {
            self.timers.cancel(running.deadline.1);
            self.retire(running.helper, watches)?;
        }
        Ok(())
    }

    /// Children not yet reaped.
    pub(super) fn children(&self) -> usize {
        self.closing.len() + usize::from(self.running.is_some())
    }

    /// Whether a read for `recipient` is in flight or waiting.
    fn reading(&self, recipient: u64) -> bool {
        self.running
            .as_ref()
            .is_some_and(|running| running.recipient == Some(recipient))
            || self
                .queue
                .iter()
                .any(|request| matches!(*request, Request::Read { recipient: r } if r == recipient))
    }

    fn start_next(&mut self, watches: &mut Watches) {
        while self.running.is_none()
            && !self.closed
            && let Some(request) = self.queue.pop_front()
        {
            match self.spawn(request, watches) {
                Ok(running) => self.running = Some(running),
                Err(error) => {
                    tracing::warn!(%error, "could not start the pasteboard helper; dropped a request");
                }
            }
        }
    }

    fn spawn(&mut self, request: Request, watches: &mut Watches) -> Result<Running> {
        let (kind, body, recipient) = match request {
            Request::Read { recipient } => (Kind::Read, Vec::new(), Some(recipient)),
            Request::Write(Some(text)) => (Kind::Write, text.into_bytes(), None),
            Request::Write(None) => (Kind::Clear, Vec::new(), None),
        };
        self.next_id += 1;
        let id = self.next_id;
        let (socket, remote) = sumie_pty::socket_pair()?;
        socket.set_nonblocking(true)?;
        let mut command = (self.launch)()?;
        let mut child =
            sumie_pty::spawn(command.stdin(OwnedFd::from(remote)).stdout(Stdio::null()))?;
        // The command holds the child's end; the child alone must keep it open.
        drop(command);
        let registered = (|| -> Result<(ExitWatch, (Instant, Key))> {
            let exit = ExitWatch::attach(&child)?;
            watches.watch_pasteboard(exit_key(id), exit.as_fd())?;
            watches.watch_pasteboard(socket_key(id), socket.as_fd())?;
            watches.watch_pasteboard(write_key(id), socket.as_fd())?;
            let at = Instant::now() + self.timeout;
            Ok((exit, (at, self.timers.register(at)?)))
        })();
        let (exit, deadline) = match registered {
            Ok(registered) => registered,
            Err(error) => {
                for key in [exit_key(id), socket_key(id), write_key(id)] {
                    watches.stop_pasteboard(key);
                }
                // Never wait: an unreaped child becomes the OS's to reap at exit.
                if child.kill().is_ok() {
                    let _ = child.try_wait();
                }
                return Err(error);
            }
        };
        Ok(Running {
            helper: Helper {
                id,
                child,
                exit,
                socket: Some(socket),
                kill: None,
                reaped: false,
            },
            recipient,
            output: frame::encode(kind, &body),
            written: 0,
            decoder: Decoder::default(),
            deadline,
        })
    }

    /// Close the helper's socket, so it exits on EOF, and keep its exit watch until it is
    /// reaped; SIGKILL follows at [`KILL_AFTER`].
    fn retire(&mut self, mut helper: Helper, watches: &mut Watches) -> Result<()> {
        watches.stop_pasteboard(socket_key(helper.id));
        watches.stop_pasteboard(write_key(helper.id));
        helper.socket = None;
        if helper.reaped {
            watches.stop_pasteboard(exit_key(helper.id));
            return Ok(());
        }
        let at = Instant::now() + KILL_AFTER;
        let result = self
            .timers
            .register(at)
            .map(|key| helper.kill = Some((at, key)));
        if result.is_err() {
            // No deadline would kill it: kill it now and reap it on its exit watch.
            let _ = helper.child.kill();
        }
        self.closing.push(helper);
        result
    }
}

impl Drop for Pasteboard {
    fn drop(&mut self) {
        let running = self.running.take().map(|running| {
            self.timers.cancel(running.deadline.1);
            running.helper
        });
        for mut helper in self.closing.drain(..).chain(running) {
            if let Some((_, key)) = helper.kill.take() {
                self.timers.cancel(key);
            }
            // Never wait here: an unreaped child is killed and becomes the OS's to reap.
            if helper.child.kill().is_ok() {
                let _ = helper.child.try_wait();
            }
        }
    }
}

impl Running {
    /// Write what is left of the request and read the reply, without blocking.
    fn progress(
        &mut self,
        watches: &mut Watches,
        mut reap: impl FnMut(&ExitWatch, &mut Child) -> io::Result<Option<ExitStatus>>,
    ) -> Result<Outcome> {
        let socket = self
            .helper
            .socket
            .as_mut()
            .expect("a running helper's socket");
        while self.written < self.output.len() {
            match socket.write(&self.output[self.written..]) {
                Ok(written) => self.written += written,
                Err(error) if error.kind() == ErrorKind::Interrupted => (),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => return Ok(Outcome::Failed(error.to_string())),
            }
        }
        if self.written == self.output.len() {
            watches.stop_pasteboard(write_key(self.helper.id));
        }
        let mut buffer = [0; 16 * 1024];
        let mut frames = Vec::new();
        loop {
            match socket.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if let Err(error) = self.decoder.feed(&buffer[..count], &mut frames) {
                        return Ok(Outcome::Failed(error.to_string()));
                    }
                    if let Some(reply) = frames.pop() {
                        return Ok(Outcome::Reply(reply));
                    }
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => (),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if self.helper.reaped {
                        break;
                    }
                    self.helper.reaped = reap(&self.helper.exit, &mut self.helper.child)?.is_some();
                    if !self.helper.reaped {
                        return Ok(Outcome::Pending);
                    }
                    // A helper can write its reply between WouldBlock and the reap.
                }
                Err(error) => return Ok(Outcome::Failed(error.to_string())),
            }
        }
        let reason = self.decoder.finish().err().map_or_else(
            || "the helper exited without a reply".to_owned(),
            |error| error.to_string(),
        );
        Ok(Outcome::Failed(reason))
    }
}

impl Helper {
    /// Reap the child or kill it at its deadline; returns whether it is reaped.
    fn service(&mut self, timers: &Timers) -> Result<bool> {
        if self.exit.try_reap(&mut self.child)?.is_some() {
            if let Some((_, key)) = self.kill.take() {
                timers.cancel(key);
            }
            return Ok(true);
        }
        if let Some((at, _)) = self.kill
            && at <= Instant::now()
        {
            self.kill = None;
            self.child.kill()?;
        }
        Ok(false)
    }
}

/// Log the outcome without its contents; returns a paste for the session.
fn finish(recipient: Option<u64>, outcome: Outcome) -> Option<Pasted> {
    let frame = match outcome {
        Outcome::Pending => unreachable!("only a finished request ends"),
        Outcome::Failed(reason) => {
            tracing::warn!(%reason, "the pasteboard request failed");
            return None;
        }
        Outcome::Reply(frame) => frame,
    };
    match (recipient, frame.kind) {
        (Some(recipient), Kind::Text) => match String::from_utf8(frame.body) {
            Ok(text) if !text.is_empty() => return Some(Pasted { text, recipient }),
            Ok(_) => tracing::debug!("the pasteboard holds an empty string"),
            Err(_) => tracing::warn!("the pasteboard helper replied with malformed text"),
        },
        (Some(_), Kind::Empty) => tracing::debug!("the pasteboard holds no text"),
        (Some(_), Kind::TooLarge) => {
            tracing::warn!(
                limit = LIMIT,
                "the pasteboard text exceeds the limit; pasted nothing"
            );
        }
        (None, Kind::Written) => (),
        (None, Kind::Refused) => tracing::warn!("the pasteboard refused the copy"),
        (_, kind) => tracing::warn!(?kind, "unexpected pasteboard reply"),
    }
    None
}

/// The helper child's entry: serve one request on this build's board.
pub(crate) fn run_child() -> Result<()> {
    #[cfg(feature = "macos-ci")]
    let board = select_board(std::env::var(BOARD_ENV).ok().as_deref())?;
    #[cfg(not(feature = "macos-ci"))]
    let board = Board::General;
    serve_child(&board)
}

/// A `macos-ci` build's board: the one `env`, the value of [`BOARD_ENV`], names. Never the
/// general board: without a name there is no board to serve.
#[cfg(any(test, feature = "macos-ci"))]
fn select_board(env: Option<&str>) -> Result<Board> {
    let name = env
        .filter(|name| !name.is_empty())
        .ok_or_else(|| format!("{BOARD_ENV} must name the pasteboard a macos-ci build serves"))?;
    Ok(Board::named(name)?)
}

/// Serve one request in the helper child. Its stdin is the app's socket.
fn serve_child(board: &Board) -> Result<()> {
    let socket = UnixStream::from(io::stdin().as_fd().try_clone_to_owned()?);
    let Some(request) = receive(&socket)? else {
        // Cancelled before the request was whole.
        return Ok(());
    };
    // The app writes nothing more, so a read returns only at its EOF, even while the board
    // waits for a provider.
    let cancel = socket.try_clone()?;
    thread::Builder::new()
        .name("sumie-pasteboard-cancel".into())
        .spawn(move || {
            let _ = (&cancel).read(&mut [0]);
            process::exit(0);
        })?;
    let (kind, body) = match request.kind {
        Kind::Read => match pasteboard::read(board) {
            None => (Kind::Empty, String::new()),
            Some(text) if text.is_empty() => (Kind::Empty, String::new()),
            Some(text) if text.len() > LIMIT => (Kind::TooLarge, String::new()),
            Some(text) => (Kind::Text, text),
        },
        Kind::Write | Kind::Clear => {
            let text = (request.kind == Kind::Write)
                .then(|| String::from_utf8(request.body))
                .transpose()?;
            let written = pasteboard::write(board, text.as_deref());
            (
                if written {
                    Kind::Written
                } else {
                    Kind::Refused
                },
                String::new(),
            )
        }
        kind => return Err(format!("unexpected pasteboard request {kind:?}").into()),
    };
    match (&socket).write_all(&frame::encode(kind, body.as_bytes())) {
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        result => Ok(result?),
    }
}

/// Read the one request frame; `None` at an EOF before it is whole.
fn receive(mut socket: &UnixStream) -> Result<Option<Frame>> {
    let mut decoder = Decoder::default();
    let mut frames = Vec::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        match socket.read(&mut buffer) {
            Ok(0) => return Ok(None),
            Ok(read) => {
                decoder.feed(&buffer[..read], &mut frames)?;
                if let Some(request) = frames.pop() {
                    return Ok(Some(request));
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => (),
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests;
