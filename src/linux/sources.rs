use std::cell::Cell;
use std::collections::{HashMap, hash_map::Entry};
use std::os::fd::BorrowedFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use calloop::{
    EventLoop, Interest, Mode, PostAction, RegistrationToken,
    generic::Generic,
    timer::{TimeoutAction, Timer},
};
use sumie_wayland::SourceHandle;

use crate::{
    Result,
    platform::{ExitWatches, PaneWatches, Ready, TimerSource},
};

use super::LinuxWindow;

const BLINK_PHASE: Duration = Duration::from_millis(600);

#[derive(Clone, Copy)]
struct BlinkPhase {
    deadline: Instant,
    hidden: bool,
}

#[derive(Default)]
struct Signals {
    ready: Cell<Ready>,
    blink: Cell<Option<BlinkPhase>>,
}

struct PaneSources {
    signals: Rc<Signals>,
    input: Option<RegistrationToken>,
    output: Option<RegistrationToken>,
    exit: RegistrationToken,
    blink: Option<RegistrationToken>,
}

impl PaneSources {
    fn stop_io(&mut self, handle: &SourceHandle) {
        remove(handle, &mut self.output);
        remove(handle, &mut self.input);
        remove(handle, &mut self.blink);
        self.signals.blink.set(None);
    }
}

pub(crate) struct Watches {
    handle: SourceHandle,
    panes: HashMap<u64, PaneSources>,
    config: Option<(RegistrationToken, Rc<Cell<bool>>)>,
}

impl TimerSource for SourceHandle {
    type Token = RegistrationToken;

    fn register(&self, at: Instant) -> Result<Self::Token> {
        Ok(self.insert(Timer::from_deadline(at), |_, ()| TimeoutAction::Drop)?)
    }

    fn cancel(&self, token: Self::Token) {
        self.remove(token);
    }
}

impl Watches {
    pub(crate) fn new(handle: SourceHandle) -> Self {
        Self {
            handle,
            panes: HashMap::new(),
            config: None,
        }
    }

    pub(crate) fn animation_timer(&self) -> impl TimerSource + use<> {
        self.handle.clone()
    }

    pub(crate) fn register_config(&mut self, fd: Option<BorrowedFd<'_>>) -> Result<()> {
        if let Some(fd) = fd {
            assert!(self.config.is_none(), "config already registered");
            let ready = Rc::new(Cell::new(false));
            let flag = Rc::clone(&ready);
            let source = Generic::new(fd.try_clone_to_owned()?, Interest::READ, Mode::Level);
            let token = self.handle.insert(source, move |_, _| {
                flag.set(true);
                Ok(PostAction::Continue)
            })?;
            self.config = Some((token, ready));
        }
        Ok(())
    }

    pub(crate) fn take_config_ready(&self) -> bool {
        self.config.as_ref().is_some_and(|(_, ready)| ready.take())
    }
}

impl Drop for Watches {
    fn drop(&mut self) {
        if let Some((token, _)) = self.config.take() {
            self.handle.remove(token);
        }
        for (_, mut pane) in self.panes.drain() {
            pane.stop_io(&self.handle);
            self.handle.remove(pane.exit);
        }
    }
}

impl ExitWatches for LinuxWindow {
    fn take_exit_ready(&mut self, id: u64) -> bool {
        self.take_ready(id).exit
    }

    fn unregister(&mut self, id: u64) {
        let mut pane = self.watches.panes.remove(&id).expect("registered pane");
        pane.stop_io(&self.watches.handle);
        self.watches.handle.remove(pane.exit);
    }
}

impl PaneWatches for LinuxWindow {
    fn register_pane(
        &mut self,
        id: u64,
        output: BorrowedFd<'_>,
        exit: BorrowedFd<'_>,
    ) -> Result<()> {
        let handle = &self.watches.handle;
        let Entry::Vacant(entry) = self.watches.panes.entry(id) else {
            panic!("pane already registered");
        };
        let signals = Rc::new(Signals::default());
        let output = watch(handle, output, Interest::READ, &signals, |ready| {
            ready.output = true;
        })?;
        let exit = watch(handle, exit, Interest::READ, &signals, |ready| {
            ready.exit = true;
        })
        .inspect_err(|_| handle.remove(output))?;
        entry.insert(PaneSources {
            signals,
            input: None,
            output: Some(output),
            exit,
            blink: None,
        });
        Ok(())
    }

    fn take_ready(&mut self, id: u64) -> Ready {
        self.watches.panes[&id].signals.ready.take()
    }

    fn watch_input(&mut self, id: u64, fd: Option<BorrowedFd<'_>>) -> Result<()> {
        let handle = &self.watches.handle;
        let pane = self.watches.panes.get_mut(&id).expect("registered pane");
        match (pane.input, fd) {
            (None, Some(fd)) => {
                pane.input = Some(watch(
                    handle,
                    fd,
                    Interest::WRITE,
                    &pane.signals,
                    |ready| {
                        ready.input = true;
                    },
                )?);
            }
            (Some(_), None) => remove(handle, &mut pane.input),
            _ => (),
        }
        Ok(())
    }

    fn stop_output(&mut self, id: u64) {
        let pane = self.watches.panes.get_mut(&id).expect("registered pane");
        remove(&self.watches.handle, &mut pane.output);
    }

    fn watch_blink(&mut self, id: u64, enabled: bool) -> Result<()> {
        let handle = &self.watches.handle;
        let pane = self.watches.panes.get_mut(&id).expect("registered pane");
        match (enabled, pane.blink) {
            (true, None) => {
                let deadline = Instant::now() + BLINK_PHASE;
                let signals = Rc::clone(&pane.signals);
                pane.blink =
                    Some(handle.insert(Timer::from_deadline(deadline), move |_, ()| {
                        let current = signals.blink.get().expect("registered blink phase");
                        let now = Instant::now();
                        let deadline = if current.deadline <= now {
                            let deadline = now + BLINK_PHASE;
                            signals.blink.set(Some(BlinkPhase {
                                deadline,
                                hidden: !current.hidden,
                            }));
                            deadline
                        } else {
                            current.deadline
                        };
                        TimeoutAction::ToInstant(deadline)
                    })?);
                pane.signals.blink.set(Some(BlinkPhase {
                    deadline,
                    hidden: false,
                }));
            }
            (false, Some(_)) => {
                remove(handle, &mut pane.blink);
                pane.signals.blink.set(None);
            }
            _ => (),
        }
        Ok(())
    }

    fn reset_blink(&mut self, id: u64) {
        let blink = &self.watches.panes[&id].signals.blink;
        if blink.get().is_some() {
            // Keep the registered timer; its callback honors this later deadline.
            blink.set(Some(BlinkPhase {
                deadline: Instant::now() + BLINK_PHASE,
                hidden: false,
            }));
        }
    }

    fn cursor_hidden(&self, id: u64) -> bool {
        self.watches.panes[&id]
            .signals
            .blink
            .get()
            .is_some_and(|phase| phase.hidden)
    }

    fn retire_pane(&mut self, id: u64) {
        let pane = self.watches.panes.get_mut(&id).expect("registered pane");
        pane.stop_io(&self.watches.handle);
        let exit = pane.signals.ready.get().exit;
        pane.signals.ready.set(Ready {
            exit,
            ..Ready::default()
        });
    }
}

fn remove(handle: &SourceHandle, token: &mut Option<RegistrationToken>) {
    if let Some(token) = token.take() {
        handle.remove(token);
    }
}

fn watch(
    handle: &SourceHandle,
    fd: BorrowedFd<'_>,
    interest: Interest,
    signals: &Rc<Signals>,
    set_ready: impl Fn(&mut Ready) + 'static,
) -> Result<RegistrationToken> {
    let signals = Rc::clone(signals);
    let source = Generic::new(fd.try_clone_to_owned()?, interest, Mode::Level);
    Ok(handle.insert(source, move |_, _| {
        let mut ready = signals.ready.get();
        set_ready(&mut ready);
        signals.ready.set(ready);
        Ok(PostAction::Continue)
    })?)
}

struct ExitWatch {
    token: RegistrationToken,
    ready: bool,
}

/// Register exit descriptors before dropping the window, then dispatch without Wayland.
pub(crate) struct Reaper {
    events: EventLoop<'static, HashMap<u64, ExitWatch>>,
    exits: HashMap<u64, ExitWatch>,
}

impl Reaper {
    pub(crate) fn new<'a>(
        mut fds: impl Iterator<Item = (u64, BorrowedFd<'a>)>,
    ) -> Result<Option<Self>> {
        let Some(first) = fds.next() else {
            return Ok(None);
        };
        let events = EventLoop::<HashMap<u64, ExitWatch>>::try_new()?;
        let handle = events.handle();
        let mut exits = HashMap::new();
        for (id, fd) in std::iter::once(first).chain(fds) {
            let source = Generic::new(fd.try_clone_to_owned()?, Interest::READ, Mode::Level);
            let token = handle.insert_source(source, move |_, _, exits| {
                exits.get_mut(&id).expect("registered exit").ready = true;
                Ok(PostAction::Continue)
            })?;
            exits.insert(
                id,
                ExitWatch {
                    token,
                    ready: false,
                },
            );
        }
        Ok(Some(Self { events, exits }))
    }

    pub(crate) fn dispatch(&mut self, deadline: Option<Instant>) -> Result<()> {
        let timeout = deadline.map(|at| at.saturating_duration_since(Instant::now()));
        self.events.dispatch(timeout, &mut self.exits)?;
        Ok(())
    }
}

impl ExitWatches for Reaper {
    fn take_exit_ready(&mut self, id: u64) -> bool {
        std::mem::take(&mut self.exits.get_mut(&id).expect("registered exit").ready)
    }

    fn unregister(&mut self, id: u64) {
        let watch = self.exits.remove(&id).expect("registered exit");
        self.events.handle().remove(watch.token);
    }
}
