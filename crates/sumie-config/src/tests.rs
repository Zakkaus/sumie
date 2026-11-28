use super::*;

use std::{
    env, fs,
    path::{Path, PathBuf},
};

pub(crate) struct Scratch(pub(crate) PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

impl Scratch {
    pub(crate) fn new(name: &str) -> Self {
        let dir = env::temp_dir().join(format!("sumie-{name}-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}

#[cfg(target_os = "linux")]
#[test]
fn fifo_include_is_rejected_without_a_writer() {
    use rustix::fs::{CWD, Mode, mkfifoat};
    use std::{sync::mpsc, thread, time::Duration};

    let scratch = Scratch::new("fifo-include");
    let main = scratch.0.join("sumie.toml");
    let fifo = scratch.0.join("fifo.toml");
    fs::write(&main, "include = ['fifo.toml']\n").unwrap();
    mkfifoat(CWD, &fifo, Mode::RUSR | Mode::WUSR).unwrap();

    let (send, recv) = mpsc::channel();
    thread::spawn(move || send.send(load(&main)).unwrap());
    let error = recv
        .recv_timeout(Duration::from_secs(2))
        .expect("loading a FIFO without a writer blocked")
        .unwrap_err();
    assert!(
        matches!(&error, Error::Invalid { line: 1, column: 12, message: Problem::Read { path, .. }, .. } if *path == fifo),
        "{error}"
    );
}

#[test]
fn oversized_font_sizes_are_rejected() {
    let scratch = Scratch::new("font-size");
    let main = scratch.0.join("sumie.toml");
    fs::write(&main, "[font]\nsize = 200\n").unwrap();
    assert_eq!(
        load(&main).unwrap().font.logical_px().to_bits(),
        (200.0 * POINT_TO_PX).to_bits()
    );

    for size in [4096.0, 200.25] {
        fs::write(&main, format!("[font]\nsize = {size}\n")).unwrap();
        assert!(matches!(
            load(&main),
            Err(Error::Invalid {
                line: 2,
                column: 8,
                message: Problem::FontSize,
                ..
            })
        ));
        let json = format!(r#"{{"font":{{"size":{size}}}}}"#);
        assert!(Config::from_json(json.as_bytes()).is_err());
    }
}

#[test]
fn load_configuration() {
    type Case<'a> = (&'a str, Result<fn(Config), (usize, usize, &'static str)>);

    let scratch = Scratch::new("config");
    let main = scratch.0.join("sumie.toml");
    fs::write(
        scratch.0.join("shared.toml"),
        "minimum-contrast = 7.0\nscrollback-limit = 104857600\n\
         [font]\nfamily = 'Iosevka'\nsize = 12\nweight = 500\n\
         hinting = 'off'\n[colors]\nforeground = '#AB12EF'\nbackground = '#123456'\n\
         [window]\npadding-x = 4\npadding-y = 9\n",
    )
    .unwrap();

    let defaults = defaults_toml();
    let palette = ["#010203"; 16];
    let colors = format!(
        "include = ['shared.toml']\nminimum-contrast = 1.0\nscrollback-limit = 0\n[colors]\n\
         background = '#fedcba'\ncursor = '#123456'\nselection-foreground = '#abcdef'\n\
         selection-background = '#654321'\npalette = {palette:?}\n"
    );
    let cases: [Case<'_>; 14] = [
        (
            "[font]\nfamily = 'Monospace'\nsize = 18\nweight = 600\nhinting = 'normal'\n",
            Ok(|config| {
                assert_eq!(config.scrollback_limit(), 50_000_000);
                assert_font(config.font, "Monospace", 24.0, 600, Hinting::Normal);
            }),
        ),
        ("[font]\nunknown = true\n", Err((2, 1, "unknown field"))),
        (
            "[colors]\nforeground = '#000000'\n",
            Ok(|config| assert_eq!(config.colors.cursor(), Some([0; 3]))),
        ),
        (
            "include = ['shared.toml']\n[font]\nsize = 15\nweight = 700\n",
            Ok(|config| {
                assert_eq!(config.minimum_contrast().to_bits(), 7.0_f32.to_bits());
                assert_eq!(config.scrollback_limit(), 104_857_600);
                assert_font(config.font, "Iosevka", 20.0, 700, Hinting::Off);
            }),
        ),
        (
            "include = ['shared.toml']\n[window]\npadding-x = 0\n",
            Ok(|config| {
                assert_eq!(config.window.padding(), [0, 9]);
                assert!(Config::default().changes(&config).padding);
            }),
        ),
        (
            "[window]\nbackground-opacity = 0.5\nbackground-blur = true\n",
            Ok(|config| assert_background(&config)),
        ),
        (
            "[font]\nhinting = 'strong'\n",
            Err((2, 11, "unknown variant")),
        ),
        ("", Ok(assert_defaults)),
        (&defaults, Ok(assert_defaults)),
        (&colors, Ok(assert_colors)),
        (
            "[colors]\nforeground = '#aa00gg'\n",
            Err((2, 14, "#rrggbb")),
        ),
        ("[colors]\npalette = ['#+00000']\n", Err((2, 12, "#rrggbb"))),
        (
            "minimum-contrast = 21.00000001\n",
            Err((1, 20, "between 1.0 and 21.0")),
        ),
        (
            "[window]\nbackground-opacity = 1.00000001\n",
            Err((2, 22, "between 0.0 and 1.0")),
        ),
    ];
    for (input, expected) in cases {
        fs::write(&main, input).unwrap();
        let result = load(&main);
        let input = input.trim_end();
        match expected {
            Ok(check) => check(result.unwrap()),
            Err(expected) => assert_invalid(result.expect_err(input), &main, expected),
        }
    }
}

fn assert_invalid(error: Error, main: &Path, (line, column, reason): (usize, usize, &str)) {
    let Error::Invalid {
        path,
        line: actual_line,
        column: actual_column,
        message,
    } = error
    else {
        panic!("{error}");
    };
    assert_eq!(path, main);
    assert_eq!((actual_line, actual_column), (line, column), "{message}");
    assert!(message.to_string().contains(reason), "{message}");
}

fn assert_font(font: Font, family: &str, px: f32, weight: u16, hinting: Hinting) {
    assert_eq!(font.logical_px().to_bits(), px.to_bits());
    assert_eq!(font.weight(), Some(weight));
    assert_eq!(font.hinting(), hinting);
    assert_eq!(font.into_family(), family);
}

fn assert_background(config: &Config) {
    assert!(Config::default().changes(config).background);
    assert!(config.window.background_blur());
    assert_eq!(
        config.window.background_opacity().to_bits(),
        0.5_f32.to_bits()
    );
}

fn assert_scroll(config: &Config) {
    assert!(config.scroll.smooth());
}

fn assert_defaults(config: Config) {
    assert_scroll(&config);
    assert_eq!(config.minimum_contrast().to_bits(), 1.1_f32.to_bits());
    assert_eq!(config.window.padding(), [8, 6]);
    assert_eq!(
        config.window.background_opacity().to_bits(),
        1.0_f32.to_bits()
    );
    assert!(!config.window.background_blur());
    assert_eq!(config.colors.foreground(), [0xff; 3]);
    assert_eq!(config.colors.background(), [0x12, 0x27, 0x38]);
    assert_eq!(config.colors.cursor(), Some([0xff; 3]));
    assert_eq!(config.colors.selection_foreground(), Some([0xff; 3]));
    assert_eq!(
        config.colors.selection_background(),
        Some([0x00, 0x50, 0xa4])
    );
    assert_eq!(config.colors.palette()[1], [0xff, 0x62, 0x8c]);
    assert_eq!(config.font.logical_px().to_bits(), 20.0_f32.to_bits());
    assert_eq!(config.font.weight(), None);
    assert_eq!(config.font.hinting(), Hinting::Light);
    assert_eq!(config.font.into_family(), "JetBrains Mono");
}

fn assert_colors(config: Config) {
    assert_eq!(config.minimum_contrast().to_bits(), 1.0_f32.to_bits());
    assert_eq!(config.scrollback_limit(), 0);
    let colors = config.colors;
    assert_eq!(colors.foreground(), [0xab, 0x12, 0xef]);
    assert_eq!(colors.background(), [0xfe, 0xdc, 0xba]);
    assert_eq!(colors.cursor(), Some([0x12, 0x34, 0x56]));
    assert_eq!(colors.selection_foreground(), Some([0xab, 0xcd, 0xef]));
    assert_eq!(colors.selection_background(), Some([0x65, 0x43, 0x21]));
    assert_eq!(colors.palette(), [[1, 2, 3]; 16]);
}

#[cfg(target_os = "macos")]
#[test]
fn resolved_settings_survive_json() {
    let scratch = Scratch(env::temp_dir().join(format!("sumie-json-{}", std::process::id())));
    fs::create_dir(&scratch.0).unwrap();
    let palette = ["#010203"; 16];
    let main = scratch.0.join("sumie.toml");
    fs::write(scratch.0.join("shared.toml"), "[scroll]\nsmooth = false\n").unwrap();
    fs::write(
        &main,
        format!(
            "include = ['shared.toml']\nminimum-contrast = 4.5\nscrollback-limit = 7\n\
             [font]\nfamily = 'Iosevka'\nsize = 12.3\nweight = 500\nhinting = 'off'\n\
             [cursor]\nstyle = 'bar'\nblink = false\n[colors]\nforeground = '#AB12EF'\n\
             background = '#123456'\ncursor = '#fedcba'\nselection-foreground = '#abcdef'\n\
             selection-background = '#654321'\npalette = {palette:?}\n\
             [images]\nthumbnail-rows = 3\n[shell]\nintegration = false\n\
             [window]\npadding-x = 4\npadding-y = 9\nbackground-opacity = 0.9\n\
             background-blur = true\n"
        ),
    )
    .unwrap();
    for config in [load(&main).unwrap(), Config::default()] {
        let json = config.to_json();
        let decoded = Config::from_json(json.as_bytes()).unwrap();
        assert_eq!(format!("{decoded:?}"), format!("{config:?}"), "{json}");
        assert_eq!(config.changes(&decoded), Changes::default());
    }
    assert!(Config::from_json(br#"{"minimum-contrast":30}"#).is_err());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn creating_a_dangling_config_target_reloads_settings() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new("dangling-watch");
    let settings = scratch.0.join("settings");
    fs::create_dir(&settings).unwrap();
    let main = scratch.0.join("sumie.toml");
    symlink("settings/link.toml", &main).unwrap();
    symlink("target.toml", settings.join("link.toml")).unwrap();

    #[cfg(target_os = "linux")]
    let mut watch = Watch::new(main).unwrap();
    #[cfg(target_os = "macos")]
    let (_stop, control) = std::os::unix::net::UnixStream::pair().unwrap();
    #[cfg(target_os = "macos")]
    let mut watch = Watch::new(Some(main), &control).unwrap();
    watch.load().unwrap().unwrap();

    fs::write(settings.join("target.toml"), "[cursor]\nstyle = 'bar'").unwrap();
    #[cfg(target_os = "linux")]
    assert!(watch.changed().unwrap());
    #[cfg(target_os = "macos")]
    {
        use std::time::{Duration, Instant};
        assert_eq!(
            watch
                .wait(Some(Instant::now() + Duration::from_secs(2)))
                .unwrap(),
            WatchEvent::Changed,
        );
    }
    assert_eq!(
        watch.load().unwrap().unwrap().cursor.style(),
        CursorStyle::Bar
    );
}

#[cfg(target_os = "macos")]
#[test]
fn rename_replacement_reloads_and_rearms() {
    use std::{
        os::unix::net::UnixStream,
        time::{Duration, Instant},
    };

    let dir = env::temp_dir().join(format!("sumie-watch-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let scratch = Scratch(dir);
    let main = scratch.0.join("sumie.toml");
    let replacement = scratch.0.join("replacement.toml");
    fs::write(&main, "[cursor]\nstyle = 'block'").unwrap();
    let (_stop, control) = UnixStream::pair().unwrap();
    let mut watch = Watch::new(Some(main.clone()), &control).unwrap();
    assert_eq!(
        watch.load().unwrap().unwrap().cursor.style(),
        CursorStyle::Block
    );

    fs::write(&replacement, "[cursor]\nstyle = 'bar'").unwrap();
    fs::rename(&replacement, &main).unwrap();
    assert_eq!(
        watch
            .wait(Some(Instant::now() + Duration::from_secs(2)))
            .unwrap(),
        WatchEvent::Changed,
    );
    assert_eq!(
        watch.load().unwrap().unwrap().cursor.style(),
        CursorStyle::Bar
    );

    // The new inode must also report in-place writes after the reload.
    fs::write(&main, "[cursor]\nstyle = 'underline'").unwrap();
    assert_eq!(
        watch
            .wait(Some(Instant::now() + Duration::from_secs(2)))
            .unwrap(),
        WatchEvent::Changed,
    );
    assert_eq!(
        watch.load().unwrap().unwrap().cursor.style(),
        CursorStyle::Underline
    );
}
