use std::io::{PipeReader, PipeWriter, Read as _, Write as _, pipe};
use std::os::fd::AsFd as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use sumie_macos::readiness::Bridge;

use super::*;

const WAIT: Duration = Duration::from_secs(2);
const QUIET: Duration = Duration::from_millis(100);
const PANE: u64 = 5;

struct Fixture {
    bridge: Bridge,
    wakes: mpsc::Receiver<()>,
    /// Wakes the bridge thread has sent, delivered or not.
    sent: Arc<AtomicUsize>,
    watches: Watches,
    output: (PipeReader, PipeWriter),
    exit: (PipeReader, PipeWriter),
}

impl Fixture {
    fn new() -> Self {
        let (sender, wakes) = mpsc::channel();
        let sent = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&sent);
        let bridge = Bridge::spawn(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = sender.send(());
        })
        .unwrap();
        let mut watches = Watches::new(bridge.readiness().clone());
        let output = pipe().unwrap();
        let exit = pipe().unwrap();
        watches
            .register_pane(PANE, output.0.as_fd(), exit.0.as_fd())
            .unwrap();
        Self {
            bridge,
            wakes,
            sent,
            watches,
            output,
            exit,
        }
    }

    /// Wait for a wake that `run` would deliver, then fold its reports like `Event::Ready`.
    fn dispatch(&mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        let mut woke = false;
        while !woke
            && self
                .wakes
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_ok()
        {
            woke = self.bridge.readiness().accept_wake();
        }
        self.watches.dispatch().unwrap();
        woke
    }

    /// Wait until the bridge thread sends its next wake, without delivering it.
    fn await_sent(&self, before: usize) {
        let deadline = Instant::now() + WAIT;
        while self.sent.load(Ordering::SeqCst) == before {
            assert!(Instant::now() < deadline, "the bridge sent no wake");
            std::thread::yield_now();
        }
    }
}

#[test]
fn an_unreaped_exit_keeps_reporting_until_unregistered() {
    let mut fixture = Fixture::new();
    // The first report is an early NOTE_EXIT: servicing it does not reap the child, and the
    // PTY re-watches, which makes its exit descriptor readable again.
    let mut reaped = false;
    for report in 0..2 {
        fixture.exit.1.write_all(b"x").unwrap();
        if !fixture.dispatch(WAIT) || !fixture.watches.take_exit_ready(PANE) {
            break;
        }
        fixture.exit.0.read_exact(&mut [0]).unwrap();
        reaped = report == 1;
    }
    assert!(
        reaped,
        "child not reaped: the exit watch stopped after the first NOTE_EXIT"
    );
    fixture.watches.unregister(PANE);
    fixture.exit.1.write_all(b"x").unwrap();
    assert!(
        !fixture.dispatch(QUIET),
        "an unregistered exit woke the main thread"
    );
}

#[test]
fn output_reports_once_per_take_and_retirement_keeps_only_exit() {
    let mut fixture = Fixture::new();
    fixture.output.1.write_all(b"out").unwrap();
    fixture.exit.1.write_all(b"x").unwrap();
    assert!(fixture.dispatch(WAIT));
    std::thread::sleep(QUIET);
    fixture.watches.dispatch().unwrap();
    fixture.watches.retire_pane(PANE);
    let ready = fixture.watches.take_ready(PANE);
    assert!(!ready.output && !ready.input && ready.exit);
    fixture.output.1.write_all(b"more").unwrap();
    assert!(
        fixture.dispatch(QUIET),
        "the still-readable exit was not re-armed"
    );
    let ready = fixture.watches.take_ready(PANE);
    assert!(!ready.output, "a retired pane reported output");
}

#[test]
fn input_interest_follows_pending_writes() {
    let mut fixture = Fixture::new();
    let (_reader, writer) = pipe().unwrap();
    fixture
        .watches
        .watch_input(PANE, Some(writer.as_fd()))
        .unwrap();
    assert!(fixture.dispatch(WAIT));
    assert!(fixture.watches.take_ready(PANE).input);
    fixture.watches.watch_input(PANE, None).unwrap();
    fixture.watches.dispatch().unwrap();
    assert!(
        !fixture.watches.take_ready(PANE).input,
        "input reported after unwatching"
    );
    fixture.watches.stop_output(PANE);
    fixture.output.1.write_all(b"late").unwrap();
    assert!(
        !fixture.dispatch(QUIET),
        "stopped output woke the main thread"
    );
}

#[test]
fn blink_flips_on_its_deadline_and_reset_shows_the_cursor() {
    let mut fixture = Fixture::new();
    fixture.watches.watch_blink(PANE, true).unwrap();
    assert!(!fixture.watches.cursor_hidden(PANE));
    assert!(fixture.dispatch(BLINK_PHASE + WAIT));
    assert!(
        fixture.watches.cursor_hidden(PANE),
        "the phase did not flip"
    );
    fixture.watches.reset_blink(PANE);
    assert!(!fixture.watches.cursor_hidden(PANE));
    // The deadline registered before the reset fires early and keeps the visible phase.
    let start = Instant::now();
    assert!(fixture.dispatch(BLINK_PHASE + WAIT));
    while !fixture.watches.cursor_hidden(PANE) {
        assert!(fixture.dispatch(BLINK_PHASE + WAIT));
    }
    assert!(start.elapsed() + Duration::from_millis(5) >= BLINK_PHASE);
    fixture.watches.watch_blink(PANE, false).unwrap();
    assert!(!fixture.watches.cursor_hidden(PANE));
}

#[test]
fn timers_and_config_wake_dispatch() {
    let mut fixture = Fixture::new();
    let timers = fixture.watches.animation_timer();
    let cancelled = timers.register(Instant::now() + QUIET / 2).unwrap();
    timers.cancel(cancelled);
    assert!(!fixture.dispatch(QUIET), "a cancelled timer fired");
    timers.register(Instant::now() + QUIET / 2).unwrap();
    assert!(fixture.dispatch(WAIT), "the timer did not wake dispatch");

    let (mut results, mut writer) = pipe().unwrap();
    let (exit, mut exited) = pipe().unwrap();
    fixture
        .watches
        .register_config(results.as_fd(), exit.as_fd())
        .unwrap();
    assert!(!fixture.watches.take_config_ready().unwrap());
    writer.write_all(b"c").unwrap();
    assert!(fixture.dispatch(WAIT));
    results.read_exact(&mut [0]).unwrap();
    assert!(fixture.watches.take_config_ready().unwrap());
    assert!(!fixture.watches.take_config_ready().unwrap());

    // A stopped result socket no longer reports; the exit watch still does.
    fixture.watches.stop_config_results();
    writer.write_all(b"d").unwrap();
    assert!(!fixture.dispatch(QUIET), "a stopped result socket reported");
    exited.write_all(b"x").unwrap();
    assert!(fixture.dispatch(WAIT));
    assert!(fixture.watches.take_config_ready().unwrap());
    fixture.watches.unregister_config();
    assert!(
        !fixture.dispatch(QUIET),
        "an unregistered exit watch reported"
    );
}

#[test]
fn a_report_queued_before_unregister_never_surfaces_after_it() {
    let mut fixture = Fixture::new();
    let (results, _writer) = pipe().unwrap();
    let (exit, mut exited) = pipe().unwrap();
    fixture
        .watches
        .register_config(results.as_fd(), exit.as_fd())
        .unwrap();
    exited.write_all(b"x").unwrap();
    assert!(fixture.dispatch(WAIT));
    let before = fixture.sent.load(Ordering::SeqCst);
    // Re-arming the undrained exit reports it again. Let the bridge send that wake before
    // unregistering, as a loaded machine sometimes does.
    assert!(fixture.watches.take_config_ready().unwrap());
    fixture.await_sent(before);
    fixture.watches.unregister_config();
    assert!(
        !fixture.dispatch(QUIET),
        "a report queued before unregister woke the main thread"
    );

    fixture.output.1.write_all(b"out").unwrap();
    assert!(
        fixture.dispatch(WAIT),
        "a refused wake blocked later reports"
    );
    assert!(fixture.watches.take_ready(PANE).output);
}
