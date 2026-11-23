use sumie_font::{FontOptions, FontSystem};
use sumie_render::{AtlasRect, AtlasUpload, DrawList, GlyphQuad, PixelRect, Rgba, SurfaceRenderer};

use crate::Result;

pub(super) struct Page {
    cell: [u32; 2],
    grid: [u32; 2],
    backgrounds: Vec<Rgba>,
    glyphs: Vec<GlyphQuad>,
}

impl Page {
    pub(super) fn new(scale: f64, target: &mut SurfaceRenderer<'_>) -> Result<Self> {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "font sizes use f32 device pixels"
        )]
        let mut font = FontSystem::new("Menlo", 20.0, scale as f32, FontOptions::default())?;
        let cell = [font.metrics().width, font.metrics().height];
        let lines: [(&str, &[u8]); 4] = [
            ("sumie / Metal", &[1; 13]),
            ("漢字かな한글", &[2; 6]),
            ("┌──┬──┐", &[1; 7]),
            ("└──┴──┘", &[1; 7]),
        ];
        let mut columns = 0;
        let mut glyphs = Vec::new();
        for (row, (text, widths)) in (0_u32..).zip(lines) {
            let run = font.shape(text, widths)?;
            columns = columns.max(run.cells);
            for glyph in run.glyphs {
                let image = font.rasterize(glyph.key)?;
                let rect = image.rect;
                let size = [rect.width, rect.height];
                if size.contains(&0) {
                    continue;
                }
                let x = cell[0].cast_signed() + glyph.x + image.left;
                let y = ((row + 1) * cell[1]).cast_signed() + glyph.y + image.top;
                glyphs.push(GlyphQuad {
                    atlas: AtlasRect {
                        origin: [rect.x, rect.y],
                        size,
                    },
                    dest: PixelRect {
                        origin: [x, y],
                        size,
                    },
                    color: [255; 4],
                    cell: [glyph.column, row],
                });
            }
        }
        let [width, height] = font.atlas().dimensions();
        target.upload_atlas(&AtlasUpload {
            width,
            height,
            bytes: font.atlas().pixels(),
            dirty: &[AtlasRect {
                origin: [0, 0],
                size: [width, height],
            }],
        });
        Ok(Self {
            cell,
            grid: [columns, u32::try_from(lines.len())?],
            backgrounds: vec![[0, 0, 0, 255]; columns as usize * lines.len()],
            glyphs,
        })
    }

    pub(super) fn draw_list(&self) -> DrawList<'_> {
        DrawList {
            grid_size: self.grid,
            cell_size: self.cell,
            origin: self.cell.map(u32::cast_signed),
            backgrounds: &self.backgrounds,
            glyphs: &self.glyphs,
            cursor: None,
        }
    }
}
