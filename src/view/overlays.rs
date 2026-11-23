//! Draw the configuration banner, search bar, and match tints without touching terminal state.

use super::{Cursor, Line, PixelRect, Snapshot, View, codepoint_width, rgba};

impl View {
    pub(super) fn terminal_grid(&self) -> [u32; 2] {
        let [columns, rows] = self.grid;
        [
            columns,
            (rows - u32::from(self.search_bar.is_some())).max(1),
        ]
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

    pub(super) fn draw_search(&mut self, snapshot: &Snapshot, banner: bool) {
        self.tint_matches(snapshot, banner);
        let [columns, rows] = self.grid;
        let row = rows - 1;
        self.backgrounds.extend(std::iter::repeat_n(
            rgba(snapshot.foreground),
            columns as usize,
        ));
        let label = self.search_bar.as_deref().expect("search bar is open");
        let (query, count) = label.rsplit_once('\t').unwrap_or((label, ""));
        let count_width = self.line.ellipsize(count, columns.saturating_sub(3));
        let count_column = columns - count_width;
        let color = rgba(snapshot.background);
        self.line.draw(
            &mut self.font,
            &mut self.font_errors,
            &mut self.glyphs,
            [count_column, row],
            color,
            super::FontStyle::default(),
        );
        let available = count_column - u32::from(count_width != 0) - 1;
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
            [0, row],
            color,
            super::FontStyle::default(),
        );
        self.cursor = Some(Cursor {
            rect: PixelRect {
                origin: self.origin([query_width + preedit_width, row]),
                size: [self.cell[0].min(2), self.cell[1]],
            },
            color,
            shape: super::CursorShape::Solid,
        });
    }

    fn tint_matches(&mut self, snapshot: &Snapshot, banner: bool) {
        let columns = self.grid[0] as usize;
        for &(row, first, last, current) in &self.search_highlights {
            if (banner && row == 0) || u32::from(row) + 1 >= self.grid[1] {
                continue;
            }
            let offset = usize::from(row) * columns;
            let cells = &snapshot.rows[usize::from(row)][usize::from(first)..=usize::from(last)];
            let backgrounds =
                &mut self.backgrounds[offset + usize::from(first)..=offset + usize::from(last)];
            let weight = if current { 4 } else { 2 };
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

    fn ellipsize(&mut self, text: &str, columns: u32) -> u32 {
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
