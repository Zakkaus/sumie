use std::time::{Duration, Instant};

use calloop::{
    RegistrationToken,
    timer::{TimeoutAction, Timer},
};
use sumie_vt::Terminal;
use sumie_wayland::SourceHandle;

use crate::Result;

#[derive(Default)]
pub(super) struct Playback {
    pub(super) next_ms: Option<u64>,
}

impl Playback {
    /// A due tick may change the final frame even when playback has no next deadline.
    pub(super) fn tick(&mut self, vt: &mut Terminal, now: u64) -> Result<bool> {
        let previous = self.next_ms;
        self.next_ms = vt.animation_tick(now)?;
        Ok(previous.is_some_and(|at| now >= at) || previous != self.next_ms)
    }
}

pub(super) struct Wake {
    origin: Instant,
    handle: SourceHandle,
    timer: Option<(u64, RegistrationToken)>,
}

impl Wake {
    pub(super) fn new(handle: SourceHandle) -> Self {
        Self {
            origin: Instant::now(),
            handle,
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
            self.handle.remove(token);
        }
        if let Some(ms) = next {
            let timer = Timer::from_deadline(self.origin + Duration::from_millis(ms));
            let token = self.handle.insert(timer, |_, ()| TimeoutAction::Drop)?;
            self.timer = Some((ms, token));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_deadline_survives_early_wakes_and_disarms_on_stop() {
        let mut vt = Terminal::new(8, 4).unwrap();
        vt.resize_pixels(8, 4, [8, 16]).unwrap();
        let mut playback = Playback::default();
        vt.write(b"\x1b_Ga=T,i=7,f=24,s=1,v=1;AAAA\x1b\\\x1b_Ga=f,i=7,f=24,s=1,v=1,z=30;////\x1b\\\x1b_Ga=a,i=7,r=1,z=50,s=3\x1b\\");
        for (now, changed, deadline) in [(100, true, 150), (149, false, 150), (157, true, 187)] {
            assert_eq!(playback.tick(&mut vt, now).unwrap(), changed);
            assert_eq!(playback.next_ms, Some(deadline));
        }
        vt.write(b"\x1b_Ga=a,i=7,s=1\x1b\\");
        playback.tick(&mut vt, 160).unwrap();
        assert_eq!(playback.next_ms, None);
        assert!(!playback.tick(&mut vt, 187).unwrap());
    }
}
