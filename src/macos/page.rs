use sumie_config::{Config, CursorStyle, Font};
use sumie_font::{FontOptions, FontSystem, Hinting};
use sumie_render::{
    AtlasRect, AtlasUpload, Cursor, CursorShape, DrawList, GlyphQuad, PixelRect, Rgba,
    SurfaceRenderer, wgpu,
};

use crate::Result;

const TITLE: &str = "sumie / Metal";
const COLUMNS: u32 = 13;
const ROWS: u32 = 4;
const CURSOR_CELL: [u32; 2] = [8, 3];

pub(super) struct Page {
    font: FontSystem,
    cell: [u32; 2],
    columns: u32,
    backgrounds: Vec<Rgba>,
    glyphs: Vec<GlyphQuad>,
    header_is_banner: bool,
    body_len: usize,
    cursor: Cursor,
    pub(super) clear: wgpu::Color,
}

impl Page {
    pub(super) fn new(config: &Font, scale: f64) -> Result<Self> {
        let options = FontOptions {
            hinting: match config.hinting() {
                sumie_config::Hinting::Off => Hinting::Off,
                sumie_config::Hinting::Light => Hinting::Light,
                sumie_config::Hinting::Normal => Hinting::Normal,
            },
            weight: config.weight(),
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "font sizes use f32 device pixels"
        )]
        let mut font =
            FontSystem::new(config.family(), config.logical_px(), scale as f32, options)?;
        let cell = [font.metrics().width, font.metrics().height];
        let mut glyphs = Vec::new();
        for (row, (text, widths)) in (1..).zip([
            ("漢字かな한글", &[2; 6][..]),
            ("┌──┬──┐", &[1; 7]),
            ("└──┴──┘", &[1; 7]),
        ]) {
            shape_row(&mut font, &mut glyphs, cell, row, text, widths)?;
        }
        let body_len = glyphs.len();
        shape_row(
            &mut font,
            &mut glyphs,
            cell,
            0,
            TITLE,
            &[1; COLUMNS as usize],
        )?;
        Ok(Self {
            font,
            cell,
            columns: COLUMNS,
            backgrounds: Vec::new(),
            glyphs,
            body_len,
            header_is_banner: false,
            cursor: Cursor {
                rect: PixelRect {
                    origin: std::array::from_fn(|i| ((CURSOR_CELL[i] + 1) * cell[i]).cast_signed()),
                    size: cell,
                },
                color: [255; 4],
                shape: CursorShape::Block {
                    cell: CURSOR_CELL,
                    columns: 1,
                },
            },
            clear: wgpu::Color::BLACK,
        })
    }

    pub(super) fn compose(
        &mut self,
        config: &Config,
        banner: Option<&str>,
        width: u32,
        target: &mut SurfaceRenderer<'_>,
    ) -> Result<()> {
        let fg = rgba(config.colors.foreground());
        let bg = rgba(config.colors.background());
        self.columns = COLUMNS;
        if let Some(message) = banner {
            self.glyphs.truncate(self.body_len);
            let visible = (width / self.cell[0]).saturating_sub(1);
            self.columns = COLUMNS.max(visible);
            let text = ellipsize(message, visible as usize);
            let widths = vec![1; text.chars().count()];
            if shape_row(
                &mut self.font,
                &mut self.glyphs,
                self.cell,
                0,
                &text,
                &widths,
            )
            .is_err()
            {
                self.glyphs.truncate(self.body_len);
                let text = ellipsize("Invalid configuration", visible as usize);
                shape_row(
                    &mut self.font,
                    &mut self.glyphs,
                    self.cell,
                    0,
                    &text,
                    &vec![1; text.chars().count()],
                )?;
            }
        } else if self.header_is_banner {
            self.glyphs.truncate(self.body_len);
            shape_row(
                &mut self.font,
                &mut self.glyphs,
                self.cell,
                0,
                TITLE,
                &[1; COLUMNS as usize],
            )?;
        }
        self.header_is_banner = banner.is_some();
        let columns = self.columns as usize;
        self.backgrounds.clear();
        self.backgrounds.resize(columns * ROWS as usize, bg);
        self.backgrounds[..columns].fill(if banner.is_some() { fg } else { bg });
        self.glyphs[..self.body_len]
            .iter_mut()
            .for_each(|glyph| glyph.color = fg);
        self.glyphs[self.body_len..]
            .iter_mut()
            .for_each(|glyph| glyph.color = if banner.is_some() { bg } else { fg });
        self.cursor.color = config.colors.cursor().map_or(fg, rgba);
        self.cursor.shape = match config.cursor.style() {
            CursorStyle::Block => CursorShape::Block {
                cell: CURSOR_CELL,
                columns: 1,
            },
            CursorStyle::Bar => CursorShape::Bar,
            CursorStyle::Underline => CursorShape::Underline,
        };
        self.clear = linear_color(config.colors.background());
        self.upload_atlas(target);
        Ok(())
    }

    fn upload_atlas(&mut self, target: &mut SurfaceRenderer<'_>) {
        let atlas = self.font.atlas();
        if !atlas.dirty_regions().is_empty() {
            let [width, height] = atlas.dimensions();
            target.upload_atlas(&AtlasUpload {
                width,
                height,
                bytes: atlas.pixels(),
                dirty: &[AtlasRect {
                    origin: [0, 0],
                    size: [width, height],
                }],
            });
            self.font.clear_dirty_regions();
        }
    }

    pub(super) fn draw_list(&self, show_cursor: bool) -> DrawList<'_> {
        DrawList {
            grid_size: [self.columns, ROWS],
            chrome: None,
            cell_size: self.cell,
            origin: self.cell.map(u32::cast_signed),
            backgrounds: &self.backgrounds,
            glyphs: &self.glyphs,
            cursor: show_cursor.then_some(self.cursor),
        }
    }
}

fn shape_row(
    font: &mut FontSystem,
    glyphs: &mut Vec<GlyphQuad>,
    cell: [u32; 2],
    row: u32,
    text: &str,
    widths: &[u8],
) -> Result<()> {
    for glyph in font.shape(text, widths)?.glyphs {
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
    Ok(())
}

fn ellipsize(text: &str, columns: usize) -> String {
    let mut chars = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch });
    let mut text: String = chars.by_ref().take(columns).collect();
    if !text.is_empty() && chars.next().is_some() {
        text.pop();
        text.push('…');
    }
    text
}

const fn rgba([r, g, b]: [u8; 3]) -> Rgba {
    [r, g, b, 255]
}

fn linear_color([r, g, b]: [u8; 3]) -> wgpu::Color {
    fn linear(value: u8) -> f64 {
        let encoded = f64::from(value) / 255.0;
        if encoded <= 0.04045 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }
    }
    wgpu::Color {
        r: linear(r),
        g: linear(g),
        b: linear(b),
        a: 1.0,
    }
}
