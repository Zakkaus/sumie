use std::fmt::Write;

use super::*;

#[test]
fn matches_logical_lines() {
    let mut terminal = Terminal::new(5, 2).unwrap();
    terminal.write("0abc界e\u{301}Z\r\n0ABC界e\u{301}z\r\nXY".as_bytes());
    assert_eq!(terminal.viewport_top().unwrap(), 3);
    terminal.reveal_search(0, 0).unwrap();
    terminal.reveal_search(1, 1).unwrap();
    assert_eq!(terminal.viewport_top().unwrap(), 0);
    terminal.write("\r\nAAAAA\r\nx\x1b[3Gz\r\nΟΣ".as_bytes());
    for (query, expected) in [
        ("c界e\u{301}z", &[((0, 3), (1, 3)), ((2, 3), (3, 3))][..]),
        ("C界", &[((2, 3), (3, 1))]),
        ("\u{301}", &[((1, 2), (1, 2)), ((3, 2), (3, 2))]),
        ("z0", &[]),
        (
            "aaa",
            &[((5, 0), (5, 2)), ((5, 1), (5, 3)), ((5, 2), (5, 4))],
        ),
        ("x z", &[((6, 0), (6, 2))]),
        ("z ", &[]),
        ("ος", &[((7, 0), (7, 1))]),
        ("Ος", &[]),
    ] {
        let mut search = Search::new(query);
        while !search.advance(&terminal).unwrap() {}
        let matches: Vec<_> = search.matches().iter().map(|m| (m.start, m.end)).collect();
        assert_eq!(matches, expected, "{query:?}");
    }
    if std::env::var_os("SUMIE_SEARCH_BENCH").is_some() {
        time_scrollback_search();
    }
}

#[test]
fn refresh_during_output() {
    for scenario in [edited_wrap as fn(), continuous_redraw] {
        scenario();
    }
}

fn edited_wrap() {
    let mut terminal = Terminal::new(5, 2).unwrap();
    terminal.write(b"def\r\nabcdeFghijkdef");
    assert_eq!(terminal.viewport_top().unwrap(), 2);
    let mut search = Search::new("def");
    while !search.advance(&terminal).unwrap() {}
    let prefix = SearchMatch {
        start: (0, 0),
        end: (0, 2),
    };
    let wrapped = SearchMatch {
        start: (1, 3),
        end: (2, 0),
    };
    let suffix = SearchMatch {
        start: (3, 1),
        end: (3, 3),
    };
    assert_eq!(search.matches(), &[prefix, wrapped, suffix]);

    terminal.write(b"\x1b[1A\rZ");
    search.refresh(&terminal, 0, 2).unwrap();
    while !search.advance(&terminal).unwrap() {}
    assert_eq!(search.matches(), &[prefix, suffix]);

    terminal.write(b"\rF");
    search.refresh(&terminal, 0, 2).unwrap();
    terminal.write(b"\x1b[2;1H\r\ndef");
    search.refresh(&terminal, 0, 2).unwrap();
    let appended = SearchMatch {
        start: (4, 0),
        end: (4, 2),
    };
    while !search.advance(&terminal).unwrap() {}
    assert_eq!(search.matches(), &[prefix, wrapped, suffix, appended]);

    terminal.write(b"\x1b[3J");
    assert_eq!(terminal.viewport_top().unwrap(), 0);
    search.refresh(&terminal, 3, 2).unwrap();
    assert!(search.advance(&terminal).unwrap());
    assert_eq!(
        search.matches(),
        &[
            SearchMatch {
                start: (0, 1),
                end: (0, 3),
            },
            SearchMatch {
                start: (1, 0),
                end: (1, 2),
            },
        ]
    );
}

fn continuous_redraw() {
    let mut vt = Terminal::new(400, 200).unwrap();
    let mut screen = vec![b'a'; 400 * 200 - 6];
    screen.extend_from_slice(b"unique");
    vt.write(&screen);
    let last = SearchMatch {
        start: (199, 394),
        end: (199, 399),
    };
    let first = SearchMatch {
        start: (0, 0),
        end: (0, 5),
    };
    let mut search = Search::new("unique");
    // Force slice boundaries regardless of CPU speed, then allow a normal
    // slice to repair the six changing cells while output is still arriving.
    for i in 0..400 {
        vt.write(if i % 2 == 0 {
            b"\x1b[Hunique"
        } else {
            b"\x1b[Haaaaaa"
        });
        search.refresh(&vt, 0, 0).unwrap();
        if search.matches().contains(&last) {
            break;
        }
        search.advance_until(&vt, Instant::now()).unwrap();
    }
    assert!(search.matches().contains(&last));
    for (text, expected) in [(b"unique", &[first, last][..]), (b"aaaaaa", &[last][..])] {
        vt.write(b"\x1b[H");
        vt.write(text);
        search.refresh(&vt, 0, 0).unwrap();
        assert!(
            search
                .advance_until(&vt, Instant::now() + Duration::from_secs(60))
                .unwrap()
        );
        assert_eq!(search.matches(), expected);
    }
}

fn time_scrollback_search() {
    let mut terminal = Terminal::new(80, 24).unwrap();
    terminal.set_scrollback_bytes(usize::MAX).unwrap();
    let mut text = String::new();
    for line in 1..=100_000 {
        write!(text, "{line}\r\n").unwrap();
    }
    terminal.write(text.as_bytes());
    assert!(terminal.viewport_top().unwrap() > 99_000);
    for (phase, query, expected) in [
        ("pass", "9999", 20),
        ("keystroke", "9", 50_000),
        ("keystroke", "99", 4_000),
        ("keystroke", "999", 300),
        ("keystroke", "9999", 20),
    ] {
        let start = Instant::now();
        let mut search = Search::new(query);
        let mut longest = Duration::ZERO;
        let mut chunks = 0;
        loop {
            let start = Instant::now();
            let done = search.advance(&terminal).unwrap();
            longest = longest.max(start.elapsed());
            chunks += 1;
            if done {
                break;
            }
        }
        eprintln!(
            "100k {phase} {query:?}: {:?} total, {longest:?} max chunk ({chunks} chunks, {} matches)",
            start.elapsed(),
            search.matches().len()
        );
        assert_eq!(search.matches().len(), expected);
    }
}
