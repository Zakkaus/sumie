//! Astryx 0.7.17; exported by `python3 scripts/astryx-tokens.py`. Do not edit.

use super::Palette;

pub(super) const TAB_HEIGHT: u32 = 32;
pub(super) const PADDING: u32 = 12;
pub(super) const RAIL_GAP: u32 = 4;
pub(super) const RULE: u32 = 1;
pub(super) const INDICATOR: u32 = 2;
pub const IMAGE_VIEWER_MARGIN: u32 = 16;
pub const LABEL_PX: u16 = 14;
pub const LABEL_WEIGHT: u16 = 500;
pub const LABEL_FAMILY: &str = "monospace";

pub(super) const LIGHT: Palette = Palette {
    surface: [241, 244, 247, 255],
    label: [68, 85, 100, 255],
    selected: [10, 19, 23, 255],
    accent: [0, 100, 224, 255],
    rule: [5, 54, 89, 25],
    inactive_pane: [5, 54, 89, 12],
    overlay: [1, 18, 40, 102],
};

pub(super) const DARK: Palette = Palette {
    surface: [17, 17, 18, 255],
    label: [176, 181, 187, 255],
    selected: [223, 226, 229, 255],
    accent: [1, 113, 227, 255],
    rule: [242, 244, 246, 25],
    inactive_pane: [17, 17, 18, 127],
    overlay: [17, 17, 18, 153],
};
