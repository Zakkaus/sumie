//! A divided, fill-width Astryx `TabList`. Layout and hit testing share integer edges.

use std::ops::Range;

use sumie_render::{Fill, PixelRect, Rgba};

use crate::tokens;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub surface: Rgba,
    pub label: Rgba,
    pub selected: Rgba,
    pub(crate) accent: Rgba,
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
pub struct TabLayout {
    width: u32,
    height: u32,
    scale: u16,
    padding: u32,
    slots: u32,
    first: usize,
    active: usize,
}

impl TabLayout {
    /// Keep the active tab in view when the strip cannot fit every label and its inline padding.
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
        let padding = scaled(tokens::PADDING, scale);
        let minimum = (2 * padding).saturating_add(label_cell).max(1);
        let slots = (width / minimum).clamp(1, u32::try_from(count).unwrap_or(u32::MAX));
        let first = active
            .saturating_sub(slots as usize / 2)
            .min(count - slots as usize);
        let padding = padding.min((width / slots).saturating_sub(label_cell) / 2);
        Some(Self {
            width,
            height,
            scale,
            padding,
            slots,
            first,
            active,
        })
    }

    #[must_use]
    pub const fn visible(self) -> Range<usize> {
        self.first..self.first + self.slots as usize
    }

    fn edge(self, slot: u32) -> u32 {
        u32::try_from(u64::from(self.width) * u64::from(slot) / u64::from(self.slots))
            .expect("edge within strip")
    }

    /// Content box for a visible label, excluding inline padding and the divider rail.
    ///
    /// # Panics
    /// The index must be in `visible()`.
    #[must_use]
    pub fn label(self, index: usize) -> PixelRect {
        let slot = u32::try_from(index - self.first).expect("visible tab slot");
        let left = self.edge(slot);
        let width = self.edge(slot + 1) - left;
        let pad = self.padding;
        PixelRect {
            origin: [(left + pad).cast_signed(), 0],
            size: [
                width - 2 * pad,
                self.height.min(scaled(tokens::TAB_HEIGHT, self.scale)),
            ],
        }
    }

    /// Device-pixel coordinates in the window. A boundary pixel belongs to the tab to its right.
    #[must_use]
    pub fn hit(self, [x, y]: [f64; 2]) -> Option<usize> {
        if !(0.0..f64::from(self.width)).contains(&x) || !(0.0..f64::from(self.height)).contains(&y)
        {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "bounded window coordinate"
        )]
        let pixel = x as u32;
        let slot = (u64::from(pixel + 1) * u64::from(self.slots) - 1) / u64::from(self.width);
        Some(self.first + usize::try_from(slot).ok()?)
    }

    #[must_use]
    pub fn fills(self, palette: Palette) -> [Fill; 3] {
        let rail = |left, width, thickness, color| {
            let h = self.height.min(scaled(thickness, self.scale).max(1));
            Fill {
                rect: PixelRect {
                    origin: [left, (self.height - h).cast_signed()],
                    size: [width, h],
                },
                color,
            }
        };
        let label = self.label(self.active);
        [
            Fill {
                rect: PixelRect {
                    origin: [0, 0],
                    size: [self.width, self.height],
                },
                color: palette.surface,
            },
            rail(0, self.width, tokens::RULE, palette.rule),
            rail(
                label.origin[0],
                label.size[0],
                tokens::INDICATOR,
                palette.accent,
            ),
        ]
    }
}
