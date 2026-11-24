use bytemuck::{Pod, Zeroable};
use sumie_core::graphics::{ImageSource, Placement};
use sumie_core::splits::Rect;

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

/// A solid device-pixel rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
    pub rect: PixelRect,
    pub color: Rgba,
}

/// Chrome is composited last, with its own font size and atlas in the same render pass.
#[derive(Clone, Copy)]
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

/// One terminal grid. All positions and sizes are in device pixels except `grid_size`.
#[derive(Clone, Copy)]
pub struct DrawList<'a> {
    /// Number of columns and rows.
    pub grid_size: [u32; 2],
    pub cell_size: [u32; 2],
    pub origin: [i32; 2],
    /// One background per cell, in row-major order.
    pub backgrounds: &'a [Rgba],
    /// Terminal default color for pane padding and below-background images.
    pub default_background: Rgba,
    /// One flag per row-major cell. `true` lets below-background images show through;
    /// an empty slice means no holes.
    pub default_backgrounds: &'a [bool],
    pub images: &'a [Placement],
    /// Supplies live pixels for admitted image and viewer revisions missing from the GPU cache.
    pub image_source: Option<&'a dyn ImageSource>,
    /// Terminal UI rectangles that images must not cover, in device pixels.
    pub image_exclusions: [Option<PixelRect>; 2],
    pub glyphs: &'a [GlyphQuad],
    pub cursor: Option<Cursor>,
    pub chrome: Option<ChromeLayer<'a>>,
}

/// A cropped image composited above a pane's terminal content and dim backdrop.
#[derive(Clone, Copy)]
pub struct ImageViewer<'a> {
    /// Samples this placement's source crop, ignoring its terminal destination.
    pub placement: &'a Placement,
    /// Window-relative device pixels, clipped to the enclosing pane.
    pub dest: PixelRect,
    /// Color blended over the full pane before the viewer image.
    pub backdrop: Rgba,
}

/// In-image chrome for placements marked as reduced thumbnails.
#[derive(Clone, Copy)]
pub struct ThumbnailChrome {
    /// Index in the pane's draw list, not its sorted GPU instance list.
    pub hovered: Option<usize>,
    pub border: Rgba,
    /// Opaque hover border and expand-chip background.
    pub accent: Rgba,
    pub on_accent: Rgba,
    /// Preferred chip side in device pixels; the border is always one device pixel.
    pub chip_size: u32,
}

/// One leaf of a split frame. Its bounds include any space left over by whole cells.
#[derive(Clone, Copy)]
pub struct PaneDraw<'a> {
    pub rect: Rect,
    /// Window-relative geometry, with cell ownership relative to this grid. The frame owns chrome.
    pub draw: DrawList<'a>,
    pub thumbnails: Option<ThumbnailChrome>,
    pub viewer: Option<ImageViewer<'a>>,
}

/// A window's terminal grids and overlays. Panes must be disjoint and the iterator repeatable.
pub struct FrameDraw<'a, P> {
    /// Color outside pane rectangles. On translucent frames its alpha also applies to the gutters.
    pub background: Rgba,
    pub panes: P,
    /// Divider and dim tints, drawn after panes and before chrome. A translucent frame preserves
    /// the alpha already present under each tint.
    pub fills: &'a [Fill],
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

pub(crate) fn write_fills(bytes: wgpu::WriteOnly<'_, [u8]>, fills: &[Fill]) {
    let mut write = instance_writer(bytes);
    for fill in fills {
        write(Instance::solid(fill.rect, fill.color));
    }
}

impl ChromeLayer<'_> {
    pub(crate) fn write_instances(&self, bytes: wgpu::WriteOnly<'_, [u8]>) {
        let (fills, glyphs) = bytes.split_at(self.fills.len() * size_of::<Instance>());
        write_fills(fills, self.fills);
        let mut write = instance_writer(glyphs);
        for glyph in self.glyphs {
            write(Instance::glyph(glyph, self.text_background));
        }
    }
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
        self.write_grid(instance_writer(bytes));
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

    pub(crate) fn write_layered(
        &self,
        bytes: wgpu::WriteOnly<'_, [u8]>,
        layers: &GridLayers,
        translucent: bool,
        viewer: bool,
    ) {
        let opaque_pane = translucent && viewer;
        let stride = size_of::<Instance>();
        let (text, cursor) = bytes.split_at(layers.text as usize * stride);
        let mut write = instance_writer(text);
        let mut write_cursor = instance_writer(cursor);
        if let Some(base) = layers.base {
            let mut color = self.default_background;
            if !translucent || opaque_pane {
                color[3] = 255;
            }
            write(Instance::solid(base, color));
        }
        if layers.base.is_some() && !self.default_backgrounds.is_empty() {
            self.write_backgrounds::<true>(&mut write, opaque_pane);
        } else {
            self.write_backgrounds::<false>(&mut write, opaque_pane);
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
    }

    fn write_backgrounds<const SKIP_DEFAULT: bool>(
        &self,
        mut write: impl FnMut(Instance),
        opaque_pane: bool,
    ) {
        let columns = self.grid_size[0] as usize;
        for (row, backgrounds) in self.backgrounds.chunks(columns.max(1)).enumerate() {
            for (col, mut color) in backgrounds.iter().copied().enumerate() {
                if SKIP_DEFAULT && self.default_backgrounds[row * columns + col] {
                    continue;
                }
                if opaque_pane {
                    color[3] = 255;
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
        self.write_backgrounds::<false>(&mut write, false);
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
