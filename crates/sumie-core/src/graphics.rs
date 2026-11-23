//! Owned image data shared between a terminal snapshot and its renderer.

use std::sync::Arc;

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
    pub z: i32,
    /// Top-left cell relative to the viewport; either coordinate can be negative.
    pub cell: [i32; 2],
    pub offset: [u32; 2],
    /// Destination size in device pixels.
    pub size: [u32; 2],
    /// Source x, y, width and height in image pixels.
    pub source: [u32; 4],
}
