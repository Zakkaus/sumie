use sumie_font::{AtlasGlyph, Decoration, FontStyle};
use sumie_render::{AtlasRect, GlyphQuad, PixelRect, Rgba};
use sumie_vt::{Cell, CellStyle, Rgb, Underline};

use super::View;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Ink {
    face: FontStyle,
    pub(super) color: Rgba,
}

impl Ink {
    pub(super) fn new([r, g, b]: Rgb, style: CellStyle) -> Self {
        Self {
            face: FontStyle {
                bold: style.bold,
                italic: style.italic,
            },
            color: [r, g, b, if style.faint { 128 } else { 255 }],
        }
    }
}

impl View {
    pub(super) fn finish_run(&mut self, origin: [u32; 2], ink: Ink) {
        self.line.trim_spaces();
        self.line.draw(
            &mut self.font,
            &mut self.font_errors,
            &mut self.glyphs,
            origin,
            ink.color,
            ink.face,
        );
        self.line.clear();
    }

    pub(super) fn draw_decorations(&mut self, pos: [u32; 2], cell: &Cell, fg: Rgba) {
        let style = cell.style;
        let underline = match style.underline {
            Underline::None => None,
            Underline::Single => Some(Decoration::Single),
            Underline::Double => Some(Decoration::Double),
            Underline::Curly => Some(Decoration::Curly),
            Underline::Dotted => Some(Decoration::Dotted),
            Underline::Dashed => Some(Decoration::Dashed),
        };
        if let Some(mark) = underline {
            let color = style
                .underline_color
                .map_or(fg, |[r, g, b]| [r, g, b, fg[3]]);
            self.draw_mark(pos, cell.width, mark, color);
        }
        if style.overline {
            self.draw_mark(pos, cell.width, Decoration::Overline, fg);
        }
        if style.strikethrough {
            self.draw_mark(pos, cell.width, Decoration::Strikethrough, fg);
        }
    }

    fn draw_mark(&mut self, [col, row]: [u32; 2], width: u8, mark: Decoration, color: Rgba) {
        match self.font.decoration(mark) {
            Ok(image) => {
                for column in col..col + u32::from(width) {
                    let cell = [column, row];
                    self.glyphs
                        .push(glyph_quad(image, self.origin(cell), cell, color));
                }
            }
            Err(error) => self.font_errors.warn(&error),
        }
    }
}

pub(super) fn glyph_quad(
    image: AtlasGlyph,
    [x, y]: [i32; 2],
    cell: [u32; 2],
    color: Rgba,
) -> GlyphQuad {
    let rect = image.rect;
    let size = [rect.width, rect.height];
    GlyphQuad {
        atlas: AtlasRect {
            origin: [rect.x, rect.y],
            size,
        },
        dest: PixelRect {
            origin: [x + image.left, y + image.top],
            size,
        },
        cell,
        color,
    }
}
