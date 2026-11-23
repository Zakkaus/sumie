use sumie_core::{CellRange, SelectionPoint};
use sumie_font::{AtlasGlyph, Decoration, FontStyle};
use sumie_render::{AtlasRect, GlyphQuad, PixelRect, Rgba};
use sumie_vt::{Cell, CellStyle, Rgb, Underline};

use super::paint::PanePainter;

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

// Spans and drawn cells both arrive in reading order, including disjoint OSC 8 labels.
pub(super) fn link_at(spans: &mut &[CellRange], row: u32, column: u32) -> bool {
    let point = (i64::from(row), column);
    let coords = |p: SelectionPoint| (p.row, u32::from(p.column));
    while spans.first().is_some_and(|span| coords(span.end) < point) {
        *spans = &spans[1..];
    }
    spans
        .first()
        .is_some_and(|span| coords(span.start) <= point)
}

#[derive(Default)]
pub(super) struct Decorations([Option<AtlasGlyph>; Decoration::Overline as usize + 1]);

impl PanePainter<'_> {
    pub(super) fn finish_run(&mut self, origin: [u32; 2], ink: Ink) {
        self.line.trim_spaces();
        self.line.draw(
            self.font,
            self.font_errors,
            &mut self.pane.glyphs,
            origin,
            ink.color,
            ink.face,
        );
        self.line.clear();
    }
}

impl Decorations {
    pub(super) fn draw(
        &mut self,
        view: &mut PanePainter<'_>,
        pos: [u32; 2],
        cell: &Cell,
        fg: Rgba,
        hovered: bool,
    ) {
        let style = cell.style;
        let underline = if hovered {
            if style.underline == Underline::Single {
                Underline::Double
            } else {
                Underline::Single
            }
        } else {
            style.underline
        };
        let underline = match underline {
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
            self.draw_mark(view, pos, cell.width, mark, color);
        }
        if style.overline {
            self.draw_mark(view, pos, cell.width, Decoration::Overline, fg);
        }
        if style.strikethrough {
            self.draw_mark(view, pos, cell.width, Decoration::Strikethrough, fg);
        }
    }

    fn draw_mark(
        &mut self,
        view: &mut PanePainter<'_>,
        [col, row]: [u32; 2],
        width: u8,
        mark: Decoration,
        color: Rgba,
    ) {
        let slot = &mut self.0[mark as usize];
        let image = match *slot {
            Some(image) => image,
            None => match view.font.decoration(mark) {
                Ok(image) => *slot.insert(image),
                Err(error) => {
                    view.font_errors.warn(&error);
                    return;
                }
            },
        };
        for column in col..col + u32::from(width) {
            let cell = [column, row];
            let origin = view.origin(cell);
            view.pane
                .glyphs
                .push(glyph_quad(image, origin, cell, color));
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
