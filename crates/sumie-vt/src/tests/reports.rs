use crate::Terminal;

#[test]
fn size_and_color_scheme_queries_get_replies() {
    let mut terminal = Terminal::new(80, 24).unwrap();
    terminal.resize_pixels(80, 24, [9, 18]).unwrap();
    terminal
        .set_colors([255; 3], [0x12, 0x34, 0x56], None, &[[0; 3]; 16])
        .unwrap();
    for (output, reply) in [
        (&b"\x1b[14t"[..], &b"\x1b[4;432;720t"[..]),
        (b"\x1b[18t", b"\x1b[8;24;80t"),
        (b"\x1b[?996n", b"\x1b[?997;1n"),
        (b"\x1b]11;#f0f0e0\x07\x1b[?996n", b"\x1b[?997;2n"),
        (b"\x1b]111\x07\x1b[?996n", b"\x1b[?997;1n"),
    ] {
        terminal.write(output);
        let mut replies = Vec::new();
        terminal.drain_replies(|bytes| replies.extend_from_slice(bytes));
        assert_eq!(
            replies.escape_ascii().to_string(),
            reply.escape_ascii().to_string(),
            "{}",
            output.escape_ascii()
        );
    }
    terminal.resize_pixels(40, 10, [9, 18]).unwrap();
    terminal.write(b"\x1b[18t");
    let mut replies = Vec::new();
    terminal.drain_replies(|bytes| replies.extend_from_slice(bytes));
    assert_eq!(replies, b"\x1b[8;10;40t");
}

#[test]
fn size_queries_wait_for_a_cell_size() {
    let mut terminal = Terminal::new(80, 24).unwrap();
    for output in [&b"\x1b[14t"[..], b"\x1b[16t", b"\x1b[18t"] {
        terminal.write(output);
        let mut replies = Vec::new();
        terminal.drain_replies(|bytes| replies.extend_from_slice(bytes));
        assert!(
            replies.is_empty(),
            "{} got {}",
            output.escape_ascii(),
            replies.escape_ascii()
        );
    }
    terminal.resize_pixels(80, 24, [9, 18]).unwrap();
    terminal.write(b"\x1b[16t");
    let mut replies = Vec::new();
    terminal.drain_replies(|bytes| replies.extend_from_slice(bytes));
    assert_eq!(replies, b"\x1b[6;18;9t");
}
