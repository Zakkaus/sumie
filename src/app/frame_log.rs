//! `SUMIE_FRAME_LOG`: per-frame stage timestamps and repainted pane IDs for the multi-pane
//! budget harness (`scripts/perf/stall.py`). Every build has it; the variable is read once
//! when the session starts. Records hold only `CLOCK_MONOTONIC` nanoseconds, pane IDs and
//! counts, never terminal content.
//!
//! The frame path appends each record to a shared buffer. A writer thread swaps the buffer
//! out and writes it at most once per second, so the frame path never performs file I/O.
//! Records buffered within the last second are lost when the process exits without dropping
//! the log (kill, abort, `process::exit`).

use std::{
    fs::File,
    io::Write,
    path::Path,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

const FLUSH_PERIOD: Duration = Duration::from_secs(1);
const HEADER: &[u8] = b"sumie-frame-log 1 monotonic-ns\n";

/// The stages of one frame, in order.
#[derive(Clone, Copy)]
pub(super) enum Stage {
    /// The frame slot was taken and snapshot refresh begins.
    Start,
    /// Every visible pane holds a current snapshot.
    Snapshots,
    /// The draw list is built: dirty panes repainted, clean panes reused.
    Painted,
    /// The surface texture was acquired and the frame's GPU work submitted.
    Submitted,
}

/// The session's log; every method is a no-op when `SUMIE_FRAME_LOG` was unset.
pub(super) struct FrameLog(Option<Recorder>);

struct Recorder {
    stamps: [Option<u64>; 4],
    dirty: usize,
    line: Vec<u8>,
    shared: Arc<Shared>,
    writer: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct Shared {
    buffer: Mutex<Vec<u8>>,
    stop: AtomicBool,
    /// Set by the writer after a write error; the recorder then stops appending.
    failed: AtomicBool,
}

impl FrameLog {
    /// Open the log named by `SUMIE_FRAME_LOG`; it stays off when unset or unopenable.
    pub(super) fn from_env() -> Self {
        Self(std::env::var_os("SUMIE_FRAME_LOG").and_then(|path| {
            Recorder::open(Path::new(&path))
                .inspect_err(|error| tracing::warn!(%error, "frame log disabled"))
                .ok()
        }))
    }

    pub(super) fn mark(&mut self, stage: Stage) {
        if let Some(log) = &mut self.0 {
            log.mark(stage);
        }
    }

    /// Record how many visible panes had new content when the frame was painted.
    pub(super) fn painted(&mut self, dirty: impl FnOnce() -> usize) {
        if let Some(log) = &mut self.0 {
            log.dirty = dirty();
            log.mark(Stage::Painted);
        }
    }

    /// Complete the frame once present returned. A frame missing a stage is dropped.
    pub(super) fn presented(&mut self, repainted: impl Iterator<Item = u64>) {
        if let Some(log) = &mut self.0 {
            log.presented(repainted);
        }
    }
}

impl Recorder {
    fn open(path: &Path) -> std::io::Result<Self> {
        let mut file = File::create(path)?;
        file.write_all(HEADER)?;
        let shared = Arc::new(Shared::default());
        let writer = std::thread::Builder::new()
            .name("sumie-frame-log".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || shared.write_to(file)
            })?;
        Ok(Self {
            stamps: [None; 4],
            dirty: 0,
            line: Vec::with_capacity(256),
            shared,
            writer: Some(writer),
        })
    }

    fn mark(&mut self, stage: Stage) {
        if matches!(stage, Stage::Start) {
            self.stamps = [None; 4];
        }
        self.stamps[stage as usize] = Some(now_ns());
    }

    fn presented(&mut self, repainted: impl Iterator<Item = u64>) {
        let presented = now_ns();
        if self.shared.failed.load(Ordering::Relaxed) {
            return;
        }
        let [Some(start), Some(snapshots), Some(painted), Some(submitted)] =
            std::mem::take(&mut self.stamps)
        else {
            return;
        };
        let line = &mut self.line;
        line.clear();
        line.extend_from_slice(b"frame");
        for stamp in [start, snapshots, painted, submitted, presented] {
            write!(line, " {stamp}").expect("write to a Vec");
        }
        write!(line, " {}", self.dirty).expect("write to a Vec");
        for id in repainted {
            write!(line, " {id}").expect("write to a Vec");
        }
        line.push(b'\n');
        self.shared
            .buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(line);
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(writer) = self.writer.take() {
            writer.thread().unpark();
            if writer.join().is_err() {
                tracing::warn!("frame log writer panicked");
            }
        }
    }
}

impl Shared {
    fn write_to(&self, mut file: File) {
        let mut out = Vec::new();
        loop {
            let stopping = self.stop.load(Ordering::Acquire);
            std::mem::swap(
                &mut *self.buffer.lock().unwrap_or_else(PoisonError::into_inner),
                &mut out,
            );
            if !out.is_empty() {
                if let Err(error) = file.write_all(&out) {
                    tracing::warn!(%error, "frame log stopped");
                    self.failed.store(true, Ordering::Relaxed);
                    self.buffer
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clear();
                    return;
                }
                out.clear();
            }
            if stopping {
                return;
            }
            std::thread::park_timeout(FLUSH_PERIOD);
        }
    }
}

/// The clock the harness's writers stamp their lines with.
fn now_ns() -> u64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    now.tv_sec.unsigned_abs() * 1_000_000_000 + now.tv_nsec.unsigned_abs()
}

#[cfg(test)]
mod tests {
    use super::{FrameLog, HEADER, Recorder, Stage};

    #[test]
    fn records_reach_the_file_without_content_and_incomplete_frames_are_dropped() {
        let dir = std::env::temp_dir().join(format!("sumie-frame-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("frames.log");
        let mut log = FrameLog(Some(Recorder::open(&path).unwrap()));
        log.mark(Stage::Snapshots);
        log.presented([7].into_iter());
        for stage in [Stage::Start, Stage::Snapshots] {
            log.mark(stage);
        }
        log.painted(|| 2);
        log.mark(Stage::Submitted);
        log.presented([3, 5].into_iter());
        drop(log);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let (header, records) = text.split_at(HEADER.len());
        assert_eq!(header.as_bytes(), HEADER);
        let lines: Vec<_> = records.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        let fields: Vec<_> = lines[0].split(' ').collect();
        assert_eq!(fields[0], "frame");
        let stamps: Vec<u64> = fields[1..6].iter().map(|f| f.parse().unwrap()).collect();
        assert!(stamps.is_sorted(), "{stamps:?}");
        assert_eq!(&fields[6..], ["2", "3", "5"]);
    }
}
