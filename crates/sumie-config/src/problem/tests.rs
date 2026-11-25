use std::{fs, io, path::Path};

use crate::{Config, Error, Problem, check, load, tests::Scratch};

fn range(key: &'static str, min: &str, max: &str) -> Problem {
    Problem::Range {
        key,
        min: min.into(),
        max: max.into(),
    }
}

#[test]
fn each_problem_is_typed_located_and_worded_as_before() {
    let scratch = Scratch::new("problem");
    let main = scratch.0.join("sumie.toml");
    let cases = [
        (
            "minimum-contrast = 21.5\n",
            (1, "21.5"),
            range("minimum-contrast", "1.0", "21.0"),
            "minimum contrast must be between 1.0 and 21.0".to_owned(),
        ),
        (
            "[font]\nsize = 0\n",
            (2, "0"),
            Problem::FontSize,
            "font size must be a positive finite number of points".to_owned(),
        ),
        (
            "[font]\nweight = 950\n",
            (2, "950"),
            range("font.weight", "100", "900"),
            "font weight must be between 100 and 900".to_owned(),
        ),
        (
            "[window]\nbackground-opacity = -0.5\n",
            (2, "-0.5"),
            range("window.background-opacity", "0.0", "1.0"),
            "background opacity must be between 0.0 and 1.0".to_owned(),
        ),
        (
            "[colors]\npalette = ['#000000', '#00000g']\n",
            (2, "'#00000g'"),
            Problem::Color {
                key: "colors.palette".into(),
                value: "#00000g".into(),
            },
            r##"invalid value: string "#00000g", expected a color in #rrggbb notation"##.to_owned(),
        ),
        (
            "[cursor]\nshape = 'bar'\n",
            (2, "shape"),
            Problem::UnknownKey {
                key: "cursor.shape".into(),
            },
            "unknown field `shape`, expected `style` or `blink`".to_owned(),
        ),
    ];
    for (text, at, problem, message) in cases {
        assert_only(&main, text, at, &problem, &message);
    }
}

#[test]
fn include_problems_are_typed_and_located() {
    let scratch = Scratch::new("includes");
    let main = scratch.0.join("sumie.toml");
    let cases = [
        (
            "include = ['sumie.toml']\n",
            (1, "'sumie.toml'"),
            Problem::IncludeCycle { path: main.clone() },
            format!("include cycle through {}", main.display()),
        ),
        (
            "include = ['absent.toml']\n",
            (1, "'absent.toml'"),
            Problem::Read {
                path: scratch.0.join("absent.toml"),
                kind: io::ErrorKind::NotFound,
                detail: "No such file or directory (os error 2)".into(),
            },
            format!(
                "{}: No such file or directory (os error 2)",
                scratch.0.join("absent.toml").display()
            ),
        ),
    ];
    for (text, at, problem, message) in cases {
        assert_only(&main, text, at, &problem, &message);
    }
}

/// `text` has exactly `problem` at the first `token` on `line`, worded as `message`.
fn assert_only(
    main: &Path,
    text: &str,
    (line, token): (usize, &str),
    problem: &Problem,
    message: &str,
) {
    fs::write(main, text).unwrap();
    let column = text.lines().nth(line - 1).unwrap().find(token).unwrap() + 1;
    let errors = check(main).unwrap_err();
    assert_eq!(errors.len(), 1, "{text}");
    let display = errors[0].to_string();
    let Error::Invalid {
        path,
        line: actual_line,
        column: actual_column,
        message: actual,
    } = &errors[0]
    else {
        panic!("{display}");
    };
    assert_eq!(
        (path.as_path(), *actual_line, *actual_column),
        (main, line, column),
        "{text}"
    );
    assert_eq!(actual, problem, "{text}");
    assert_eq!(
        display,
        format!("{}:{line}:{column}: {message}", main.display())
    );
}

#[test]
fn check_reports_every_problem_in_a_file() {
    let scratch = Scratch::new("problems");
    let main = scratch.0.join("sumie.toml");
    fs::write(
        &main,
        "minimum-contrast = 30\n[font]\nweight = 50\n[colors]\nforeground = 'red'\n",
    )
    .unwrap();
    let errors = check(&main).unwrap_err();
    let lines: Vec<_> = errors
        .iter()
        .map(|error| match error {
            Error::Invalid { line, message, .. } => (*line, message.key()),
            Error::Read { .. } => panic!("{error}"),
        })
        .collect();
    assert_eq!(
        lines,
        [
            (1, Some("minimum-contrast")),
            (3, Some("font.weight")),
            (5, Some("colors.foreground")),
        ]
    );
    assert_eq!(load(&main).unwrap_err().to_string(), errors[0].to_string());
    // Decoded settings keep the serde-side check and its wording.
    let json = Config::from_json(br#"{"minimum-contrast":30}"#).unwrap_err();
    assert!(
        json.to_string()
            .starts_with("minimum contrast must be between 1.0 and 21.0")
    );
}

#[test]
fn syntax_and_type_errors_keep_the_parser_message() {
    for text in [
        "[font\n",
        "[font]\nsize = 'big'\n",
        "[font]\nhinting = 'strong'\n",
    ] {
        let Err(serde) = toml::from_str::<Config>(text) else {
            panic!("{text}");
        };
        let scratch = Scratch::new("syntax");
        let main = scratch.0.join("sumie.toml");
        fs::write(&main, text).unwrap();
        let Error::Invalid { message, .. } = load(&main).unwrap_err() else {
            panic!("{text}");
        };
        assert_eq!(message, Problem::Syntax(serde.message().into()), "{text}");
    }
}

#[test]
fn unknown_key_wording_matches_serde() {
    for table in [
        "", "font", "cursor", "colors", "images", "shell", "window", "scroll",
    ] {
        let (text, key) = if table.is_empty() {
            ("bogus = 1".to_owned(), "bogus".to_owned())
        } else {
            (format!("[{table}]\nbogus = 1"), format!("{table}.bogus"))
        };
        let serde = toml::from_str::<Config>(&text).unwrap_err();
        assert_eq!(
            Problem::UnknownKey { key }.to_string(),
            serde.message(),
            "{text}"
        );
    }
}
