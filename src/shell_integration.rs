use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const SCRIPTS: &[(&str, &str)] = &[
    (
        "bash/sumie.bash",
        include_str!("../shell-integration/bash/sumie.bash"),
    ),
    (
        "zsh/.zshenv",
        include_str!("../shell-integration/zsh/.zshenv"),
    ),
    ("cwd.sh", include_str!("../shell-integration/cwd.sh")),
    (
        "fish/vendor_conf.d/sumie.fish",
        include_str!("../shell-integration/fish/vendor_conf.d/sumie.fish"),
    ),
];

/// Runs the bash hooks from `PROMPT_COMMAND` on macOS; `bash/sumie.bash` replaces this text.
#[cfg(target_os = "macos")]
const BASH_BOOT: &str = r#". "$SUMIE_BASH_SCRIPT" && eval "$_sumie_start""#;

static DIRECTORY: LazyLock<Option<PathBuf>> = LazyLock::new(|| match install() {
    Ok(dir) => Some(dir),
    Err(error) => {
        tracing::warn!(%error, "could not install shell integration");
        None
    }
});

/// The installed scripts, installing them on the first call.
pub(super) fn directory() -> Option<&'static Path> {
    DIRECTORY.as_deref()
}

fn install() -> io::Result<PathBuf> {
    let dir = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env::home_dir().map(|home| home.join(".local/share")))
        .ok_or_else(|| io::Error::other("no user data directory"))?
        .join("sumie/shell-integration");
    fs::create_dir_all(&dir)?;
    let lock = File::create(dir.join("lock"))?;
    lock.lock()?;
    for &(name, script) in SCRIPTS {
        let dest = dir.join(name);
        if !fs::read(&dest).is_ok_and(|bytes| bytes == script.as_bytes()) {
            fs::create_dir_all(dest.parent().unwrap())?;
            // A shell in another process can source the old file during an upgrade.
            let temp = dir.join("install.tmp");
            fs::write(&temp, script)?;
            fs::rename(temp, dest)?;
        }
    }
    Ok(dir)
}

/// Returns replacement login arguments only when bash needs POSIX startup.
pub(super) fn inject(env: &mut BTreeMap<OsString, OsString>) -> Option<[OsString; 3]> {
    let shell = Path::new(env.get(OsStr::new("SHELL"))?)
        .file_name()?
        .to_str()?;
    if !matches!(shell, "bash" | "zsh" | "fish") {
        return None;
    }
    let dir = directory()?;
    match shell {
        // macOS's bash 3.2 reads ENV only when started as `sh`, so it starts as a plain login
        // shell, reads its own profiles, and runs the hooks from PROMPT_COMMAND.
        #[cfg(target_os = "macos")]
        "bash" => {
            env.insert(
                "SUMIE_BASH_SCRIPT".into(),
                dir.join("bash/sumie.bash").into(),
            );
            let mut command = OsString::from(BASH_BOOT);
            if let Some(requested) = env.remove(OsStr::new("PROMPT_COMMAND")) {
                command.push("\n");
                command.push(requested);
            }
            env.insert("PROMPT_COMMAND".into(), command);
            None
        }
        #[cfg(target_os = "linux")]
        "bash" => {
            save(env, "ENV", "SUMIE_ORIG_ENV");
            // ENV is shell-expanded: keep the path itself out of the expression.
            env.insert("ENV".into(), "${SUMIE_BASH_SCRIPT}".into());
            env.insert(
                "SUMIE_BASH_SCRIPT".into(),
                dir.join("bash/sumie.bash").into(),
            );
            Some(["--login".into(), "--posix".into(), "--noprofile".into()])
        }
        "zsh" => {
            save(env, "ZDOTDIR", "SUMIE_ORIG_ZDOTDIR");
            env.insert("ZDOTDIR".into(), dir.join("zsh").into());
            None
        }
        _ => {
            save(env, "XDG_DATA_DIRS", "SUMIE_ORIG_DATA_DIRS");
            let dirs = env
                .get(OsStr::new("SUMIE_ORIG_DATA_DIRS"))
                .filter(|s| !s.is_empty());
            let mut value = dir.as_os_str().to_owned();
            value.push(":");
            value.push(dirs.map_or(
                OsStr::new("/usr/local/share:/usr/share"),
                OsString::as_os_str,
            ));
            env.insert("XDG_DATA_DIRS".into(), value);
            None
        }
    }
}

fn save(env: &mut BTreeMap<OsString, OsString>, key: &str, saved: &str) {
    if let Some(value) = env.remove(OsStr::new(key)) {
        env.insert(saved.into(), value);
    } else {
        env.remove(OsStr::new(saved));
    }
}
