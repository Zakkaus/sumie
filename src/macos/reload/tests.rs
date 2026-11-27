use std::{
    collections::VecDeque,
    env,
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
    sync::mpsc,
};

use sumie_config::CursorStyle;
use sumie_macos::readiness::Bridge;

use super::*;

const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(300);
const CHILD_ENV: &str = "SUMIE_TEST_CONFIG_CHILD";
/// `O_NONBLOCK` from Darwin's `<sys/fcntl.h>`.
const O_NONBLOCK: i32 = 0x0004;

/// Runs the watcher when another test starts this binary as its child.
#[test]
#[ignore = "the configuration child of the other tests"]
fn config_child() {
    if env::var_os(CHILD_ENV).is_some() {
        watch_child().unwrap();
    }
}

fn child(xdg: &Path) -> Command {
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args(["--exact", "macos::reload::tests::config_child", "--ignored"])
        .env(CHILD_ENV, "1")
        .env("XDG_CONFIG_HOME", xdg);
    command
}

fn shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = env::temp_dir().join(format!("sumie-reload-{name}-{}", process::id()));
        fs::create_dir_all(dir.join("sumie")).unwrap();
        Self(dir)
    }

    fn path(&self, file: &str) -> PathBuf {
        self.0.join("sumie").join(file)
    }

    /// Replace the main file atomically, as editors do.
    fn save(&self, text: &str) {
        let staged = self.path(".staged");
        fs::write(&staged, text).unwrap();
        fs::rename(staged, self.path("sumie.toml")).unwrap();
    }

    /// Include a FIFO before the child starts, so its first `load()` reads it.
    fn include_fifo(&self) {
        self.save("include = ['fifo.toml']\n");
        let fifo = self.path("fifo.toml");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
    }

    /// Hold the FIFO's writer open once the child opens it: `load()` stays blocked reading
    /// it until the writer closes.
    fn hold_fifo(&self) -> File {
        let fifo = self.path("fifo.toml");
        let deadline = Instant::now() + WAIT;
        loop {
            // Opening for writing without blocking fails until a reader has it open.
            match OpenOptions::new()
                .write(true)
                .custom_flags(O_NONBLOCK)
                .open(&fifo)
            {
                Ok(writer) => return writer,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                Err(error) => panic!("the child never opened the FIFO: {error}"),
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Kills a child after `WAIT` unless disarmed first, so a regression that waits for a child
/// that never exits fails within seconds instead of hanging. `finish` settles the thread and
/// reports whether the kill was needed.
struct Rescue {
    disarm: mpsc::Sender<()>,
    thread: thread::JoinHandle<bool>,
}

impl Rescue {
    fn arm(pid: u32) -> Self {
        let (disarm, armed) = mpsc::channel();
        let thread = thread::spawn(move || {
            let late = armed.recv_timeout(WAIT) == Err(mpsc::RecvTimeoutError::Timeout);
            if late {
                let _ = Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .status();
            }
            late
        });
        Self { disarm, thread }
    }

    fn finish(self) -> bool {
        drop(self.disarm);
        self.thread.join().unwrap()
    }
}

/// Plays the event loop: a bridge wake is `Event::Ready`.
struct Fixture {
    reload: Reload,
    watches: Watches,
    wakes: mpsc::Receiver<()>,
    pending: VecDeque<Update>,
    _bridge: Bridge,
}

impl Fixture {
    fn spawn(command: Command) -> Self {
        let (sender, wakes) = mpsc::channel();
        let bridge = Bridge::spawn(move || {
            let _ = sender.send(());
        })
        .unwrap();
        let mut watches = Watches::new(bridge.readiness().clone());
        let reload = Reload::spawn(command, &mut watches).unwrap();
        Self {
            reload,
            watches,
            wakes,
            pending: VecDeque::new(),
            _bridge: bridge,
        }
    }

    /// Wait up to `within` for a wake and service it.
    fn step(&mut self, within: Duration) {
        let _ = self.wakes.recv_timeout(within);
        self.watches.dispatch().unwrap();
        let updates = self.reload.service(&mut self.watches).unwrap();
        self.pending.extend(updates);
    }

    fn next(&mut self) -> Update {
        let deadline = Instant::now() + WAIT;
        while self.pending.is_empty() && Instant::now() < deadline {
            self.step(deadline.saturating_duration_since(Instant::now()));
        }
        self.pending
            .pop_front()
            .expect("no result from the watcher")
    }

    fn reap(&mut self) {
        let deadline = Instant::now() + WAIT;
        while !self.reload.reaped() && Instant::now() < deadline {
            self.step(deadline.saturating_duration_since(Instant::now()));
        }
        assert!(self.reload.reaped(), "the watcher was not reaped");
    }

    /// Closing is nonblocking and takes microseconds; waiting for the child to answer the EOF
    /// would take until the kill escalation, so [`KILL_AFTER`] bounds the call alone.
    fn close_and_reap(mut self) {
        let started = Instant::now();
        self.reload.close(&mut self.watches).unwrap();
        let closing = started.elapsed();
        assert!(
            closing <= KILL_AFTER,
            "close spent {closing:?} on the main thread"
        );
        self.reap();
    }
}

#[test]
fn edits_arrive_as_settings_or_located_diagnostics() {
    let scratch = Scratch::new("edits");
    scratch.save("[cursor]\nstyle = 'bar'\n");
    let mut fixture = Fixture::spawn(child(&scratch.0));
    let first = fixture.next();
    assert!(
        matches!(&first, Update::Settings(config) if config.cursor.style() == CursorStyle::Bar),
        "{first:?}"
    );
    scratch.save("[cursor]\nstyle = 'bar'\nbogus\n");
    let broken = fixture.next();
    assert!(
        matches!(&broken, Update::Diagnostic(message) if message.starts_with("sumie.toml:3:")),
        "{broken:?}"
    );
    // Repairing to identical settings still reports them, so the app can drop the banner.
    scratch.save("[cursor]\nstyle = 'bar'\n");
    let repaired = fixture.next();
    assert!(
        matches!(&repaired, Update::Settings(config) if config.cursor.style() == CursorStyle::Bar),
        "{repaired:?}"
    );
    fixture.close_and_reap();
}

#[test]
fn a_child_blocked_on_a_fifo_include_is_reaped_after_close_without_waiting() {
    let scratch = Scratch::new("fifo");
    scratch.include_fifo();
    let mut fixture = Fixture::spawn(child(&scratch.0));
    let writer = scratch.hold_fifo();
    fixture.step(QUIET);
    assert!(
        fixture.pending.is_empty(),
        "load() returned through a blocked FIFO"
    );
    fixture.close_and_reap();
    drop(writer);
}

#[test]
fn dropping_a_blocked_child_does_not_wait() {
    let scratch = Scratch::new("drop");
    scratch.include_fifo();
    let fixture = Fixture::spawn(child(&scratch.0));
    let writer = scratch.hold_fifo();
    let rescue = Rescue::arm(fixture.reload.child.id());
    drop(fixture);
    assert!(!rescue.finish(), "dropping waited for the blocked child");
    drop(writer);
}

#[test]
fn malformed_results_and_early_eof_are_faults_that_end_the_watcher() {
    // `ignores_eof` marks a child that never exits by itself; only the deadline kill ends it.
    for (script, fault, ignores_eof) in [
        ("exit 0", "the watcher exited", false),
        // A grandchild keeps the socket open, so only the exit report ends the watcher.
        ("sleep 3 <&0 & exit 0", "the watcher exited", false),
        (
            r"printf '\000\000' >&0",
            "stream ended 2 bytes into a frame",
            false,
        ),
        (
            r"printf '\000\000\000\002\011\001{}' >&0; exec sleep 600",
            "unsupported frame version 9",
            true,
        ),
        (
            r"printf '\000\020\000\001\001\001' >&0; exec sleep 600",
            "frame body of 1048577 bytes exceeds 1048576",
            true,
        ),
        (
            r"printf '\000\000\000\001\001\001{' >&0; exec sleep 600",
            "malformed settings",
            true,
        ),
    ] {
        let mut fixture = Fixture::spawn(shell(script));
        let rescue = ignores_eof.then(|| Rescue::arm(fixture.reload.child.id()));
        let started = Instant::now();
        let update = fixture.next();
        assert!(
            matches!(&update, Update::Diagnostic(message) if message.contains(fault)),
            "{script}: {update:?}"
        );
        // A fault closes the socket without waiting: a child that ignores the EOF still runs
        // until the deadline kills it. A thread preempted past the deadline sees that kill
        // already, so a stopped child is a failure only before that deadline could pass.
        if let Some(rescue) = rescue {
            let unwaited = fixture.reload.child.try_wait().unwrap().is_none()
                || started.elapsed() >= KILL_AFTER;
            let rescued = rescue.finish();
            assert!(
                unwaited && !rescued,
                "{script}: the fault waited for the child"
            );
        }
        fixture.reap();
        assert!(
            fixture.pending.is_empty(),
            "{script}: results after the fault"
        );
        fixture.close_and_reap();
    }
}
