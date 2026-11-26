//! Bridges descriptor and timer readiness to the main thread without touching `AppKit`.
//!
//! One thread waits on a kqueue that holds duplicated descriptors and one-shot timers. It
//! records ready keys and sends a single coalesced wake until the main thread takes them;
//! winit's proxy source fires in all common run-loop modes, so menu tracking, live resize and
//! `NSTerminateLater`'s modal-panel loop still deliver it. A descriptor reports once, then stays
//! disabled until the main thread re-arms it after servicing. Every registration has its own
//! generation, so a report for a removed or replaced registration is dropped, and a wake sent
//! only for such reports is refused on delivery.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};

/// What a registration reports. Descriptors report readability except [`Interest::Input`]
/// and [`Interest::PasteboardWrite`], which report writability; [`Interest::Blink`] and
/// [`Interest::Timer`] are deadlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Interest {
    Output,
    Input,
    Exit,
    Config,
    /// A pasteboard helper's socket or exit report.
    Pasteboard,
    /// A pasteboard helper's socket while a request waits to be written.
    PasteboardWrite,
    Blink,
    Timer,
}

impl Interest {
    const fn filter(self) -> EventFilter {
        match self {
            Self::Output | Self::Exit | Self::Config | Self::Pasteboard => EventFilter::EVFILT_READ,
            Self::Input | Self::PasteboardWrite => EventFilter::EVFILT_WRITE,
            Self::Blink | Self::Timer => EventFilter::EVFILT_TIMER,
        }
    }
}

/// A registration: a pane ID (or timer token) and what it waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub id: u64,
    pub interest: Interest,
}

impl Key {
    #[must_use]
    pub const fn new(id: u64, interest: Interest) -> Self {
        Self { id, interest }
    }
}

struct Registration {
    generation: u64,
    ident: usize,
    /// The duplicated descriptor; closing it removes the kqueue registration.
    fd: Option<OwnedFd>,
}

#[derive(Default)]
struct State {
    next_generation: u64,
    live: HashMap<Key, Registration>,
    keys: HashMap<u64, Key>,
    ready: Vec<Key>,
    signalled: bool,
}

impl State {
    fn forget(&mut self, key: Key) -> Option<Registration> {
        let registration = self.live.remove(&key)?;
        self.keys.remove(&registration.generation);
        self.ready.retain(|ready| *ready != key);
        Some(registration)
    }
}

struct Shared {
    queue: Kqueue,
    state: Mutex<State>,
}

/// Main-thread handle for registrations. Clones share one bridge thread.
#[derive(Clone)]
pub struct Readiness(Arc<Shared>);

impl Readiness {
    /// Watch a duplicate of `fd`. The caller keeps its own descriptor.
    ///
    /// # Errors
    /// Returns duplication and registration errors; nothing stays registered for `key`.
    ///
    /// # Panics
    /// Panics if `key` is already registered or names a deadline.
    pub fn watch(&self, key: Key, fd: BorrowedFd<'_>) -> io::Result<()> {
        let filter = key.interest.filter();
        assert!(
            filter != EventFilter::EVFILT_TIMER,
            "deadlines use set_deadline"
        );
        let fd = fd.try_clone_to_owned()?;
        let ident = usize::try_from(fd.as_raw_fd()).expect("open descriptor");
        self.register(key, ident, Some(fd), |generation| {
            KEvent::new(
                ident,
                filter,
                EvFlags::EV_ADD | EvFlags::EV_DISPATCH,
                FilterFlag::empty(),
                0,
                udata(generation),
            )
        })
    }

    /// Report `key` once at `at`, replacing an earlier deadline for the same key.
    ///
    /// # Errors
    /// Returns the registration error; the earlier deadline is gone either way.
    ///
    /// # Panics
    /// Panics if `key` names a descriptor interest.
    pub fn set_deadline(&self, key: Key, at: Instant) -> io::Result<()> {
        assert!(
            key.interest.filter() == EventFilter::EVFILT_TIMER,
            "descriptors use watch"
        );
        self.remove(key);
        let nanos = at.saturating_duration_since(Instant::now()).as_nanos();
        let nanos = isize::try_from(nanos).unwrap_or(isize::MAX).max(1);
        self.register(key, 0, None, |generation| {
            KEvent::new(
                ident(generation),
                EventFilter::EVFILT_TIMER,
                EvFlags::EV_ADD | EvFlags::EV_ONESHOT,
                FilterFlag::NOTE_NSECONDS,
                nanos,
                udata(generation),
            )
        })
    }

    /// Allocate a [`Interest::Timer`] key with its own deadline.
    ///
    /// # Errors
    /// Returns the registration error.
    pub fn timer(&self, at: Instant) -> io::Result<Key> {
        let id = {
            let mut state = self.lock();
            state.next_generation += 1;
            state.next_generation
        };
        let key = Key::new(id, Interest::Timer);
        self.set_deadline(key, at)?;
        Ok(key)
    }

    fn register(
        &self,
        key: Key,
        ident: usize,
        fd: Option<OwnedFd>,
        event: impl FnOnce(u64) -> KEvent,
    ) -> io::Result<()> {
        let mut state = self.lock();
        assert!(!state.live.contains_key(&key), "{key:?} already registered");
        state.next_generation += 1;
        let generation = state.next_generation;
        let event = event(generation);
        let ident = if fd.is_some() { ident } else { event.ident() };
        // Register under the lock so the bridge can resolve the generation it reports.
        self.0.queue.kevent(&[event], &mut [], None)?;
        state.keys.insert(generation, key);
        state.live.insert(
            key,
            Registration {
                generation,
                ident,
                fd,
            },
        );
        Ok(())
    }

    /// Stop watching `key` and drop any report not yet taken. Unknown keys are ignored.
    pub fn remove(&self, key: Key) {
        let mut state = self.lock();
        let Some(registration) = state.forget(key) else {
            return;
        };
        if registration.fd.is_none() {
            let event = KEvent::new(
                registration.ident,
                EventFilter::EVFILT_TIMER,
                EvFlags::EV_DELETE,
                FilterFlag::empty(),
                0,
                0,
            );
            match self.0.queue.kevent(&[event], &mut [], None) {
                Ok(_) | Err(Errno::ENOENT) => (),
                Err(error) => tracing::error!(%error, "could not delete a readiness timer"),
            }
        }
    }

    /// Let a descriptor that already reported fire again while it stays ready.
    ///
    /// # Errors
    /// Returns the registration error.
    pub fn rearm(&self, key: Key) -> io::Result<()> {
        let state = self.lock();
        let Some(registration) = state.live.get(&key).filter(|live| live.fd.is_some()) else {
            return Ok(());
        };
        let event = KEvent::new(
            registration.ident,
            key.interest.filter(),
            EvFlags::EV_ADD | EvFlags::EV_ENABLE | EvFlags::EV_DISPATCH,
            FilterFlag::empty(),
            0,
            udata(registration.generation),
        );
        self.0.queue.kevent(&[event], &mut [], None)?;
        Ok(())
    }

    /// Whether a wake still carries reports. [`Readiness::remove`] drops a removed key's
    /// report but cannot recall a wake already sent for it, so check this before handling a
    /// wake: a removed key never surfaces after `remove` returns. A stale wake re-enables
    /// signalling, so the next report sends a new one.
    #[must_use]
    pub fn accept_wake(&self) -> bool {
        let mut state = self.lock();
        let live = !state.ready.is_empty();
        state.signalled &= live;
        live
    }

    /// Take the keys reported since the last call; the next report sends a new wake.
    #[must_use]
    pub fn take(&self) -> Vec<Key> {
        let mut state = self.lock();
        state.signalled = false;
        std::mem::take(&mut state.ready)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    fn queue(&self) -> &Kqueue {
        &self.0.queue
    }
}

/// Owns the bridge thread; dropping it stops the thread. Registrations made through a
/// remaining [`Readiness`] clone no longer wake anyone.
pub struct Bridge {
    readiness: Readiness,
    thread: Option<JoinHandle<()>>,
}

const STOP: usize = 0;

impl Bridge {
    /// Start the bridge. It calls `wake` on its own thread, once per batch of reports that
    /// the main thread has not taken yet.
    ///
    /// # Errors
    /// Returns kqueue and thread creation errors.
    pub fn spawn(wake: impl Fn() + Send + 'static) -> io::Result<Self> {
        let queue = Kqueue::new()?;
        fcntl(&queue, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        let stop = KEvent::new(
            STOP,
            EventFilter::EVFILT_USER,
            EvFlags::EV_ADD | EvFlags::EV_CLEAR,
            FilterFlag::empty(),
            0,
            0,
        );
        queue.kevent(&[stop], &mut [], None)?;
        let readiness = Readiness(Arc::new(Shared {
            queue,
            state: Mutex::default(),
        }));
        let shared = Arc::clone(&readiness.0);
        let thread = thread::Builder::new()
            .name("sumie-readiness".into())
            .spawn(move || bridge(&shared, &wake))?;
        Ok(Self {
            readiness,
            thread: Some(thread),
        })
    }

    #[must_use]
    pub fn readiness(&self) -> &Readiness {
        &self.readiness
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let stop = KEvent::new(
            STOP,
            EventFilter::EVFILT_USER,
            EvFlags::empty(),
            FilterFlag::NOTE_TRIGGER,
            0,
            0,
        );
        if let Err(error) = self.readiness.0.queue.kevent(&[stop], &mut [], None) {
            tracing::error!(%error, "could not stop the readiness bridge");
            return;
        }
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("readiness bridge panicked");
        }
    }
}

fn bridge(shared: &Shared, wake: &dyn Fn()) {
    let empty = KEvent::new(
        0,
        EventFilter::EVFILT_USER,
        EvFlags::empty(),
        FilterFlag::empty(),
        0,
        0,
    );
    let mut events = [empty; 32];
    loop {
        let count = match shared.queue.kevent(&[], &mut events, None) {
            Ok(count) => count,
            Err(Errno::EINTR) => continue,
            Err(error) => {
                tracing::error!(%error, "readiness bridge stopped");
                return;
            }
        };
        let mut stop = false;
        let send = {
            let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            for event in &events[..count] {
                if event.filter() == Ok(EventFilter::EVFILT_USER) {
                    stop = true;
                    continue;
                }
                let generation = u64::try_from(event.udata()).unwrap_or(0);
                let Some(&key) = state.keys.get(&generation) else {
                    continue;
                };
                if key.interest.filter() == EventFilter::EVFILT_TIMER {
                    // One-shot: the kernel already removed the timer.
                    state.live.remove(&key);
                    state.keys.remove(&generation);
                }
                if !state.ready.contains(&key) {
                    state.ready.push(key);
                }
            }
            !state.ready.is_empty() && !std::mem::replace(&mut state.signalled, true)
        };
        if send {
            wake();
        }
        if stop {
            return;
        }
    }
}

fn udata(generation: u64) -> isize {
    isize::try_from(generation).expect("generation fits kevent udata")
}

fn ident(generation: u64) -> usize {
    usize::try_from(generation).expect("generation fits a kevent ident")
}

#[cfg(test)]
mod tests;
