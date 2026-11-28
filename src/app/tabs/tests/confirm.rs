//! Close and paste confirmations.

use super::*;

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
