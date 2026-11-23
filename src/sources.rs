use std::cell::Cell;
use std::os::fd::BorrowedFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use calloop::{
    EventLoop, Interest, Mode, PostAction, RegistrationToken,
    generic::Generic,
    timer::{TimeoutAction, Timer},
};
use sumie_pty::Pty;
use sumie_wayland::SourceHandle;

use crate::Result;

const OUTPUT: u8 = 1;
const INPUT: u8 = 2;
const EXIT: u8 = 4;
const BLINK_PHASE: Duration = Duration::from_millis(600);

#[derive(Clone, Copy)]
struct BlinkPhase {
    deadline: Instant,
    hidden: bool,
}

pub(crate) struct Sources {
    handle: SourceHandle,
    ready: Rc<Cell<u8>>,
    input: Option<RegistrationToken>,
    output: Option<RegistrationToken>,
    exit: RegistrationToken,
    blink: Option<RegistrationToken>,
    blink_phase: Rc<Cell<Option<BlinkPhase>>>,
}

impl Sources {
    pub(crate) fn new(handle: SourceHandle, pty: &Pty) -> Result<Self> {
        let ready = Rc::new(Cell::new(0));
        let output = watch(
            &handle,
            pty.output_fd(),
            Interest::READ,
            Rc::clone(&ready),
            OUTPUT,
        )?;
        let exit = watch(
            &handle,
            pty.exit_fd().expect("new child watcher"),
            Interest::READ,
            Rc::clone(&ready),
            EXIT,
        )
        .inspect_err(|_| handle.remove(output))?;
        Ok(Self {
            handle,
            ready,
            input: None,
            output: Some(output),
            exit,
            blink: None,
            blink_phase: Rc::default(),
        })
    }

    pub(crate) fn take_ready(&self) -> Ready {
        Ready(self.ready.take())
    }

    pub(crate) fn watch_input(&mut self, fd: Option<BorrowedFd<'_>>) -> Result<()> {
        match (self.input, fd) {
            (None, Some(fd)) => {
                self.input = Some(watch(
                    &self.handle,
                    fd,
                    Interest::WRITE,
                    Rc::clone(&self.ready),
                    INPUT,
                )?);
            }
            (Some(token), None) => {
                self.handle.remove(token);
                self.input = None;
            }
            _ => (),
        }
        Ok(())
    }

    pub(crate) fn watch_blink(&mut self, enabled: bool) -> Result<()> {
        match (enabled, self.blink) {
            (true, None) => {
                let deadline = Instant::now() + BLINK_PHASE;
                let phase = Rc::clone(&self.blink_phase);
                self.blink = Some(self.handle.insert(
                    Timer::from_deadline(deadline),
                    move |_, ()| {
                        let current = phase.get().expect("registered blink phase");
                        let now = Instant::now();
                        let deadline = if current.deadline <= now {
                            let deadline = now + BLINK_PHASE;
                            phase.set(Some(BlinkPhase {
                                deadline,
                                hidden: !current.hidden,
                            }));
                            deadline
                        } else {
                            current.deadline
                        };
                        TimeoutAction::ToInstant(deadline)
                    },
                )?);
                self.blink_phase.set(Some(BlinkPhase {
                    deadline,
                    hidden: false,
                }));
            }
            (false, Some(token)) => {
                self.handle.remove(token);
                self.blink = None;
                self.blink_phase.set(None);
            }
            _ => (),
        }
        Ok(())
    }

    pub(crate) fn reset_blink(&self) {
        if self.blink_phase.get().is_some() {
            // Keep the registered timer; its callback honors this later deadline.
            self.blink_phase.set(Some(BlinkPhase {
                deadline: Instant::now() + BLINK_PHASE,
                hidden: false,
            }));
        }
    }

    pub(crate) fn cursor_hidden(&self) -> bool {
        self.blink_phase.get().is_some_and(|phase| phase.hidden)
    }

    pub(crate) fn remove_output(&mut self) {
        if let Some(token) = self.output.take() {
            self.handle.remove(token);
        }
    }
}

impl Drop for Sources {
    fn drop(&mut self) {
        self.remove_output();
        if let Some(token) = self.input {
            self.handle.remove(token);
        }
        self.handle.remove(self.exit);
        if let Some(token) = self.blink {
            self.handle.remove(token);
        }
    }
}

pub(crate) struct Ready(u8);

impl Ready {
    pub(crate) const fn output(&self) -> bool {
        self.0 & OUTPUT != 0
    }
    pub(crate) const fn input(&self) -> bool {
        self.0 & INPUT != 0
    }
    pub(crate) const fn exit(&self) -> bool {
        self.0 & EXIT != 0
    }
}

fn watch(
    handle: &SourceHandle,
    fd: BorrowedFd<'_>,
    interest: Interest,
    ready: Rc<Cell<u8>>,
    mask: u8,
) -> Result<RegistrationToken> {
    let source = Generic::new(fd.try_clone_to_owned()?, interest, Mode::Level);
    Ok(handle.insert(source, move |_, _| {
        ready.set(ready.get() | mask);
        Ok(PostAction::Continue)
    })?)
}

/// After the window is gone, continue servicing the child until it is reaped.
pub(crate) fn reap(pty: &mut Pty) -> Result<()> {
    let Some(exit) = pty.exit_fd() else {
        return Ok(());
    };
    let mut events = EventLoop::<bool>::try_new()?;
    events.handle().insert_source(
        Generic::new(exit.try_clone_to_owned()?, Interest::READ, Mode::Level),
        |_, _, ready| {
            *ready = true;
            Ok(PostAction::Continue)
        },
    )?;
    loop {
        let timeout = pty
            .close_deadline()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        let mut ready = false;
        events.dispatch(timeout, &mut ready)?;
        let due = pty
            .close_deadline()
            .is_some_and(|deadline| deadline <= Instant::now());
        if ready || due {
            pty.service_child()?;
            if pty.exit_fd().is_none() {
                return Ok(());
            }
        }
    }
}
