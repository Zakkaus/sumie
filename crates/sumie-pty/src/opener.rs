use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};

use sumie_core::LinkUri;

/// Start the desktop opener in a separate process group. A detached worker
/// spawns and reaps it; the caller never waits for exec or exit.
///
/// # Errors
/// Returns an error if the worker thread cannot start.
pub fn open_url(uri: LinkUri) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "macos")]
    let program = "open";

    std::thread::Builder::new()
        .name("sumie-link-opener".into())
        .spawn(move || {
            if let Err(error) = launch(program, &uri) {
                tracing::warn!(%error, "desktop opener failed");
            }
        })?;
    Ok(())
}

fn launch(program: &str, uri: &LinkUri) -> io::Result<()> {
    let status = Command::new(program)
        .arg(uri.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(status.to_string()))
    }
}
