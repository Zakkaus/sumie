use std::{ffi::OsStr, os::fd::BorrowedFd, time::Duration};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use sumie_core::notify::Notification;
use sumie_core::{KeyAction, KeyCode, KeyModifiers, PhysicalKey};
use sumie_pty::{Command, Size, SpawnOptions};

use super::*;
use crate::{
    frame_window::FrameWindow,
    platform::{CaretBounds, CursorIcon, ExitWatches, LogicalKey, LogicalRect, Ready},
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
    paste_request: Option<u64>,
    notifications: Vec<Notification>,
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
    fn read_clipboard(&mut self, recipient: u64) {
        self.paste_request = Some(recipient);
    }
    fn read_primary(&mut self, _: u64) {}
    fn write_clipboard(&mut self, _: String, _: bool) {}
    fn write_terminal_clipboard(&mut self, bytes: Option<Vec<u8>>) {
        self.clipboard.push(bytes);
    }
    fn queue_title(&mut self, _: String) {}
    fn notify(&mut self, request: Notification) {
        self.notifications.push(request);
    }
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
fn paste_shortcut_restores_a_missing_recipient() {
    let config = sumie_config::Config::default();
    let mut window = TestWindow::default();
    let mut session = session(&mut window, &config);
    session.active_mut().text_recipient = None;
    let macos = cfg!(target_os = "macos");
    let event = Event::Key {
        physical: PhysicalKey::Code(KeyCode::KeyV),
        logical: LogicalKey::Character('v'),
        utf8: None,
        modifiers: KeyModifiers {
            super_key: macos,
            control: !macos,
            shift: !macos,
            ..KeyModifiers::default()
        },
        action: KeyAction::Press,
    };
    session.input(event, &mut window, false, None).unwrap();
    session
        .input(
            Event::ClipboardText {
                text: "paste".into(),
                generation: window.paste_request.unwrap(),
            },
            &mut window,
            false,
            None,
        )
        .unwrap();
    let pane = &mut session.active_mut().pane;
    pane.command(PaneCommand::Text(b"\n")).unwrap();
    assert!(pane.pty.flush_input().unwrap());
    crate::tests::wait_for_line(pane, "paste");
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
    session.input(commit, &mut window, false, None).unwrap();
    assert!(session.active().pane.pty.input_fd().is_none());

    let pane = &mut session.tabs.as_mut_slice()[0].get_mut(original).pane;
    assert!(pane.pty.input_fd().is_some());
    pane.command(PaneCommand::Text(b"\n")).unwrap();
    assert!(pane.pty.flush_input().unwrap());
    crate::tests::wait_for_line(pane, "original");
}

#[test]
fn pointer_hits_use_the_old_scroll_offset_until_presented() {
    use sumie_core::mouse::MousePhase;
    use sumie_render::SurfaceFrame;

    let mut session = session(&mut TestWindow::default(), &sumie_config::Config::default());
    let rect = session.active().rect;
    let row_height = session.view.cell[1];
    let [pad_x, pad_y] = session.view.pane_padding();
    let near_row_end = Position::DevicePx(
        f64::from(rect.x + pad_x + 1),
        f64::from(rect.y + pad_y + row_height - 1),
    );
    let terminal = &mut session.active_mut().pane.terminal;
    terminal.write(b"one\r\n".repeat(40).as_slice());
    terminal.scroll_rows(-2).unwrap();
    session
        .active_mut()
        .smooth_wheel(MousePhase::Touch(24.0), 120, Duration::ZERO);
    session.step_scroll(Instant::now()).unwrap();
    for (frame, row) in [
        (SurfaceFrame::Timeout, 0),
        (SurfaceFrame::Presented { suboptimal: false }, 1),
    ] {
        session.present_pointer_hits(frame);
        let (cell, _, _) =
            session
                .view
                .pointer_position(near_row_end, rect, session.active().scroll.shown());
        assert_eq!(cell.row, row);
    }
}

#[test]
fn zoomed_out_pane_can_notify_from_the_active_tab() {
    let config = sumie_config::Config::default();
    let mut window = TestWindow::default();
    let mut session = session(&mut window, &config);
    let first = session.active().id;
    session
        .apply(
            Action::Pane(PaneAction::Split(sumie_core::splits::Axis::Horizontal)),
            &mut window,
            &config,
        )
        .unwrap();
    let focused = session.active().id;
    assert_ne!(first, focused);
    let now = Instant::now();
    for zoomed in [false, true] {
        if zoomed {
            session
                .pane_action(PaneAction::Zoom, focused, &mut window, &config)
                .unwrap();
        }
        session
            .tabs
            .active_mut()
            .unwrap()
            .get_mut(first)
            .pane
            .terminal
            .write(b"\x1b]9;hidden pane\x07");
        window.ready.output = true;
        session.service_pty(now, true, &mut window).unwrap();
        assert_eq!(window.notifications.len(), usize::from(zoomed));
    }
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

fn confirmation_key(physical: KeyCode, logical: LogicalKey) -> Event {
    Event::Key {
        physical: PhysicalKey::Code(physical),
        logical,
        utf8: None,
        modifiers: KeyModifiers::default(),
        action: KeyAction::Press,
    }
}

#[cfg(target_os = "linux")]
#[test]
fn window_close_uses_osc133_before_foreground_group() {
    let config = sumie_config::Config::default();
    let mut window = TestWindow::default();
    let mut session = session(&mut window, &config);
    session.active_mut().pane = crate::tests::ready_pane(
        "set -m; /bin/sh -c 'printf \"READY\\n\"; read -r line'; :",
        &std::collections::BTreeMap::from([("SHELL".into(), "/bin/sh".into())]),
    );
    assert!(!session.request_window_close(&config).unwrap());
    assert!(matches!(
        session.confirmation,
        Some((Confirmation::Window, _))
    ));

    let confirm = || confirmation_key(KeyCode::KeyY, LogicalKey::Character('y'));
    assert!(
        !session
            .dispatch_input(confirm(), &mut window, true, &config)
            .unwrap()
    );
    session.confirmation_presented = true;
    let cancel = confirmation_key(KeyCode::Enter, LogicalKey::Enter);
    assert!(
        !session
            .dispatch_input(cancel, &mut window, true, &config)
            .unwrap()
    );
    assert!(session.confirmation.is_none());

    let terminal = &mut session.active_mut().pane.terminal;
    terminal.write(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    assert!(session.request_window_close(&config).unwrap());
    assert!(session.confirmation.is_none());

    session
        .active_mut()
        .pane
        .terminal
        .write(b"\x1b]133;C\x07running\r\n");
    let disabled =
        sumie_config::Config::from_json(br#"{"window":{"confirm-close":false}}"#).unwrap();
    assert!(session.request_window_close(&disabled).unwrap());
    assert!(!session.request_window_close(&config).unwrap());
    session.confirmation_presented = true;
    assert!(
        session
            .dispatch_input(confirm(), &mut window, true, &config)
            .unwrap()
    );
}

#[test]
fn tab_and_pane_close_wait_for_confirmation() {
    let config = sumie_config::Config::default();
    let mut window = TestWindow::default();
    let mut session = session(&mut window, &config);
    session
        .apply(Action::Key(TabAction::Open), &mut window, &config)
        .unwrap();
    session
        .apply(
            Action::Pane(PaneAction::Split(sumie_core::splits::Axis::Horizontal)),
            &mut window,
            &config,
        )
        .unwrap();
    session
        .active_mut()
        .pane
        .terminal
        .write(b"\x1b]133;A\x07$ \x1b]133;C\x07running\r\n");
    let index = session.tabs.active_index().unwrap();
    let running = session.active().id;
    session
        .apply(Action::Close(index), &mut window, &config)
        .unwrap();
    assert_eq!(session.tabs.as_slice().len(), 2);
    session.confirmation_presented = true;
    let cancel = || confirmation_key(KeyCode::Enter, LogicalKey::Enter);
    assert!(
        !session
            .dispatch_input(cancel(), &mut window, true, &config)
            .unwrap()
    );
    assert_eq!(session.tabs.as_slice().len(), 2);
    session
        .apply(Action::Key(TabAction::Close), &mut window, &config)
        .unwrap();
    assert_eq!(session.active().id, running);
    session.confirmation_presented = true;
    assert!(
        !session
            .dispatch_input(cancel(), &mut window, true, &config)
            .unwrap()
    );
    assert_eq!(session.active().id, running);
    session
        .apply(Action::Key(TabAction::Close), &mut window, &config)
        .unwrap();
    session.confirmation_presented = true;
    let confirm = confirmation_key(KeyCode::KeyY, LogicalKey::Character('y'));
    assert!(
        !session
            .dispatch_input(confirm, &mut window, true, &config)
            .unwrap()
    );
    assert_ne!(session.active().id, running);
    assert_eq!(session.tabs.as_slice().len(), 2);
    assert_eq!(session.tabs.as_slice()[index].panes.len(), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn shell_exit_retires_busy_pane_without_confirmation() {
    let config = sumie_config::Config::default();
    let mut window = TestWindow::default();
    let mut session = session(&mut window, &config);
    let pane = &mut session.active_mut().pane;
    *pane = crate::tests::ready_pane(
        "printf '\\033]133;A\\007\\033]133;C\\007READY\\n'; read -r line",
        &std::collections::BTreeMap::new(),
    );
    assert_eq!(pane.terminal.semantic_prompt().unwrap(), Some(false));
    pane.command(PaneCommand::Text(b"\n")).unwrap();
    assert!(pane.pty.flush_input().unwrap());
    wait_ready(pane.pty.exit_fd().unwrap());
    window.ready.exit = true;
    assert!(
        session
            .service_pty(Instant::now(), true, &mut window)
            .unwrap()
    );
    assert!(session.close_exited(&mut window).unwrap());
    assert!(session.confirmation.is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn multiline_paste_waits_unless_bracketed() {
    let config = sumie_config::Config::default();
    let mut window = TestWindow::default();
    let mut session = session(&mut window, &config);
    session.active_mut().pane = crate::tests::ready_pane(
        "stty -echo; printf 'READY\\n'; read -r a; read -r b; printf 'INPUT:%s,%s\\n' \"$a\" \"$b\"; read -r c",
        &std::collections::BTreeMap::new(),
    );
    let generation = session.routing.generation;
    let paste = |text: &str| Event::ClipboardText {
        text: text.into(),
        generation,
    };
    session
        .dispatch_input(paste("cancel\rthis\r"), &mut window, false, &config)
        .unwrap();
    assert!(session.active().pane.pty.input_fd().is_none());
    let cancel = confirmation_key(KeyCode::Escape, LogicalKey::Escape);
    session
        .dispatch_input(cancel, &mut window, true, &config)
        .unwrap();
    assert!(session.active().pane.pty.input_fd().is_none());

    session
        .dispatch_input(paste("first\nsecond\n"), &mut window, false, &config)
        .unwrap();
    assert!(session.active().pane.pty.input_fd().is_none());
    session.confirmation_presented = true;
    let confirm = confirmation_key(KeyCode::Enter, LogicalKey::Enter);
    session
        .dispatch_input(confirm, &mut window, true, &config)
        .unwrap();
    let pane = &mut session.active_mut().pane;
    assert!(pane.pty.flush_input().unwrap());
    crate::tests::wait_for_line(pane, "INPUT:first,second");

    pane.terminal.write(b"\x1b[?2004h");
    session
        .dispatch_input(paste("bracketed\npaste"), &mut window, false, &config)
        .unwrap();
    assert!(session.active().pane.pty.input_fd().is_some());
}
