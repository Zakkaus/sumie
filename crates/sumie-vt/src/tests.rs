mod keys;
mod kitty_budget;
mod reports;

use sumie_core::graphics::{ImageSource, PixelFormat};

use super::Terminal;

#[test]
fn snapshot_updates_ansi_yellow_without_changing_screen_contents() {
    let mut terminal = Terminal::new(5, 2).unwrap();
    let mut palette = [[0; 3]; 16];
    palette[1] = [4, 5, 6];
    palette[3] = [1, 2, 3];
    terminal
        .set_colors([255; 3], [0; 3], None, &palette)
        .unwrap();
    terminal.write(b"text");
    let mut expected = terminal.render_snapshot().unwrap();
    assert_eq!(expected.ansi_yellow, palette[3]);
    assert_eq!(expected.ansi_red, palette[1]);
    let contents = terminal.screen_contents().unwrap();

    for (output, yellow) in [
        (b"\x1b]4;3;rgb:ab/cd/ef\x07".as_slice(), [0xab, 0xcd, 0xef]),
        (b"\x1b]104;3\x07".as_slice(), palette[3]),
    ] {
        terminal.write(output);
        expected.ansi_yellow = yellow;
        assert_eq!(terminal.render_snapshot().unwrap(), expected);
        assert!(terminal.screen_contents().unwrap() == contents);
    }
}

#[test]
fn snapshot_carries_the_row_below_a_scrolled_viewport() {
    let mut terminal = Terminal::new(4, 2).unwrap();
    terminal.write(b"a\r\nb\r\nc\r\nd");
    let text = |cells: &[super::Cell]| cells.iter().map(|c| c.text.as_str()).collect::<String>();
    // At the bottom nothing lies below the viewport.
    assert!(terminal.render_snapshot().unwrap().below.is_none());
    for (delta, below, extent) in [(-1, "d", (1, 1)), (-1, "c", (0, 2))] {
        terminal.scroll_rows(delta).unwrap();
        let snapshot = terminal.render_snapshot().unwrap();
        assert_eq!(text(snapshot.below.as_ref().unwrap()).trim(), below);
        assert_eq!(snapshot.rows.len(), 2);
        assert_eq!(terminal.scroll_extent().unwrap(), extent);
    }
}

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
fn focus_reports_only_in_mode_1004() {
    let mut terminal = Terminal::new(5, 2).unwrap();
    for (mode, focused, expected) in [
        ("", true, &b""[..]),
        ("\x1b[?1004h", true, b"\x1b[I"),
        ("", false, b"\x1b[O"),
        ("\x1b[?1004l", false, b""),
    ] {
        terminal.write(mode.as_bytes());
        assert_eq!(terminal.encode_focus(focused).unwrap(), expected);
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
            assert!(!p.thumbnail);
            let pixels = vt.pixels(p).unwrap();
            if p.image_id == 7 {
                assert_eq!(p.image.format, PixelFormat::Rgb);
                assert_eq!((p.z, p.cell, p.size), (-1, [0, 0], [16, 32]));
                assert_eq!(pixels, &[17, 34, 51, 68, 85, 102]);
            } else {
                assert_eq!(p.image.format, PixelFormat::Rgba);
                assert_eq!(pixels, &[17, 34, 51, 255]);
            }
        }
    }
}

#[test]
fn kitty_snapshot_retains_fallback_candidates_within_the_texture_limit() {
    let mut vt = Terminal::new(6, 3).unwrap();
    vt.resize_pixels(6, 3, [8, 16]).unwrap();
    vt.set_image_dimension_limit(1);
    for (command, visible_ids) in [
        ("a=T,f=24,i=8,C=1,s=1,v=1,c=1,r=1,q=2;AAD/", &[8][..]),
        ("a=T,f=24,i=7,C=1,s=1,v=1,c=1,r=1,q=2;/wAA", &[7, 8]),
        ("a=T,f=24,i=10,C=1,s=2,v=1,c=1,r=1,q=2;AAAAAAAA", &[7, 8]),
        ("a=T,f=32,i=9,C=1,s=1,v=1,c=1,r=1,q=2;AAAAAA==", &[7, 8, 9]),
        ("a=d,d=I,i=8,q=2", &[7, 9]),
    ] {
        vt.write(format!("\x1b_G{command}\x1b\\").as_bytes());
        let snapshot = vt.render_snapshot().unwrap();
        assert!(
            snapshot
                .images
                .iter()
                .map(|p| p.image_id)
                .eq(visible_ids.iter().copied()),
            "{command}"
        );
    }
}

const KITTY_RED: &str = "a=T,f=32,i=7,p=2,s=1,v=1,C=1;/wAA/w==";
const KITTY_BLUE_FRAME: &str = "a=f,f=32,i=7,s=1,v=1,z=40;AAD//w==";

#[test]
fn kitty_native_animation_tick_accepts_empty_storage() {
    let mut vt = Terminal::new(6, 3).unwrap();
    assert_eq!(vt.animation_tick(0).unwrap(), None);
}

#[test]
fn kitty_animation_commands_update_current_snapshot_pixels() {
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const YELLOW: [u8; 4] = [255, 255, 0, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    let mut vt = Terminal::new(6, 3).unwrap();
    vt.resize_pixels(6, 3, [8, 16]).unwrap();
    for (command, reply, now, next, pixel) in [
        (KITTY_RED, "i=7,p=2;OK", 1000, None, RED),
        (
            "a=f,f=32,i=7,s=1,v=1,z=-1;AP8A/w==",
            "i=7,r=2;OK",
            1000,
            None,
            RED,
        ),
        (KITTY_BLUE_FRAME, "i=7,r=3;OK", 1000, None, RED),
        ("a=a,i=7,r=1,z=30,s=2", "", 1000, Some(1030), RED),
        (
            "a=f,f=32,i=7,s=1,v=1,z=20,m=1;//8A",
            "",
            1010,
            Some(1030),
            RED,
        ),
        ("", "", 1030, Some(1070), BLUE),
        ("a=f,m=0;/w==", "i=7,r=4;OK", 1030, Some(1070), BLUE),
        ("", "", 1070, Some(1090), YELLOW),
        ("", "", 1090, None, YELLOW),
        ("", "", 1120, None, YELLOW),
        (
            "a=f,f=32,i=7,s=1,v=1,z=25;/////w==",
            "i=7,r=5;OK",
            1140,
            Some(1165),
            WHITE,
        ),
        ("a=c,i=7,r=2,c=5", "i=7;OK", 1141, Some(1165), GREEN),
        ("a=a,i=7,s=3", "", 1165, Some(1195), RED),
        ("a=a,i=7,c=2,s=1", "", 1195, None, GREEN),
        (
            "a=d,d=I,i=7\x1b\\\x1b_Ga=T,f=32,i=7,p=2,s=1,v=1,C=1;AAD//w==",
            "i=7,p=2;OK",
            1195,
            None,
            BLUE,
        ),
    ] {
        if !command.is_empty() {
            vt.write(format!("\x1b_G{command}\x1b\\").as_bytes());
        }
        let mut actual = Vec::new();
        vt.drain_replies(|bytes| actual.extend_from_slice(bytes));
        if reply.is_empty() {
            assert!(actual.is_empty(), "{command} at {now}");
        } else {
            assert_eq!(
                actual,
                format!("\x1b_G{reply}\x1b\\").as_bytes(),
                "{command}"
            );
        }
        let snapshot = vt.render_snapshot().unwrap();
        let (changed, deadline) = vt
            .animation_tick_visible(now, snapshot.images.iter(), None)
            .unwrap();
        assert_eq!(deadline, next, "{command} at {now}");
        let snapshot = if changed {
            vt.render_snapshot().unwrap()
        } else {
            snapshot
        };
        assert_eq!(snapshot.images.len(), 1, "{command} at {now}");
        assert_eq!(
            vt.pixels(&snapshot.images[0]).unwrap(),
            pixel.as_slice(),
            "{command} at {now}"
        );
    }
}

fn two_playing_images() -> (Terminal, super::Snapshot) {
    let mut vt = Terminal::new(6, 3).unwrap();
    vt.resize_pixels(6, 3, [8, 16]).unwrap();
    let red_9 = KITTY_RED.replace("i=7", "i=9");
    let blue_9 = KITTY_BLUE_FRAME.replace("i=7", "i=9");
    vt.write(
        format!(
            "\x1b_G{KITTY_RED}\x1b\\\x1b_G{KITTY_BLUE_FRAME}\x1b\\\
             \x1b[1;2H\x1b_Ga=p,i=7,p=3,C=1\x1b\\\x1b[1;3H\x1b_G{red_9}\x1b\\\x1b_G{blue_9}\x1b\\\
             \x1b_Ga=a,i=7,r=1,z=50,s=3\x1b\\\x1b_Ga=a,i=9,r=1,z=60,s=3\x1b\\"
        )
        .as_bytes(),
    );
    let initial = vt.render_snapshot().unwrap();
    assert_eq!(initial.images.len(), 3);
    assert_eq!(
        vt.animation_tick_visible(100, initial.images.iter(), None)
            .unwrap(),
        (false, Some(150))
    );
    assert_eq!(
        vt.animation_tick_visible(157, initial.images.iter(), None)
            .unwrap(),
        (true, Some(160))
    );
    let snapshot = vt.render_snapshot().unwrap();
    for placement in snapshot.images.iter() {
        let expected = if placement.image_id == 7 {
            [0, 0, 255, 255]
        } else {
            [255, 0, 0, 255]
        };
        assert_eq!(vt.pixels(placement).unwrap(), expected.as_slice());
    }
    (vt, snapshot)
}

#[test]
fn kitty_animation_ticks_viewport_and_offscreen_viewer_only() {
    let (mut vt, snapshot) = two_playing_images();
    let original = snapshot.images.iter().find(|p| p.image_id == 7).unwrap();
    let mut viewer = original.clone();

    vt.write(b"A\r\nB\r\nC\r\nD\r\n\x1b_Ga=T,f=32,i=8,s=1,v=1,C=1;/wAA/w==\x1b\\");
    let viewport = vt.render_snapshot().unwrap();
    assert_eq!(viewport.images.len(), 1);
    assert_eq!(viewport.images[0].image_id, 8);
    assert_eq!(
        vt.animation_tick_visible(190, viewport.images.iter(), None)
            .unwrap(),
        (false, None)
    );
    assert!(snapshot.images.iter().all(|p| vt.has_image_placement(p)));

    for (now, next, pixels) in [(190, 240, [255, 0, 0, 255]), (350, 370, [0, 0, 255, 255])] {
        assert_eq!(
            vt.animation_tick_visible(now, viewport.images.iter(), Some(&viewer))
                .unwrap(),
            (true, Some(next))
        );
        assert!(!vt.has_image_placement(&viewer));
        assert!(vt.refresh_viewer(&mut viewer).unwrap());
        assert_eq!(vt.pixels(&viewer).unwrap(), pixels.as_slice());
    }
    assert!(vt.pixels(original).is_none());

    vt.write(format!("\x1b_G{KITTY_RED}\x1b\\").as_bytes());
    assert!(!vt.refresh_viewer(&mut viewer).unwrap());
}

#[test]
fn kitty_thumbnails_size_and_advance_before_rendering() {
    let pixels = "AAAA".repeat(12 * 80);
    let transmit = format!("\x1b_Ga=t,i=7,f=24,s=12,v=80,q=2;{pixels}\x1b\\");
    for (prefix, animated, rows, args, size, cursor, thumbnail) in [
        (b"".as_slice(), false, 3, "", [7, 48], [1, 2], true),
        (b"", false, 0, "", [12, 80], [2, 4], false),
        (b"", false, 3, ",r=10", [24, 160], [3, 9], false),
        (b"", false, 2, ",c=2", [16, 107], [2, 6], false),
        (b"", false, 2, ",c=3,r=5", [24, 80], [3, 4], false),
        (b"", false, 2, ",h=24", [12, 24], [2, 1], false),
        (b"", false, 2, ",h=32", [12, 32], [2, 1], false),
        (b"", false, 1, ",w=1", [1, 16], [1, 0], true),
        (b"", false, 2, ",C=1", [12, 80], [0, 0], false),
        (b"", false, 1, ",Y=4294967295", [1, 1], [1, 0], true),
        (b"\x1b[?1049h", false, 2, "", [12, 80], [2, 4], false),
        (b"", true, 2, "", [12, 80], [2, 4], false),
    ] {
        let mut vt = Terminal::new(12, 12).unwrap();
        vt.resize_pixels(12, 12, [8, 16]).unwrap();
        vt.write(prefix);
        vt.set_thumbnail_rows(3);
        vt.write(transmit.as_bytes());
        if animated {
            vt.write(format!("\x1b_G{KITTY_BLUE_FRAME}\x1b\\").as_bytes());
        }
        vt.set_thumbnail_rows(rows);
        vt.write(format!("\x1b_Ga=p,i=7,p=2,q=2{args}\x1b\\@").as_bytes());
        let snapshot = vt.render_snapshot().unwrap();
        let [col, row] = cursor;
        assert_eq!(snapshot.rows[row][col].text, "@", "{rows}: {args}");
        assert_eq!(snapshot.images.len(), 1, "{rows}: {args}");
        let placement = &snapshot.images[0];
        assert_eq!(placement.cell, [0, 0], "{rows}: {args}");
        assert_eq!(placement.size, size, "{rows}: {args}");
        assert_eq!(
            placement.offset[1],
            if args.starts_with(",Y=") { 15 } else { 0 }
        );
        assert_eq!(placement.thumbnail, thumbnail, "{rows}: {args}");
        assert_eq!(placement.image.size, [12, 80], "{rows}: {args}");
    }
}

#[test]
fn kitty_thumbnail_keeps_source_crop_and_offsets_after_reload() {
    let pixels = "AAAA".repeat(12 * 80);
    let mut vt = Terminal::new(12, 8).unwrap();
    vt.resize_pixels(12, 8, [8, 16]).unwrap();
    vt.set_thumbnail_rows(2);
    let command = "a=T,i=7,p=2,f=24,s=12,v=80,x=4,y=8,w=8,h=48,X=3,Y=4,q=2";
    vt.write(format!("\x1b_G{command};{pixels}\x1b\\").as_bytes());
    vt.set_thumbnail_rows(0);
    let snapshot = vt.render_snapshot().unwrap();
    let cursor = snapshot.cursor.unwrap();
    assert_eq!([cursor.column, cursor.row], [1, 1]);
    let placement = &snapshot.images[0];
    assert!(placement.thumbnail);
    assert_eq!(placement.size, [5, 28]);
    assert_eq!(placement.offset, [3, 4]);
    assert_eq!(placement.source, [4, 8, 8, 48]);
    assert_eq!(placement.image.size, [12, 80]);
}

#[test]
fn kitty_relative_and_virtual_placements_keep_natural_size() {
    let pixels = "AAAA".repeat(12 * 80);
    let mut vt = Terminal::new(12, 12).unwrap();
    vt.resize_pixels(12, 12, [8, 16]).unwrap();
    vt.set_thumbnail_rows(2);
    let transmit = format!("\x1b_Ga=T,i=7,p=1,f=24,s=12,v=80,C=1,q=2;{pixels}\x1b\\");
    vt.write(transmit.as_bytes());
    vt.write(b"\x1b_Ga=p,i=7,p=2,P=7,Q=1,H=2,q=2\x1b\\");
    vt.write(b"\x1b_Ga=p,i=7,p=3,U=1,q=2\x1b\\");
    // The fifth row would be outside a virtual placement shortened to two rows.
    vt.write("\x1b[38;5;7m\u{10eeee}\u{0312}\u{0305}\x1b[0m@".as_bytes());
    let snapshot = vt.render_snapshot().unwrap();
    assert_eq!(snapshot.rows[0][1].text, "@");
    assert_eq!(snapshot.images.len(), 3);
    assert!(
        snapshot
            .images
            .iter()
            .all(|p| !p.thumbnail && vt.has_image_placement(p))
    );
    let child = snapshot
        .images
        .iter()
        .find(|p| p.placement_id == 2)
        .unwrap();
    assert_eq!((child.cell, child.size), ([2, 0], [12, 80]));
}

const KITTY_TEST_PIXEL: &[u8] = b"\x1b_Ga=T,i=7,p=2,f=24,s=1,v=1,C=1,q=2;AAAA\x1b\\";

#[test]
fn kitty_placement_lifetime_checks_current_screen_and_exact_revisions() {
    for (change, clears) in [
        (b"\x1b_Ga=d,d=i,i=7,p=2,q=2\x1b\\".as_slice(), false),
        (b"\x1b_Ga=p,i=7,p=2,C=1,q=2\x1b\\", false),
        (KITTY_TEST_PIXEL, false),
        (b"\x1b[2J", true),
        (b"\x1bc", true),
        (b"\x1b[?47h", false),
    ] {
        let mut vt = Terminal::new(6, 3).unwrap();
        vt.resize_pixels(6, 3, [8, 16]).unwrap();
        vt.write(KITTY_TEST_PIXEL);
        let snapshot = vt.render_snapshot().unwrap();
        let placement = &snapshot.images[0];
        let epoch = vt.screen_clear_epoch();
        assert!(vt.has_image_placement(placement));
        vt.write(change);
        assert!(!vt.has_image_placement(placement), "{change:?}");
        assert_eq!(epoch != vt.screen_clear_epoch(), clears, "{change:?}");
    }
}

#[test]
fn kitty_placement_lifetime_distinguishes_anonymous_placements() {
    let mut vt = Terminal::new(6, 3).unwrap();
    vt.resize_pixels(6, 3, [8, 16]).unwrap();
    vt.write(b"\x1b_Ga=T,i=7,f=24,s=1,v=1,C=1,q=2;AAAA\x1b\\");
    vt.write(b"\x1b[2G\x1b_Ga=p,i=7,C=1,q=2\x1b\\");
    vt.write(b"\x1b[3G\x1b_Ga=p,i=7,p=1,C=1,q=2\x1b\\");
    let snapshot = vt.render_snapshot().unwrap();
    let placement = |col| snapshot.images.iter().find(|p| p.cell == [col, 0]).unwrap();
    assert!(snapshot.images.iter().all(|p| vt.has_image_placement(p)));
    vt.write(b"\x1b[2G\x1b_Ga=d,d=c,q=2\x1b\\");
    assert!(vt.has_image_placement(placement(0)));
    assert!(!vt.has_image_placement(placement(1)));
    assert!(vt.has_image_placement(placement(2)));
}

#[test]
fn kitty_placement_lifetime_is_independent_of_viewport_and_clear_epoch() {
    let mut vt = Terminal::new(6, 3).unwrap();
    vt.resize_pixels(6, 3, [8, 16]).unwrap();
    vt.write(KITTY_TEST_PIXEL);
    let snapshot = vt.render_snapshot().unwrap();
    let placement = &snapshot.images[0];
    let epoch = vt.screen_clear_epoch();
    vt.write(b"A\r\nB\r\nC\r\nD\r\n");
    assert!(vt.render_snapshot().unwrap().images.is_empty());
    vt.scroll_rows(-100).unwrap();
    assert_eq!(vt.render_snapshot().unwrap().images.len(), 1);
    vt.scroll_bottom().unwrap();
    assert!(vt.has_image_placement(placement));
    assert_eq!(vt.screen_clear_epoch(), epoch);
    vt.write(b"\x1b[2J");
    assert_ne!(vt.screen_clear_epoch(), epoch);
    assert!(vt.has_image_placement(placement));
    let epoch = vt.screen_clear_epoch();
    vt.write(b"\x1b[22J");
    assert_ne!(vt.screen_clear_epoch(), epoch);
    assert!(vt.has_image_placement(placement));
    vt.write(b"\x1b[3J");
    assert!(!vt.has_image_placement(placement));
}
