//! A divided Astryx tab bar. Paint and hits share integer device-pixel edges.

use std::ops::Range;

use sumie_render::{Fill, PixelRect, Rgba, ThumbnailChrome};

use crate::tokens;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub surface: Rgba,
    pub label: Rgba,
    pub selected: Rgba,
    pub muted: Rgba,
    pub active_number: Rgba,
    pub(crate) affordance: Rgba,
    pub(crate) active: Rgba,
    pub(crate) hover: Rgba,
    pub(crate) close_hover: Rgba,
    pub(crate) accent: Rgba,
    pub(crate) on_accent: Rgba,
    pub rule: Rgba,
    pub inactive_pane: Rgba,
    pub overlay: Rgba,
}

impl Palette {
    /// Choose the matching color scheme at the WCAG black/white contrast crossover.
    #[must_use]
    pub fn for_luminance(linear_luminance: f64) -> Self {
        if (linear_luminance + 0.05).powi(2) >= 1.05 * 0.05 {
            tokens::LIGHT
        } else {
            tokens::DARK
        }
    }

    #[must_use]
    pub fn link_hint(self) -> Rgba {
        let [r, g, b, _] = self.accent;
        [r, g, b, 128]
    }

    #[must_use]
    pub fn thumbnails(self, scale: u16, hovered: Option<usize>) -> ThumbnailChrome {
        ThumbnailChrome {
            hovered,
            border: self.rule,
            accent: self.accent,
            on_accent: self.on_accent,
            chip_size: scaled(tokens::THUMBNAIL_CHIP, scale),
        }
    }
}

fn scaled(logical: u32, scale: u16) -> u32 {
    (logical * u32::from(scale) + 60) / 120
}

/// Height in device pixels at a scale given in 120ths. Leave at least one pixel for the PTY.
#[must_use]
pub fn tab_bar_height(count: usize, scale: u16, device_height: u32) -> u32 {
    if count < 2 {
        return 0;
    }
    scaled(tokens::TAB_HEIGHT + tokens::RAIL_GAP + tokens::RULE, scale)
        .min(device_height.saturating_sub(1))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabHit {
    Tab(usize),
    Close(usize),
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabLayout {
    width: u32,
    height: u32,
    scale: u16,
    tab_height: u32,
    tab_width: u32,
    rule: u32,
    number_reserve: u32,
    slots: u32,
    first: usize,
    active: usize,
}

impl TabLayout {
    /// Reserve the new-tab button and keep the active tab in the visible run.
    #[must_use]
    pub fn new(
        size: [u32; 2],
        scale: u16,
        count: usize,
        active: usize,
        label_cell: u32,
    ) -> Option<Self> {
        let [width, device_height] = size;
        let height = tab_bar_height(count, scale, device_height);
        if width == 0 || height == 0 || active >= count {
            return None;
        }
        let tab_width = width.saturating_sub(scaled(tokens::PLUS_WIDTH, scale));
        let px = |logical| scaled(logical, scale);
        let number_reserve = label_cell.saturating_mul(count.ilog10() + 1);
        let minimum = (px(tokens::PADDING_LEFT)
            + px(tokens::PADDING_RIGHT)
            + px(tokens::CLOSE_SIZE)
            + 3 * px(tokens::ITEM_GAP)
            + px(tokens::DOT_SIZE)
            - px(tokens::DOT_PULL))
        .saturating_add(number_reserve)
        .saturating_add(label_cell)
        .max(1);
        let slots = if tab_width == 0 {
            0
        } else {
            (tab_width / minimum).clamp(1, u32::try_from(count).unwrap_or(u32::MAX))
        };
        let first = active
            .saturating_sub(slots as usize / 2)
            .min(count - slots as usize);
        let rule = px(tokens::RULE).min(height - 1);
        Some(Self {
            width,
            height,
            scale,
            tab_height: px(tokens::TAB_HEIGHT).min(height - rule),
            tab_width,
            rule,
            number_reserve,
            slots,
            first,
            active,
        })
    }

    #[must_use]
    pub const fn visible(self) -> Range<usize> {
        self.first..self.first + self.slots as usize
    }

    fn px(self, logical: u32) -> u32 {
        scaled(logical, self.scale)
    }

    fn edge(self, slot: u32) -> u32 {
        u32::try_from(u64::from(self.tab_width) * u64::from(slot) / u64::from(self.slots))
            .expect("edge within strip")
    }

    fn tab(self, index: usize) -> PixelRect {
        assert!(self.visible().contains(&index), "tab index not visible");
        let slot = u32::try_from(index - self.first).expect("visible tab slot");
        let left = self.edge(slot);
        rect(left, 0, self.edge(slot + 1) - left, self.tab_height)
    }

    fn close(self, tab: PixelRect) -> Option<PixelRect> {
        let side = self.px(tokens::CLOSE_SIZE).max(1);
        let trailing = self.px(tokens::PADDING_RIGHT) + side;
        let reserved = self.px(tokens::PADDING_LEFT) + self.px(tokens::ITEM_GAP) + trailing;
        if tab.size[0] < reserved.saturating_add(self.number_reserve) || tab.size[1] < side {
            return None;
        }
        Some(rect(
            tab.origin[0].cast_unsigned() + tab.size[0] - trailing,
            (tab.size[1] - side) / 2,
            side,
            side,
        ))
    }

    /// Padded content box for the number, optional dot and title, excluding the close button.
    ///
    /// # Panics
    /// The index must be in `visible()`.
    #[must_use]
    pub fn label(self, index: usize) -> PixelRect {
        let tab = self.tab(index);
        let [width, height] = tab.size;
        let left = tab.origin[0].cast_unsigned();
        let space = width.saturating_sub(self.number_reserve.max(1));
        let padding = self.px(tokens::PADDING_LEFT).min(space);
        let end = self.close(tab).map_or_else(
            || left + width - self.px(tokens::PADDING_RIGHT).min(space - padding),
            |close| close.origin[0].cast_unsigned() - self.px(tokens::ITEM_GAP),
        );
        rect(left + padding, 0, end - left - padding, height)
    }

    /// The leading number box, given its measured width in device pixels.
    ///
    /// # Panics
    /// The index must be in `visible()`.
    #[must_use]
    pub fn number(self, index: usize, number_width: u32) -> PixelRect {
        let mut label = self.label(index);
        label.size[0] = label.size[0].min(number_width);
        label
    }

    /// The title starts after the number and any visible activity dot.
    ///
    /// # Panics
    /// The index must be in `visible()`.
    #[must_use]
    pub fn title(self, index: usize, number_width: u32, activity: bool) -> PixelRect {
        let mut label = self.label(index);
        let gap = self.px(tokens::ITEM_GAP);
        let mut skip = number_width.saturating_add(gap).min(label.size[0]);
        if activity && let Some(dot) = self.dot(index, label, number_width) {
            skip =
                dot.origin[0].cast_unsigned() - label.origin[0].cast_unsigned() + dot.size[0] + gap;
        }
        label.origin[0] = (label.origin[0].cast_unsigned() + skip).cast_signed();
        label.size[0] -= skip;
        label
    }

    fn dot(self, index: usize, label: PixelRect, number_width: u32) -> Option<PixelRect> {
        let size = self.px(tokens::DOT_SIZE).max(1);
        let gap = self.px(tokens::ITEM_GAP);
        let offset = number_width.saturating_add(gap - self.px(tokens::DOT_PULL));
        if index == self.active
            || size > label.size[1]
            || offset.saturating_add(size + gap) >= label.size[0]
        {
            return None;
        }
        Some(rect(
            label.origin[0].cast_unsigned() + offset,
            (label.size[1] - size) / 2,
            size,
            size,
        ))
    }

    /// Append the unread-activity mark for a visible label, if there's room for it.
    ///
    /// # Panics
    /// The index must be in `visible()`.
    pub fn activity_dot(
        self,
        palette: Palette,
        index: usize,
        number_width: u32,
        out: &mut Vec<Fill>,
    ) {
        if let Some(dot) = self.dot(index, self.label(index), number_width) {
            rounded(out, dot, dot.size[0], palette.accent);
        }
    }

    fn plus(self) -> PixelRect {
        rect(
            self.tab_width,
            0,
            self.width - self.tab_width,
            self.tab_height,
        )
    }

    /// A boundary pixel belongs to the button to its right. Close describes the candidate box;
    /// the caller must check whether that close button was shown in the presented frame.
    #[must_use]
    pub fn hit(self, [x, y]: [f64; 2]) -> Option<TabHit> {
        if !(0.0..f64::from(self.width)).contains(&x)
            || !(0.0..f64::from(self.tab_height)).contains(&y)
        {
            return None;
        }
        if x >= f64::from(self.tab_width) {
            return Some(TabHit::New);
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "bounded window coordinate"
        )]
        let pixel = x as u32;
        let slot = (u64::from(pixel + 1) * u64::from(self.slots) - 1) / u64::from(self.tab_width);
        let index = self.first + usize::try_from(slot).ok()?;
        if let Some(close) = self.close(self.tab(index))
            && (0..2).zip([x, y]).all(|(i, point)| {
                let start = f64::from(close.origin[i]);
                (start..start + f64::from(close.size[i])).contains(&point)
            })
        {
            return Some(TabHit::Close(index));
        }
        Some(TabHit::Tab(index))
    }

    /// Append the bar fills and affordances in paint order, reusing the caller's buffer.
    pub fn fills(self, palette: Palette, hover: Option<TabHit>, out: &mut Vec<Fill>) {
        let rule = blend(palette.surface, palette.rule);
        let rule_y = self.height - self.rule;
        fill(out, rect(0, 0, self.width, self.height), palette.surface);
        fill(out, rect(0, rule_y, self.width, self.rule), rule);

        if self.slots != 0 {
            let hovered = match hover {
                Some(TabHit::Tab(index) | TabHit::Close(index)) => Some(index),
                _ => None,
            };
            self.paint_tab(
                out,
                palette,
                self.tab(self.active),
                true,
                hover == Some(TabHit::Close(self.active)),
            );
            if let Some(index) =
                hovered.filter(|&i| i != self.active && self.visible().contains(&i))
            {
                self.paint_tab(
                    out,
                    palette,
                    self.tab(index),
                    false,
                    hover == Some(TabHit::Close(index)),
                );
            }
            self.dividers(out, rule);
        }
        self.paint_plus(out, palette, rule, hover == Some(TabHit::New));
    }

    fn dividers(self, out: &mut Vec<Fill>, color: Rgba) {
        let inset = self.px(tokens::DIVIDER_INSET);
        let height = self.tab_height.saturating_sub(2 * inset);
        for slot in 1..self.slots {
            let index = self.first + slot as usize;
            if index != self.active && index - 1 != self.active {
                fill(out, rect(self.edge(slot), inset, self.rule, height), color);
            }
        }
    }

    fn paint_tab(
        self,
        out: &mut Vec<Fill>,
        palette: Palette,
        tab: PixelRect,
        active: bool,
        close_hover: bool,
    ) {
        let background = if active {
            let rule_y = self.height - self.rule;
            let indicator = self.px(tokens::INDICATOR).min(rule_y - self.tab_height);
            let rail_y = rule_y - indicator;
            let x = tab.origin[0].cast_unsigned();
            let width = tab.size[0];
            fill(out, rect(x, 0, width, rail_y), palette.active);
            fill(out, rect(x, rail_y, width, indicator), palette.accent);
            palette.active
        } else {
            fill(out, tab, palette.hover);
            palette.hover
        };
        if let Some(close) = self.close(tab) {
            let color = if close_hover {
                rounded(
                    out,
                    close,
                    2 * self.px(tokens::CLOSE_RADIUS),
                    blend(background, palette.close_hover),
                );
                palette.selected
            } else if active {
                palette.affordance
            } else {
                palette.muted
            };
            self.paint_close(out, close, color);
        }
    }

    fn paint_close(self, out: &mut Vec<Fill>, close: PixelRect, color: Rgba) {
        let [x, y] = close.origin.map(i32::cast_unsigned);
        let side = self.px(tokens::CLOSE_MARK).max(1).min(close.size[0]);
        // A diagonal needs a wider scanline than the axis-aligned rule to carry the same weight.
        let stroke = ((side + 3) / 6).clamp(1, side);
        let pad = (close.size[0] - side) / 2;
        // Rasterize the two diagonal strokes together so joined pixels aren't painted twice.
        for row in 0..side {
            let start = row.min(side - 1 - row).saturating_sub((stroke - 1) / 2);
            let end = (start + stroke).min(side);
            let right = side - end;
            let y = y + pad + row;
            let x = x + pad;
            if end >= right {
                let left = start.min(right);
                fill(out, rect(x + left, y, side - 2 * left, 1), color);
            } else {
                fill(out, rect(x + start, y, end - start, 1), color);
                fill(out, rect(x + right, y, end - start, 1), color);
            }
        }
    }

    fn paint_plus(self, out: &mut Vec<Fill>, palette: Palette, rule: Rgba, hovered: bool) {
        let plus = self.plus();
        let [left, _] = plus.origin.map(i32::cast_unsigned);
        let [width, height] = plus.size;
        let color = if hovered {
            fill(out, plus, palette.hover);
            palette.selected
        } else {
            palette.affordance
        };
        if left != 0 {
            fill(out, rect(left, 0, self.rule.min(width), height), rule);
        }
        let side = self.px(tokens::PLUS_MARK).max(1).min(width).min(height);
        if side != 0 {
            let stroke = self.rule.clamp(1, side);
            let x = left + (width - side) / 2;
            let y = (height - side) / 2;
            let pad = (side - stroke) / 2;
            fill(out, rect(x + pad, y, stroke, side), color);
            fill(out, rect(x, y + pad, side, stroke), color);
        }
    }
}

fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
    PixelRect {
        origin: [x.cast_signed(), y.cast_signed()],
        size: [width, height],
    }
}

fn fill(out: &mut Vec<Fill>, rect: PixelRect, color: Rgba) {
    if !rect.size.contains(&0) {
        out.push(Fill { rect, color });
    }
}

fn blend(base: Rgba, over: Rgba) -> Rgba {
    let alpha = u16::from(over[3]);
    let channel = |i: usize| {
        let sum = u16::from(base[i]) * (255 - alpha) + u16::from(over[i]) * alpha;
        u8::try_from((sum + 127) / 255).expect("weighted color channel")
    };
    [channel(0), channel(1), channel(2), 255]
}

fn rounded(out: &mut Vec<Fill>, rect_box: PixelRect, diameter: u32, color: Rgba) {
    let [x, y] = rect_box.origin.map(i32::cast_unsigned);
    let [width, height] = rect_box.size;
    let d = diameter.min(width).min(height);
    let cap_height = d / 2;
    let mut cap = |start, end, inset| {
        fill(
            out,
            rect(x + inset, y + start, width - 2 * inset, end - start),
            color,
        );
        fill(
            out,
            rect(x + inset, y + height - end, width - 2 * inset, end - start),
            color,
        );
    };
    // Test pixel centers against the circular corner in doubled coordinates; coalesce equal rows.
    let mut start = 0;
    let mut inset = cap_height;
    for row in 0..cap_height {
        let dy = d - 1 - 2 * row;
        let previous = inset;
        while inset > 0 {
            let dx = d + 1 - 2 * inset;
            if dx * dx + dy * dy > d * d {
                break;
            }
            inset -= 1;
        }
        if inset != previous {
            cap(start, row, previous);
            start = row;
        }
    }
    if inset != 0 {
        cap(start, cap_height, inset);
        start = cap_height;
    }
    fill(out, rect(x, y + start, width, height - 2 * start), color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_tabs_share_edges_with_hit_regions() {
        for (scale, body, strip, edge, close_x, close_y, side, padding, gap) in [
            (120, 32, 629, 209, 185, 7, 18, 12, 8),
            (150, 40, 620, 206, 175, 8, 23, 15, 10),
            (180, 48, 611, 203, 167, 10, 27, 18, 12),
            (240, 64, 593, 197, 149, 14, 36, 24, 16),
        ] {
            let layout = TabLayout::new([665, 120], scale, 3, 1, scaled(8, scale)).unwrap();
            assert_eq!(layout.visible(), 0..3);
            assert_eq!(
                layout.label(0),
                rect(padding, 0, close_x - gap - padding, body)
            );
            let [body, strip, edge, close_x, close_y, side] =
                [body, strip, edge, close_x, close_y, side].map(f64::from);
            for (point, expected) in [
                ([close_x - 0.01, close_y], Some(TabHit::Tab(0))),
                ([close_x, close_y - 0.01], Some(TabHit::Tab(0))),
                ([close_x, close_y], Some(TabHit::Close(0))),
                (
                    [close_x + side - 0.01, close_y + side - 0.01],
                    Some(TabHit::Close(0)),
                ),
                ([close_x + side, close_y], Some(TabHit::Tab(0))),
                ([edge - 0.01, 0.0], Some(TabHit::Tab(0))),
                ([edge, 0.0], Some(TabHit::Tab(1))),
                ([strip - 0.01, 0.0], Some(TabHit::Tab(2))),
                ([strip, body - 0.01], Some(TabHit::New)),
                ([strip, body], None),
                ([665.0, 0.0], None),
            ] {
                assert_eq!(
                    layout.hit(point),
                    expected,
                    "scale {scale}, point {point:?}"
                );
            }
        }
    }

    #[test]
    fn overflow_keeps_active_and_plus_in_view() {
        assert_eq!(tab_bar_height(1, 120, 100), 0);
        let layout = TabLayout::new([250, 100], 120, 16, 12, 8).unwrap();
        assert_eq!(layout.visible(), 11..13);
        assert_eq!(layout.hit([107.0, 0.0]), Some(TabHit::Tab(12)));
        assert_eq!(layout.hit([214.0, 0.0]), Some(TabHit::New));

        let clipped = TabLayout::new([35, 2], 120, 16, 12, 8).unwrap();
        assert!(clipped.visible().is_empty());
        assert_eq!(clipped.hit([0.0, 0.0]), Some(TabHit::New));
        assert_eq!(clipped.hit([0.0, 1.0]), None);
    }
}
