use bytemuck::{Pod, Zeroable};
use sumie_core::graphics::Placement;

use crate::{AtlasRect, AtlasUpload};

/// Straight (not premultiplied) sRGB RGBA8; alpha is linear.
pub type Rgba = [u8; 4];

/// An axis-aligned rectangle in integer device pixels, relative to the target's top left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub origin: [i32; 2],
    pub size: [u32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphQuad {
    pub atlas: AtlasRect,
    /// Must have the same size as `atlas` for 1:1 sampling.
    pub dest: PixelRect,
    pub color: Rgba,
    /// Owning cell, `[column, row]`, for contrast and coverage correction.
    pub cell: [u32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    /// Draw behind text and recolor glyphs owned by these cells.
    Block {
        cell: [u32; 2],
        columns: u8,
    },
    Bar,
    Underline,
    Hollow,
    /// Fill the rectangle without affecting glyph colors, for IME preedit.
    Solid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// Cell bounds in device pixels, or the already-sized rectangle for `Solid`.
    pub rect: PixelRect,
    pub color: Rgba,
    pub shape: CursorShape,
}

impl Cursor {
    fn instance_count(self) -> usize {
        let [w, h] = self.rect.size;
        if self.shape == CursorShape::Hollow && w > 2 && h > 2 {
            4
        } else {
            1
        }
    }

    fn write(self, mut write: impl FnMut(Instance)) {
        let [x, y] = self.rect.origin;
        let [w, h] = self.rect.size;
        let stroke = || ((h + 8) / 16).max(1);
        let mut solid =
            |origin, size| write(Instance::solid(PixelRect { origin, size }, self.color));
        match self.shape {
            CursorShape::Bar => solid([x, y], [stroke().min(w), h]),
            CursorShape::Underline => {
                let height = stroke().min(h);
                solid([x, y + (h - height).cast_signed()], [w, height]);
            }
            CursorShape::Hollow if self.instance_count() == 4 => {
                solid([x, y], [w, 1]);
                solid([x, y + h.cast_signed() - 1], [w, 1]);
                solid([x, y + 1], [1, h - 2]);
                solid([x + w.cast_signed() - 1, y + 1], [1, h - 2]);
            }
            CursorShape::Block { .. } | CursorShape::Hollow | CursorShape::Solid => {
                solid([x, y], [w, h]);
            }
        }
    }
}

/// A solid device-pixel rectangle outside the terminal grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
    pub rect: PixelRect,
    pub color: Rgba,
}

/// Chrome is composited last, with its own font size and atlas in the same render pass.
pub struct ChromeLayer<'a> {
    pub fills: &'a [Fill],
    /// Cell ownership does not apply to chrome; all masks use `text_background` for contrast.
    pub glyphs: &'a [GlyphQuad],
    pub text_background: Rgba,
    pub atlas: AtlasUpload<'a>,
}

impl ChromeLayer<'_> {
    pub(crate) fn instance_count(&self) -> u32 {
        u32::try_from(self.fills.len() + self.glyphs.len()).expect("too many chrome instances")
    }
}

/// One complete frame. All positions and sizes are in device pixels except `grid_size`.
pub struct DrawList<'a> {
    /// Number of columns and rows.
    pub grid_size: [u32; 2],
    pub cell_size: [u32; 2],
    pub origin: [i32; 2],
    /// One background per cell, in row-major order.
    pub backgrounds: &'a [Rgba],
    /// Terminal default color; its RGB is used as the opaque base for below-background images.
    pub default_background: Rgba,
    /// One flag per row-major cell. `true` lets below-background images show through;
    /// an empty slice means no holes.
    pub default_backgrounds: &'a [bool],
    pub images: &'a [Placement],
    /// Terminal UI rectangles that images must not cover, in device pixels.
    pub image_exclusions: [Option<PixelRect>; 2],
    pub glyphs: &'a [GlyphQuad],
    pub cursor: Option<Cursor>,
    pub chrome: Option<ChromeLayer<'a>>,
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub(crate) struct Instance {
    origin: [i32; 2],
    size: [u32; 2],
    atlas_origin: [u32; 2],
    color: Rgba,
    background: Rgba,
}

impl Instance {
    pub(crate) const ATTRS: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
        0 => Sint32x2, 1 => Uint32x2, 2 => Uint32x2, 3 => Unorm8x4, 4 => Unorm8x4
    ];

    fn solid(rect: PixelRect, color: Rgba) -> Self {
        Self {
            origin: rect.origin,
            size: rect.size,
            atlas_origin: [u32::MAX, 0],
            color,
            background: [0; 4],
        }
    }

    fn glyph(glyph: &GlyphQuad, background: Rgba) -> Self {
        debug_assert_eq!(glyph.atlas.size, glyph.dest.size);
        Self {
            origin: glyph.dest.origin,
            size: glyph.dest.size,
            atlas_origin: glyph.atlas.origin,
            color: glyph.color,
            background,
        }
    }

    fn cursor_glyph(glyph: &GlyphQuad, text: Rgba, fill: Rgba) -> Self {
        let mut instance = Self::glyph(glyph, fill);
        // Atlas coordinates never use the high bit; the shader uses it to skip contrast correction.
        instance.atlas_origin[0] |= 1 << 31;
        instance.color[..3].copy_from_slice(&text[..3]);
        instance
    }
}

pub(crate) struct GridLayers {
    pub base: Option<PixelRect>,
    pub backgrounds: u32,
    pub text: u32,
    pub end: u32,
}

struct BlockCursor {
    cursor: Cursor,
    columns: std::ops::Range<u32>,
    row: u32,
}

impl BlockCursor {
    fn owns(&self, glyph: &GlyphQuad) -> bool {
        let [column, row] = glyph.cell;
        row == self.row && self.columns.contains(&column)
    }
}

fn instance_writer(bytes: wgpu::WriteOnly<'_, [u8]>) -> impl FnMut(Instance) + '_ {
    let mut out = bytes
        .into_chunks::<{ size_of::<Instance>() }>()
        .0
        .into_iter();
    move |instance| out.next().unwrap().write(bytemuck::cast(instance))
}

impl DrawList<'_> {
    pub(crate) fn grid_instances(&self) -> u32 {
        let [columns, rows] = self.grid_size;
        assert_eq!(self.backgrounds.len(), columns as usize * rows as usize);
        u32::try_from(
            self.backgrounds.len()
                + self.glyphs.len()
                + self.cursor.map_or(0, Cursor::instance_count),
        )
        .expect("too many instances")
    }

    pub(crate) fn write_instances(&self, bytes: wgpu::WriteOnly<'_, [u8]>) {
        let mut write = instance_writer(bytes);
        self.write_grid(&mut write);
        self.write_chrome(write);
    }

    pub(crate) fn image_layers(&self, base: Option<PixelRect>, grid: u32) -> GridLayers {
        let mut backgrounds = self.backgrounds.len();
        if base.is_some() {
            let holes = self.default_backgrounds;
            assert!(holes.is_empty() || holes.len() == backgrounds);
            backgrounds = 1 + backgrounds - holes.iter().filter(|&&hole| hole).count();
        }
        let cursor_glyphs = self.block_cursor().map_or(0, |block| {
            self.glyphs.iter().filter(|glyph| block.owns(glyph)).count()
        });
        let convert = |count| u32::try_from(count).expect("too many grid instances");
        GridLayers {
            base,
            backgrounds: convert(backgrounds),
            text: convert(backgrounds + self.glyphs.len() - cursor_glyphs),
            end: grid - convert(self.backgrounds.len()) + convert(backgrounds),
        }
    }

    pub(crate) fn write_layered(&self, bytes: wgpu::WriteOnly<'_, [u8]>, layers: &GridLayers) {
        let stride = size_of::<Instance>();
        let (text, rest) = bytes.split_at(layers.text as usize * stride);
        let (cursor, chrome) = rest.split_at((layers.end - layers.text) as usize * stride);
        let mut write = instance_writer(text);
        let mut write_cursor = instance_writer(cursor);
        if let Some(base) = layers.base {
            let [r, g, b, _] = self.default_background;
            write(Instance::solid(base, [r, g, b, 255]));
        }
        if layers.base.is_some() && !self.default_backgrounds.is_empty() {
            self.write_backgrounds::<true>(&mut write);
        } else {
            self.write_backgrounds::<false>(&mut write);
        }
        let block = self.block_cursor();
        if let Some(cursor) = self.cursor {
            cursor.write(&mut write_cursor);
        }
        for glyph in self.glyphs {
            let background = self.glyph_background(glyph);
            if let Some(block) = &block
                && block.owns(glyph)
            {
                write_cursor(Instance::cursor_glyph(
                    glyph,
                    background,
                    block.cursor.color,
                ));
            } else {
                write(Instance::glyph(glyph, background));
            }
        }
        self.write_chrome(instance_writer(chrome));
    }

    fn write_chrome(&self, mut write: impl FnMut(Instance)) {
        if let Some(layer) = &self.chrome {
            for fill in layer.fills {
                write(Instance::solid(fill.rect, fill.color));
            }
            for glyph in layer.glyphs {
                write(Instance::glyph(glyph, layer.text_background));
            }
        }
    }

    fn write_backgrounds<const SKIP_DEFAULT: bool>(&self, mut write: impl FnMut(Instance)) {
        let columns = self.grid_size[0] as usize;
        for (row, backgrounds) in self.backgrounds.chunks(columns.max(1)).enumerate() {
            for (col, &color) in backgrounds.iter().enumerate() {
                if SKIP_DEFAULT && self.default_backgrounds[row * columns + col] {
                    continue;
                }
                let offset = [
                    i32::try_from(col * self.cell_size[0] as usize).expect("grid too wide"),
                    i32::try_from(row * self.cell_size[1] as usize).expect("grid too tall"),
                ];
                let rect = PixelRect {
                    origin: [self.origin[0] + offset[0], self.origin[1] + offset[1]],
                    size: self.cell_size,
                };
                write(Instance::solid(rect, color));
            }
        }
    }

    fn block_cursor(&self) -> Option<BlockCursor> {
        let cursor = self.cursor?;
        let CursorShape::Block {
            cell: [col, row],
            columns,
        } = cursor.shape
        else {
            return None;
        };
        Some(BlockCursor {
            cursor,
            columns: col..col + u32::from(columns),
            row,
        })
    }

    fn glyph_background(&self, glyph: &GlyphQuad) -> Rgba {
        let [col, row] = glyph.cell;
        debug_assert!(col < self.grid_size[0] && row < self.grid_size[1]);
        self.backgrounds[row as usize * self.grid_size[0] as usize + col as usize]
    }

    fn write_grid(&self, mut write: impl FnMut(Instance)) {
        self.write_backgrounds::<false>(&mut write);
        let block = self.block_cursor();
        if let Some(block) = &block {
            block.cursor.write(&mut write);
        }
        for glyph in self.glyphs {
            let background = self.glyph_background(glyph);
            if let Some(block) = &block
                && block.owns(glyph)
            {
                write(Instance::cursor_glyph(
                    glyph,
                    background,
                    block.cursor.color,
                ));
            } else {
                write(Instance::glyph(glyph, background));
            }
        }
        if block.is_none()
            && let Some(cursor) = self.cursor
        {
            cursor.write(write);
        }
    }
}
