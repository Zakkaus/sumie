//! D3+ terminal chrome. Paint and hits share one table of device-pixel edges.

use std::ops::Range;
use std::sync::Arc;

use sumie_render::{Fill, PixelRect, Rgba};

use crate::palette::{TabPalette, black, tint_over};
use crate::{scaled, tokens};

/// Height in device pixels at a scale given in 120ths. Leave at least one pixel for the PTY.
#[must_use]
pub fn tab_bar_height(count: usize, scale: u16, device_height: u32) -> u32 {
    if count < 2 {
        return 0;
    }
    scaled(tokens::TAB_HEIGHT, scale).min(device_height.saturating_sub(1))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabHit {
    Tab(usize),
    Close(usize),
    New,
}

/// Measured number and title advances in device pixels, before gaps or chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabMeasurement {
    pub number: u32,
    pub title: u32,
    pub activity: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot {
    end: u32,
    number: u32,
    activity: bool,
}

#[derive(Clone, Copy)]
struct Sizing {
    chrome: u32,
    max: u32,
    ellipsis: u32,
    gap: u32,
    dot: u32,
    active: usize,
}

impl Sizing {
    fn widths(self, index: usize, tab: TabMeasurement) -> (u32, u32) {
        let dot = if tab.activity && index != self.active {
            self.dot + self.gap
        } else {
            0
        };
        let gap = if tab.title != 0 { self.gap } else { 0 };
        let fixed = (self.chrome + dot + gap).saturating_add(tab.number);
        (
            fixed
                .saturating_add(self.ellipsis.min(tab.title))
                .min(self.max),
            fixed.saturating_add(tab.title).min(self.max),
        )
    }

    // Add neighboring tabs while the minimum widths of the contiguous run still fit.
    fn visible(self, tabs: &[TabMeasurement], budget: u32) -> Range<usize> {
        if budget == 0 {
            return self.active..self.active;
        }
        let (mut first, mut end) = (self.active, self.active + 1);
        let mut remaining = budget.saturating_sub(self.widths(first, tabs[first]).0);
        loop {
            let before = first..end;
            if first > 0 {
                let min = self.widths(first - 1, tabs[first - 1]).0;
                if min <= remaining {
                    remaining -= min;
                    first -= 1;
                }
            }
            if end < tabs.len() {
                let min = self.widths(end, tabs[end]).0;
                if min <= remaining {
                    remaining -= min;
                    end += 1;
                }
            }
            if before == (first..end) {
                return before;
            }
        }
    }

    fn slots(
        self,
        tabs: &[TabMeasurement],
        visible: Range<usize>,
        start: u32,
        budget: u32,
    ) -> Arc<[Slot]> {
        let tabs = tabs[visible.clone()].iter().enumerate();
        let widths = |index, &tab| self.widths(visible.start + index, tab);
        let total = |shrink: u32| {
            tabs.clone()
                .map(|(slot, tab)| {
                    let (min, ideal) = widths(slot, tab);
                    ideal.saturating_sub(shrink).max(min)
                })
                .fold(0_u32, u32::saturating_add)
        };
        let (mut lo, mut hi) = (0, self.max);
        if total(0) > budget {
            // Find the same reduction for every tab that can still shrink.
            while lo < hi {
                let mid = u32::midpoint(lo, hi);
                if total(mid) > budget {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
        }
        let mut spare = if lo == 0 {
            0
        } else {
            budget.saturating_sub(total(lo))
        };
        let mut end = start;
        // Distribute leftover pixels from rounding only among tabs that can still shrink.
        tabs.map(|(slot, tab)| {
            let (min, ideal) = widths(slot, tab);
            let reduced = ideal.saturating_sub(lo);
            let extra = u32::from(spare > 0 && reduced >= min && reduced < ideal);
            spare -= extra;
            end += (reduced.max(min) + extra).min(budget);
            Slot {
                end,
                number: tab.number,
                activity: tab.activity && visible.start + slot != self.active,
            }
        })
        .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabLayout {
    width: u32,
    height: u32,
    remaining_height: u32,
    scale: u16,
    start: u32,
    slots: Arc<[Slot]>,
    first: usize,
    active: usize,
}

impl TabLayout {
    /// Fit measured device-pixel widths, keeping a contiguous run with the active tab visible.
    #[must_use]
    pub fn new(
        size: [u32; 2],
        scale: u16,
        active: usize,
        measurements: &[TabMeasurement],
        label_cell: u32,
    ) -> Option<Self> {
        let [width, device_height] = size;
        let height = tab_bar_height(measurements.len(), scale, device_height);
        if width == 0 || height == 0 || active >= measurements.len() {
            return None;
        }
        let px = |logical| scaled(logical, scale);
        let available = width.saturating_sub(px(tokens::PLUS_WIDTH));
        let start = px(tokens::BAR_INSET).min(available.saturating_sub(1) / 2);
        let budget = available - 2 * start;
        let gap = px(tokens::ITEM_GAP);
        let sizing = Sizing {
            chrome: px(tokens::PADDING_LEFT)
                + px(tokens::PADDING_RIGHT)
                + px(tokens::CLOSE_SIZE).max(1)
                + gap,
            max: px(tokens::TAB_MAX_WIDTH),
            ellipsis: label_cell,
            gap,
            dot: px(tokens::DOT_SIZE).max(1),
            active,
        };
        let visible = sizing.visible(measurements, budget);
        let slots = sizing.slots(measurements, visible.clone(), start, budget);
        Some(Self {
            width,
            height,
            remaining_height: device_height - height,
            scale,
            start,
            slots,
            first: visible.start,
            active,
        })
    }

    #[must_use]
    pub fn visible(&self) -> Range<usize> {
        self.first..self.first + self.slots.len()
    }

    fn px(&self, logical: u32) -> u32 {
        scaled(logical, self.scale)
    }

    fn slot(&self, index: usize) -> Slot {
        self.slots[index - self.first]
    }

    fn tab(&self, index: usize) -> PixelRect {
        let slot = self.slot(index);
        let left = if index == self.first {
            self.start
        } else {
            self.slot(index - 1).end
        };
        rect(left, 0, slot.end - left, self.height)
    }

    fn close(&self, tab: PixelRect, number: u32) -> Option<PixelRect> {
        let side = self.px(tokens::CLOSE_SIZE).max(1);
        let trailing = self.px(tokens::PADDING_RIGHT) + side;
        let reserved = self.px(tokens::PADDING_LEFT) + self.px(tokens::ITEM_GAP) + trailing;
        if tab.size[0] < reserved.saturating_add(number) || tab.size[1] < side {
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
    pub fn label(&self, index: usize) -> PixelRect {
        let tab = self.tab(index);
        let number = self.slot(index).number;
        let [width, height] = tab.size;
        let left = tab.origin[0].cast_unsigned();
        let space = width.saturating_sub(number.max(1));
        let padding = self.px(tokens::PADDING_LEFT).min(space);
        let end = self.close(tab, number).map_or_else(
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
    pub fn number(&self, index: usize) -> PixelRect {
        let mut label = self.label(index);
        if let Some(dot) = self.dot(index, label) {
            let offset = dot.size[0] + self.px(tokens::ITEM_GAP);
            label.origin[0] += offset.cast_signed();
            label.size[0] -= offset;
        }
        label.size[0] = label.size[0].min(self.slot(index).number);
        label
    }

    /// The title starts after the number and any visible activity dot.
    ///
    /// # Panics
    /// The index must be in `visible()`.
    #[must_use]
    pub fn title(&self, index: usize) -> PixelRect {
        let label = self.label(index);
        let gap = self.px(tokens::ITEM_GAP);
        let dot = self.dot(index, label).map_or(0, |dot| dot.size[0] + gap);
        let skip = self
            .slot(index)
            .number
            .saturating_add(gap + dot)
            .min(label.size[0]);
        rect(
            label.origin[0].cast_unsigned() + skip,
            0,
            label.size[0] - skip,
            label.size[1],
        )
    }

    fn dot(&self, index: usize, label: PixelRect) -> Option<PixelRect> {
        let slot = self.slot(index);
        let size = self.px(tokens::DOT_SIZE).max(1);
        let needed = slot.number.saturating_add(size + self.px(tokens::ITEM_GAP));
        if !slot.activity || size > label.size[1] || needed > label.size[0] {
            return None;
        }
        Some(rect(
            label.origin[0].cast_unsigned(),
            (label.size[1] - size) / 2,
            size,
            size,
        ))
    }

    /// Append the unread-activity mark for a visible label, if there's room for it.
    ///
    /// # Panics
    /// The index must be in `visible()`.
    pub fn activity_dot(&self, palette: TabPalette, index: usize, out: &mut Vec<Fill>) {
        if let Some(dot) = self.dot(index, self.label(index)) {
            rounded(out, dot, dot.size[0], palette.active_number);
        }
    }

    /// Underline only the active label, from the number through the rendered title.
    pub fn underline(
        &self,
        palette: TabPalette,
        index: usize,
        title_end: u32,
        baseline: u32,
        out: &mut Vec<Fill>,
    ) {
        if index != self.active || !self.visible().contains(&index) {
            return;
        }
        let label = self.label(index);
        let x = label.origin[0].cast_unsigned();
        let y = baseline.saturating_add(self.px(tokens::RULE));
        let width = title_end.min(x + label.size[0]).saturating_sub(x);
        let height = self
            .px(tokens::INDICATOR)
            .min(self.height.saturating_sub(y));
        fill(out, rect(x, y, width, height), palette.active_number);
    }

    fn plus(&self) -> PixelRect {
        let left = self.slots.last().map_or(self.start, |slot| slot.end);
        rect(
            left,
            0,
            self.px(tokens::PLUS_WIDTH).min(self.width - left),
            self.height,
        )
    }

    /// Close describes the candidate box. The caller checks whether the presented frame drew it.
    #[must_use]
    pub fn hit(&self, [x, y]: [f64; 2]) -> Option<TabHit> {
        if !(f64::from(self.start)..f64::from(self.width)).contains(&x)
            || !(0.0..f64::from(self.height)).contains(&y)
        {
            return None;
        }
        let slot = self.slots.partition_point(|slot| f64::from(slot.end) <= x);
        if slot == self.slots.len() {
            return contains(self.plus(), [x, y]).then_some(TabHit::New);
        }
        let index = self.first + slot;
        if self
            .close(self.tab(index), self.slots[slot].number)
            .is_some_and(|close| contains(close, [x, y]))
        {
            return Some(TabHit::Close(index));
        }
        Some(TabHit::Tab(index))
    }

    /// Append the bar fills and affordances in paint order, reusing the caller's buffer.
    pub fn fills(&self, palette: TabPalette, hover: Option<TabHit>, out: &mut Vec<Fill>) {
        fill(out, rect(0, 0, self.width, self.height), palette.surface);
        self.shadow(out);
        self.dividers(out, tint_over(palette.surface, palette.selected, 23));

        if self.visible().contains(&self.active) {
            self.paint_tab(
                out,
                palette,
                self.active,
                hover == Some(TabHit::Close(self.active)),
            );
        }
        if let Some(TabHit::Tab(index) | TabHit::Close(index)) = hover
            && index != self.active
            && self.visible().contains(&index)
        {
            self.paint_tab(out, palette, index, hover == Some(TabHit::Close(index)));
        }
        self.paint_plus(out, palette, hover == Some(TabHit::New));
    }

    fn shadow(&self, out: &mut Vec<Fill>) {
        let depth = self.px(tokens::SHADOW_HEIGHT);
        for row in 0..depth.min(self.remaining_height) {
            let alpha = (41 * (depth - row) + depth / 2) / depth;
            let color = black(u8::try_from(alpha).expect("shadow alpha fits in a byte"));
            fill(out, rect(0, self.height + row, self.width, 1), color);
        }
        let rule = self.px(tokens::RULE).min(self.remaining_height);
        fill(out, rect(0, self.height, self.width, rule), black(89));
    }

    fn dividers(&self, out: &mut Vec<Fill>, color: Rgba) {
        let inset = self.px(tokens::DIVIDER_INSET);
        let height = self.height.saturating_sub(2 * inset);
        let rule = self.px(tokens::RULE);
        for index in self.visible().skip(1) {
            if index != self.active && index - 1 != self.active {
                fill(
                    out,
                    rect(self.slot(index - 1).end, inset, rule, height),
                    color,
                );
            }
        }
    }

    fn paint_tab(&self, out: &mut Vec<Fill>, palette: TabPalette, index: usize, close_hover: bool) {
        if let Some(close) = self.close(self.tab(index), self.slot(index).number) {
            let label = if index == self.active {
                palette.selected
            } else {
                palette.hovered
            };
            let color = if close_hover {
                rounded(
                    out,
                    close,
                    2 * self.px(tokens::CLOSE_RADIUS),
                    tint_over(palette.surface, palette.selected, 31),
                );
                label
            } else {
                tint_over(palette.surface, label, 179)
            };
            self.paint_close(out, close, color);
        }
    }

    fn paint_close(&self, out: &mut Vec<Fill>, close: PixelRect, color: Rgba) {
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

    fn paint_plus(&self, out: &mut Vec<Fill>, palette: TabPalette, hovered: bool) {
        let plus = self.plus();
        let left = plus.origin[0].cast_unsigned();
        let [width, height] = plus.size;
        let color = if hovered {
            palette.selected
        } else {
            tint_over(palette.surface, palette.selected, 140)
        };
        let side = self.px(tokens::PLUS_MARK).min(width).min(height);
        if side != 0 {
            let stroke = self.px(tokens::RULE).clamp(1, side);
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

fn contains(rect: PixelRect, point: [f64; 2]) -> bool {
    (0..2).zip(point).all(|(axis, position)| {
        let start = f64::from(rect.origin[axis]);
        (start..start + f64::from(rect.size[axis])).contains(&position)
    })
}

fn fill(out: &mut Vec<Fill>, rect: PixelRect, color: Rgba) {
    if !rect.size.contains(&0) {
        out.push(Fill { rect, color });
    }
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
mod tests;
