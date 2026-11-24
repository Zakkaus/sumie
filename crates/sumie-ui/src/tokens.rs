//! Astryx 0.7.17; exported by `python3 scripts/astryx-tokens.py`. Do not edit.

use super::Palette;

pub(super) const TAB_HEIGHT: u32 = 32;
pub(super) const PLUS_WIDTH: u32 = 36;
pub(super) const PADDING_LEFT: u32 = 12;
pub(super) const PADDING_RIGHT: u32 = 6;
pub(super) const ITEM_GAP: u32 = 8;
pub(super) const DIVIDER_INSET: u32 = 8;
pub(super) const RAIL_GAP: u32 = 4;
pub(super) const RULE: u32 = 1;
pub(super) const INDICATOR: u32 = 2;
pub(super) const CLOSE_RADIUS: u32 = 4;
pub(super) const CLOSE_MARK: u32 = 8;
pub(super) const PLUS_MARK: u32 = 12;
pub(super) const DOT_SIZE: u32 = 6;
pub(super) const DOT_PULL: u32 = 2;
pub(super) const THUMBNAIL_CHIP: u32 = 16;
pub(super) const CLOSE_SIZE: u32 = 18;
pub const IMAGE_VIEWER_MARGIN: u32 = 16;
pub const LABEL_PX: u16 = 14;
pub const LABEL_WEIGHT: u16 = 500;
pub const LABEL_FAMILY: &str = "monospace";

pub(super) const LIGHT: Palette = Palette {
    surface: [241, 244, 247, 255],
    label: [68, 85, 100, 255],
    selected: [10, 19, 23, 255],
    accent: [0, 100, 224, 255],
    active_number: [0, 100, 224, 255],
    on_accent: [255, 255, 255, 255],
    rule: [5, 54, 89, 25],
    inactive_pane: [5, 54, 89, 12],
    overlay: [1, 18, 40, 102],
    muted: [146, 157, 166, 255],
    active: [221, 228, 237, 255],
    hover: [231, 234, 237, 255],
    close_hover: [0, 0, 0, 31],
    affordance: [78, 96, 111, 255],
};

pub(super) const DARK: Palette = Palette {
    surface: [17, 17, 18, 255],
    label: [176, 181, 187, 255],
    selected: [223, 226, 229, 255],
    accent: [1, 113, 227, 255],
    active_number: [62, 158, 251, 255],
    on_accent: [255, 255, 255, 255],
    rule: [242, 244, 246, 25],
    inactive_pane: [17, 17, 18, 127],
    overlay: [17, 17, 18, 153],
    muted: [104, 107, 111, 255],
    active: [24, 29, 36, 255],
    hover: [27, 27, 27, 255],
    close_hover: [255, 255, 255, 31],
    affordance: [104, 107, 111, 255],
};
