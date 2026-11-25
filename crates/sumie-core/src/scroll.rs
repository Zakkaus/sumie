//! Smooth scroll motion in device pixels, positive down. The caller supplies the time
//! between frames from presentation timestamps, so speed does not depend on refresh rate.

use std::time::Duration;

/// Time constant of the glide that follows a wheel notch.
const WHEEL_TAU: f64 = 0.06;
/// Time constant of the slide that follows a touchpad lift-off.
const FLING_TAU: f64 = 0.325;
const FLING_STOP: f64 = 8.0;
const FLING_MAX: f64 = 20_000.0;
/// A finger that rested this long before lifting does not fling.
const REST: Duration = Duration::from_millis(100);
const MIN_SAMPLE: f64 = 0.004;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Motion {
    /// Wheel distance left to cover, eased out.
    glide: f64,
    /// Finger distance not yet applied; the content follows the finger one to one.
    follow: f64,
    /// Lift-off speed in pixels per second, decaying.
    fling: f64,
    /// Finger speed estimated from recent events.
    speed: f64,
    last_touch: Option<Duration>,
}

impl Motion {
    pub fn wheel(&mut self, px: f64) {
        if self.glide * px < 0.0 {
            self.glide = 0.0;
        }
        self.fling = 0.0;
        self.glide += px;
    }

    /// Finger movement at monotonic time `at`.
    pub fn touch(&mut self, px: f64, at: Duration) {
        self.glide = 0.0;
        self.fling = 0.0;
        let elapsed = self
            .last_touch
            .map(|last| at.saturating_sub(last).as_secs_f64());
        self.speed = match elapsed {
            Some(dt) if dt < REST.as_secs_f64() => self.speed.midpoint(px / dt.max(MIN_SAMPLE)),
            _ => 0.0,
        };
        self.last_touch = Some(at);
        self.follow += px;
    }

    /// The finger lifted at `at` (`axis_stop`): slide on at the speed it had.
    pub fn release(&mut self, at: Duration) {
        let moving = self
            .last_touch
            .is_some_and(|last| at.saturating_sub(last) < REST);
        if moving {
            self.fling = self.speed.clamp(-FLING_MAX, FLING_MAX);
        }
        self.speed = 0.0;
        self.last_touch = None;
    }

    /// Advance by `dt` seconds and return the distance to scroll.
    pub fn step(&mut self, dt: f64) -> f64 {
        let mut moved = std::mem::take(&mut self.follow);
        if self.glide != 0.0 {
            let part = self.glide * (1.0 - (-dt / WHEEL_TAU).exp());
            self.glide -= part;
            moved += part;
            if self.glide.abs() < 0.5 {
                moved += std::mem::take(&mut self.glide);
            }
        }
        if self.fling != 0.0 {
            let decay = (-dt / FLING_TAU).exp();
            moved += self.fling * FLING_TAU * (1.0 - decay);
            self.fling *= decay;
            if self.fling.abs() < FLING_STOP {
                self.fling = 0.0;
            }
        }
        moved
    }

    #[must_use]
    pub fn active(&self) -> bool {
        self.glide != 0.0 || self.follow != 0.0 || self.fling != 0.0
    }

    /// Drop all motion, at the end of the history or when another input takes over.
    pub fn stop(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn travel(motion: &mut Motion, hz: u32, seconds: u32) -> f64 {
        (0..hz * seconds)
            .map(|_| motion.step(1.0 / f64::from(hz)))
            .sum()
    }

    #[test]
    fn distance_does_not_depend_on_refresh_rate() {
        for hz in [60, 144, 240] {
            let mut motion = Motion::default();
            motion.wheel(100.0);
            assert!((travel(&mut motion, hz, 1) - 100.0).abs() < 1e-9, "{hz}");
            assert!(!motion.active());
        }
    }

    #[test]
    fn a_lifted_finger_slides_on_unless_it_rested() {
        for (rest_ms, slides) in [(8, true), (150, false)] {
            let mut motion = Motion::default();
            for i in 0..10 {
                motion.touch(10.0, Duration::from_millis(8 * i));
            }
            motion.release(Duration::from_millis(72 + rest_ms));
            assert_eq!(motion.step(0.0), 100.0);
            let slid = travel(&mut motion, 120, 3);
            // 1250 px/s decaying with a 0.325 s time constant covers about 406 px.
            assert_eq!(slid > 300.0, slides);
            assert!(!motion.active());
        }
    }
}
