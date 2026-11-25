use std::io::{Read as _, Write as _, pipe};
use std::os::fd::AsFd;
use std::sync::mpsc;
use std::time::Duration;

use super::*;

const WAIT: Duration = Duration::from_secs(2);
const QUIET: Duration = Duration::from_millis(100);

fn bridge() -> (Bridge, mpsc::Receiver<()>) {
    let (sender, wakes) = mpsc::channel();
    let bridge = Bridge::spawn(move || {
        let _ = sender.send(());
    })
    .unwrap();
    (bridge, wakes)
}

fn cloexec(fd: BorrowedFd<'_>) -> bool {
    FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD).unwrap()).contains(FdFlag::FD_CLOEXEC)
}

#[test]
fn a_ready_descriptor_reports_once_until_rearmed() {
    let (bridge, wakes) = bridge();
    let readiness = bridge.readiness();
    let (reader, mut writer) = pipe().unwrap();
    let output = Key::new(7, Interest::Output);
    readiness.watch(output, reader.as_fd()).unwrap();
    assert!(
        wakes.recv_timeout(QUIET).is_err(),
        "an idle pipe woke the main thread"
    );

    writer.write_all(b"ab").unwrap();
    wakes
        .recv_timeout(WAIT)
        .expect("a readable pipe did not wake");
    writer.write_all(b"c").unwrap();
    assert!(
        wakes.recv_timeout(QUIET).is_err(),
        "a disabled registration woke again"
    );
    assert_eq!(readiness.take(), [output]);

    // Still readable: re-arming reports it again, with a fresh wake.
    readiness.rearm(output).unwrap();
    wakes
        .recv_timeout(WAIT)
        .expect("a re-armed readable pipe did not wake");
    assert_eq!(readiness.take(), [output]);
}

#[test]
fn removal_drops_reports_from_the_old_generation() {
    let (bridge, wakes) = bridge();
    let readiness = bridge.readiness();
    let (reader, mut writer) = pipe().unwrap();
    let exit = Key::new(3, Interest::Exit);
    readiness.watch(exit, reader.as_fd()).unwrap();
    writer.write_all(b"x").unwrap();
    wakes.recv_timeout(WAIT).unwrap();
    readiness.remove(exit);
    assert!(
        readiness.take().is_empty(),
        "a removed registration was reported"
    );

    let (idle, _idle_writer) = pipe().unwrap();
    readiness.watch(exit, idle.as_fd()).unwrap();
    assert!(wakes.recv_timeout(QUIET).is_err());
    assert!(
        readiness.take().is_empty(),
        "the old descriptor reported for the new generation"
    );
}

#[test]
fn an_exit_watch_reports_every_rearm_until_removed() {
    // An early NOTE_EXIT leaves the child unreaped; the watch must survive the first report.
    let (bridge, wakes) = bridge();
    let readiness = bridge.readiness();
    let (mut reader, mut writer) = pipe().unwrap();
    let exit = Key::new(9, Interest::Exit);
    readiness.watch(exit, reader.as_fd()).unwrap();
    for round in 0..3 {
        writer.write_all(b"e").unwrap();
        wakes
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("exit report {round} missing"));
        assert_eq!(readiness.take(), [exit]);
        reader.read_exact(&mut [0]).unwrap();
        readiness.rearm(exit).unwrap();
    }
}

#[test]
fn deadlines_fire_once_and_replacement_keeps_one() {
    let (bridge, wakes) = bridge();
    let readiness = bridge.readiness();
    let blink = Key::new(4, Interest::Blink);
    let start = Instant::now();
    readiness
        .set_deadline(blink, start + Duration::from_secs(60))
        .unwrap();
    readiness
        .set_deadline(blink, start + Duration::from_millis(20))
        .unwrap();
    wakes
        .recv_timeout(WAIT)
        .expect("the replaced deadline did not fire");
    assert!(start.elapsed() >= Duration::from_millis(20));
    assert_eq!(readiness.take(), [blink]);
    assert!(
        wakes.recv_timeout(QUIET).is_err(),
        "a one-shot deadline fired twice"
    );

    let cancelled = readiness
        .timer(Instant::now() + Duration::from_millis(20))
        .unwrap();
    readiness.remove(cancelled);
    readiness.remove(cancelled);
    assert!(
        wakes.recv_timeout(QUIET).is_err(),
        "a cancelled timer fired"
    );
    assert_eq!(readiness.take(), Vec::<Key>::new());
}

#[test]
fn reports_coalesce_into_one_wake_per_take() {
    let (bridge, wakes) = bridge();
    let readiness = bridge.readiness();
    let pipes: Vec<_> = (0..4).map(|_| pipe().unwrap()).collect();
    for (id, (reader, _)) in (0..).zip(&pipes) {
        readiness
            .watch(Key::new(id, Interest::Output), reader.as_fd())
            .unwrap();
    }
    for (_, writer) in &pipes {
        (&*writer).write_all(b"w").unwrap();
    }
    wakes.recv_timeout(WAIT).unwrap();
    std::thread::sleep(QUIET);
    let mut ready = readiness.take();
    ready.sort_by_key(|key| key.id);
    assert_eq!(
        ready,
        (0..4)
            .map(|id| Key::new(id, Interest::Output))
            .collect::<Vec<_>>()
    );
    assert!(
        wakes.try_recv().is_err(),
        "one batch sent more than one wake"
    );
}

#[test]
fn bridge_descriptors_close_on_exec() {
    let (bridge, _wakes) = bridge();
    let readiness = bridge.readiness();
    assert!(
        cloexec(readiness.queue().as_fd()),
        "the bridge kqueue is inherited across exec"
    );
    let (reader, _writer) = pipe().unwrap();
    let key = Key::new(1, Interest::Config);
    readiness.watch(key, reader.as_fd()).unwrap();
    let state = readiness.lock();
    let duplicate = state.live[&key].fd.as_ref().unwrap();
    assert!(
        cloexec(duplicate.as_fd()),
        "a duplicated descriptor is inherited across exec"
    );
}
