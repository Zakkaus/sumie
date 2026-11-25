//! Watches configuration paths with vnode events. Callers own scheduling and presentation.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            fs::{MetadataExt, OpenOptionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    time::Instant,
};

use nix::{
    errno::Errno,
    fcntl::{FcntlArg, FdFlag, fcntl},
    libc::{O_DIRECTORY, O_EVTONLY},
    sys::{
        event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue},
        time::TimeSpec,
    },
};

use crate::{Config, Error, load_visited};

/// Reason the watcher's wait completed.
#[derive(Debug, PartialEq, Eq)]
pub enum WatchEvent {
    Changed,
    Control,
    Deadline,
}

/// Watches the primary configuration and its includes. Does not spawn a thread.
pub struct Watch {
    queue: Kqueue,
    main: Option<PathBuf>,
    valid: HashSet<PathBuf>,
    vnodes: HashMap<usize, Vnode>,
}

impl Watch {
    /// Create a watch with a control stream that can interrupt `wait`.
    /// Keep the stream open while using the watch. Call `load` before waiting for edits.
    ///
    /// # Errors
    /// Returns an error if the queue or control registration cannot be created.
    pub fn new(main: Option<PathBuf>, control: &UnixStream) -> io::Result<Self> {
        let queue = Kqueue::new()?;
        fcntl(&queue, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        register(
            &queue,
            KEvent::new(
                ident(control.as_raw_fd()),
                EventFilter::EVFILT_READ,
                EvFlags::EV_ADD,
                FilterFlag::empty(),
                0,
                0,
            ),
        )?;
        Ok(Self {
            queue,
            main,
            valid: HashSet::new(),
            vnodes: HashMap::new(),
        })
    }

    /// Read settings and refresh watches, retaining the last valid include set on parse errors.
    ///
    /// # Errors
    /// The outer error is a watch failure; the inner error is a configuration failure.
    pub fn load(&mut self) -> io::Result<Result<Config, Error>> {
        let Some(main) = &self.main else {
            return Ok(Ok(Config::default()));
        };
        let mut paths = HashSet::new();
        let mut scan = Scan {
            queue: &self.queue,
            vnodes: HashMap::new(),
            parents: HashMap::new(),
        };
        let mut watch_result = Ok(());
        let config = load_visited(main, &mut |path| {
            if watch_result.is_ok() && !paths.contains(path) {
                paths.insert(path.to_owned());
                watch_result = scan.watch_path(path);
            }
        });
        watch_result?;
        if config.is_ok() {
            self.valid = paths;
        } else {
            for path in self.valid.difference(&paths) {
                scan.watch_path(path)?;
            }
        }
        // Keep the old registrations alive until the new ones have covered every read.
        self.vnodes = scan.vnodes;
        Ok(config)
    }

    /// Wait on the calling thread for a relevant change, readable control stream, or deadline.
    ///
    /// # Errors
    /// Returns an error if kqueue cannot read notifications.
    pub fn wait(&mut self, deadline: Option<Instant>) -> io::Result<WatchEvent> {
        let mut events = [KEvent::new(
            0,
            EventFilter::EVFILT_VNODE,
            EvFlags::empty(),
            FilterFlag::empty(),
            0,
            0,
        )];
        loop {
            let timeout = deadline
                .map(|at| *TimeSpec::from(at.saturating_duration_since(Instant::now())).as_ref());
            match self.queue.kevent(&[], &mut events, timeout) {
                Ok(0) => return Ok(WatchEvent::Deadline),
                Ok(_) => (),
                Err(Errno::EINTR) => continue,
                Err(error) => return Err(error.into()),
            }
            let event = events[0];
            if event.filter()? == EventFilter::EVFILT_READ {
                return Ok(WatchEvent::Control);
            }
            if self
                .vnodes
                .get_mut(&event.ident())
                .is_some_and(|node| node.changed(event.fflags()))
            {
                return Ok(WatchEvent::Changed);
            }
        }
    }
}

struct Vnode {
    _fd: File,
    children: HashMap<PathBuf, Stamp>,
}

impl Vnode {
    fn changed(&mut self, flags: FilterFlag) -> bool {
        let mut changed =
            self.children.is_empty() || flags.intersects(SELF_CHANGE | FilterFlag::NOTE_ATTRIB);
        for (path, before) in &mut self.children {
            let after = stamp(path);
            changed |= *before != after;
            *before = after;
        }
        changed
    }
}

const SELF_CHANGE: FilterFlag = FilterFlag::NOTE_RENAME
    .union(FilterFlag::NOTE_DELETE)
    .union(FilterFlag::NOTE_REVOKE);

struct Scan<'a> {
    queue: &'a Kqueue,
    vnodes: HashMap<usize, Vnode>,
    parents: HashMap<PathBuf, usize>,
}

impl Scan<'_> {
    fn watch_path(&mut self, path: &Path) -> io::Result<()> {
        self.watch_parent(path)?;
        if let Some(file) = open(path, 0) {
            self.watch(file)?;
        }
        // Watch both directory entries when a symlink resolves outside its lexical parent.
        if let Ok(real) = path.canonicalize()
            && real != path
        {
            self.watch_parent(&real)?;
        }
        Ok(())
    }

    fn watch_parent(&mut self, path: &Path) -> io::Result<()> {
        for child in path.ancestors().filter(|child| child.file_name().is_some()) {
            let Some(parent) = child.parent() else { break };
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            let id = if let Some(&id) = self.parents.get(parent) {
                id
            } else if let Some(directory) = open(parent, O_DIRECTORY) {
                let id = self.watch(directory)?;
                self.parents.insert(parent.to_owned(), id);
                id
            } else {
                continue;
            };
            // Directory vnode events carry no filename; compare only watched entries.
            let children = &mut self.vnodes.get_mut(&id).unwrap().children;
            if !children.contains_key(child) {
                children.insert(child.to_owned(), stamp(child));
            }
            break;
        }
        Ok(())
    }

    fn watch(&mut self, file: File) -> io::Result<usize> {
        let id = ident(file.as_raw_fd());
        register(
            self.queue,
            KEvent::new(
                id,
                EventFilter::EVFILT_VNODE,
                EvFlags::EV_ADD | EvFlags::EV_CLEAR,
                SELF_CHANGE
                    | FilterFlag::NOTE_ATTRIB
                    | FilterFlag::NOTE_LINK
                    | FilterFlag::NOTE_WRITE
                    | FilterFlag::NOTE_EXTEND,
                0,
                0,
            ),
        )?;
        self.vnodes.insert(
            id,
            Vnode {
                _fd: file,
                children: HashMap::new(),
            },
        );
        Ok(id)
    }
}

fn ident(fd: RawFd) -> usize {
    usize::try_from(fd).expect("open descriptor")
}

fn register(queue: &Kqueue, event: KEvent) -> io::Result<()> {
    loop {
        match queue.kevent(&[event], &mut [], None) {
            Ok(_) => return Ok(()),
            Err(Errno::EINTR) => (),
            Err(error) => return Err(error.into()),
        }
    }
}

// A path that cannot be watched (missing, a symlink loop, out of descriptors) is
// treated as absent: loading reports any resulting config error in the banner,
// and the parent directory watch picks the file up when it becomes readable.
fn open(path: &Path, flags: i32) -> Option<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(O_EVTONLY | flags)
        .open(path)
        .ok()
}

type Stamp = Option<(u64, u64, i64, i64)>;

fn stamp(path: &Path) -> Stamp {
    let meta = fs::symlink_metadata(path).ok()?;
    Some((meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()))
}
