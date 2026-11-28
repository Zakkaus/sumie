use std::{env, fmt, fs, io::Write as _, os::unix::fs::OpenOptionsExt as _, path::PathBuf};

#[derive(Default)]
pub(crate) struct Log(Option<PathBuf>);

impl Log {
    pub(crate) fn from_environment() -> Self {
        let absolute = |name| {
            env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        Self(
            absolute("XDG_STATE_HOME")
                .or_else(|| absolute("HOME").map(|home| home.join(".local/state")))
                .map(|dir| dir.join("sumie/settings.log")),
        )
    }

    pub(crate) fn error(&self, error: &dyn fmt::Debug) {
        let Some(path) = &self.0 else { return };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = fs::OpenOptions::new()
            .mode(0o600)
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{error:?}");
        }
    }
}
