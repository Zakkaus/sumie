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
    use super::Underline::{None, Single};

    let mut terminal = Terminal::new(5, 2).unwrap();
    terminal.write("hel\x1b[4m 中文".as_bytes());
    let screen = terminal.snapshot().unwrap();
    let expected = [
        [("h", 1), ("e", 1), ("l", 1), (" ", 1), ("", 1)],
        [("中", 2), ("", 0), ("文", 2), ("", 0), ("", 1)],
    ];
    assert_eq!(
        [(0, 3), (0, 4), (1, 0)].map(|(y, x)| screen[y][x].style.underline),
        [Single, None, Single],
    );
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

#[test]
fn kitty_commands_query_transmit_place_and_delete() {
    const PNG: &str = "a=T,f=100,i=9,C=1;iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGMQVDL+DwACFAFmBODefwAAAABJRU5ErkJggg==";
    let mut vt = Terminal::new(12, 5).unwrap();
    vt.resize_pixels(12, 5, [8, 16]).unwrap();
    vt.set_image_dimension_limit(2);
    for (command, reply, visible, cursor) in [
        ("a=q,i=7,f=24,s=1,v=1;ESIz", "i=7;OK", 0, [0, 0]),
        (
            "a=q,i=7,f=24,s=1,v=1;EQ==",
            "i=7;EINVAL: invalid data",
            0,
            [0, 0],
        ),
        ("a=t,t=d,f=24,s=2,v=1,i=7,m=1;ESIz", "", 0, [0, 0]),
        ("m=0;RFVm", "i=7;OK", 0, [0, 0]),
        ("a=p,i=7,p=3,c=2,r=2,z=-1", "i=7,p=3;OK", 1, [2, 1]),
        ("a=d,d=I,i=7", "", 0, [2, 1]),
        (PNG, "i=9;OK", 1, [2, 1]),
        ("a=d,d=A", "", 0, [2, 1]),
        (
            "a=T,i=10,f=24,s=3,v=1,C=1;AAECAwQFBgcI",
            "i=10;OK",
            0,
            [2, 1],
        ),
    ] {
        vt.write(format!("\x1b_G{command}\x1b\\").as_bytes());
        let expected = if reply.is_empty() {
            String::new()
        } else {
            format!("\x1b_G{reply}\x1b\\")
        };
        let mut actual = Vec::new();
        vt.drain_replies(|bytes| actual.extend_from_slice(bytes));
        assert_eq!(actual, expected.as_bytes(), "{command}");
        let snapshot = vt.render_snapshot().unwrap();
        assert_eq!(snapshot.images.len(), visible, "{command}");
        let caret = snapshot.cursor.unwrap();
        assert_eq!([caret.column, caret.row], cursor, "{command}");
        if let Some(p) = snapshot.images.first() {
            assert_eq!(&p.image.rgba[..4], &[17, 34, 51, 255]);
            if p.image_id == 7 {
                assert_eq!((p.z, p.cell, p.size), (-1, [0, 0], [16, 32]));
                assert_eq!(&p.image.rgba[4..], &[68, 85, 102, 255]);
            }
        }
    }
}
