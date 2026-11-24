#![forbid(unsafe_code)]

//! Owns the drawn tab bar, sidebar, command palette, menus, and hints. Does not
//! make business decisions or call Wayland or `AppKit` directly.

mod palette;
mod tabs;
mod tokens;

pub use palette::{Palette, TabPalette};
pub use tabs::{TabHit, TabLayout, TabMeasurement, tab_bar_height};
pub use tokens::{
    IMAGE_VIEWER_MARGIN, LABEL_FAMILY, LABEL_PX, LABEL_WEIGHT, NUMBER_FAMILY, NUMBER_PX,
};

fn scaled(logical: u32, scale: u16) -> u32 {
    (logical * u32::from(scale) + 60) / 120
}
