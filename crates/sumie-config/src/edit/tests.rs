use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    time::{Duration, Instant},
};

use super::lookup;
use crate::{
    Config, CursorStyle, Refusal, Snapshot, Value, Watch, WriteError, load, save, settings,
    tests::Scratch, winner, write,
};

fn set(main: &Path, key: &str, value: &Value) -> Result<Snapshot, WriteError> {
    let snapshot = Snapshot::read(&winner(main, key)).unwrap();
    save(main, &snapshot, key, Some(value))
}

fn text(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn edit_keeps_comments_and_order() {
    let scratch = Scratch::new("edit-comments");
    let main = scratch.0.join("sumie.toml");
    let shared = scratch.0.join("shared.toml");
    let main_text = "# Main settings\ninclude = ['shared.toml']\n\n[font]\n# Body text\n\
                     size = 13.5   # small\nfamily = 'Iosevka'\n";
    fs::write(&main, main_text).unwrap();
    fs::write(
        &shared,
        "# Shared\n[cursor]\nblink = false # calm\n\n[colors]\nbackground = '#101010'\n",
    )
    .unwrap();

    // Only the file that defines the key changes.
    set(&main, "cursor.blink", &Value::Boolean(true)).unwrap();
    assert_eq!(text(&main), main_text);
    assert_eq!(
        text(&shared),
        "# Shared\n[cursor]\nblink = true # calm\n\n[colors]\nbackground = '#101010'\n"
    );

    set(&main, "font.size", &Value::Float(15.0)).unwrap();
    set(&main, "font.weight", &Value::Integer(500)).unwrap();
    set(&main, "minimum-contrast", &Value::Float(4.5)).unwrap();
    set(&main, "scroll.smooth", &Value::Boolean(false)).unwrap();
    let family = Snapshot::read(&main).unwrap();
    save(&main, &family, "font.family", None).unwrap();
    assert_eq!(
        text(&main),
        "# Main settings\ninclude = ['shared.toml']\nminimum-contrast = 4.5\n\n[font]\n\
         # Body text\nsize = 15.0   # small\nweight = 500\n\n[scroll]\nsmooth = false\n"
    );
    let config = load(&main).unwrap();
    assert!(config.cursor.blink() && !config.scroll.smooth());
    assert_eq!(config.font.weight(), Some(500));
}

#[test]
fn winner_matches_merge() {
    let scratch = Scratch::new("edit-winner");
    let main = scratch.0.join("sumie.toml");
    for (name, body) in [
        (
            "sumie.toml",
            "include = ['a.toml', 'b.toml']\nminimum-contrast = 4.5\n[cursor]\nstyle = 'bar'\n",
        ),
        ("a.toml", "include = ['c.toml']\n[font]\nsize = 18.5\n"),
        (
            "b.toml",
            "language = 'ja'\n[cursor]\nstyle = 'underline'\nblink = false\n\
             [colors]\nbackground = '#202020'\n",
        ),
        (
            "c.toml",
            "[font]\nsize = 11.5\nfamily = 'Iosevka'\n[colors]\nbackground = '#101010'\n\
             foreground = '#eeeeee'\n",
        ),
    ] {
        fs::write(scratch.0.join(name), body).unwrap();
    }
    let merged: serde_json::Value = serde_json::from_str(&load(&main).unwrap().to_json()).unwrap();
    let mut defined = 0;
    for info in settings().iter().filter(|info| info.path != "include") {
        let file = winner(&main, &info.path);
        let table = text(&file).parse().unwrap();
        let expected = lookup(&table, &info.path).map(|value| serde_json::to_value(value).unwrap());
        let pointer = format!("/{}", info.path.replace('.', "/"));
        let actual = merged.pointer(&pointer).filter(|value| !value.is_null());
        assert_eq!(
            actual,
            expected.as_ref(),
            "{} from {}",
            info.path,
            file.display()
        );
        if expected.is_none() {
            assert_eq!(file, main, "{}", info.path);
        }
        defined += usize::from(expected.is_some());
    }
    assert_eq!(defined, 8);
}

#[test]
fn rebase_on_foreign_write() {
    let scratch = Scratch::new("edit-rebase");
    let main = scratch.0.join("sumie.toml");
    fs::write(&main, "[font]\nsize = 13.5\n").unwrap();

    // A save to another key since the snapshot is kept.
    let snapshot = Snapshot::read(&main).unwrap();
    fs::write(&main, "[font]\nsize = 13.5\n[cursor]\nstyle = 'bar'\n").unwrap();
    save(&main, &snapshot, "font.size", Some(&Value::Float(15.0))).unwrap();
    assert_eq!(
        text(&main),
        "[font]\nsize = 15.0\n[cursor]\nstyle = 'bar'\n"
    );

    // A save to the same key is never overwritten.
    let snapshot = Snapshot::read(&main).unwrap();
    fs::write(&main, "[font]\nsize = 20.0\n").unwrap();
    let conflict = save(&main, &snapshot, "font.size", Some(&Value::Float(16.0)));
    assert!(
        matches!(conflict, Err(WriteError::Conflict { .. })),
        "{conflict:?}"
    );
    assert_eq!(text(&main), "[font]\nsize = 20.0\n");

    // A save between staging and the rename restarts the edit on top of it.
    let snapshot = Snapshot::read(&main).unwrap();
    let foreign = main.clone();
    write::BEFORE_COMPARE.set(Some(Box::new(move || {
        fs::write(&foreign, "[font]\nsize = 20.0\n[cursor]\nblink = false\n").unwrap();
    })));
    save(&main, &snapshot, "font.size", Some(&Value::Float(17.0))).unwrap();
    assert_eq!(
        text(&main),
        "[font]\nsize = 17.0\n[cursor]\nblink = false\n"
    );
}

#[test]
fn a_key_set_meanwhile_in_an_overriding_file_is_a_conflict() {
    let scratch = Scratch::new("edit-shadow");
    let main = scratch.0.join("sumie.toml");
    let shared = scratch.0.join("shared.toml");
    fs::write(&main, "include = ['shared.toml']\n").unwrap();
    fs::write(&shared, "[cursor]\nblink = false\n").unwrap();
    let snapshot = Snapshot::read(&winner(&main, "cursor.blink")).unwrap();
    assert_eq!(snapshot.path, shared);
    // The main file now overrides the include, so writing the include would change nothing.
    fs::write(&main, "include = ['shared.toml']\n[cursor]\nblink = true\n").unwrap();
    let shadowed = save(
        &main,
        &snapshot,
        "cursor.blink",
        Some(&Value::Boolean(false)),
    );
    match shadowed {
        Err(WriteError::Conflict { path, .. }) => assert_eq!(path, main),
        other => panic!("{other:?}"),
    }
    assert_eq!(text(&shared), "[cursor]\nblink = false\n");
}

#[test]
fn edits_that_add_problems_are_refused() {
    let scratch = Scratch::new("edit-invalid");
    let main = scratch.0.join("sumie.toml");
    let original = "[font]\nweight = 950\n";
    fs::write(&main, original).unwrap();
    // A new problem, and a value that stays wrong on the edited key, are both refused.
    let added = set(&main, "minimum-contrast", &Value::Float(30.0));
    assert!(matches!(added, Err(WriteError::Invalid(_))), "{added:?}");
    let still = set(&main, "font.weight", &Value::Integer(50));
    assert!(matches!(still, Err(WriteError::Invalid(_))), "{still:?}");
    assert_eq!(text(&main), original);
    // An edit elsewhere leaves the existing problem as it was.
    set(&main, "font.size", &Value::Float(15.0)).unwrap();
    // A file that does not parse suspends every write.
    fs::write(&main, "[font\n").unwrap();
    let suspended = set(&main, "font.size", &Value::Float(16.0));
    assert!(
        matches!(suspended, Err(WriteError::Invalid(_))),
        "{suspended:?}"
    );
    assert_eq!(text(&main), "[font\n");
    // A missing main file is created.
    fs::remove_file(&main).unwrap();
    set(&main, "font.size", &Value::Float(16.0)).unwrap();
    assert_eq!(text(&main), "[font]\nsize = 16.0\n");
}

#[test]
fn unsafe_targets_are_refused_cleanly() {
    let scratch = Scratch::new("edit-refusals");
    let main = scratch.0.join("sumie.toml");
    let original = "[font]\nsize = 13.5\n";
    fs::write(&main, original).unwrap();
    let refused = |result: Result<Snapshot, WriteError>, expected: Refusal| match result {
        Err(WriteError::Refused { reason, .. }) => assert_eq!(reason, expected),
        other => panic!("{other:?}"),
    };

    let twin = scratch.0.join("twin.toml");
    fs::hard_link(&main, &twin).unwrap();
    refused(
        set(&main, "font.size", &Value::Float(15.0)),
        Refusal::HardLink,
    );
    fs::remove_file(&twin).unwrap();

    let mut foreign = Snapshot::read(&main).unwrap();
    foreign.stat.as_mut().unwrap().uid += 1;
    assert!(matches!(
        write::replace(&foreign, b"[font]\n"),
        Err(WriteError::Refused {
            reason: Refusal::Owner,
            ..
        })
    ));
    assert_eq!(text(&main), original);

    let locked = scratch.0.join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("sumie.toml"), original).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    let denied = set(&locked.join("sumie.toml"), "font.size", &Value::Float(15.0));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    refused(denied, Refusal::Permission);

    // Every refusal left the files alone and removed its staged copy.
    assert_eq!(text(&locked.join("sumie.toml")), original);
    let mut names: Vec<_> = fs::read_dir(&scratch.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["locked", "sumie.toml"]);
}

#[test]
fn symlinks_modes_and_watch_survive_writes() {
    let scratch = Scratch::new("edit-links");
    let (dotfiles, config) = (scratch.0.join("dotfiles"), scratch.0.join("config"));
    fs::create_dir(&dotfiles).unwrap();
    fs::create_dir(&config).unwrap();
    for (name, body, mode) in [
        (
            "main.toml",
            "include = ['shared.toml']\n[cursor]\nstyle = 'bar'\n",
            0o600,
        ),
        ("shared.toml", "[cursor]\nblink = false\n", 0o640),
    ] {
        let real = dotfiles.join(name);
        fs::write(&real, body).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(mode)).unwrap();
    }
    let main = config.join("sumie.toml");
    symlink(dotfiles.join("main.toml"), &main).unwrap();
    symlink(dotfiles.join("shared.toml"), config.join("shared.toml")).unwrap();

    let mut watch = Watched::new(&main);
    // Two writes in a row to each file: every write replaces the inode the watch was on.
    for (key, value, style, blink) in [
        ("cursor.style", "underline", CursorStyle::Underline, false),
        ("cursor.style", "block", CursorStyle::Block, false),
        ("cursor.blink", "", CursorStyle::Block, true),
        ("cursor.blink", "", CursorStyle::Block, false),
    ] {
        let value = if value.is_empty() {
            Value::Boolean(blink)
        } else {
            Value::String(value.into())
        };
        set(&main, key, &value).unwrap();
        let reported = watch
            .reports(|config| config.cursor.style() == style && config.cursor.blink() == blink);
        assert!(reported, "{key} = {style:?}, blink {blink}");
    }

    for (link, real, mode) in [
        ("sumie.toml", "main.toml", 0o600),
        ("shared.toml", "shared.toml", 0o640),
    ] {
        assert!(
            fs::symlink_metadata(config.join(link))
                .unwrap()
                .is_symlink(),
            "{link}"
        );
        let metadata = fs::metadata(dotfiles.join(real)).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o7777, mode, "{real}");
    }
    assert_eq!(fs::read_dir(&dotfiles).unwrap().count(), 2);
}

#[test]
fn a_link_to_a_missing_file_stays_a_link() {
    let scratch = Scratch::new("edit-dangling");
    let (dotfiles, config) = (scratch.0.join("dotfiles"), scratch.0.join("config"));
    fs::create_dir(&dotfiles).unwrap();
    fs::create_dir(&config).unwrap();
    let main = config.join("sumie.toml");
    // Two hops, the first relative to the link's own directory.
    symlink("../dotfiles/link.toml", &main).unwrap();
    symlink(dotfiles.join("sumie.toml"), dotfiles.join("link.toml")).unwrap();
    let mut watch = Watched::new(&main);

    set(&main, "font.size", &Value::Float(16.0)).unwrap();
    let expected = 16.0 * crate::POINT_TO_PX;
    assert!(watch.reports(|config| (config.font.logical_px() - expected).abs() < f32::EPSILON));
    assert!(fs::symlink_metadata(&main).unwrap().is_symlink());
    assert!(
        fs::symlink_metadata(dotfiles.join("link.toml"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(text(&dotfiles.join("sumie.toml")), "[font]\nsize = 16.0\n");
}

/// A loaded watch on `main`, waited on the way the platform's event loop does.
struct Watched {
    watch: Watch,
    #[cfg(target_os = "macos")]
    _control: (
        std::os::unix::net::UnixStream,
        std::os::unix::net::UnixStream,
    ),
}

impl Watched {
    #[cfg(target_os = "linux")]
    fn new(main: &Path) -> Self {
        let mut watch = Watch::new(main.to_owned()).unwrap();
        watch.load().unwrap().unwrap();
        Self { watch }
    }

    #[cfg(target_os = "macos")]
    fn new(main: &Path) -> Self {
        let control = std::os::unix::net::UnixStream::pair().unwrap();
        let mut watch = Watch::new(Some(main.to_owned()), &control.1).unwrap();
        watch.load().unwrap().unwrap();
        Self {
            watch,
            _control: control,
        }
    }

    /// Whether a change reported within two seconds loads settings that satisfy `expected`.
    fn reports(&mut self, expected: impl Fn(&Config) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.changed(deadline) && expected(&self.watch.load().unwrap().unwrap()) {
                return true;
            }
        }
        false
    }

    #[cfg(target_os = "linux")]
    fn changed(&mut self, deadline: Instant) -> bool {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};

        let wait = deadline.saturating_duration_since(Instant::now());
        let timeout = Timespec::try_from(wait).unwrap();
        poll(
            &mut [PollFd::new(&self.watch, PollFlags::IN)],
            Some(&timeout),
        )
        .unwrap();
        self.watch.changed().unwrap()
    }

    #[cfg(target_os = "macos")]
    fn changed(&mut self, deadline: Instant) -> bool {
        self.watch.wait(Some(deadline)).unwrap() == crate::WatchEvent::Changed
    }
}
