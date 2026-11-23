use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::thread;

use rustix::event::{PollFd, PollFlags, poll};
use rustix::io::{Errno, retry_on_intr};

use crate::child::Cleanup;
use crate::wake::Wake;

const CHUNK_SIZE: usize = 8192;
/// Maximum number of chunks waiting for the terminal owner thread.
pub const OUTPUT_SLOTS: usize = 64;

pub(super) struct Reader {
    output: Option<Receiver<Vec<u8>>>,
    wake: Arc<Wake>,
    stop: Arc<Wake>,
    cleanup: Option<Sender<Cleanup>>,
}

impl Reader {
    pub(super) fn start(master: OwnedFd) -> io::Result<Self> {
        let (sender, output) = mpsc::sync_channel(OUTPUT_SLOTS);
        let (cleanup, cleanup_rx) = mpsc::channel();
        let wake = Arc::new(Wake::new()?);
        let stop = Arc::new(Wake::new()?);
        let (reader_wake, reader_stop) = (Arc::clone(&wake), Arc::clone(&stop));
        thread::Builder::new()
            .name("sumie-pty".into())
            .spawn(move || run(master, sender, &reader_wake, &reader_stop, &cleanup_rx))?;
        Ok(Self {
            output: Some(output),
            wake,
            stop,
            cleanup: Some(cleanup),
        })
    }

    pub(super) fn output_fd(&self) -> BorrowedFd<'_> {
        self.wake.fd()
    }

    pub(super) fn clear_wake(&self) -> io::Result<()> {
        Ok(self.wake.clear()?)
    }

    pub(super) fn try_recv(&self) -> Result<Vec<u8>, TryRecvError> {
        self.output
            .as_ref()
            .ok_or(TryRecvError::Disconnected)?
            .try_recv()
    }

    pub(super) fn stop(&mut self) -> io::Result<()> {
        // Disconnect first in case the bounded channel is full and the reader is in send().
        self.output = None;
        Ok(self.stop.notify()?)
    }

    pub(super) fn release_cleanup(&mut self) {
        self.cleanup = None;
    }

    pub(super) fn handoff(&mut self, child: Cleanup) {
        let sender = self
            .cleanup
            .take()
            .expect("unreaped child owns the cleanup sender");
        if sender.send(child).is_err() {
            tracing::error!("PTY reader stopped before accepting child cleanup");
        }
    }
}

fn run(
    master: OwnedFd,
    sender: SyncSender<Vec<u8>>,
    wake: &Wake,
    stop: &Wake,
    cleanup: &Receiver<Cleanup>,
) {
    log_error(
        read_output(&master, &sender, wake, stop),
        "PTY reader failed",
    );
    drop(master);
    drop(sender);
    log_error(
        wake.notify(),
        "could not notify event loop that PTY output ended",
    );
    if let Ok(child) = cleanup.recv() {
        child.finish();
    }
}

// One call site per message keeps tracing's macro expansion (larger when another
// crate enables its `log` feature) out of the callers' cognitive complexity.
fn log_error<E: std::fmt::Display>(result: Result<(), E>, message: &'static str) {
    if let Err(err) = result {
        tracing::error!(%err, "{message}");
    }
}

fn read_output(
    master: &OwnedFd,
    sender: &SyncSender<Vec<u8>>,
    wake: &Wake,
    stop: &Wake,
) -> rustix::io::Result<()> {
    let mut buf = [MaybeUninit::<u8>::uninit(); CHUNK_SIZE];
    let mut fds = [
        PollFd::new(master, PollFlags::IN),
        PollFd::from_borrowed_fd(stop.fd(), PollFlags::IN),
    ];
    loop {
        retry_on_intr(|| poll(&mut fds, None))?;
        if !fds[1].revents().is_empty() {
            return Ok(());
        }
        match rustix::io::read(master, &mut buf[..]) {
            Ok(([], _)) => return Ok(()),
            Ok((bytes, _)) => {
                if sender.send(bytes.to_vec()).is_err() {
                    return Ok(());
                }
                wake.notify()?;
            }
            Err(Errno::AGAIN | Errno::INTR) => (),
            #[cfg(target_os = "linux")]
            Err(Errno::IO) => return Ok(()), // The last slave descriptor closed.
            Err(err) => return Err(err),
        }
    }
}
