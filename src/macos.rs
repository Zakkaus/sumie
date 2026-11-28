//! Runs the terminal driver in the `AppKit` window, or, with `macos-ci`, a runner scenario.

use sumie_macos::Platform;

use crate::{Result, capture, frame_window::FrameWindow};

mod driver;
pub(crate) mod login;
mod pasteboard;
mod reload;
#[cfg(feature = "macos-ci")]
mod runner;
mod sources;
mod surface;
mod window;
pub(crate) use pasteboard::{CHILD_ARG as PASTEBOARD_ARG, run_child as run_pasteboard};
pub(crate) use reload::{CHILD_ARG as CONFIG_WATCH_ARG, watch_child as run_config_watch};

impl FrameWindow for Platform {
    fn frame_slot_available(&self) -> bool {
        !self.device_size().contains(&0)
    }

    fn arm_frame_callback(&mut self) {
        self.before_present();
    }
}

pub(crate) fn run(options: capture::Options) -> Result<()> {
    #[cfg(feature = "macos-ci")]
    if let Some(mode) = runner::Mode::from_env()? {
        return runner::run(mode, options);
    }
    driver::run(options)
}
