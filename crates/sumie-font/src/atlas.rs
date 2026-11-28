use std::collections::HashMap;

use zeno::Placement;

use crate::{FontError, GlyphKey};

/// Pixel rectangle in the atlas, with its origin at the top left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtlasGlyph {
    /// Empty for a glyph with no ink, such as a space.
    pub rect: Rect,
    /// Mask offset from the shaped glyph origin, in pixels right and down.
    pub left: i32,
    pub top: i32,
    pub color: bool,
}

/// Tightly packed R8 coverage or premultiplied sRGB RGBA8. Rectangles survive growth.
pub struct Atlas {
    side: u32,
    channels: usize,
    pixels: Vec<u8>,
    entries: HashMap<GlyphKey, AtlasGlyph>,
    dirty: Vec<Rect>,
    resized: bool,
    x: u32,
    y: u32,
    shelf_height: u32,
}

impl Atlas {
    pub(crate) fn new(side: u32, channels: usize) -> Self {
        Self {
            side,
            channels,
            pixels: vec![0; (side as usize).pow(2) * channels],
            entries: HashMap::new(),
            // A replacement atlas can reuse a GPU texture with the same dimensions.
            dirty: vec![Rect {
                x: 0,
                y: 0,
                width: side,
                height: side,
            }],
            resized: true,
            x: 0,
            y: 0,
            shelf_height: 0,
        }
    }

    #[must_use]
    pub const fn dimensions(&self) -> [u32; 2] {
        [self.side, self.side]
    }

    /// Row-major pixels with no row padding.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Regions changed since acknowledgement. If dimensions changed, upload the full atlas.
    #[must_use]
    pub fn dirty_regions(&self) -> &[Rect] {
        &self.dirty
    }

    #[must_use]
    pub fn get(&self, key: GlyphKey) -> Option<AtlasGlyph> {
        self.entries.get(&key).copied()
    }

    pub(crate) fn clear_dirty_regions(&mut self) {
        self.dirty.clear();
        self.resized = false;
    }

    pub(crate) fn insert_blank(&mut self, key: GlyphKey) -> AtlasGlyph {
        let entry = AtlasGlyph::default();
        self.entries.insert(key, entry);
        entry
    }

    pub(crate) fn insert(
        &mut self,
        key: GlyphKey,
        placement: Placement,
        draw: impl FnOnce(&mut [u8], usize),
    ) -> Result<AtlasGlyph, FontError> {
        let Placement {
            left,
            top,
            width,
            height,
        } = placement;
        let (x, y, shelf_height) = if width <= self.side - self.x {
            (self.x, self.y, self.shelf_height)
        } else {
            (0, self.y + self.shelf_height, 0)
        };
        let required = width.max(y.checked_add(height).ok_or(FontError::AtlasFull)?);
        if required > self.side {
            self.grow(required)?;
        }
        self.x = x + width;
        self.y = y;
        self.shelf_height = shelf_height.max(height);
        let stride = self.side as usize * self.channels;
        let offset = y as usize * stride + x as usize * self.channels;
        draw(&mut self.pixels[offset..], stride);

        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        let entry = AtlasGlyph {
            rect,
            left,
            top,
            color: self.channels == 4,
        };
        self.entries.insert(key, entry);
        if !self.resized {
            self.dirty.push(rect);
        }
        Ok(entry)
    }

    fn grow(&mut self, required: u32) -> Result<(), FontError> {
        let side = required
            .checked_next_power_of_two()
            .ok_or(FontError::AtlasFull)?;
        let stride = (side as usize)
            .checked_mul(self.channels)
            .ok_or(FontError::AtlasFull)?;
        let len = stride
            .checked_mul(side as usize)
            .ok_or(FontError::AtlasFull)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(len)
            .map_err(|_| FontError::AtlasFull)?;
        pixels.resize(len, 0);
        let old_stride = self.side as usize * self.channels;
        for (src, dst) in self
            .pixels
            .chunks_exact(old_stride)
            .zip(pixels.chunks_exact_mut(stride))
            .take((self.y + self.shelf_height) as usize)
        {
            dst[..old_stride].copy_from_slice(src);
        }
        self.side = side;
        self.pixels = pixels;
        self.dirty.clear();
        self.dirty.push(Rect {
            x: 0,
            y: 0,
            width: side,
            height: side,
        });
        self.resized = true;
        Ok(())
    }
}
