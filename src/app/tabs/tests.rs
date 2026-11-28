use std::{ffi::OsStr, os::fd::BorrowedFd, time::Duration};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use sumie_core::notify::Notification;
use sumie_pty::{Command, Size, SpawnOptions};

use super::*;
use crate::{
    frame_window::FrameWindow,
    platform::{CaretBounds, CursorIcon, ExitWatches, LogicalRect, Ready},
};

#[test]
fn middle_click_on_a_tab_closes_it_and_captures_release() {
    let mut pointer = BarPointer::default();
    assert!(matches!(
        pointer.route(Some(TabHit::Tab(1)), true, Phase::Press(Button::Middle, 0)),
        Break(Some(Action::Close(1)))
    ));
    assert!(matches!(
        pointer.route(None, false, Phase::Release(Button::Middle)),
        Break(None)
    ));
}

#[test]
fn wheel_counts_whole_notches_and_keeps_the_remainder() {
    let mut pointer = BarPointer::default();
    for (delta, expected) in [(375., 3), (105., 1), (-90., 0), (-150., -2)] {
        let steps = match pointer.route(None, true, Phase::Wheel(delta)) {
            Break(Some(Action::Key(TabAction::Cycle(steps)))) => steps,
            Break(None) => 0,
            _ => panic!("wheel did not stay on the tab bar"),
        };
        assert_eq!(steps, expected);
    }
}

#[derive(Default)]
struct TestWindow {
    ready: Ready,
    clipboard: Vec<Option<Vec<u8>>>,
}

impl FrameWindow for TestWindow {
    fn frame_slot_available(&self) -> bool {
        false
    }
    fn arm_frame_callback(&mut self) {}
}

impl ExitWatches for TestWindow {
    fn take_exit_ready(&mut self, _: u64) -> bool {
        false
    }
    fn unregister(&mut self, _: u64) {}
}

impl PaneWatches for TestWindow {
    fn register_pane(&mut self, _: u64, _: BorrowedFd<'_>, _: BorrowedFd<'_>) -> Result<()> {
        Ok(())
    }
    fn take_ready(&mut self, _: u64) -> Ready {
        std::mem::take(&mut self.ready)
    }
    fn watch_input(&mut self, _: u64, _: Option<BorrowedFd<'_>>) -> Result<()> {
        Ok(())
    }
    fn stop_output(&mut self, _: u64) {}
    fn watch_blink(&mut self, _: u64, _: bool) -> Result<()> {
        Ok(())
    }
    fn reset_blink(&mut self, _: u64) {}
    fn cursor_hidden(&self, _: u64) -> bool {
        false
    }
    fn retire_pane(&mut self, _: u64) {}
}

impl WindowOps for TestWindow {
    fn click_interval() -> Option<u32> {
        None
    }
    fn hostname() -> Result<String> {
        Ok(String::new())
    }
    fn logical_rect(_: CaretBounds) -> Option<LogicalRect> {
        None
    }
    fn recover_frame_slot(&mut self) {}
    fn set_cursor_rectangle(&mut self, _: LogicalRect) {}
    fn set_pointer_icon(&mut self, _: CursorIcon) {}
    fn set_input_generation(&mut self, _: u64, _: u64) {}
    fn read_clipboard(&mut self, _: u64) {}
    fn read_primary(&mut self, _: u64) {}
    fn write_clipboard(&mut self, _: String, _: bool) {}
    fn write_terminal_clipboard(&mut self, bytes: Option<Vec<u8>>) {
        self.clipboard.push(bytes);
    }
    fn queue_title(&mut self, _: String) {}
    fn notify(&mut self, _: Notification) {}
    fn set_background_effects(&mut self, _: bool, _: bool) -> bool {
        false
    }
    fn background_commit_needed(&self) -> bool {
        false
    }
}

fn session(window: &mut TestWindow, config: &sumie_config::Config) -> Session {
    let environment = Environment::new([("SHELL".into(), "/bin/cat".into())]).unwrap();
    Session::new(
        120,
        [640, 320],
        [640, 320],
        window,
        PaneHost::new(environment, 4096, None, false),
        String::new(),
        config,
    )
    .unwrap()
}

fn wait_ready(fd: BorrowedFd<'_>) {
    let timeout = Timespec::try_from(Duration::from_secs(5)).unwrap();
    assert_eq!(
        poll(
            &mut [PollFd::from_borrowed_fd(fd, PollFlags::IN)],
            Some(&timeout)
        )
        .unwrap(),
        1
    );
}

#[cfg(target_os = "linux")]
#[test]
fn queued_ime_commit_reaches_the_original_pane() {
    let config = sumie_config::Config::default();
    let mut window = TestWindow::default();
    let mut session = session(&mut window, &config);
    let original = session.active().id;
    let commit = Event::ImeCommit {
        text: "original".into(),
        recipient: session.active().text_recipient.unwrap(),
    };
    session
        .apply(Action::Key(TabAction::Open), &mut window, &config)
        .unwrap();
    assert_ne!(session.active().id, original);
    session.input(commit, &mut window, false).unwrap();
    assert!(session.active().pane.pty.input_fd().is_none());

    let pane = &mut session.tabs.as_mut_slice()[0].get_mut(original).pane;
    assert!(pane.pty.input_fd().is_some());
    pane.command(PaneCommand::Text(b"\n")).unwrap();
    assert!(pane.pty.flush_input().unwrap());
    crate::tests::wait_for_line(pane, "original");
}

#[test]
fn exit_readiness_keeps_final_terminal_clipboard_write() {
    let config = sumie_config::Config::default();
    let mut window = TestWindow::default();
    let mut session = session(&mut window, &config);
    let args = [
        "-c".into(),
        "read -r line; printf '\\033]52;c;ZmluYWw=\\007'".into(),
    ];
    let pane = &mut session.active_mut().pane;
    *pane = LocalPane::spawn(
        SpawnOptions::new(
            Command::Argv {
                program: OsStr::new("/bin/sh"),
                args: &args,
            },
            &std::collections::BTreeMap::new(),
            Size {
                cols: 40,
                rows: 10,
                width_px: 480,
                height_px: 240,
            },
        ),
        [12, 24],
        &config,
    )
    .unwrap();
    pane.command(PaneCommand::Text(b"\x04")).unwrap();
    assert!(pane.pty.flush_input().unwrap());
    wait_ready(pane.pty.exit_fd().unwrap());
    wait_ready(pane.pty.output_fd());
    window.ready = Ready {
        output: true,
        exit: true,
        input: false,
    };

    assert!(
        session
            .service_pty(Instant::now(), true, &mut window)
            .unwrap()
    );
    assert_eq!(window.clipboard, [Some(b"final".to_vec())]);
    assert!(session.close_exited(&mut window).unwrap());
}
