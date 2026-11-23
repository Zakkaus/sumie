//! Compose native tab chrome in a label-sized font atlas; never reshape the hidden tabs.

use sumie_font::{FontOptions, FontStyle, FontSystem};
use sumie_render::{AtlasRect, ChromeLayer, Fill};
use sumie_ui::{LABEL_FAMILY, LABEL_PX, LABEL_WEIGHT, Palette, TabLayout, tab_bar_height};
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
    presented: Option<TabLayout>,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            count: 1,
            height: 0,
            presented_height: 0,
            paint: None,
            presented: None,
        }
    }
}

struct TabPaint {
    font: FontSystem,
    glyphs: Glyphs,
    dirty: Vec<AtlasRect>,
    labels: Vec<Line>,
    scratch: Line,
    frame: Option<(TabLayout, Palette, [Fill; 3])>,
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
        let (_, palette, fills) = paint.frame.as_ref()?;
        Some(ChromeLayer {
            fills,
            glyphs: &paint.glyphs.quads,
            text_background: palette.surface,
            atlas: upload(&paint.font, &paint.dirty),
        })
    }

    pub(super) fn presented(&mut self) {
        self.presented_height = self.height;
        if let Some(paint) = &mut self.paint {
            self.presented = paint.frame.as_ref().map(|(layout, _, _)| *layout);
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
        if let Some(paint) = &mut self.tabs.paint {
            paint.frame = None;
        }
        self.tabs.count = count;
        let old = self.tabs.height;
        self.resize_strip();
        Ok(old != self.tabs.height)
    }

    pub(super) fn resize_strip(&mut self) {
        self.tabs.height = tab_bar_height(self.tabs.count, self.scale_units(), self.device[1]);
    }

    fn scale_units(&self) -> u16 {
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
        self.presented_search = None;
    }

    pub(crate) fn over_tab_bar(&self, position: (f64, f64)) -> bool {
        let [x, y] = self.window_pixel(position);
        let occluded = self.tabs.height.max(self.tabs.presented_height);
        (0.0..f64::from(self.device[0])).contains(&x) && (0.0..f64::from(occluded)).contains(&y)
    }

    pub(crate) fn tab_hit(&self, position: (f64, f64)) -> Option<usize> {
        self.tabs.presented?.hit(self.window_pixel(position))
    }

    /// Call after composing the terminal panes, before presenting. Titles are in window order.
    pub(crate) fn draw_tabs<'a>(
        &mut self,
        titles: impl Iterator<Item = &'a str>,
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
            layout,
            active,
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
            scratch: Line::default(),
            frame: None,
        })
    }

    fn build<'a>(
        &mut self,
        layout: TabLayout,
        active: usize,
        titles: impl Iterator<Item = &'a str>,
        palette: Palette,
        errors: &mut FontErrors,
    ) {
        let cells = self.font.metrics();
        let visible = layout.visible();
        let mut changed = self
            .frame
            .as_ref()
            .is_none_or(|(g, p, _)| *g != layout || *p != palette);
        self.labels.resize_with(visible.len(), Line::default);
        for ((index, label), title) in visible
            .clone()
            .zip(&mut self.labels)
            .zip(titles.skip(visible.start))
        {
            let width = layout.label(index).size[0] / cells.width;
            self.scratch.ellipsize(title, width);
            if self.scratch.text != label.text {
                std::mem::swap(&mut self.scratch, label);
                changed = true;
            }
        }
        if !changed {
            return;
        }
        self.glyphs.quads.clear();
        for (index, label) in visible.zip(&mut self.labels) {
            let rect = layout.label(index);
            let columns = label.widths.iter().map(|&w| u32::from(w)).sum::<u32>();
            self.glyphs.offset = [
                rect.origin[0]
                    + (rect.size[0].saturating_sub(columns * cells.width) / 2).cast_signed(),
                (rect.size[1].cast_signed() - cells.height.cast_signed()) / 2,
            ];
            self.glyphs.cover = Some(Mask::Inside(rect));
            let color = if index == active {
                palette.selected
            } else {
                palette.label
            };
            label.draw(
                &mut self.font,
                errors,
                &mut self.glyphs,
                [0, 0],
                color,
                FontStyle::default(),
            );
        }
        collect_dirty(&self.font, &mut self.dirty);
        self.frame = Some((layout, palette, layout.fills(palette)));
    }
}
