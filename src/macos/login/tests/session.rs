//! Drives a real login shell on a PTY from a test process that owns its whole environment.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use sumie_pty::{Pty, Size, SpawnOptions};

use crate::login::Environment;

const SCENARIO: &str = "SUMIE_LOGIN_SCENARIO";
const WAIT: Duration = Duration::from_secs(10);
/// A scenario still running after this has hung.
const SCENARIO_WAIT: Duration = Duration::from_secs(60);
const SIZE: Size = Size {
    cols: 120,
    rows: 40,
    width_px: 0,
    height_px: 0,
};

/// The scenario's home directory. The test process that starts the scenario removes it.
pub(super) struct Home {
    pub(super) path: PathBuf,
    owned: bool,
}

impl Home {
    pub(super) fn write(&self, name: &str, text: &str) {
        let path = self.path.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    pub(super) fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.path.join(name)).unwrap_or_default()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Run `module::test` alone in a fresh test process whose environment is exactly `env` (a
/// `{home}` in a value names the scratch home), so the scenario owns its locale, `SHELL`,
/// data directory and descriptor table. Returns the home inside that process, where the
/// caller runs the scenario, and `None` in the caller's own process.
pub(super) fn isolated(module: &str, test: &str, env: &[(&str, &str)]) -> Option<Home> {
    if std::env::var_os(SCENARIO).is_some() {
        let path = std::env::home_dir().unwrap();
        return Some(Home { path, owned: false });
    }
    let home = Home {
        path: std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("sumie-login-{}-{test}", std::process::id())),
        owned: true,
    };
    let _ = std::fs::remove_dir_all(&home.path);
    std::fs::create_dir_all(home.path.join("data")).unwrap();
    let root = home.path.to_str().unwrap();
    // Test names omit the crate.
    let name = format!("{}::{test}", module.split_once("::").unwrap().1);
    let log = home.path.join("scenario.log");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([name.as_str(), "--exact", "--test-threads=1"])
        .env_clear()
        .env(SCENARIO, "1")
        .env("HOME", root)
        .env("XDG_DATA_HOME", home.path.join("data"))
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("BASH_SILENCE_DEPRECATION_WARNING", "1")
        .envs(
            env.iter()
                .map(|&(key, value)| (key, value.replace("{home}", root))),
        )
        .stdout(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + SCENARIO_WAIT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = home.read("scenario.log");
    assert!(
        status.is_some_and(|status| status.success()) && output.contains("1 passed"),
        "{name} failed in its own process ({status:?}; None if it hung):\n{output}"
    );
    None
}

/// A login shell started the way a pane starts one, with all of its output so far.
pub(super) struct Session {
    pub(super) pty: Pty,
    pub(super) output: Vec<u8>,
    cursor: usize,
}

impl Session {
    pub(super) fn login(home: &Home) -> Self {
        let login = Environment::new(std::env::vars_os()).unwrap().login(true);
        let mut options = SpawnOptions::new(login.command(), &login.env, SIZE);
        options.cwd = Some(&home.path);
        Self {
            pty: Pty::spawn(options).unwrap(),
            output: Vec::new(),
            cursor: 0,
        }
    }

    pub(super) fn send(&mut self, text: &str) {
        self.pty.enqueue_input(text.as_bytes()).unwrap();
        while self.pty.input_fd().is_some() {
            assert!(self.pty.flush_input().unwrap(), "the shell closed its PTY");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Wait for `needle` after the previous match and return the output skipped to reach it.
    pub(super) fn until(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(at) = find(&self.output[self.cursor..], needle.as_bytes()) {
                let skipped = &self.output[self.cursor..self.cursor + at];
                let skipped = String::from_utf8_lossy(skipped).into_owned();
                self.cursor += at + needle.len();
                return skipped;
            }
            assert!(
                Instant::now() < deadline,
                "no {needle:?} after {:?}",
                String::from_utf8_lossy(&self.output[self.cursor..])
            );
            std::thread::sleep(Duration::from_millis(5));
            self.pty.clear_output_wake().unwrap();
            while let Ok(chunk) = self.pty.try_recv_output() {
                self.output.extend(chunk);
            }
        }
    }

    pub(super) fn all(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The OSC 7 host the shell hooks send: the native hostname, limited to URI-safe bytes.
pub(super) fn osc7_host() -> String {
    let output = Command::new("/bin/hostname").output().unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        .collect()
}

/// Descriptor identities from `lsof -F ftdDi` output: the kernel object for a pipe or socket,
/// the device and inode for a file or terminal. Kqueues have none and are listed by number.
#[derive(Default)]
pub(super) struct Descriptors {
    pub(super) identities: BTreeMap<u32, String>,
    pub(super) kqueues: BTreeSet<u32>,
}

impl Descriptors {
    pub(super) fn of(pid: u32) -> Self {
        let output = Command::new("/usr/sbin/lsof")
            .args(["-nP", "-F", "ftdDi", "-p", &pid.to_string()])
            .output()
            .unwrap();
        Self::parse(&String::from_utf8(output.stdout).unwrap())
    }

    pub(super) fn parse(text: &str) -> Self {
        let mut descriptors = Self::default();
        let mut fields: Vec<(char, &str)> = Vec::new();
        for line in text.lines().chain(["f"]) {
            let Some(field) = line.chars().next() else {
                continue;
            };
            if field == 'f' {
                descriptors.add(&fields);
                fields.clear();
            }
            fields.push((field, &line[1..]));
        }
        descriptors
    }

    fn add(&mut self, fields: &[(char, &str)]) {
        let value = |name| fields.iter().find(|(field, _)| *field == name).map(|f| f.1);
        let Some(fd) = value('f').and_then(|fd| fd.parse().ok()) else {
            return;
        };
        let kind = value('t').unwrap_or_default();
        if kind == "KQUEUE" {
            self.kqueues.insert(fd);
        } else if let Some(object) = value('d') {
            self.identities.insert(fd, format!("{kind} {object}"));
        } else if let (Some(device), Some(inode)) = (value('D'), value('i')) {
            self.identities
                .insert(fd, format!("{kind} {device} {inode}"));
        }
    }
}
