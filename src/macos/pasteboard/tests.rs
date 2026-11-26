use std::{
    env,
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
};

use sumie_macos::readiness::Bridge;

use super::*;

const WAIT: Duration = Duration::from_secs(5);
/// The main-thread budget for one request's calls.
const BUDGET: Duration = Duration::from_millis(100);
const CHILD_ENV: &str = "SUMIE_TEST_PASTEBOARD_CHILD";

/// Serves one request when another test starts this binary as its child.
#[test]
#[ignore = "the pasteboard child of the other tests"]
fn pasteboard_child() {
    if let Some(board) = env::var_os(CHILD_ENV) {
        serve_child(Some(board.into_string().unwrap())).unwrap();
    }
}

/// A fresh named board that nothing else writes.
fn unique() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let next = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("org.sumie.test.{}.{next}", process::id())
}

fn child(board: &str) -> Launch {
    let board = board.to_owned();
    Box::new(move || {
        let mut command = Command::new(env::current_exe()?);
        command
            .args([
                "--exact",
                "macos::pasteboard::tests::pasteboard_child",
                "--ignored",
            ])
            .env(CHILD_ENV, &board);
        Ok(command)
    })
}

/// Plays the event loop, like the configuration child's fixture: a bridge wake is
/// `Event::Ready`, and `main` sums the time spent in main-thread calls.
struct Fixture {
    pasteboard: Pasteboard,
    watches: Watches,
    wakes: mpsc::Receiver<()>,
    pasted: Vec<(String, u64)>,
    main: Duration,
    _bridge: Bridge,
}

impl Fixture {
    fn new(launch: Launch, timeout: Duration) -> Self {
        let (sender, wakes) = mpsc::channel();
        let bridge = Bridge::spawn(move || {
            let _ = sender.send(());
        })
        .unwrap();
        let watches = Watches::new(bridge.readiness().clone());
        Self {
            pasteboard: Pasteboard::with_launch(launch, timeout, &watches),
            watches,
            wakes,
            pasted: Vec::new(),
            main: Duration::ZERO,
            _bridge: bridge,
        }
    }

    fn request(&mut self, request: Request) {
        let started = Instant::now();
        self.pasteboard.request(request, &mut self.watches);
        self.main += started.elapsed();
    }

    /// Service wakes until nothing is queued, in flight or unreaped.
    fn settle(&mut self) {
        let deadline = Instant::now() + 3 * WAIT;
        while self.pasteboard.children() > 0 || !self.pasteboard.queue.is_empty() {
            assert!(Instant::now() < deadline, "the helpers did not finish");
            let _ = self.wakes.recv_timeout(Duration::from_millis(50));
            let started = Instant::now();
            self.watches.dispatch().unwrap();
            let pasted = self.pasteboard.service(&mut self.watches).unwrap();
            self.main += started.elapsed();
            self.pasted.extend(
                pasted
                    .into_iter()
                    .map(|paste| (paste.text, paste.recipient)),
            );
        }
    }
}

#[test]
fn requests_run_in_order_and_reads_keep_their_recipient() {
    let board = unique();
    let mut fixture = Fixture::new(child(&board), TIMEOUT);
    for request in [
        Request::Write(Some("one 墨".to_owned())),
        Request::Read { recipient: 7 },
        Request::Write(Some("two".to_owned())),
        Request::Read { recipient: 9 },
        Request::Write(None),
        Request::Read { recipient: 11 },
    ] {
        fixture.request(request);
    }
    fixture.settle();
    assert_eq!(
        fixture.pasted,
        [("one 墨".to_owned(), 7), ("two".to_owned(), 9)]
    );
    assert!(
        fixture.main <= 6 * BUDGET,
        "{:?} on the main thread",
        fixture.main
    );
}

#[test]
fn an_empty_or_oversized_board_pastes_nothing() {
    let name = unique();
    let board = Board::Named(name.clone());
    let mut fixture = Fixture::new(child(&name), TIMEOUT);
    assert!(pasteboard::write(&board, Some("")));
    fixture.request(Request::Read { recipient: 1 });
    fixture.settle();
    assert!(pasteboard::write(&board, Some(&"x".repeat(LIMIT + 1))));
    fixture.request(Request::Read { recipient: 2 });
    fixture.settle();
    assert!(pasteboard::write(&board, Some(&"x".repeat(LIMIT))));
    fixture.request(Request::Read { recipient: 3 });
    fixture.settle();
    let lengths: Vec<_> = fixture
        .pasted
        .iter()
        .map(|(text, recipient)| (text.len(), *recipient))
        .collect();
    assert_eq!(lengths, [(LIMIT, 3)]);
}

#[test]
fn a_stalled_helper_times_out_without_blocking_and_is_reaped() {
    // It never reads its socket, so the cancel's EOF does not end it: SIGKILL does.
    let stalled: Launch = Box::new(|| {
        let mut command = Command::new("/bin/sleep");
        command.arg("60");
        Ok(command)
    });
    let timeout = Duration::from_millis(300);
    let mut fixture = Fixture::new(stalled, timeout);
    let started = Instant::now();
    fixture.request(Request::Read { recipient: 1 });
    fixture.request(Request::Write(Some("queued".to_owned())));
    fixture.settle();
    let elapsed = started.elapsed();
    assert_eq!(fixture.pasted, []);
    // Each request waits its own timeout, then the kill deadline ends its child.
    assert!(
        elapsed >= 2 * timeout && elapsed < 2 * (timeout + KILL_AFTER) + WAIT,
        "{elapsed:?}"
    );
    assert!(
        fixture.main <= 2 * BUDGET,
        "{:?} on the main thread",
        fixture.main
    );
}

#[test]
fn closing_drops_waiting_requests_and_reaps_the_running_one() {
    let stalled: Launch = Box::new(|| {
        let mut command = Command::new("/bin/sleep");
        command.arg("60");
        Ok(command)
    });
    let mut fixture = Fixture::new(stalled, TIMEOUT);
    fixture.request(Request::Read { recipient: 1 });
    fixture.request(Request::Read { recipient: 2 });
    fixture.pasteboard.close(&mut fixture.watches).unwrap();
    fixture.request(Request::Read { recipient: 3 });
    let started = Instant::now();
    fixture.settle();
    assert!(started.elapsed() < WAIT, "{:?}", started.elapsed());
    assert_eq!(fixture.pasted, []);
}
