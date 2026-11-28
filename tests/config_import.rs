#![forbid(unsafe_code)]
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{self, Command, Output},
};

use sumie_core::{KeyAction, KeyCode, KeyModifiers, PhysicalKey, Platform, Shortcut, Shortcuts};

/// A temporary directory that is also `HOME` and `XDG_CONFIG_HOME` for the command.
struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("sumie-ghostty-{name}-{}", process::id()));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }

    fn sumie_config(&self) -> PathBuf {
        self.0.join("sumie/sumie.toml")
    }

    fn write(&self, path: &str, text: &str) -> PathBuf {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    fn import(&self, source: &Path) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sumie"))
            .args(["config", "import", "ghostty"])
            .arg(source)
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", &self.0)
            .env("LC_ALL", "C")
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn translates_ghostty_config_and_keybinds() {
    let fixture = Fixture::new("map");
    let source = fixture.write(
        "ghostty",
        r#"
# a comment
font-family = "Primary Mono"
font-family = Fallback Mono
font-size = 13.5
background = #aabbcc
foreground = 112233
palette = 1=#ee1122
keybind = ctrl+k=copy_to_clipboard
keybind = ctrl+j=text:not-supported
window-padding-x = 4
"#,
    );
    let summary = stdout(&fixture.import(&source));
    let config = sumie_config::load(&fixture.sumie_config()).unwrap();
    assert_eq!(config.font.family(), "Primary Mono");
    assert_eq!(
        config.font.logical_px().to_bits(),
        (13.5 * sumie_config::POINT_TO_PX).to_bits()
    );
    assert_eq!(config.colors.background(), [0xaa, 0xbb, 0xcc]);
    assert_eq!(config.colors.foreground(), [0x11, 0x22, 0x33]);
    assert_eq!(config.colors.palette()[1], [0xee, 0x11, 0x22]);
    let path = source.display();
    for skipped in [4, 10, 11] {
        assert!(
            summary.contains(&format!("{path}:{skipped}: ")),
            "{summary}"
        );
    }
    assert_eq!(summary.matches("(skipped)").count(), 3, "{summary}");

    let mut shortcuts = Shortcuts::new(Platform::Linux);
    shortcuts.set_bindings(config.keybinds.overrides());
    let ctrl = KeyModifiers {
        control: true,
        ..KeyModifiers::default()
    };
    let key = PhysicalKey::Code(KeyCode::KeyK);
    let matched = shortcuts.match_key(key, Some('k'), ctrl, KeyAction::Press, false, || {
        Ok::<_, ()>(true)
    });
    assert_eq!(matched, Ok(Some(Shortcut::Copy)));
}

#[test]
fn keeps_what_the_sumie_config_already_sets() {
    let fixture = Fixture::new("fill-only");
    let before = "[font]\nsize = 17 # mine\n";
    fixture.write("sumie/sumie.toml", before);
    let source = fixture.write("ghostty", "font-size = 22\nbackground = #010203\n");
    let summary = stdout(&fixture.import(&source));
    assert!(summary.contains(":1: font-size (skipped)"), "{summary}");
    let after = fs::read_to_string(fixture.sumie_config()).unwrap();
    assert!(after.starts_with(before), "{after}");
    assert!(after.contains("background = \"#010203\""), "{after}");
}

#[test]
fn fails_without_a_source() {
    let fixture = Fixture::new("missing");
    let output = fixture.import(&fixture.0.join("nothing"));
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No Ghostty configuration file found"),
        "{stderr}"
    );
    assert!(!fixture.sumie_config().exists());
}
