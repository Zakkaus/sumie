use std::time::{Duration, Instant};

use sumie_vt::{ScreenContents, Terminal};

use super::Result;

pub(super) const RESIZE_QUIET: Duration = Duration::from_millis(300);

pub(super) struct Batch(Option<ScreenContents>);

impl Batch {
    pub(super) fn begin(
        terminal: &mut Terminal,
        watch_at: Option<Instant>,
        quiet_until: Option<Instant>,
    ) -> Result<Self> {
        // Take the baseline before this batch, not from the last painted or watched state.
        let watch = watch_at.is_some_and(|now| quiet_until.is_none_or(|until| now >= until));
        Ok(Self(watch.then(|| terminal.screen_contents()).transpose()?))
    }

    pub(super) fn changed(self, terminal: &mut Terminal) -> Result<bool> {
        Ok(match self.0 {
            Some(before) => before != terminal.screen_contents()?,
            None => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_activity_uses_the_current_pty_batch() {
        let mut terminal = Terminal::new(10, 3).unwrap();
        terminal.resize(11, 3).unwrap();
        let start = Instant::now();
        let cases: [(u64, &[u8], bool); 7] = [
            (0, b"old", false),
            (299, b"\rnew", false),
            (300, b"\x1b]0;first\x07", false),
            (300, b"\x1b]2;second\x1b\\", false),
            (300, b"\rnew", false),
            (300, b"\x1b[H\x1b[?25l", false),
            (300, b"\x1b]2;third\x07nEw\x1b[H", true),
        ];
        for (millis, bytes, expected) in cases {
            let now = start + Duration::from_millis(millis);
            let batch = Batch::begin(&mut terminal, Some(now), Some(start + RESIZE_QUIET)).unwrap();
            terminal.write(bytes);
            assert_eq!(batch.changed(&mut terminal).unwrap(), expected, "{bytes:?}");
        }
    }
}
