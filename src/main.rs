#![forbid(unsafe_code)]

//! Assembles the crates, parses the command line, and owns the platform traits.
//! Converts settings to core options and passes font atlas data to rendering.
//! Keep business logic in the library crates.

#[cfg(target_os = "linux")]
mod app;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod capture;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod frame_window;
#[cfg(target_os = "linux")]
mod gpu;
#[cfg(target_os = "linux")]
mod input;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod pane;
#[cfg(target_os = "linux")]
mod platform;
// Only the readiness traits build on macOS until the terminal driver does.
#[cfg(all(target_os = "macos", any(test, feature = "macos-ci")))]
#[path = "platform/watches.rs"]
mod platform;
#[cfg(target_os = "linux")]
mod reload;
#[cfg(target_os = "linux")]
mod view;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod grid_tests;

#[cfg(all(test, target_os = "linux"))]
mod tests;

type Error = Box<dyn std::error::Error>;
type Result<T, E = Error> = std::result::Result<T, E>;

fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sumie: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    use std::io::Write as _;

    let mut args = std::env::args_os().skip(1).peekable();
    #[cfg(target_os = "linux")]
    if args
        .next_if(|arg| arg == "--internal-link-opener")
        .is_some()
    {
        let uri = args
            .next()
            .filter(|_| args.next().is_none())
            .ok_or("link opener requires one URI")?;
        return run_link_opener(&uri);
    }

    if args.next_if(|arg| arg == "config").is_some() {
        let mut stdout = std::io::stdout().lock();
        return match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
            Some("schema") if args.next().is_none() => {
                Ok(writeln!(stdout, "{}", sumie_config::schema_json()?)?)
            }
            Some("show")
                if args.next().is_some_and(|arg| arg == "--defaults") && args.next().is_none() =>
            {
                Ok(write!(stdout, "{}", sumie_config::defaults_toml())?)
            }
            _ => Err("usage: sumie config {schema | show --defaults}".into()),
        };
    }

    #[cfg(target_os = "linux")]
    return run_app(args);
    // TODO(P1): Watch configuration on macOS with kqueue.
    #[cfg(target_os = "macos")]
    return macos::run(&capture::Options::parse(args)?);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_link_opener(raw: &std::ffi::OsStr) -> Result<()> {
    use std::ffi::CString;

    use nix::errno::Errno;
    use nix::unistd::execv;

    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")?;
    let uri = raw
        .to_str()
        .and_then(|raw| sumie_core::LinkUri::parse(raw, host.trim_end()))
        .ok_or("invalid link URI")?;
    let uri = CString::new(uri.as_str())?;
    let argv = [c"xdg-open", uri.as_c_str()];
    close_link_opener_fds()?;

    // execvp can run a shell for an executable without a recognized format.
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
    let mut lookup_error = Errno::ENOENT;
    for dir in std::env::split_paths(&path) {
        let candidate = CString::new(dir.join("xdg-open").into_os_string().into_encoded_bytes())?;
        match execv(&candidate, &argv) {
            Ok(never) => match never {},
            Err(Errno::ENOENT | Errno::ENOTDIR) => (),
            Err(Errno::EACCES) => lookup_error = Errno::EACCES,
            Err(error) => return Err(error.into()),
        }
    }
    Err(lookup_error.into())
}

#[cfg(target_os = "linux")]
fn close_link_opener_fds() -> Result<()> {
    use std::os::fd::{AsRawFd, RawFd};

    use nix::dir::Dir;
    use nix::fcntl::OFlag;
    use nix::sys::stat::Mode;

    // Only the freshly exec'd opener mode calls this, before it starts threads.
    let mut dir = Dir::open(
        c"/proc/self/fd",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    let dir_fd = dir.as_raw_fd();
    let mut inherited = Vec::new();
    for entry in dir.iter() {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        let fd: RawFd = std::str::from_utf8(name)?.parse()?;
        if fd > 2 && fd != dir_fd {
            inherited.push(fd);
        }
    }
    drop(dir);
    for fd in inherited {
        nix::unistd::close(fd)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_app(args: impl Iterator<Item = std::ffi::OsString>) -> Result<()> {
    use tracing_subscriber::filter::LevelFilter;

    let capture = capture::Options::parse(args)?;
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(LevelFilter::INFO);
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .init();

    linux::run(capture)
}
