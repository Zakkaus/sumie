#![forbid(unsafe_code)]

//! Assembles the crates, parses the command line, and owns the platform traits.
//! Converts settings to core options and passes font atlas data to rendering.
//! Keep business logic in the library crates.

#[cfg(target_os = "linux")]
mod app;
#[cfg(target_os = "linux")]
mod capture;
#[cfg(target_os = "linux")]
mod gpu;
#[cfg(target_os = "linux")]
mod input;
#[cfg(target_os = "linux")]
mod keycodes;
#[cfg(target_os = "linux")]
mod pane;
#[cfg(target_os = "linux")]
mod reload;
#[cfg(target_os = "linux")]
mod sources;
#[cfg(target_os = "linux")]
mod view;

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
    #[cfg(not(target_os = "linux"))]
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

    app::run(capture)
}
