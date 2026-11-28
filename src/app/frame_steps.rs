//! Platform-neutral steps of one event-loop turn. A platform driver owns the native loop
//! and calls them in order: one batch (ready PTYs, then the batch's effects, then
//! retirement), then a frame (geometry, snapshots and drawing at a free frame slot).

use std::{os::fd::BorrowedFd, time::Instant};

use super::{Session, animation::Wake};
use crate::{
    Result, capture,
    gpu::Gpu,
    login::Environment,
    platform::{PaneWatches, TimerSource, WindowOps},
};

/// The window's session and the state that outlives it on one event-loop thread.
pub(crate) struct App {
    pub(super) gpu: Gpu,
    pub(super) capture: capture::Options,
    pub(super) config: sumie_config::Config,
    /// The settings diagnostic over the active pane's first row.
    pub(super) banner: Option<String>,
    /// The first-run hint, on the banner row below a diagnostic until the first key or click.
    pub(super) hint: Option<String>,
    /// Whether the hint was handed to the latest draw; only then does a press dismiss it.
    pub(super) hint_drawn: bool,
    /// What every pane's login shell starts from; the session takes it when it starts.
    pub(super) environment: Option<Environment>,
    pub(super) session: Option<Session>,
    pub(super) focused: bool,
    pub(super) suspended: bool,
}

/// The steps of one dispatched batch, in the order `run_batch` runs them.
trait Batch {
    /// Service closing children and ready PTYs. Returns whether a live pane's child exited.
    fn ready_ptys(&mut self) -> Result<bool>;
    /// Apply the batch's window events in order. Returns whether the window closes.
    fn effects(&mut self) -> Result<bool>;
    /// Retire panes whose child exited. Returns whether no tab remains.
    fn retire(&mut self) -> Result<bool>;
}

/// Each event takes effect before the next, and a child exit seen in this batch cannot
/// retarget its input: retirement waits until the batch's events reached their panes.
fn run_batch(batch: &mut impl Batch) -> Result<bool> {
    let exited = batch.ready_ptys()?;
    if batch.effects()? {
        return Ok(true);
    }
    Ok(exited && batch.retire()?)
}

struct Turn<'a, W, F> {
    app: &'a mut App,
    window: &'a mut W,
    deliver: F,
}

impl<W, F> Batch for Turn<'_, W, F>
where
    W: WindowOps,
    F: FnMut(&mut App, &mut W) -> Result<bool>,
{
    fn ready_ptys(&mut self) -> Result<bool> {
        let visible = self.app.focused && !self.app.suspended;
        let Some(session) = &mut self.app.session else {
            return Ok(false);
        };
        let now = Instant::now();
        session.service_closing(now, &mut *self.window)?;
        session.service_pty(now, visible, &mut *self.window)
    }

    fn effects(&mut self) -> Result<bool> {
        (self.deliver)(&mut *self.app, &mut *self.window)
    }

    fn retire(&mut self) -> Result<bool> {
        self.app
            .session
            .as_mut()
            .map_or(Ok(false), |session| session.close_exited(&mut *self.window))
    }
}

/// Whether the window may rebuild snapshots and draw a frame. The loop offers a paced
/// snapshot deadline only under this condition, so a hidden or retrying window does not
/// wake for output it cannot show.
fn frame_ready(
    suspended: bool,
    slot_available: bool,
    retry: Option<Instant>,
    now: Instant,
) -> bool {
    !suspended && slot_available && retry.is_none_or(|at| now >= at)
}

impl App {
    /// Start from the first settings, under the banner if that result was a diagnostic.
    pub(crate) fn new(
        gpu: Gpu,
        capture: capture::Options,
        config: sumie_config::Config,
        banner: Option<String>,
        environment: Environment,
    ) -> Self {
        let mut app = Self {
            gpu,
            capture,
            config,
            banner: None,
            hint: None,
            hint_drawn: false,
            environment: Some(environment),
            session: None,
            focused: false,
            suspended: false,
        };
        if let Some(message) = banner {
            super::window_events::show_error(&mut app.banner, message);
        }
        app
    }

    /// The earliest time dispatch must return even without native events.
    pub(crate) fn wake_deadline(&self, slot_available: bool) -> Option<Instant> {
        let session = self.session.as_ref();
        let retry = session.and_then(|session| session.frame_retry);
        // When held output may be shown, if the window can show it.
        let paced = session
            .filter(|session| {
                frame_ready(
                    self.suspended,
                    slot_available,
                    session.frame_retry,
                    Instant::now(),
                )
            })
            .and_then(Session::snapshot_deadline);
        retry
            .into_iter()
            .chain(paced)
            .chain(session.and_then(Session::closing_deadline))
            .chain(session.and_then(Session::progress_deadline))
            .chain(
                session
                    .filter(|session| session.active().search_pending())
                    .map(|_| Instant::now()),
            )
            .min()
    }

    /// Run one dispatched batch; `deliver` applies its window events through
    /// [`App::handle_event`]. Returns whether the window closes.
    pub(crate) fn service_batch<W: WindowOps>(
        &mut self,
        window: &mut W,
        deliver: impl FnMut(&mut Self, &mut W) -> Result<bool>,
    ) -> Result<bool> {
        run_batch(&mut Turn {
            app: self,
            window,
            deliver,
        })
    }

    /// After a batch and any config reload: advance search, sync input interest, then at a
    /// free frame slot apply geometry, refresh snapshots and draw. Returns whether the
    /// requested final frame was presented.
    pub(crate) fn frame<W: WindowOps>(
        &mut self,
        window: &mut W,
        animation: &mut Wake<impl TimerSource>,
    ) -> Result<bool> {
        let Some(session) = &mut self.session else {
            return Ok(false);
        };
        session.active_mut().advance_search()?;
        session.sync_input(window)?;
        let gpu = &mut self.gpu;
        if !self.suspended && session.pane_pointer.take_frame_recovery() {
            window.recover_frame_slot();
        }
        let ready = frame_ready(
            self.suspended,
            window.frame_slot_available(),
            session.frame_retry,
            Instant::now(),
        ) && (!session.view.resize_pending()
            || window.may_configure(|| gpu.queue_empty()));
        let next = if ready {
            session.apply_geometry(gpu)?;
            session.step_scroll(Instant::now())?;
            let now = session.frame_retry.is_none().then(|| animation.now_ms());
            session.refresh_visible(now, window)?
        } else {
            None
        };
        let confirmation = session.confirmation_text();
        self.hint_drawn =
            ready && confirmation.is_none() && self.banner.is_none() && self.hint.is_some();
        let banner = confirmation
            .or(self.banner.as_deref())
            .or(self.hint.as_deref());
        session.update_link_hover(window, banner.is_some(), self.focused)?;
        session.watch_blink(window, self.focused && !self.suspended, banner)?;
        if ready && session.draw(gpu, window, &self.capture, banner, self.focused)? {
            return Ok(true);
        }
        animation
            .arm(next.filter(|_| session.frame_retry.is_none() && window.frame_slot_available()))?;
        Ok(false)
    }

    /// Retire every pane; closing children keep their exit watches until reaped.
    pub(crate) fn close_all(&mut self, watches: &mut impl PaneWatches) -> Result<()> {
        self.session
            .as_mut()
            .map_or(Ok(()), |session| session.close_all(watches))
    }

    #[cfg_attr(
        target_os = "macos",
        expect(
            dead_code,
            reason = "macOS keeps reaping through the window's exit watches"
        )
    )]
    pub(crate) fn closing_fds(&self) -> impl Iterator<Item = (u64, BorrowedFd<'_>)> {
        self.session.iter().flat_map(Session::closing_fds)
    }

    #[cfg(target_os = "macos")]
    pub(crate) const fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// The focused pane's last snapshot as columns and rows: the grid the view draws.
    #[cfg(feature = "macos-ci")]
    pub(crate) fn snapshot_grid(&self) -> Option<[usize; 2]> {
        let snapshot = self.session.as_ref()?.active().snapshot.as_ref()?;
        Some([
            snapshot.rows.first().map_or(0, Vec::len),
            snapshot.rows.len(),
        ])
    }

    /// Whether the focused pane's cursor is at an OSC 133 prompt, and its last OSC 7 URI.
    #[cfg(feature = "macos-ci")]
    pub(crate) fn shell_state(&self) -> Result<Option<(bool, String)>> {
        let Some(session) = &self.session else {
            return Ok(None);
        };
        let terminal = &session.active().pane.terminal;
        let pwd = terminal.with_pwd(|uri| String::from_utf8_lossy(uri).into_owned())?;
        Ok(Some((terminal.cursor_at_prompt()?, pwd)))
    }

    /// The centre of a cell of the focused pane in window device pixels, once it shows.
    #[cfg(feature = "macos-ci")]
    pub(crate) fn cell_center(&self, cell: [u16; 2]) -> Option<[f64; 2]> {
        let session = self.session.as_ref()?;
        Some(session.view.cell_center(session.active().rect, cell))
    }

    /// The composition overlay's text and its selected clause.
    #[cfg(feature = "macos-ci")]
    pub(crate) fn preedit(&self) -> Option<(&str, Option<(usize, usize)>)> {
        let view = &self.session.as_ref()?.view;
        Some((&view.preedit, view.preedit_cursor))
    }

    /// The presented confirmation or settings banner.
    #[cfg(feature = "macos-ci")]
    pub(crate) fn banner(&self) -> Option<&str> {
        self.session
            .as_ref()
            .filter(|session| session.confirmation_presented)
            .and_then(Session::confirmation_text)
            .or(self.banner.as_deref())
    }

    /// Drop the GPU, so the window can drop next, and keep the session to reap children.
    pub(crate) fn into_session(self) -> Option<Session> {
        self.session
    }
}

impl Session {
    /// At a free frame slot, apply the latest configure to every grid, PTY and the surface
    /// before taking snapshots. Intermediate sizes never reach the terminal.
    fn apply_geometry(&mut self, gpu: &mut Gpu) -> Result<()> {
        if self.view.apply_pending_resize()? {
            self.resize_grid()?;
            gpu.resize(self.view.device);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_hidden_or_retrying_window_is_not_ready() {
        let now = Instant::now();
        let later = Some(now + Duration::from_millis(1));
        // (suspended, slot available, retry, ready)
        let cases = [
            (false, true, None, true),
            (false, true, Some(now), true),
            (false, true, later, false),
            (false, false, None, false),
            (true, true, None, false),
        ];
        for (suspended, slot, retry, expected) in cases {
            assert_eq!(frame_ready(suspended, slot, retry, now), expected);
        }
    }

    /// One batch: whether a child exited, whether an event closes the window, and whether
    /// retirement leaves no tab.
    struct Recorded {
        exited: bool,
        closes: bool,
        empty: bool,
        order: Vec<&'static str>,
    }

    impl Recorded {
        fn step(&mut self, name: &'static str, result: bool) -> bool {
            self.order.push(name);
            result
        }
    }

    impl Batch for Recorded {
        fn ready_ptys(&mut self) -> Result<bool> {
            Ok(self.step("ready", self.exited))
        }
        fn effects(&mut self) -> Result<bool> {
            Ok(self.step("effects", self.closes))
        }
        fn retire(&mut self) -> Result<bool> {
            Ok(self.step("retire", self.empty))
        }
    }

    #[test]
    fn a_batch_runs_ready_ptys_then_effects_then_retirement() {
        // (exited, closes, empty) → steps run, window closes
        let cases: [(_, &[_], _); 5] = [
            ((true, false, false), &["ready", "effects", "retire"], false),
            ((true, false, true), &["ready", "effects", "retire"], true),
            ((false, false, true), &["ready", "effects"], false),
            ((true, true, false), &["ready", "effects"], true),
            ((false, true, false), &["ready", "effects"], true),
        ];
        for ((exited, closes, empty), steps, closed) in cases {
            let mut batch = Recorded {
                exited,
                closes,
                empty,
                order: Vec::new(),
            };
            assert_eq!(run_batch(&mut batch).unwrap(), closed);
            assert_eq!(batch.order, steps, "exited {exited}, closes {closes}");
        }
    }
}
