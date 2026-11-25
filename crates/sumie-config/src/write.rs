//! Replace one settings file by rename without losing a save made since it was read.
//!
//! Editors honor no cross-process lock, so a save in the microseconds between the last
//! identity check and the rename can still be lost; that window is documented, not closed.

use std::{
    fs::{self, File, OpenOptions, Permissions},
    io::{self, Write as _},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use crate::WriteError;

/// A settings file as read, with the identity a later write compares against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Path as the loader names it.
    pub path: PathBuf,
    /// Resolved file a write replaces, so symlinks along `path` stay links.
    pub real_path: PathBuf,
    /// Contents; empty when the file does not exist.
    pub bytes: Vec<u8>,
    pub(crate) stat: Option<Stat>,
}

/// Identity and content markers of a file. Any difference means another program wrote it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stat {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
    pub(crate) uid: u32,
    pub(crate) nlink: u64,
    mode: u32,
}

impl Stat {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            len: metadata.len(),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
            uid: metadata.uid(),
            nlink: metadata.nlink(),
            mode: metadata.mode() & 0o7777,
        }
    }
}

impl Snapshot {
    /// Read `path`, which may not exist yet.
    ///
    /// # Errors
    /// Returns an error if the file or its directory cannot be read.
    pub fn read(path: &Path) -> io::Result<Self> {
        let real_path = match fs::canonicalize(path) {
            Ok(real_path) => real_path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => absent(path)?,
            Err(error) => return Err(error),
        };
        let (bytes, stat) = match File::open(&real_path) {
            // Stat before reading: a save during the read then changes the stat a write compares.
            Ok(mut file) => {
                let stat = Stat::of(&file.metadata()?);
                let mut bytes = Vec::new();
                io::Read::read_to_end(&mut file, &mut bytes)?;
                (bytes, Some(stat))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (Vec::new(), None),
            Err(error) => return Err(error),
        };
        Ok(Self {
            path: path.to_owned(),
            real_path,
            bytes,
            stat,
        })
    }
}

/// Where a missing file will be created: its resolved directory, or `path` itself.
fn absent(path: &Path) -> io::Result<PathBuf> {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if parent.exists() => Ok(fs::canonicalize(parent)?.join(name)),
        _ => Ok(path.to_owned()),
    }
}

pub(crate) enum Replaced {
    Done(Snapshot),
    /// The file changed after `current` was read; nothing was written.
    Changed,
}

#[cfg(test)]
thread_local! {
    /// Runs between staging and the identity check, where a foreign save is hardest to see.
    pub(crate) static BEFORE_COMPARE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Stage `text` beside `current.real_path`, then rename it over the file unless the file changed.
pub(crate) fn replace(current: &Snapshot, text: &[u8]) -> Result<Replaced, WriteError> {
    let path = &current.real_path;
    let failed = |source| WriteError::Io {
        path: current.path.clone(),
        source,
    };
    let refused = |reason| WriteError::Refused {
        path: current.path.clone(),
        reason,
    };
    if current.stat.is_some_and(|stat| stat.nlink > 1) {
        return Err(refused(Refusal::HardLink));
    }
    let dir = path
        .parent()
        .ok_or_else(|| failed(io::ErrorKind::InvalidInput.into()))?;
    if current.stat.is_none() {
        fs::create_dir_all(dir).map_err(|error| refusal(error).map_or_else(failed, refused))?;
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let staged = dir.join(format!(".{name}.sumie-{}", std::process::id()));
    let file = create(&staged).map_err(|error| refusal(error).map_or_else(failed, refused))?;
    let discard = |error| {
        let _ = fs::remove_file(&staged);
        error
    };
    stage(file, current.stat, text)
        .map_err(|error| discard(refusal(error).map_or_else(failed, refused)))?;
    // The staged file belongs to the effective user, so a differing owner means the rename
    // would hand the settings to another user.
    let staged_uid = fs::metadata(&staged)
        .map_err(|error| discard(failed(error)))?
        .uid();
    if current.stat.is_some_and(|stat| stat.uid != staged_uid) {
        return Err(discard(refused(Refusal::Owner)));
    }
    #[cfg(test)]
    if let Some(hook) = BEFORE_COMPARE.take() {
        hook();
    }
    if stat(path).map_err(|error| discard(failed(error)))? != current.stat {
        let _ = fs::remove_file(&staged);
        return Ok(Replaced::Changed);
    }
    fs::rename(&staged, path).map_err(|error| discard(failed(error)))?;
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(failed)?;
    let written = Snapshot::read(&current.path).map_err(failed)?;
    if written.bytes != text {
        return Err(WriteError::Verify {
            path: current.path.clone(),
        });
    }
    Ok(Replaced::Done(written))
}

fn create(staged: &Path) -> io::Result<File> {
    let open = || {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o666)
            .open(staged)
    };
    match open() {
        // Only this process uses its own name, so the file is left from an earlier crash.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(staged)?;
            open()
        }
        result => result,
    }
}

fn stage(mut file: File, original: Option<Stat>, text: &[u8]) -> io::Result<()> {
    if let Some(stat) = original {
        file.set_permissions(Permissions::from_mode(stat.mode))?;
    }
    file.write_all(text)?;
    file.sync_all()
}

fn stat(path: &Path) -> io::Result<Option<Stat>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(Stat::of(&metadata))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Why a file is left alone even though the edit itself is valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the file belongs to another user")]
    Owner,
    #[error("the file has other hard links")]
    HardLink,
    #[error("no write permission")]
    Permission,
    #[error("read-only file system")]
    ReadOnly,
}

fn refusal(error: io::Error) -> Result<Refusal, io::Error> {
    match error.kind() {
        io::ErrorKind::PermissionDenied => Ok(Refusal::Permission),
        io::ErrorKind::ReadOnlyFilesystem => Ok(Refusal::ReadOnly),
        _ => Err(error),
    }
}
