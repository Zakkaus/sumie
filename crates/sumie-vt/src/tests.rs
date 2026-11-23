use super::Terminal;

#[test]
fn paste_filters_controls_in_both_modes() {
    let mut terminal = Terminal::new(5, 2).unwrap();
    for (mode, input, expected) in [
        (
            "\x1b[?2004h",
            "a\x00\x01\x1b[201~\u{7f}\u{80}\u{9f}\t\nb\x1b中",
            "\x1b[200~a\t\nb中\x1b[201~",
        ),
        ("\x1b[?2004h", "\x01\x1b[201~\u{9b}", ""),
        (
            "\x1b[?2004l",
            "a\x00\x01\x1b\u{7f}\u{80}\u{9f}\t\rb\n中",
            "a\t\rb\r中",
        ),
    ] {
        terminal.write(mode.as_bytes());
        assert_eq!(
            terminal.encode_paste(input.into()).unwrap(),
            expected.as_bytes()
        );
    }
}

#[test]
fn snapshot_keeps_wide_cells_whole() {
    let mut terminal = Terminal::new(5, 2).unwrap();
    terminal.write("hello\r\n中文".as_bytes());
    let screen = terminal.snapshot().unwrap();
    let expected = [
        [("h", 1), ("e", 1), ("l", 1), ("l", 1), ("o", 1)],
        [("中", 2), ("", 0), ("文", 2), ("", 0), ("", 1)],
    ];
    assert_eq!(screen.len(), expected.len());
    for (row, expected) in screen.iter().zip(expected) {
        assert_eq!(row.len(), expected.len());
        assert!(
            row.iter()
                .map(|cell| (cell.text.as_str(), cell.width))
                .eq(expected),
            "unexpected row: {row:?}"
        );
    }
}

#[test]
fn resize_preserves_content() {
    let mut terminal = Terminal::new(5, 2).unwrap();
    terminal.write(b"hello");
    terminal.resize(8, 3).unwrap();
    let screen = terminal.snapshot().unwrap();
    assert_eq!(screen.len(), 3);
    assert!(screen.iter().all(|row| row.len() == 8));
    assert_eq!(
        screen[0][..5]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>(),
        "hello"
    );
}

#[test]
fn repeatedly_create_and_drop() {
    for _ in 0..200 {
        let mut terminal = Terminal::new(5, 2).unwrap();
        terminal.write(b"hello");
        terminal.snapshot().unwrap();
    }
}
