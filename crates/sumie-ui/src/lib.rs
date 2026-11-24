#![forbid(unsafe_code)]

//! Owns the drawn tab bar, sidebar, command palette, menus, and hints. Does not
//! make business decisions or call Wayland or `AppKit` directly.

mod tabs;
mod tokens;

pub use tabs::{Palette, TabHit, TabLayout, tab_bar_height};
pub use tokens::{IMAGE_VIEWER_MARGIN, LABEL_FAMILY, LABEL_PX, LABEL_WEIGHT};
