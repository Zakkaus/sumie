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
mod sources;
#[cfg(target_os = "linux")]
mod view;

#[cfg(all(test, target_os = "linux"))]
mod tests;

#[cfg(target_os = "linux")]
type Error = Box<dyn std::error::Error>;
#[cfg(target_os = "linux")]
type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    use tracing_subscriber::filter::LevelFilter;

    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(LevelFilter::INFO);

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .init();
    let result = capture::Options::parse(std::env::args_os().skip(1)).and_then(app::run);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sumie: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {}
