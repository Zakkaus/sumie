use bytemuck::{Pod, Zeroable};

use crate::AtlasRect;

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

/// One complete frame. All positions and sizes are in device pixels except `grid_size`.
pub struct DrawList<'a> {
    /// Number of columns and rows.
    pub grid_size: [u32; 2],
    pub cell_size: [u32; 2],
    pub origin: [i32; 2],
    /// One background per cell, in row-major order.
    pub backgrounds: &'a [Rgba],
    pub glyphs: &'a [GlyphQuad],
    pub cursor: Option<Cursor>,
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
        instance.color = text;
        instance
    }
}

impl DrawList<'_> {
    pub(crate) fn instance_count(&self) -> u32 {
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
        let columns = self.grid_size[0] as usize;
        let mut out = bytes
            .into_chunks::<{ size_of::<Instance>() }>()
            .0
            .into_iter();
        let mut write = |instance| out.next().unwrap().write(bytemuck::cast(instance));
        for (row, backgrounds) in self.backgrounds.chunks(columns.max(1)).enumerate() {
            for (col, &color) in backgrounds.iter().enumerate() {
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
        let (cursor_cells, overlay) = match self.cursor {
            Some(
                cursor @ Cursor {
                    shape:
                        CursorShape::Block {
                            cell: [col, row],
                            columns,
                        },
                    ..
                },
            ) => {
                cursor.write(&mut write);
                (
                    Some((col..col + u32::from(columns), row, cursor.color)),
                    None,
                )
            }
            overlay => (None, overlay),
        };
        for glyph in self.glyphs {
            let [col, row] = glyph.cell;
            debug_assert!(col < self.grid_size[0] && row < self.grid_size[1]);
            let background = self.backgrounds[row as usize * columns + col as usize];
            if let Some((cells, cursor_row, fill)) = &cursor_cells
                && row == *cursor_row
                && cells.contains(&col)
            {
                write(Instance::cursor_glyph(glyph, background, *fill));
            } else {
                write(Instance::glyph(glyph, background));
            }
        }
        if let Some(cursor) = overlay {
            cursor.write(write);
        }
    }
}
