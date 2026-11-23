use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    mem::MaybeUninit,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::{Path, PathBuf},
};

use rustix::{
    fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags},
    io::{self, Errno},
};

use crate::{Config, Error, load_visited};

type Targets = HashMap<i32, HashSet<OsString>>;

/// Nonblocking Linux watch for the primary configuration and its includes.
/// Poll its descriptor on the caller's event loop; this type spawns no thread.
pub struct Watch {
    fd: OwnedFd,
    main: PathBuf,
    valid: HashSet<PathBuf>,
    targets: Targets,
}

impl AsFd for Watch {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl Watch {
    /// Create the descriptor; call `load` to register paths before reading them.
    ///
    /// # Errors
    /// Returns an error if inotify cannot allocate the descriptor.
    pub fn new(main: PathBuf) -> io::Result<Self> {
        Ok(Self {
            fd: inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)?,
            main,
            valid: HashSet::new(),
            targets: Targets::new(),
        })
    }

    /// Read settings and refresh watches, retaining the last valid include set on parse errors.
    ///
    /// # Errors
    /// The outer error is an inotify failure; the inner error is a configuration failure.
    pub fn load(&mut self) -> io::Result<Result<Config, Error>> {
        let mut paths = HashSet::new();
        let mut targets = Targets::new();
        let mut watch_result = Ok(());
        let config = load_visited(&self.main, &mut |path| {
            if watch_result.is_ok() && !paths.contains(path) {
                paths.insert(path.to_owned());
                watch_result = watch_path(&self.fd, &mut targets, path);
            }
        });
        watch_result?;
        if config.is_ok() {
            self.valid = paths;
        } else {
            for path in self.valid.difference(&paths) {
                watch_path(&self.fd, &mut targets, path)?;
            }
        }
        for &wd in self.targets.keys().filter(|wd| !targets.contains_key(wd)) {
            match inotify::remove_watch(&self.fd, wd) {
                Ok(()) | Err(Errno::INVAL) => (), // The kernel already removed a replaced inode.
                Err(error) => return Err(error),
            }
        }
        self.targets = targets;
        Ok(config)
    }

    /// Drain queued notifications and indicate whether any can change the loaded settings.
    ///
    /// # Errors
    /// Returns an error if inotify cannot be read.
    pub fn changed(&self) -> io::Result<bool> {
        let mut buf = [MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.fd, &mut buf);
        let mut changed = false;
        loop {
            match reader.next() {
                Ok(event) => {
                    let flags = event.events();
                    changed |= flags.contains(ReadFlags::QUEUE_OVERFLOW)
                        || (!flags.contains(ReadFlags::IGNORED)
                            && self.targets.get(&event.wd()).is_some_and(|names| {
                                event.file_name().is_none_or(|name| {
                                    names.contains(std::ffi::OsStr::from_bytes(name.to_bytes()))
                                })
                            }));
                }
                Err(Errno::INTR) => (),
                Err(Errno::AGAIN) => return Ok(changed),
                Err(error) => return Err(error),
            }
        }
    }
}

fn watch_path(fd: &OwnedFd, targets: &mut Targets, path: &Path) -> io::Result<()> {
    watch_parent(fd, targets, path)?;
    let flags = WatchFlags::CLOSE_WRITE
        | WatchFlags::ATTRIB
        | WatchFlags::DELETE_SELF
        | WatchFlags::MOVE_SELF
        | WatchFlags::MASK_ADD;
    if let Some(wd) = add_watch(fd, path, flags)? {
        targets.entry(wd).or_default();
    }
    // The lexical parent catches replacement of the link; the resolved parent catches replacement
    // of its target, which inotify otherwise watches by inode.
    if let Ok(resolved) = path.canonicalize()
        && resolved != path
    {
        watch_parent(fd, targets, &resolved)?;
    }
    Ok(())
}

fn watch_parent(fd: &OwnedFd, targets: &mut Targets, path: &Path) -> io::Result<()> {
    let flags = WatchFlags::CLOSE_WRITE
        | WatchFlags::ATTRIB
        | WatchFlags::CREATE
        | WatchFlags::DELETE
        | WatchFlags::DELETE_SELF
        | WatchFlags::MOVE_SELF
        | WatchFlags::MOVE
        | WatchFlags::ONLYDIR
        | WatchFlags::MASK_ADD;
    // The config directory may not exist until the user saves their first file.
    for child in path.ancestors() {
        if let Some((parent, name)) = child.parent().zip(child.file_name())
            && let Some(wd) = add_watch(fd, parent, flags)?
        {
            targets.entry(wd).or_default().insert(name.to_owned());
            break;
        }
    }
    Ok(())
}

fn add_watch(fd: &OwnedFd, path: &Path, flags: WatchFlags) -> io::Result<Option<i32>> {
    match inotify::add_watch(fd, path, flags) {
        Ok(wd) => Ok(Some(wd)),
        Err(Errno::NOENT | Errno::NOTDIR | Errno::ACCESS) => Ok(None),
        Err(error) => Err(error),
    }
}
