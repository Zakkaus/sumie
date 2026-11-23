use zeno::{Command, Fill, Mask, PathBuilder, Placement, Point, Scratch, Stroke, Style};

use crate::{Atlas, AtlasGlyph, CellMetrics, FaceId, FontError, GlyphKey};

mod box_lines;

pub(super) const FACE: FaceId = FaceId(usize::MAX);

pub(super) fn covers(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{259f}' | '\u{e0b0}'..='\u{e0bf}')
}

pub(super) fn draw(
    key: GlyphKey,
    metrics: CellMetrics,
    atlas: &mut Atlas,
    scratch: &mut Scratch,
    path: &mut Vec<Command>,
) -> Result<AtlasGlyph, FontError> {
    let CellMetrics { width, height, .. } = metrics;
    let placement = Placement {
        width,
        height,
        left: 0,
        top: 0,
    };
    atlas.insert(key, placement, |pixels, pitch| {
        let mut cell = Cell {
            pixels,
            pitch,
            width,
            height,
            light: ((height + 8) / 16).max(1),
        };
        let cp = key.glyph_id();
        match cp {
            0x2580..=0x259f => cell.block(cp),
            0x256d..=0x2573 | 0xe0b0..=0xe0bf => {
                path.clear();
                let style = if cp >= 0xe0b0 {
                    cell.powerline(cp, path)
                } else {
                    cell.box_curve(cp, path)
                };
                Mask::with_scratch(&*path, scratch)
                    .style(style)
                    .size(width, height)
                    .render_into(cell.pixels, Some(pitch));
            }
            _ => cell.box_lines(cp),
        }
    })
}

struct Cell<'a> {
    pixels: &'a mut [u8],
    pitch: usize,
    width: u32,
    height: u32,
    light: u32,
}

impl Cell<'_> {
    fn rect(&mut self, x0: u32, y0: u32, x1: u32, y1: u32, alpha: u8) {
        for y in y0..y1 {
            let row = y as usize * self.pitch;
            self.pixels[row + x0 as usize..row + x1 as usize].fill(alpha);
        }
    }

    fn block(&mut self, cp: u32) {
        let (w, h) = (self.width, self.height);
        let eighth = |n, extent| (n * extent + 4) / 8;
        let (mx, my) = (w.div_ceil(2), h.div_ceil(2));
        match cp {
            0x2580 => self.rect(0, 0, w, my, 255),
            0x2581..=0x2588 => self.rect(0, eighth(0x2588 - cp, h), w, h, 255),
            0x2589..=0x258f => self.rect(0, 0, eighth(0x2590 - cp, w), h, 255),
            0x2590 => self.rect(mx, 0, w, h, 255),
            0x2591..=0x2593 => self.rect(0, 0, w, h, [64, 128, 191][(cp - 0x2591) as usize]),
            0x2594 => self.rect(0, 0, w, eighth(1, h), 255),
            0x2595 => self.rect(eighth(7, w), 0, w, h, 255),
            _ => {
                let quadrants = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14][(cp - 0x2596) as usize];
                for (bit, (x0, y0, x1, y1)) in [
                    (0, 0, mx, my),
                    (mx, 0, w, my),
                    (0, my, mx, h),
                    (mx, my, w, h),
                ]
                .into_iter()
                .enumerate()
                {
                    if quadrants & (1 << bit) != 0 {
                        self.rect(x0, y0, x1, y1, 255);
                    }
                }
            }
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn box_curve(&self, cp: u32, path: &mut Vec<Command>) -> Style<'static> {
        let (w, h, t) = (self.width as f32, self.height as f32, self.light as f32);
        if cp >= 0x2571 {
            if cp != 0x2572 {
                path.move_to([w, 0.0]).line_to([0.0, h]);
            }
            if cp != 0x2571 {
                path.move_to([0.0, 0.0]).line_to([w, h]);
            }
        } else {
            // Share the straight strokes' snapped center, even on an odd-sized cell.
            let cx = (self.width.saturating_sub(self.light) / 2) as f32 + t * 0.5;
            let cy = (self.height.saturating_sub(self.light) / 2) as f32 + t * 0.5;
            let (ex, dx) = if matches!(cp, 0x256e | 0x256f) {
                (0.0, -1.0)
            } else {
                (w, 1.0)
            };
            let (ey, dy) = if cp >= 0x256f { (0.0, -1.0) } else { (h, 1.0) };
            let r = (ex - cx).abs().min((ey - cy).abs());
            path.move_to([ex, cy])
                .line_to([cx + dx * r, cy])
                .quad_to([cx, cy], [cx, cy + dy * r])
                .line_to([cx, ey]);
        }
        Stroke::new(t).into()
    }

    #[allow(clippy::cast_precision_loss)]
    fn powerline(&self, cp: u32, path: &mut Vec<Command>) -> Style<'static> {
        let (w, h) = (self.width as f32, self.height as f32);
        let point = |x, y| {
            Point::new(
                if cp & 2 == 0 { x } else { w - x },
                if cp >= 0xe0bc { h - y } else { y },
            )
        };
        let solid = cp & 1 == 0;
        // Keep one opaque column against the adjoining background, including at both corners.
        let base = if solid { 1.0 } else { 0.0 };
        path.move_to(point(base, 0.0));
        match cp {
            0xe0b0..=0xe0b3 => {
                path.line_to(point(w, h * 0.5)).line_to(point(base, h));
            }
            0xe0b4..=0xe0b7 => {
                // Cubic approximation of the two quarter ellipses.
                const K: f32 = 0.552_284_8;
                let cx = base + K * (w - base);
                let cy = (1.0 - K) * h * 0.5;
                path.curve_to(point(cx, 0.0), point(w, cy), point(w, h * 0.5))
                    .curve_to(point(w, h - cy), point(cx, h), point(base, h));
            }
            _ => {
                path.line_to(point(w, h));
            }
        }
        if solid {
            path.line_to(point(0.0, h)).line_to(point(0.0, 0.0)).close();
            Fill::NonZero.into()
        } else {
            Stroke::new(self.light as f32).into()
        }
    }
}
