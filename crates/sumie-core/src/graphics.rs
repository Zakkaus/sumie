//! Owned image data shared between a terminal snapshot and its renderer.

use std::sync::Arc;

pub const IMAGE_BUDGET: usize = 320 * 1024 * 1024;

/// Bytes in a single uploadable RGBA8 texture. The VT checks this before copying pixels.
#[must_use]
pub fn texture_bytes([w, h]: [u32; 2], max_dimension: u32) -> Option<usize> {
    if w == 0 || h == 0 || w.max(h) > max_dimension {
        return None;
    }
    let pixels = u64::from(w) * u64::from(h);
    usize::try_from(pixels)
        .ok()
        .filter(|&n| n <= IMAGE_BUDGET / 4)
        .map(|n| n * 4)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    /// Identifies this pixel revision across terminals, screens and resets.
    pub generation: u64,
    pub size: [u32; 2],
    /// Tightly packed, top-down, straight sRGB RGBA8.
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub image: Arc<Image>,
    pub image_id: u32,
    pub placement_id: u32,
    /// Opaque VT identity for this stored placement; zero for synthetic geometry.
    pub storage_key: [u64; 2],
    pub z: i32,
    /// Top-left cell relative to the viewport; either coordinate can be negative.
    pub cell: [i32; 2],
    pub offset: [u32; 2],
    /// Destination size in device pixels.
    pub size: [u32; 2],
    /// Source x, y, width and height in image pixels.
    pub source: [u32; 4],
    /// This placement was reduced to the configured thumbnail row limit.
    pub thumbnail: bool,
}
#[cfg(test)]
mod tests {
    use super::texture_bytes;

    #[test]
    fn rgb_image_must_fit_after_rgba_expansion() {
        assert_eq!(texture_bytes([10_000, 10_000], 16_384), None);
    }
}
