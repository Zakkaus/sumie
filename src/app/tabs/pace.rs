use std::time::{Duration, Instant};

/// Shortest wait between snapshots of a pane in an output burst.
const SNAPSHOT_PACE: Duration = Duration::from_millis(4);

/// Output that arrives in a stream at least this large is a burst.
const BURST_BYTES: usize = 64 * 1024;

/// Paces snapshot rebuilds while a pane floods output.
///
/// A large Kitty frame arrives in thousands of chunks, and rebuilding every cell after each
/// one costs more than decoding the frame. Output is a burst once at least `BURST_BYTES`
/// arrived with gaps shorter than `SNAPSHOT_PACE`; a quieter gap starts the count again.
/// Only a burst waits for the pace after the last rebuild, so a keystroke echo and the
/// shell redraw that follows it milliseconds later are always shown at once.
#[derive(Default)]
pub(in crate::app) struct SnapshotPace {
    rebuilt: Option<Instant>,
    held: bool,
    last_output: Option<Instant>,
    streak: usize,
}

impl SnapshotPace {
    /// Returns whether `bytes` of new output should mark the snapshot dirty now. A burst that
    /// arrives within the pace of the last rebuild is held until `due`, unless something else
    /// already made the snapshot dirty.
    pub(in crate::app) fn output(&mut self, now: Instant, dirty: bool, bytes: usize) -> bool {
        if self.last_output.is_none_or(|at| now >= at + SNAPSHOT_PACE) {
            self.streak = 0;
        }
        self.last_output = Some(now);
        self.streak = self.streak.saturating_add(bytes);
        let ready = dirty
            || self.streak < BURST_BYTES
            || self.rebuilt.is_none_or(|at| now >= at + SNAPSHOT_PACE);
        self.held |= !ready;
        ready
    }

    /// Returns whether held output may be shown at `now`.
    pub(in crate::app) fn released(&self, now: Instant) -> bool {
        self.due().is_some_and(|due| now >= due)
    }

    pub(in crate::app) fn due(&self) -> Option<Instant> {
        self.rebuilt
            .filter(|_| self.held)
            .map(|at| at + SNAPSHOT_PACE)
    }

    pub(in crate::app) fn held(&self) -> bool {
        self.held
    }

    pub(in crate::app) fn rebuilt(&mut self, now: Instant) {
        self.rebuilt = Some(now);
        self.held = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_burst_waits_for_the_pace() {
        let start = Instant::now();
        let ms = |n| start + Duration::from_millis(n);
        let mut pace = SnapshotPace::default();
        assert!(
            pace.output(ms(0), false, BURST_BYTES),
            "first output is shown at once"
        );
        // (earlier output in the burst at 0 ms, arrival, bytes, snapshot already dirty,
        // expected dirty, held output released at 3 and 4 ms)
        let flood = BURST_BYTES / 2;
        let cases = [
            (0, 1, 100, false, true, None),
            (0, 3, 100, false, true, None),
            (BURST_BYTES, 4, 100, false, true, None),
            (flood, 1, flood, false, false, Some((false, true))),
            (flood, 1, flood, true, true, None),
            (flood, 5, flood, false, true, None),
        ];
        for (before, arrival, bytes, dirty, expected, release) in cases {
            let mut pace = SnapshotPace::default();
            pace.rebuilt(ms(0));
            if before > 0 {
                pace.output(ms(0), true, before);
            }
            let label = format!("{bytes} bytes after {before} at {arrival} ms");
            assert_eq!(pace.output(ms(arrival), dirty, bytes), expected, "{label}");
            let held = pace
                .held()
                .then(|| (pace.released(ms(3)), pace.released(ms(4))));
            assert_eq!(held, release, "{label} release");
            pace.rebuilt(ms(10));
            assert!(!pace.held() && pace.due().is_none());
        }
    }

    #[test]
    fn a_quiet_gap_ends_the_burst() {
        let start = Instant::now();
        let ms = |n| start + Duration::from_millis(n);
        let mut pace = SnapshotPace::default();
        pace.rebuilt(ms(0));
        assert!(pace.output(ms(0), true, BURST_BYTES));
        assert!(!pace.output(ms(2), false, 1), "still streaming");
        pace.rebuilt(ms(2));
        assert!(pace.output(ms(7), false, 1), "after a 5 ms gap");
    }
}
