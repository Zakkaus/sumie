//! Compose native tab chrome in a label-sized font atlas; never reshape the hidden tabs.

use sumie_font::{FontOptions, FontStyle, FontSystem};
use sumie_render::{AtlasRect, ChromeLayer, Fill, GlyphQuad, PixelRect, Rgba};
use sumie_ui::{LABEL_FAMILY, LABEL_PX, LABEL_WEIGHT, Palette, TabHit, TabLayout, tab_bar_height};
use sumie_vt::Rgb;

use super::{
    FontErrors, Glyphs, Line, View, collect_dirty, overlays::Mask, palette_for_background, upload,
};
use crate::Result;

pub(super) struct Tabs {
    count: usize,
    height: u32,
    presented_height: u32,
    paint: Option<TabPaint>,
    hover: Option<TabHit>,
    presented: Option<TabPlacement>,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            count: 1,
            height: 0,
            presented_height: 0,
            paint: None,
            hover: None,
            presented: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TabPlacement {
    layout: TabLayout,
    active: usize,
    hover: Option<TabHit>,
}

impl TabPlacement {
    fn hovered_tab(self) -> Option<usize> {
        match self.hover {
            Some(TabHit::Tab(index) | TabHit::Close(index)) => Some(index),
            _ => None,
        }
    }
}

#[derive(Default)]
struct TabLabel {
    text: String,
    ink: Glyphs,
    activity: bool,
}

#[derive(Default)]
struct TabDigits {
    ink: Glyphs,
    edges: Option<[usize; 11]>,
}

struct TabPaint {
    font: FontSystem,
    glyphs: Glyphs,
    dirty: Vec<AtlasRect>,
    labels: Vec<TabLabel>,
    digits: TabDigits,
    scratch: Line,
    frame: Option<(TabPlacement, Palette)>,
    fills: Vec<Fill>,
}

impl Tabs {
    pub(super) fn rescale(&mut self, scale: u32) -> Result<()> {
        self.paint = (self.count >= 2)
            .then(|| TabPaint::new(scale))
            .transpose()?;
        Ok(())
    }

    pub(super) fn layer(&self) -> Option<ChromeLayer<'_>> {
        let paint = self.paint.as_ref()?;
        let (_, palette) = paint.frame?;
        Some(ChromeLayer {
            fills: &paint.fills,
            glyphs: &paint.glyphs.quads,
            text_background: palette.surface,
            atlas: upload(&paint.font, &paint.dirty),
        })
    }

    pub(super) fn presented(&mut self) {
        self.presented_height = self.height;
        self.presented = self
            .paint
            .as_ref()
            .and_then(|paint| paint.frame.map(|(placement, _)| placement));
        if let Some(paint) = &mut self.paint {
            paint.font.clear_dirty_regions();
            paint.dirty.clear();
        }
    }
}

impl View {
    /// Returns true only when showing or hiding the strip changes the available PTY pixels.
    pub(crate) fn set_tab_count(&mut self, count: usize) -> Result<bool> {
        if count == self.tabs.count {
            return Ok(false);
        }
        if count >= 2 && self.tabs.paint.is_none() {
            self.tabs.paint = Some(TabPaint::new(self.scale)?);
        }
        self.reset_hits();
        self.tabs.count = count;
        let old = self.tabs.height;
        self.resize_strip();
        Ok(old != self.tabs.height)
    }

    pub(super) fn resize_strip(&mut self) {
        self.tabs.height = tab_bar_height(self.tabs.count, self.scale_units(), self.device[1]);
    }

    pub(super) fn scale_units(&self) -> u16 {
        u16::try_from(self.scale).expect("validated device scale")
    }

    pub(super) const fn bar_height(&self) -> u32 {
        self.tabs.height
    }

    /// Viewporter stretches the rounded device buffer to the exact logical size.
    pub(crate) fn window_pixel(&self, (x, y): (f64, f64)) -> [f64; 2] {
        std::array::from_fn(|i| [x, y][i] * f64::from(self.device[i]) / f64::from(self.logical[i]))
    }

    /// A layout change must not turn a hit on old chrome into an action on a different tab.
    pub(crate) fn reset_hits(&mut self) {
        self.tabs.presented = None;
        self.tabs.hover = None;
        if let Some(paint) = &mut self.tabs.paint {
            paint.frame = None;
        }
        self.presented_search = None;
    }

    pub(crate) fn over_tab_bar(&self, position: (f64, f64)) -> bool {
        let [x, y] = self.window_pixel(position);
        let occluded = self.tabs.height.max(self.tabs.presented_height);
        (0.0..f64::from(self.device[0])).contains(&x) && (0.0..f64::from(occluded)).contains(&y)
    }

    pub(crate) fn set_tab_hover(&mut self, hover: Option<TabHit>) -> bool {
        std::mem::replace(&mut self.tabs.hover, hover) != hover
    }

    /// Includes the reserved close area, even before the hover frame reveals the button.
    pub(crate) fn tab_hover_hit(&self, position: (f64, f64)) -> Option<TabHit> {
        self.tabs.presented?.layout.hit(self.window_pixel(position))
    }

    pub(crate) fn tab_hit(&self, position: (f64, f64)) -> Option<TabHit> {
        let presented = self.tabs.presented?;
        match presented.layout.hit(self.window_pixel(position))? {
            TabHit::Close(index)
                if index != presented.active && presented.hovered_tab() != Some(index) =>
            {
                Some(TabHit::Tab(index))
            }
            hit => Some(hit),
        }
    }

    /// Call after composing the terminal panes, before presenting. Labels and activity flags
    /// are in window order.
    pub(crate) fn draw_tabs<'a>(
        &mut self,
        titles: impl Iterator<Item = (&'a str, bool)>,
        active: usize,
        background: Rgb,
    ) {
        let scale = self.scale_units();
        let Some(paint) = &mut self.tabs.paint else {
            return;
        };
        let Some(layout) = TabLayout::new(
            self.device,
            scale,
            self.tabs.count,
            active,
            paint.font.metrics().width,
        ) else {
            paint.frame = None;
            return;
        };
        paint.build(
            TabPlacement {
                layout,
                active,
                hover: self.tabs.hover,
            },
            titles,
            palette_for_background(background),
            &mut self.font_errors,
        );
    }
}

impl TabPaint {
    fn new(scale: u32) -> Result<Self> {
        Ok(Self {
            font: FontSystem::new(
                LABEL_FAMILY,
                f32::from(LABEL_PX),
                f32::from(u16::try_from(scale)?) / 120.0,
                FontOptions {
                    weight: Some(LABEL_WEIGHT),
                    ..FontOptions::default()
                },
            )?,
            glyphs: Glyphs::default(),
            dirty: Vec::new(),
            labels: Vec::new(),
            digits: TabDigits::default(),
            scratch: Line::default(),
            frame: None,
            fills: Vec::new(),
        })
    }

    fn build<'a>(
        &mut self,
        placement: TabPlacement,
        titles: impl Iterator<Item = (&'a str, bool)>,
        palette: Palette,
        errors: &mut FontErrors,
    ) {
        let labels_changed = self.update_labels(placement, titles, errors);
        if !labels_changed && self.frame == Some((placement, palette)) {
            return;
        }
        self.digits
            .prepare(&mut self.font, &mut self.scratch, errors);
        self.glyphs.quads.clear();
        self.fills.clear();
        let TabPlacement {
            layout,
            active,
            hover,
        } = placement;
        layout.fills(palette, hover, &mut self.fills);
        let cells = self.font.metrics();
        let hovered = placement.hovered_tab();
        for (index, label) in layout.visible().zip(&self.labels) {
            let number = index + 1;
            let number_width = number_width(number, cells.width);
            let rect = layout.number(index, number_width);
            let top = (rect.size[1].cast_signed() - cells.height.cast_signed()) / 2;
            self.digits.draw(
                number,
                cells.width,
                rect,
                top,
                if index == active {
                    palette.active_number
                } else {
                    palette.muted
                },
                &mut self.glyphs,
            );
            if label.activity {
                layout.activity_dot(palette, index, number_width, &mut self.fills);
            }
            let rect = layout.title(index, number_width, label.activity);
            self.glyphs.cover = Some(Mask::Inside(rect));
            place_ink(
                &mut self.glyphs,
                &label.ink.quads,
                [rect.origin[0], top],
                if index == active || hovered == Some(index) {
                    palette.selected
                } else {
                    palette.label
                },
            );
        }
        collect_dirty(&self.font, &mut self.dirty);
        self.frame = Some((placement, palette));
    }

    fn update_labels<'a>(
        &mut self,
        placement: TabPlacement,
        titles: impl Iterator<Item = (&'a str, bool)>,
        errors: &mut FontErrors,
    ) -> bool {
        let visible = placement.layout.visible();
        self.labels
            .resize_with(self.labels.len().max(visible.len()), TabLabel::default);
        let cell_width = self.font.metrics().width;
        let mut changed = false;
        for ((index, label), (title, activity)) in visible
            .clone()
            .zip(&mut self.labels)
            .zip(titles.skip(visible.start))
        {
            let activity = activity && index != placement.active;
            changed |= std::mem::replace(&mut label.activity, activity) != activity;
            let width = placement
                .layout
                .title(index, number_width(index + 1, cell_width), activity)
                .size[0];
            self.scratch.ellipsize(title, width / cell_width);
            if label.text == self.scratch.text {
                continue;
            }
            // Cache the clipped source separately: drawing can replace unsupported glyphs.
            label.text.clone_from(&self.scratch.text);
            label.ink.quads.clear();
            cache_ink(&mut self.scratch, &mut self.font, errors, &mut label.ink);
            changed = true;
        }
        changed
    }
}

impl TabDigits {
    fn prepare(&mut self, font: &mut FontSystem, scratch: &mut Line, errors: &mut FontErrors) {
        if self.edges.is_some() {
            return;
        }
        // The existing ChromeLayer has one atlas at the label size. Cache its ten
        // tabular digits once, independent of which numbers happen to be visible.
        let mut edges = [0; 11];
        for (edge, digit) in edges[1..].iter_mut().zip('0'..='9') {
            scratch.clear();
            scratch.text.push(digit);
            scratch.widths.push(1);
            cache_ink(scratch, font, errors, &mut self.ink);
            *edge = self.ink.quads.len();
        }
        self.edges = Some(edges);
    }

    fn draw(
        &self,
        mut number: usize,
        cell_width: u32,
        rect: PixelRect,
        top: i32,
        color: Rgba,
        glyphs: &mut Glyphs,
    ) {
        let edges = self.edges.as_ref().expect("tab digits prepared");
        glyphs.cover = Some(Mask::Inside(rect));
        let mut x = rect.origin[0] + (number.ilog10() * cell_width).cast_signed();
        while number != 0 {
            let digit = number % 10;
            place_ink(
                glyphs,
                &self.ink.quads[edges[digit]..edges[digit + 1]],
                [x, top],
                color,
            );
            number /= 10;
            x -= cell_width.cast_signed();
        }
    }
}

fn number_width(number: usize, cell_width: u32) -> u32 {
    (number.ilog10() + 1) * cell_width
}

fn cache_ink(line: &mut Line, font: &mut FontSystem, errors: &mut FontErrors, ink: &mut Glyphs) {
    line.draw(font, errors, ink, [0, 0], [0; 4], FontStyle::default());
}

fn place_ink(target: &mut Glyphs, ink: &[GlyphQuad], [x, y]: [i32; 2], color: Rgba) {
    for &glyph in ink {
        let mut glyph = glyph;
        glyph.dest.origin[0] += x;
        glyph.dest.origin[1] += y;
        glyph.color = color;
        target.push(glyph);
    }
}
