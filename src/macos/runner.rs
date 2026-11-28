//! The `macos-ci` runner: the terminal driver or the wake scenario under an external
//! controller.

use std::{env, ops::ControlFlow};

use sumie_macos::{Event, Platform, runner as native};

use crate::{Result, capture};

mod terminal;
mod wake;

/// The wake scenario's window size in points.
const WAKE_SIZE: [u32; 2] = [480, 180];

#[derive(Clone, Copy)]
pub(super) enum Mode {
    /// A pipe write reaches the handler while `AppKit` runs its own loops.
    Wake,
    /// The terminal driver, with typed text, counters and one line per present.
    Terminal,
}

impl Mode {
    pub(super) fn from_env() -> Result<Option<Self>> {
        match env::var("SUMIE_MACOS_CI_RUNNER") {
            Ok(value) if value == "wake" => Ok(Some(Self::Wake)),
            Ok(value) if value == "terminal" => Ok(Some(Self::Terminal)),
            Err(env::VarError::NotPresent) => Ok(None),
            _ => Err("invalid SUMIE_MACOS_CI_RUNNER mode".into()),
        }
    }
}

pub(super) fn run(mode: Mode, options: capture::Options) -> Result<()> {
    if options.frames.is_some() || options.screenshot.is_some() {
        return Err("the runner cannot be combined with frame limits or a screenshot".into());
    }
    match mode {
        Mode::Terminal => terminal::run(options),
        Mode::Wake => run_wake(),
    }
}

/// Run the wake scenario in its own window until its legs are reported.
fn run_wake() -> Result<()> {
    let mut wake = None;
    native::run(|context, readiness, event| {
        match (&mut wake, event) {
            (None, Event::Resumed) => {
                let window = Platform::create(context, "sumie", WAKE_SIZE)?;
                native::name_window(&window);
                wake = Some(wake::Wake::start(context, readiness, window)?);
            }
            (Some(wake), Event::Ready) => return wake.ready(),
            (_, Event::CloseRequested | Event::Quit(_)) => {
                return Err("unexpected native close or Quit".into());
            }
            _ => (),
        }
        Ok(ControlFlow::Continue(()))
    })?;
    println!("runner dropped");
    Ok(())
}
