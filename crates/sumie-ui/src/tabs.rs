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
    min_title: u32,
    gap: u32,
    dot: u32,
    active: usize,
}

impl Sizing {
    fn minimum(self, index: usize, tab: TabMeasurement) -> u32 {
        let dot = if tab.activity && index != self.active {
            self.dot + self.gap
        } else {
            0
        };
        let gap = if tab.title != 0 { self.gap } else { 0 };
        let fixed = (self.chrome + dot + gap).saturating_add(tab.number);
        fixed
            .saturating_add(self.min_title.min(tab.title))
            .min(self.max)
    }

    // Add neighboring tabs while the minimum widths of the contiguous run still fit.
    fn visible(self, tabs: &[TabMeasurement], budget: u32) -> Range<usize> {
        if budget == 0 {
            return self.active..self.active;
        }
        let (mut first, mut end) = (self.active, self.active + 1);
        let mut remaining = budget.saturating_sub(self.minimum(first, tabs[first]));
        loop {
            let before = first..end;
            if first > 0 {
                let min = self.minimum(first - 1, tabs[first - 1]);
                if min <= remaining {
                    remaining -= min;
                    first -= 1;
                }
            }
            if end < tabs.len() {
                let min = self.minimum(end, tabs[end]);
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
        if visible.is_empty() {
            return Arc::default();
        }
        let count = u32::try_from(visible.len()).expect("visible tabs fit in the strip");
        let used = |width: u32| {
            tabs.clone()
                .map(|(i, &tab)| u64::from(width.max(self.minimum(visible.start + i, tab))))
                .sum::<u64>()
        };
        // Only tabs at their minimum can force their peers below the equal share.
        let mut width = (budget / count).min(self.max);
        let mut total = used(width);
        if total > u64::from(budget) {
            let mut low = 0;
            while low < width {
                let mid = low + (width - low).div_ceil(2);
                if used(mid) <= u64::from(budget) {
                    low = mid;
                } else {
                    width = mid - 1;
                }
            }
            total = used(width);
        }
        let mut spare = u64::from(budget).saturating_sub(total);
        let mut end = start;
        tabs.map(|(slot, &tab)| {
            let min = self.minimum(visible.start + slot, tab);
            let extra = u32::from(spare > 0 && width < self.max && width >= min);
            spare -= u64::from(extra);
            end += (width.max(min) + extra).min(budget);
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
    /// Share space equally up to the preferred width, keeping the active tab visible on overflow.
    #[must_use]
    pub fn new(
        size: [u32; 2],
        scale: u16,
        active: usize,
        measurements: &[TabMeasurement],
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
            chrome: 2 * px(tokens::TAB_PADDING) + px(tokens::CLOSE_SIZE).max(1) + gap,
            max: px(tokens::TAB_MAX_WIDTH),
            min_title: px(tokens::TAB_MIN_TITLE_WIDTH),
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

    fn content(&self, index: usize) -> PixelRect {
        let mut content = self.tab(index);
        let spare = content.size[0].saturating_sub(self.slot(index).number.max(1));
        let pad = self.px(tokens::TAB_PADDING).min(spare / 2);
        content.origin[0] += pad.cast_signed();
        content.size[0] -= 2 * pad;
        content
    }

    fn close(&self, index: usize) -> Option<PixelRect> {
        let content = self.content(index);
        let side = self.px(tokens::CLOSE_SIZE).max(1);
        let reserved = side + self.px(tokens::ITEM_GAP);
        if content.size[0] < reserved.saturating_add(self.slot(index).number)
            || content.size[1] < side
        {
            return None;
        }
        Some(rect(
            content.origin[0].cast_unsigned() + content.size[0] - side,
            (content.size[1] - side) / 2,
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
        let mut label = self.content(index);
        if let Some(close) = self.close(index) {
            label.size[0] -= close.size[0] + self.px(tokens::ITEM_GAP);
        }
        label
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

    fn underline(&self, palette: TabPalette, out: &mut Vec<Fill>) {
        let content = self.content(self.active);
        let height = self.px(tokens::INDICATOR).min(self.height);
        let y = self
            .height
            .saturating_sub(height + self.px(tokens::UNDERLINE_INSET));
        fill(
            out,
            rect(
                content.origin[0].cast_unsigned(),
                y,
                content.size[0],
                height,
            ),
            palette.active_number,
        );
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
            .close(index)
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
            self.underline(palette, out);
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
        if let Some(close) = self.close(index) {
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
