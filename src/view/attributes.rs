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

#[derive(Default)]
pub(crate) struct LinkHighlight<'a> {
    pub(crate) ranges: &'a [CellRange],
    pub(crate) control: bool,
}

impl LinkHighlight<'_> {
    // Spans and cells arrive in reading order, including disjoint OSC 8 labels.
    pub(super) fn at(&mut self, row: u32, column: u32) -> Option<bool> {
        let point = (i64::from(row), column);
        let coords = |p: SelectionPoint| (p.row, u32::from(p.column));
        while self
            .ranges
            .first()
            .is_some_and(|span| coords(span.end) < point)
        {
            self.ranges = &self.ranges[1..];
        }
        self.ranges
            .first()
            .filter(|span| coords(span.start) <= point)
            .map(|_| self.control)
    }
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
        hovered: Option<bool>,
    ) {
        let style = cell.style;
        if let Some((mark, color)) =
            underline(style, fg, view.link_hint, hovered, cell.wide_wrap_spacer)
        {
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
            let mut quad = glyph_quad(image, origin, cell, color);
            if mark == Decoration::LinkHint {
                quad.atlas.origin[0] += origin[0].rem_euclid(3).cast_unsigned();
                quad.atlas.size[0] = view.cell[0];
                quad.dest.size[0] = view.cell[0];
            }
            view.pane.glyphs.push(quad);
        }
    }
}

fn underline(
    style: CellStyle,
    fg: Rgba,
    hint: Rgba,
    hovered: Option<bool>,
    wide_wrap_spacer: bool,
) -> Option<(Decoration, Rgba)> {
    let mark = match style.underline {
        Underline::None if wide_wrap_spacer => return None,
        Underline::None if hovered == Some(true) => return Some((Decoration::Single, fg)),
        Underline::None if style.hyperlink || hovered.is_some() => {
            return Some((Decoration::LinkHint, hint));
        }
        Underline::None => return None,
        Underline::Single => Decoration::Single,
        Underline::Double => Decoration::Double,
        Underline::Curly => Decoration::Curly,
        Underline::Dotted => Decoration::Dotted,
        Underline::Dashed => Decoration::Dashed,
    };
    let color = style
        .underline_color
        .map_or(fg, |[r, g, b]| [r, g, b, fg[3]]);
    Some((mark, color))
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
        color_glyph: image.color,
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

#[cfg(test)]
mod tests {
    use super::{CellStyle, Decoration, Underline, underline};

    #[test]
    fn link_underline_precedence() {
        let fg = [255; 4];
        let hint = [0, 100, 224, 128];
        let plain = CellStyle::default();
        let osc = CellStyle {
            hyperlink: true,
            ..plain
        };
        let app = CellStyle {
            underline: Underline::Double,
            underline_color: Some([255, 0, 0]),
            ..osc
        };
        for (style, hovered, spacer, expected) in [
            (osc, None, false, Some((Decoration::LinkHint, hint))),
            (
                app,
                Some(false),
                false,
                Some((Decoration::Double, [255, 0, 0, 255])),
            ),
            (plain, None, false, None),
            (
                plain,
                Some(false),
                false,
                Some((Decoration::LinkHint, hint)),
            ),
            (plain, Some(true), false, Some((Decoration::Single, fg))),
            (plain, Some(true), true, None),
            (
                app,
                Some(true),
                false,
                Some((Decoration::Double, [255, 0, 0, 255])),
            ),
        ] {
            assert_eq!(underline(style, fg, hint, hovered, spacer), expected);
        }
    }
}
