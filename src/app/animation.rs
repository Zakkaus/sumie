use std::time::{Duration, Instant};

use calloop::{
    RegistrationToken,
    timer::{TimeoutAction, Timer},
};
use sumie_wayland::SourceHandle;

use crate::Result;

pub(super) trait TimerSource {
    type Token: Copy;

    fn register(&self, at: Instant) -> Result<Self::Token>;
    fn cancel(&self, token: Self::Token);
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

pub(super) struct Wake<S: TimerSource> {
    origin: Instant,
    source: S,
    timer: Option<(u64, S::Token)>,
}

impl<S: TimerSource> Wake<S> {
    pub(super) fn new(source: S) -> Self {
        Self {
            origin: Instant::now(),
            source,
            timer: None,
        }
    }

    pub(super) fn now_ms(&self) -> u64 {
        self.origin
            .elapsed()
            .as_millis()
            .try_into()
            .expect("monotonic clock fits u64 milliseconds")
    }

    /// The timer only wakes dispatch. The compositor's frame slot gates the next tick.
    pub(super) fn arm(&mut self, next: Option<u64>) -> Result<()> {
        if next == self.timer.map(|(at, _)| at) {
            return Ok(());
        }
        if let Some((_, token)) = self.timer.take() {
            self.source.cancel(token);
        }
        if let Some(ms) = next {
            let token = self
                .source
                .register(self.origin + Duration::from_millis(ms))?;
            self.timer = Some((ms, token));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use sumie_vt::Terminal;

    #[derive(Default)]
    struct Clock {
        alarm: Cell<Option<Instant>>,
        registrations: Cell<u32>,
    }

    impl TimerSource for Clock {
        type Token = Instant;

        fn register(&self, at: Instant) -> Result<Instant> {
            assert!(self.alarm.replace(Some(at)).is_none());
            self.registrations.set(self.registrations.get() + 1);
            Ok(at)
        }

        fn cancel(&self, token: Instant) {
            assert_eq!(self.alarm.take(), Some(token));
        }
    }

    #[test]
    fn wake_keeps_an_unchanged_deadline_and_cancels_a_stopped_animation() {
        let mut vt = Terminal::new(8, 4).unwrap();
        vt.resize_pixels(8, 4, [8, 16]).unwrap();
        vt.write(b"\x1b_Ga=T,i=7,f=24,s=1,v=1;AAAA\x1b\\\x1b_Ga=f,i=7,f=24,s=1,v=1,z=30;////\x1b\\\x1b_Ga=a,i=7,r=1,z=50,s=3\x1b\\");
        let snapshot = vt.render_snapshot().unwrap();
        let mut wake = Wake::new(Clock::default());
        for (now, advanced, deadline, registrations) in [
            (100, false, 150, 1),
            (149, false, 150, 1),
            (157, true, 180, 2),
        ] {
            let (changed, next) = vt
                .animation_tick_visible(now, snapshot.images.iter(), None)
                .unwrap();
            assert_eq!(changed, advanced);
            wake.arm(next).unwrap();
            assert_eq!(
                wake.source.alarm.get(),
                Some(wake.origin + Duration::from_millis(deadline))
            );
            assert_eq!(wake.source.registrations.get(), registrations);
        }
        vt.write(b"\x1b_Ga=a,i=7,s=1\x1b\\");
        let (_, next) = vt
            .animation_tick_visible(160, snapshot.images.iter(), None)
            .unwrap();
        wake.arm(next).unwrap();
        assert_eq!(wake.source.alarm.get(), None);
    }
}
