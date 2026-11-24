use std::{collections::VecDeque, time::Duration};

use smithay_client_toolkit::reexports::calloop;

use super::{Event, Platform};

impl Platform {
    /// Wait up to `timeout` and dispatch ready sources. `None` waits indefinitely.
    ///
    /// # Errors
    /// Returns an error from calloop or the Wayland event source.
    pub fn dispatch(&mut self, timeout: Option<Duration>) -> calloop::Result<()> {
        self.event_loop.dispatch(timeout, &mut self.state)
    }

    /// Drain events before the next dispatch or presentation.
    pub fn drain_events(&mut self) -> impl Iterator<Item = Event> + '_ {
        self.state.events.drain(..)
    }

    /// Exchange an empty reusable queue for this dispatch's events. Events
    /// generated while handling the batch stay queued until the next dispatch.
    pub fn swap_events(&mut self, batch: &mut VecDeque<Event>) {
        debug_assert!(batch.is_empty());
        std::mem::swap(batch, &mut self.state.events);
    }
}
