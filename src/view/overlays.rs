//! Draw the configuration banner, search bar, and match tints without touching terminal state.

use std::ops::Range;

use super::{
    Cursor, FontStyle, GlyphQuad, Line, PixelRect, Rgb, Snapshot, View, codepoint_width, rgba,
};
use crate::input::SearchTarget;

/// Cell-aligned bounds; glyph masks and pointer events use the same integer pixel edges.
#[derive(Clone, Copy)]
pub(super) struct SearchBox {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
}

pub(super) struct PresentedSearch {
    panel: SearchBox,
    cell: [u32; 2],
    logical: [u32; 2],
    device: [u32; 2],
    grid_top: u32,
}

impl SearchBox {
    pub(super) fn place(
        [cols, rows]: [u32; 2],
        banner: bool,
        spans: &[(u16, u16, u16, bool)],
    ) -> Self {
        const WIDTH: u32 = 42;
        // Leave some terminal visible beside the panel in smaller windows.
        let width = cols.min(WIDTH).min((cols - cols / 4).max(24));
        let inset_x = u32::from(cols >= width + 2);
        let inset_y = u32::from(rows >= 8);
        let height = if rows >= 4 { 2 } else { 1 };
        let top = u32::from(banner && rows > 1);
        let bottom = rows - height;
        let right = cols - width;
        let mut best = Self {
            left: right - inset_x,
            top: top.max(inset_y),
            width,
            height,
        };
        let mut minimum = u32::MAX;
        for (left, top) in [
            (best.left, best.top),
            (0, top),
            (right, bottom),
            (0, bottom),
            (right, top),
        ] {
            let candidate = Self { left, top, ..best };
            let overlap = candidate.match_overlap(spans);
            if overlap == 0 {
                return candidate;
            }
            if overlap < minimum {
                best = candidate;
                minimum = overlap;
            }
        }
        best
    }

    fn match_overlap(self, spans: &[(u16, u16, u16, bool)]) -> u32 {
        let start = spans.partition_point(|span| u32::from(span.0) < self.top);
        spans[start..]
            .iter()
            .take_while(|span| u32::from(span.0) < self.top + self.height)
            .filter(|span| span.3)
            .map(|&(_, first, last, _)| {
                (u32::from(last) + 1)
                    .min(self.left + self.width)
                    .saturating_sub(u32::from(first).max(self.left))
            })
            .sum()
    }

    fn columns(self) -> Range<u32> {
        self.left..self.left + self.width
    }

    fn pixels(self, columns: Range<u32>, [w, h]: [u32; 2]) -> PixelRect {
        PixelRect {
            origin: [
                (columns.start * w).cast_signed(),
                (self.top * h).cast_signed(),
            ],
            size: [(columns.end - columns.start) * w, self.height * h],
        }
    }

    pub(super) fn bounds(self, cell: [u32; 2]) -> PixelRect {
        self.pixels(self.columns(), cell)
    }

    fn buttons(self) -> impl Iterator<Item = (Range<u32>, &'static str, SearchTarget)> {
        let width = if self.width >= 24 { 3 } else { 1 };
        // Keep a caret cell even when only the close button fits.
        let count = match self.width {
            1 => 0,
            2..=5 => 1,
            _ => 3,
        };
        [
            ("×", SearchTarget::Close),
            ("↓", SearchTarget::Next),
            ("↑", SearchTarget::Previous),
        ]
        .into_iter()
        .zip(1..=count)
        .map(move |((label, action), i)| {
            let start = self.columns().end - i * width;
            (start..start + width, label, action)
        })
    }
}

#[derive(Clone, Copy)]
pub(super) enum Mask {
    Inside(PixelRect),
    Outside(PixelRect),
}

/// Clip terminal ink, including decoration and neighboring glyph overhangs, before
/// drawing the box. Splitting the mask preserves the visible part of a wide glyph.
#[derive(Default)]
pub(super) struct Glyphs {
    pub(super) quads: Vec<GlyphQuad>,
    pub(super) cover: Option<Mask>,
    pub(super) offset: [i32; 2],
}

impl Glyphs {
    pub(super) fn push(&mut self, glyph: GlyphQuad) {
        let Some(mask) = self.cover else {
            self.quads.push(glyph);
            return;
        };
        let [x, y] = glyph.dest.origin;
        let right = x + glyph.dest.size[0].cast_signed();
        let bottom = y + glyph.dest.size[1].cast_signed();
        let (Mask::Inside(cover) | Mask::Outside(cover)) = mask;
        let [cx, cy] = cover.origin;
        let left = x.max(cx);
        let top = y.max(cy);
        let r = right.min(cx + cover.size[0].cast_signed());
        let b = bottom.min(cy + cover.size[1].cast_signed());
        let inside = matches!(mask, Mask::Inside(_));
        if left >= r || top >= b {
            if !inside {
                self.quads.push(glyph);
            }
            return;
        }
        let mut piece = |(x0, y0, x1, y1): (i32, i32, i32, i32)| {
            if x0 < x1 && y0 < y1 {
                let mut piece = glyph;
                piece.dest = PixelRect {
                    origin: [x0, y0],
                    size: [(x1 - x0).cast_unsigned(), (y1 - y0).cast_unsigned()],
                };
                piece.atlas.origin[0] += (x0 - x).cast_unsigned();
                piece.atlas.origin[1] += (y0 - y).cast_unsigned();
                piece.atlas.size = piece.dest.size;
                self.quads.push(piece);
            }
        };
        if inside {
            piece((left, top, r, b));
        } else {
            piece((x, y, right, top));
            piece((x, b, right, bottom));
            piece((x, top, left, b));
            piece((r, top, right, b));
        }
    }
}

impl View {
    pub(super) fn set_overlay_bounds(&mut self, banner: bool) {
        self.search_box = self
            .search_bar
            .as_ref()
            .map(|_| SearchBox::place(self.grid, banner, &self.search_highlights));
        let search = self.search_box.map(|b| {
            let mut rect = b.bounds(self.cell);
            rect.origin[1] += self.glyphs.offset[1];
            rect
        });
        self.glyphs.cover = search.map(Mask::Outside);
        self.image_exclusions = [
            banner.then(|| PixelRect {
                origin: self.glyphs.offset,
                size: [self.grid[0] * self.cell[0], self.cell[1]],
            }),
            search,
        ];
    }

    pub(crate) fn frame_presented(&mut self) {
        self.tabs.presented();
        self.presented_search = self.search_box.map(|panel| PresentedSearch {
            panel,
            cell: self.cell,
            logical: self.logical,
            device: self.device,
            grid_top: self.bar_height(),
        });
    }

    pub(crate) fn search_hit(&self, (x, y): (f64, f64)) -> Option<SearchTarget> {
        let frame = self.presented_search.as_ref()?;
        let pixel = [
            x * f64::from(frame.device[0]) / f64::from(frame.logical[0]),
            y * f64::from(frame.device[1]) / f64::from(frame.logical[1])
                - f64::from(frame.grid_top),
        ];
        let contains = |rect: PixelRect| {
            (0..2).all(|i| {
                (0.0..f64::from(rect.size[i])).contains(&(pixel[i] - f64::from(rect.origin[i])))
            })
        };
        let panel = frame.panel;
        contains(panel.bounds(frame.cell)).then(|| {
            panel
                .buttons()
                .find_map(|(cols, _, target)| {
                    contains(panel.pixels(cols, frame.cell)).then_some(target)
                })
                .unwrap_or(SearchTarget::Field)
        })
    }

    pub(super) fn draw_banner(&mut self, snapshot: &Snapshot, message: &str) {
        self.backgrounds.extend(std::iter::repeat_n(
            rgba(snapshot.foreground),
            self.grid[0] as usize,
        ));
        self.line.ellipsize(message, self.grid[0]);
        self.line.draw(
            &mut self.font,
            &mut self.font_errors,
            &mut self.glyphs,
            [0, 0],
            rgba(snapshot.background),
            super::FontStyle::default(),
        );
    }

    pub(super) fn draw_search(&mut self, snapshot: &Snapshot, panel: SearchBox, banner: bool) {
        self.tint_matches(snapshot, banner);
        let ink = readable(snapshot.foreground, snapshot.background);
        let field = mix(snapshot.background, ink, 1);
        let controls = mix(snapshot.background, ink, 2);
        let rail = panel
            .buttons()
            .last()
            .map_or(panel.columns().end, |(cols, _, _)| cols.start);
        let stride = self.grid[0] as usize;
        for row in panel.top..panel.top + panel.height {
            let row = row as usize * stride;
            self.backgrounds[row + panel.left as usize..row + rail as usize].fill(rgba(field));
            self.backgrounds[row + rail as usize..row + panel.columns().end as usize]
                .fill(rgba(controls));
            if !self.default_backgrounds.is_empty() {
                self.default_backgrounds
                    [row + panel.left as usize..row + panel.columns().end as usize]
                    .fill(false);
            }
        }
        let first = self.glyphs.quads.len();
        self.draw_query(panel, rail, readable(ink, field));
        let control_ink = rgba(readable(ink, controls));
        for (cols, label, _) in panel.buttons() {
            self.line.clear();
            self.line.push(label, 1);
            self.line.draw(
                &mut self.font,
                &mut self.font_errors,
                &mut self.glyphs,
                [cols.start.midpoint(cols.end - 1), panel.top],
                control_ink,
                FontStyle::default(),
            );
        }
        let inset = ((panel.height - 1) * self.cell[1] / 2).cast_signed();
        for glyph in &mut self.glyphs.quads[first..] {
            glyph.dest.origin[1] += inset;
        }
        if let Some(cursor) = &mut self.cursor {
            cursor.rect.origin[1] += inset;
        }
    }

    fn draw_query(&mut self, panel: SearchBox, rail: u32, ink: Rgb) {
        let label = self.search_bar.as_deref().expect("search bar is open");
        let (query, count) = label.rsplit_once('\t').unwrap_or((label, ""));
        let left = panel.left + u32::from(panel.width >= 12);
        let room = rail - left;
        let count_width = self.line.ellipsize(count, room.saturating_sub(4));
        let count_column = rail - u32::from(room >= 8) - count_width;
        let color = rgba(ink);
        self.line.draw(
            &mut self.font,
            &mut self.font_errors,
            &mut self.glyphs,
            [count_column, panel.top],
            color,
            FontStyle::default(),
        );
        let available = count_column - left - u32::from(count_width != 0) - 1;
        let (preedit, preedit_width) = visible_suffix(&self.preedit, available);
        let (query, query_width) = if preedit.len() == self.preedit.len() {
            visible_suffix(query, available - preedit_width)
        } else {
            ("", 0)
        };
        self.line.clear();
        self.line.append_overlay_text(query);
        self.line.append_overlay_text(preedit);
        self.line.draw(
            &mut self.font,
            &mut self.font_errors,
            &mut self.glyphs,
            [left, panel.top],
            color,
            FontStyle::default(),
        );
        self.cursor = Some(Cursor {
            rect: PixelRect {
                origin: self.origin([left + query_width + preedit_width, panel.top]),
                size: [self.cell[0].min(2), self.cell[1]],
            },
            color,
            shape: super::CursorShape::Solid,
        });
    }

    fn tint_matches(&mut self, snapshot: &Snapshot, banner: bool) {
        let columns = self.grid[0] as usize;
        for &(row, first, last, current) in &self.search_highlights {
            if banner && row == 0 {
                continue;
            }
            let offset = usize::from(row) * columns;
            let cells = &snapshot.rows[usize::from(row)][usize::from(first)..=usize::from(last)];
            let backgrounds =
                &mut self.backgrounds[offset + usize::from(first)..=offset + usize::from(last)];
            let weight = if current { 4 } else { 2 };
            if !self.default_backgrounds.is_empty() {
                self.default_backgrounds[offset + usize::from(first)..=offset + usize::from(last)]
                    .fill(false);
            }
            let accent = [255, 183, 71].map(|channel| channel * weight);
            for (background, cell) in backgrounds.iter_mut().zip(cells) {
                if !cell.wide_wrap_spacer {
                    *background = rgba(std::array::from_fn(|i| {
                        ((u16::from(background[i]) * (8 - weight) + accent[i]) / 8)
                            .try_into()
                            .expect("weighted average fits into u8")
                    }));
                }
            }
        }
    }
}

fn mix(base: Rgb, accent: Rgb, eighths: u16) -> Rgb {
    std::array::from_fn(|i| {
        ((u16::from(base[i]) * (8 - eighths) + u16::from(accent[i]) * eighths) / 8)
            .try_into()
            .expect("weighted average fits into u8")
    })
}

fn readable(ink: Rgb, surface: Rgb) -> Rgb {
    let luminance = |color| {
        let c = super::clear_color(color);
        0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b + 0.05
    };
    let fg = luminance(ink);
    let bg = luminance(surface);
    if fg.max(bg) >= 4.5 * fg.min(bg) {
        ink
    } else if bg / 0.05 >= 1.05 / bg {
        [0; 3]
    } else {
        [255; 3]
    }
}

impl Line {
    fn append_overlay_text(&mut self, text: &str) {
        for ch in text.chars().map(display_char) {
            let width = codepoint_width(ch);
            if width != 0 || !self.text.is_empty() {
                self.text.push(ch);
                self.widths.push(width);
            }
        }
    }

    pub(super) fn ellipsize(&mut self, text: &str, columns: u32) -> u32 {
        self.clear();
        if columns == 0 {
            return 0;
        }
        let mut cells = 0;
        let mut last = (0, 0);
        for ch in text.chars().map(display_char) {
            let width = codepoint_width(ch);
            if cells + u32::from(width) > columns {
                if cells == columns {
                    cells -= u32::from(self.widths[last.1]);
                    self.text.truncate(last.0);
                    self.widths.truncate(last.1);
                }
                self.push("…", 1);
                return cells + 1;
            }
            if width != 0 {
                last = (self.text.len(), self.widths.len());
            } else if cells == 0 {
                continue;
            }
            self.text.push(ch);
            self.widths.push(width);
            cells += u32::from(width);
        }
        cells
    }
}

fn display_char(ch: char) -> char {
    if ch.is_control() { ' ' } else { ch }
}

fn visible_suffix(text: &str, columns: u32) -> (&str, u32) {
    let mut start = text.len();
    let mut cells = 0;
    for (i, ch) in text.char_indices().rev() {
        let width = u32::from(codepoint_width(display_char(ch)));
        if cells + width > columns {
            break;
        }
        cells += width;
        start = i;
    }
    (&text[start..], cells)
}
