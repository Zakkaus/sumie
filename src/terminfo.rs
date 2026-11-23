use std::env;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::{fs, fs::File};

pub(super) const NAME: &str = "xterm-sumie";
const SOURCE: &str = include_str!("../terminfo/xterm-sumie.terminfo");
static DIRECTORY: LazyLock<Option<PathBuf>> = LazyLock::new(|| match install() {
    Ok(path) => Some(path),
    Err(error) => {
        tracing::warn!(%error, "bundled terminfo unavailable; using xterm-256color");
        None
    }
});

pub(super) fn directory() -> Option<&'static Path> {
    DIRECTORY.as_deref()
}

fn install() -> io::Result<PathBuf> {
    let data = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::home_dir().map(|home| home.join(".local/share")))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no user data directory"))?;
    let dir = data.join("sumie/terminfo");
    fs::create_dir_all(&dir)?;
    let lock = File::create(dir.join("lock"))?;
    lock.lock()?;

    let stamp = dir.join("source-hash");
    let mut hasher = DefaultHasher::new();
    SOURCE.hash(&mut hasher);
    let hash = hasher.finish().to_be_bytes();
    let entry_exists = || {
        ["x", "78"]
            .iter()
            .any(|prefix| dir.join(prefix).join(NAME).is_file())
    };
    if !entry_exists() || !fs::read(&stamp).is_ok_and(|saved| saved == hash) {
        compile(&dir)?;
        if !entry_exists() {
            return Err(io::Error::other("tic did not install xterm-sumie"));
        }
        fs::write(stamp, hash)?;
    }
    Ok(dir)
}

fn compile(dir: &Path) -> io::Result<()> {
    let mut child = Command::new("tic")
        .args(["-x", "-o"])
        .arg(dir)
        .arg("/dev/stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| io::Error::other(format!("could not start tic: {error}")))?;
    let sent = child.stdin.take().unwrap().write_all(SOURCE.as_bytes());
    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::other(format!("tic failed: {status}")));
    }
    sent
}
