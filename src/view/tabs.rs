//! Measure and draw tab chrome in its own atlas; hit-test only the presented strip.

use sumie_core::notify::Progress;
use sumie_font::{FontError, FontOptions, FontSystem, TextGlyph, TextRun};
use sumie_render::{AtlasRect, ChromeLayer, Fill, GlyphQuad, PixelRect, Rgba};
use sumie_ui::{
    LABEL_FAMILY, LABEL_PX, LABEL_WEIGHT, NUMBER_FAMILY, NUMBER_PX, TabHit, TabLayout,
    TabMeasurement, TabPalette, tab_bar_height,
};
use sumie_vt::Snapshot;
use unicode_segmentation::UnicodeSegmentation;

use super::{
    FontErrors, Glyphs, View, attributes::glyph_quad, collect_dirty, overlays::Mask, rgba, upload,
};
use crate::Result;

pub(super) struct Tabs {
    count: usize,
    height: u32,
    presented_height: u32,
    paint: Option<TabPaint>,
    pointer: Option<(f64, f64)>,
    presented: Option<TabPlacement>,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            count: 1,
            height: 0,
            presented_height: 0,
            paint: None,
            pointer: None,
            presented: None,
        }
    }
}

#[derive(Clone)]
struct TabPlacement {
    layout: TabLayout,
    size: [u32; 2],
    active: usize,
    hover: Option<TabHit>,
}

impl TabPlacement {
    fn hovered_tab(&self) -> Option<usize> {
        match self.hover {
            Some(TabHit::Tab(index) | TabHit::Close(index)) => Some(index),
            _ => None,
        }
    }
}

#[derive(Default)]
struct TabLabel {
    source: String,
    run: TextRun,
    ink: Vec<GlyphQuad>,
    fit: Option<u32>,
}

struct TabDigits {
    ink: [Option<GlyphQuad>; 10],
    cell: u32,
}

struct TabPaint {
    font: FontSystem,
    glyphs: Glyphs,
    dirty: Vec<AtlasRect>,
    labels: Vec<TabLabel>,
    measurements: Vec<TabMeasurement>,
    digits: TabDigits,
    ellipsis: TextRun,
    scratch: String,
    frame: Option<(TabPlacement, TabPalette)>,
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
        let (_, palette) = paint.frame.as_ref()?;
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
            .and_then(|paint| paint.frame.as_ref().map(|(placement, _)| placement.clone()));
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

    /// Remember the logical position even when the pointer is outside the current strip.
    /// Returns whether the built frame needs a hover repaint.
    pub(crate) fn set_tab_pointer(&mut self, position: Option<(f64, f64)>) -> bool {
        self.tabs.pointer = position;
        self.tabs
            .paint
            .as_ref()
            .and_then(|paint| paint.frame.as_ref())
            .is_some_and(|(placement, _)| {
                let hover = position.and_then(|p| placement.layout.hit(self.window_pixel(p)));
                hover != placement.hover
            })
    }

    /// Includes the reserved close area, even before the hover frame reveals the button.
    #[cfg(test)]
    pub(crate) fn tab_hover_hit(&self, position: (f64, f64)) -> Option<TabHit> {
        self.tabs
            .presented
            .as_ref()?
            .layout
            .hit(self.window_pixel(position))
    }

    pub(crate) fn tab_hit(&self, position: (f64, f64)) -> Option<TabHit> {
        let presented = self.tabs.presented.as_ref()?;
        match presented.layout.hit(self.window_pixel(position))? {
            TabHit::Close(index)
                if index != presented.active && presented.hovered_tab() != Some(index) =>
            {
                Some(TabHit::Tab(index))
            }
            hit => Some(hit),
        }
    }

    /// Call after composing the terminal panes, before presenting. Labels, activity flags and progress
    /// are in window order.
    pub(crate) fn draw_tabs<'a>(
        &mut self,
        titles: impl Iterator<Item = (&'a str, bool, Option<Progress>)>,
        active: usize,
        snapshot: &Snapshot,
    ) {
        let scale = self.scale_units();
        let pointer = self.tabs.pointer.map(|p| self.window_pixel(p));
        let Some(paint) = &mut self.tabs.paint else {
            return;
        };
        if self.tabs.height == 0 {
            paint.frame = None;
            return;
        }
        let (measured, shaped) =
            paint.measure(self.tabs.count, active, titles, &mut self.font_errors);
        let palette = TabPalette::new(
            rgba(snapshot.background),
            rgba(snapshot.foreground),
            rgba(snapshot.ansi_yellow),
            rgba(snapshot.ansi_red),
        );
        // Paint follows the new layout; actions continue to use only the presented placement.
        let hit = |layout: &TabLayout| pointer.and_then(|point| layout.hit(point));
        let placement = match &paint.frame {
            Some((previous, previous_palette))
                if !measured && previous.size == self.device && previous.active == active =>
            {
                let hover = hit(&previous.layout);
                if !shaped && previous.hover == hover && *previous_palette == palette {
                    return;
                }
                TabPlacement {
                    hover,
                    ..previous.clone()
                }
            }
            _ => {
                let Some(layout) = TabLayout::new(self.device, scale, active, &paint.measurements)
                else {
                    paint.frame = None;
                    return;
                };
                TabPlacement {
                    hover: hit(&layout),
                    layout,
                    size: self.device,
                    active,
                }
            }
        };
        paint.build(placement, palette, &mut self.font_errors);
    }
}

impl TabPaint {
    fn new(scale: u32) -> Result<Self> {
        let scale = f32::from(u16::try_from(scale)?) / 120.0;
        let options = FontOptions {
            weight: Some(LABEL_WEIGHT),
            ..FontOptions::default()
        };
        let mut font = FontSystem::new(LABEL_FAMILY, LABEL_PX, scale, options)
            .or_else(|_| FontSystem::new(NUMBER_FAMILY, LABEL_PX, scale, options))?;
        let digits = TabDigits::new(&mut font, NUMBER_PX * scale)?;
        let ellipsis = shape_truncation_suffix(&mut font, ["…", "..."]);
        Ok(Self {
            font,
            glyphs: Glyphs::default(),
            dirty: Vec::new(),
            labels: Vec::new(),
            measurements: Vec::new(),
            digits,
            ellipsis,
            scratch: String::new(),
            frame: None,
            fills: Vec::new(),
        })
    }

    fn measure<'a>(
        &mut self,
        count: usize,
        active: usize,
        titles: impl Iterator<Item = (&'a str, bool, Option<Progress>)>,
        errors: &mut FontErrors,
    ) -> (bool, bool) {
        let mut measured = count != self.labels.len();
        let mut shaped = false;
        self.labels.resize_with(count, TabLabel::default);
        self.measurements.resize_with(count, || TabMeasurement {
            number: 0,
            title: 0,
            activity: false,
            progress: None,
        });
        for (index, ((label, measurement), (title, activity, progress))) in self
            .labels
            .iter_mut()
            .zip(&mut self.measurements)
            .zip(titles)
            .enumerate()
        {
            shaped |= label.update(title, &mut self.scratch, &mut self.font, errors);
            let next = TabMeasurement {
                number: self.digits.width(index + 1),
                title: label.run.width(),
                activity: activity && index != active,
                progress,
            };
            measured |= *measurement != next;
            *measurement = next;
        }
        (measured, shaped)
    }

    fn build(&mut self, placement: TabPlacement, palette: TabPalette, errors: &mut FontErrors) {
        self.glyphs.quads.clear();
        self.fills.clear();
        placement
            .layout
            .fills(palette, placement.hover, &mut self.fills);
        for index in placement.layout.visible() {
            self.draw_label(index, &placement, palette, errors);
        }
        collect_dirty(&self.font, &mut self.dirty);
        self.frame = Some((placement, palette));
    }

    fn draw_label(
        &mut self,
        index: usize,
        placement: &TabPlacement,
        palette: TabPalette,
        errors: &mut FontErrors,
    ) {
        let layout = &placement.layout;
        let rect = layout.number(index);
        let metrics = self.font.metrics();
        let top = (rect.size[1].cast_signed() - metrics.height.cast_signed()) / 2;
        let selected = index == placement.active;
        let color = if selected {
            palette.selected
        } else if placement.hovered_tab() == Some(index) {
            palette.hovered
        } else {
            palette.label
        };
        let digit_color = if selected {
            palette.active_number
        } else {
            palette.digits()
        };
        self.digits
            .draw(index + 1, rect, top, digit_color, &mut self.glyphs);
        layout.activity_dot(palette, index, &mut self.fills);
        let title = layout.title(index);
        let label = &mut self.labels[index];
        label.fit_ink(title.size[0], &self.ellipsis, &mut self.font, errors);
        self.glyphs.cover = Some(Mask::Inside(title));
        place_ink(&mut self.glyphs, &label.ink, [title.origin[0], top], color);
    }
}

fn shape_truncation_suffix(font: &mut FontSystem, candidates: [&str; 2]) -> TextRun {
    let mut run = TextRun::default();
    for candidate in candidates {
        if font
            .shape_text_into(candidate.graphemes(true), &mut run)
            .is_ok()
        {
            break;
        }
    }
    run
}

impl TabLabel {
    fn update(
        &mut self,
        source: &str,
        scratch: &mut String,
        font: &mut FontSystem,
        errors: &mut FontErrors,
    ) -> bool {
        if self.source == source {
            return false;
        }
        self.source.clear();
        self.source.push_str(source);
        self.fit = None;
        scratch.clear();
        if source.chars().any(char::is_control) {
            scratch.extend(
                source
                    .chars()
                    .map(|ch| if ch.is_control() { ' ' } else { ch }),
            );
        }
        loop {
            let input = if scratch.is_empty() {
                source
            } else {
                scratch.as_str()
            };
            let Err(error) = font.shape_text_into(input.graphemes(true), &mut self.run) else {
                break;
            };
            errors.warn(&error);
            let FontError::MissingGlyph(value) = error else {
                break;
            };
            let Some(ch) = char::from_u32(value).filter(|&ch| ch != '\u{fffd}') else {
                break;
            };
            if scratch.is_empty() {
                scratch.push_str(source);
            }
            let mut start = 0;
            while let Some(offset) = scratch[start..].find(ch) {
                start += offset;
                scratch.replace_range(start..start + ch.len_utf8(), "\u{fffd}");
                start += '\u{fffd}'.len_utf8();
            }
        }
        true
    }

    fn fit_ink(
        &mut self,
        width: u32,
        ellipsis: &TextRun,
        font: &mut FontSystem,
        errors: &mut FontErrors,
    ) {
        if self.fit == Some(width) {
            return;
        }
        let (prefix, suffix_x, _) = self.run.fit(width, ellipsis);
        self.ink.clear();
        cache_ink(font, errors, &mut self.ink, prefix, 0);
        if let Some(x) = suffix_x {
            cache_ink(font, errors, &mut self.ink, &ellipsis.glyphs, x);
        }
        self.fit = Some(width);
    }
}

fn cache_ink(
    font: &mut FontSystem,
    errors: &mut FontErrors,
    ink: &mut Vec<GlyphQuad>,
    run: &[TextGlyph],
    offset: i32,
) {
    for glyph in run {
        match font.rasterize(glyph.key) {
            Ok(image) if image.rect.width != 0 && image.rect.height != 0 => {
                ink.push(glyph_quad(
                    image,
                    [glyph.x + offset, glyph.y],
                    [0, 0],
                    [0; 4],
                ));
            }
            Ok(_) => (),
            Err(error) => errors.warn(&error),
        }
    }
}

impl TabDigits {
    fn new(font: &mut FontSystem, ppem: f32) -> Result<Self> {
        let (keys, cell) = font.load_digits(NUMBER_FAMILY, ppem)?;
        let mut ink = [None; 10];
        for (quad, key) in ink.iter_mut().zip(keys) {
            let image = font.rasterize(key)?;
            *quad = Some(glyph_quad(
                image,
                [0, font.metrics().baseline],
                [0, 0],
                [0; 4],
            ));
        }
        Ok(Self { ink, cell })
    }

    fn width(&self, number: usize) -> u32 {
        (number.ilog10() + 1) * self.cell
    }

    fn draw(&self, mut number: usize, rect: PixelRect, top: i32, color: Rgba, glyphs: &mut Glyphs) {
        glyphs.cover = Some(Mask::Inside(rect));
        let mut x = rect.origin[0] + (number.ilog10() * self.cell).cast_signed();
        while number != 0 {
            let digit = number % 10;
            place_ink(glyphs, self.ink[digit].as_slice(), [x, top], color);
            number /= 10;
            x -= self.cell.cast_signed();
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_truncation_glyph_tries_dots_then_omits_the_suffix() {
        const MISSING: &str = "\u{10ffff}";
        let mut font =
            FontSystem::new(LABEL_FAMILY, LABEL_PX, 1.0, FontOptions::default()).unwrap();
        let mut dots = TextRun::default();
        font.shape_text_into("...".graphemes(true), &mut dots)
            .unwrap();

        for (fallback, width, glyphs) in [("...", dots.width(), dots.glyphs.len()), (MISSING, 0, 0)]
        {
            let suffix = shape_truncation_suffix(&mut font, [MISSING, fallback]);
            assert_eq!((suffix.width(), suffix.glyphs.len()), (width, glyphs));
        }
    }
}
