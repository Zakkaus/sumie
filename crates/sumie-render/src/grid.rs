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
pub struct Cursor {
    pub rect: PixelRect,
    pub color: Rgba,
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
}

impl DrawList<'_> {
    pub(crate) fn instance_count(&self) -> u32 {
        let [columns, rows] = self.grid_size;
        assert_eq!(self.backgrounds.len(), columns as usize * rows as usize);
        u32::try_from(
            self.backgrounds.len() + self.glyphs.len() + usize::from(self.cursor.is_some()),
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
        for glyph in self.glyphs {
            let [col, row] = glyph.cell;
            debug_assert!(col < self.grid_size[0] && row < self.grid_size[1]);
            let background = self.backgrounds[row as usize * columns + col as usize];
            write(Instance::glyph(glyph, background));
        }
        if let Some(cursor) = self.cursor {
            write(Instance::solid(cursor.rect, cursor.color));
        }
    }
}
