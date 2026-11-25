//! The Metal surface and its frame slot: at most one present per refresh of the window's
//! screen, and a bounded back-off after the drawable acquire times out.

use std::time::{Duration, Instant};

use sumie_macos::Platform;
use sumie_render::{backends, instance_descriptor, wgpu};

use crate::{Result, gpu::Gpu};

/// The first wait after a timed-out acquire; each further timeout doubles it.
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(8);

/// One instance, device and queue for the window's surface (FIFO, frame latency 2).
pub(super) fn create_gpu(window: &Platform, capture: bool, minimum_contrast: f32) -> Result<Gpu> {
    let instance = wgpu::Instance::new(instance_descriptor(
        wgpu::InstanceDescriptor::new_without_display_handle(),
        backends(),
    ));
    let surface = instance.create_surface(window.window_owner())?;
    Gpu::new(instance, surface, capture, minimum_contrast)
}

/// When the next frame may present. The slot is free from `free_at`; whoever holds it arms a
/// one-shot wake for that instant, so an idle window has no timer and its first frame draws
/// immediately.
pub(super) struct FrameSlot {
    refresh: Duration,
    free_at: Instant,
    /// The current wait after timed-out acquires, until a present or a visibility change.
    backoff: Option<Duration>,
}

impl FrameSlot {
    pub(super) fn new(frames_per_second: u32) -> Self {
        Self {
            refresh: refresh(frames_per_second),
            free_at: Instant::now(),
            backoff: None,
        }
    }

    pub(super) const fn refresh(&self) -> Duration {
        self.refresh
    }

    /// Read again after a screen or scale change.
    pub(super) fn set_rate(&mut self, frames_per_second: u32) {
        self.refresh = refresh(frames_per_second);
    }

    pub(super) fn available(&self, now: Instant) -> bool {
        now >= self.free_at
    }

    /// A frame is about to present: hold the slot until the next refresh. A busy window keeps
    /// the cadence of the slot it used, so the time spent drawing does not lower the rate.
    /// Returns the wake.
    pub(super) fn present(&mut self, now: Instant) -> Instant {
        self.backoff = None;
        let next = self.free_at + self.refresh;
        self.hold(if next > now { next } else { now + self.refresh })
    }

    /// Pending geometry waits for the GPU: try again one refresh later.
    pub(super) fn defer(&mut self, now: Instant) -> Instant {
        self.hold(now + self.refresh)
    }

    /// The acquire timed out: no acquire for 1 s, doubling up to 8 s. Returns the wake.
    pub(super) fn timed_out(&mut self, now: Instant) -> Instant {
        let wait = self
            .backoff
            .map_or(FIRST_BACKOFF, |wait| (wait * 2).min(MAX_BACKOFF));
        self.backoff = Some(wait);
        self.hold(now + wait)
    }

    /// An occlusion, screen or Space change: the next acquire may succeed, so stop waiting.
    /// A slot held for pacing stays held. Returns whether a back-off ended.
    pub(super) fn visibility_changed(&mut self, now: Instant) -> bool {
        if self.backoff.is_none() || self.available(now) {
            return false;
        }
        self.free_at = now;
        true
    }

    /// The window became visible or active again: free a slot held for pacing. A drawable
    /// back-off stays; only a visibility change ends it. Returns whether the slot was freed.
    pub(super) fn release(&mut self, now: Instant) -> bool {
        if self.backoff.is_some() || self.available(now) {
            return false;
        }
        self.free_at = now;
        true
    }

    fn hold(&mut self, until: Instant) -> Instant {
        self.free_at = until;
        until
    }
}

fn refresh(frames_per_second: u32) -> Duration {
    Duration::from_secs(1) / frames_per_second.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_present_holds_the_slot_for_one_refresh() {
        let mut slot = FrameSlot::new(60);
        let refresh = Duration::from_secs(1) / 60;
        let idle = Instant::now() + Duration::from_secs(1);
        assert!(
            slot.available(idle),
            "the first frame after idle draws at once"
        );
        let wake = slot.present(idle);
        assert_eq!(wake, idle + refresh);
        assert!(!slot.available(wake.checked_sub(Duration::from_micros(1)).unwrap()));
        assert!(slot.available(wake));
        // Drawing late in a slot keeps the cadence: still one present per refresh.
        let late = wake + Duration::from_millis(2);
        assert_eq!(slot.present(late), wake + refresh);
        slot.set_rate(120);
        let idle = late + Duration::from_secs(1);
        assert_eq!(slot.present(idle) - idle, Duration::from_secs(1) / 120);
    }

    #[test]
    fn timeouts_back_off_from_one_to_eight_seconds_until_a_present() {
        let start = Instant::now();
        let mut slot = FrameSlot::new(60);
        let waits: Vec<_> = (0..5)
            .map(|_| slot.timed_out(start) - start)
            .map(|wait| wait.as_secs())
            .collect();
        assert_eq!(waits, [1, 2, 4, 8, 8]);
        slot.present(start);
        assert_eq!(slot.timed_out(start) - start, FIRST_BACKOFF);
    }

    #[test]
    fn a_visibility_change_ends_a_backoff_but_not_pacing() {
        let start = Instant::now();
        let mut slot = FrameSlot::new(60);
        slot.present(start);
        assert!(!slot.visibility_changed(start));
        assert!(!slot.available(start));
        slot.timed_out(start);
        assert!(slot.visibility_changed(start));
        assert!(slot.available(start));
        // The back-off keeps growing until a frame presents.
        assert_eq!(slot.timed_out(start) - start, Duration::from_secs(2));
    }

    #[test]
    fn a_refocus_frees_pacing_but_not_a_backoff() {
        let start = Instant::now();
        let mut slot = FrameSlot::new(60);
        slot.present(start);
        assert!(slot.release(start));
        assert!(slot.available(start));
        assert!(!slot.release(start), "a free slot has nothing to release");
        slot.timed_out(start);
        assert!(!slot.release(start));
        assert!(!slot.available(start + Duration::from_millis(999)));
        assert!(slot.available(start + FIRST_BACKOFF));
    }
}
