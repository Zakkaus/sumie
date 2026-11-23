use super::*;

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn load_configuration() {
    type Case<'a> = (&'a str, Result<fn(Config), (usize, usize, &'static str)>);

    let dir = env::temp_dir().join(format!("sumie-config-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let scratch = Scratch(dir);
    let main = scratch.0.join("sumie.toml");
    fs::write(
        scratch.0.join("shared.toml"),
        "minimum-contrast = 7.0\nscrollback-limit = 104857600\n\
         [font]\nfamily = 'Iosevka'\nsize = 12\nweight = 500\n\
         hinting = 'off'\n[colors]\nforeground = '#AB12EF'\nbackground = '#123456'\n",
    )
    .unwrap();

    let defaults = defaults_toml();
    let palette = ["#010203"; 16];
    let colors = format!(
        "include = ['shared.toml']\nminimum-contrast = 1.0\nscrollback-limit = 0\n[colors]\n\
         background = '#fedcba'\ncursor = '#123456'\nselection-foreground = '#abcdef'\n\
         selection-background = '#654321'\npalette = {palette:?}\n"
    );
    let cases: [Case<'_>; 9] = [
        (
            "[font]\nfamily = 'Monospace'\nsize = 18\nweight = 600\nhinting = 'normal'\n",
            Ok(|config| {
                assert_eq!(config.scrollback_limit(), 50_000_000);
                assert_font(config.font, "Monospace", 24.0, 600, Hinting::Normal);
            }),
        ),
        ("[font]\nunknown = true\n", Err((2, 1, "unknown field"))),
        (
            "include = ['shared.toml']\n[font]\nsize = 15\nweight = 700\n",
            Ok(|config| {
                assert_eq!(config.minimum_contrast().to_bits(), 7.0_f32.to_bits());
                assert_eq!(config.scrollback_limit(), 104_857_600);
                assert_font(config.font, "Iosevka", 20.0, 700, Hinting::Off);
            }),
        ),
        (
            "[font]\nhinting = 'strong'\n",
            Err((2, 11, "unknown variant")),
        ),
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
    ];
    for (input, expected) in cases {
        fs::write(&main, input).unwrap();
        let result = load(&main);
        match expected {
            Ok(check) => check(result.unwrap()),
            Err((line, column, reason)) => {
                let error = result.unwrap_err();
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
                assert!(message.contains(reason), "{message}");
            }
        }
    }
}

fn assert_font(font: Font, family: &str, px: f32, weight: u16, hinting: Hinting) {
    assert_eq!(font.logical_px().to_bits(), px.to_bits());
    assert_eq!(font.weight(), Some(weight));
    assert_eq!(font.hinting(), hinting);
    assert_eq!(font.into_family(), family);
}

fn assert_defaults(config: Config) {
    assert_eq!(config.minimum_contrast().to_bits(), 1.1_f32.to_bits());
    assert_eq!(config.colors.foreground(), [0xcc; 3]);
    assert_eq!(config.colors.background(), [0x1e; 3]);
    assert_eq!(config.colors.palette()[1], [0xcd, 0x31, 0x31]);
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
