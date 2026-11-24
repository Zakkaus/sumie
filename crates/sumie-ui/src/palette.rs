use sumie_render::{Rgba, ThumbnailChrome};

use crate::tokens;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub surface: Rgba,
    pub label: Rgba,
    pub selected: Rgba,
    pub muted: Rgba,
    pub active_number: Rgba,
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
        if is_light(linear_luminance) {
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
            chip_size: crate::scaled(tokens::THUMBNAIL_CHIP, scale),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabPalette {
    pub surface: Rgba,
    pub label: Rgba,
    pub hovered: Rgba,
    pub selected: Rgba,
    pub active_number: Rgba,
    digits: Rgba,
}

impl TabPalette {
    /// Derive the strip from the terminal's default colors and ANSI yellow, ignoring input alpha.
    #[must_use]
    pub fn new(background: Rgba, foreground: Rgba, mut yellow: Rgba) -> Self {
        let luminance = relative_luminance(background);
        let surface = tint_over(
            background,
            black(255),
            if is_light(luminance) { 10 } else { 56 },
        );
        let bar = relative_luminance(surface) + 0.05;
        let contrasts = |color| {
            let accent = relative_luminance(color) + 0.05;
            bar.max(accent) >= 3.0 * bar.min(accent)
        };
        if !contrasts(yellow) {
            let accent = Palette::for_luminance(luminance).accent;
            yellow = if contrasts(accent) {
                accent
            } else {
                foreground
            };
        }
        yellow[3] = 255;
        Self {
            surface,
            label: tint_over(surface, foreground, 128),
            hovered: tint_over(surface, foreground, 209),
            selected: tint_over(surface, foreground, 255),
            active_number: yellow,
            digits: tint_over(surface, foreground, 153),
        }
    }

    /// The inactive number color, at 60% terminal foreground regardless of hover.
    #[must_use]
    pub const fn digits(self) -> Rgba {
        self.digits
    }
}

fn is_light(luminance: f64) -> bool {
    (luminance + 0.05).powi(2) >= 1.05 * 0.05
}

fn relative_luminance([r, g, b, _]: Rgba) -> f64 {
    let linear = |channel| {
        let srgb = f64::from(channel) / 255.0;
        if srgb <= 0.04045 {
            srgb / 12.92
        } else {
            ((srgb + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

pub(super) const fn black(alpha: u8) -> Rgba {
    [0, 0, 0, alpha]
}

/// Bake the CSS-style sRGB tint for an opaque surface before linear GPU blending.
pub(super) fn tint_over(base: Rgba, color: Rgba, alpha: u8) -> Rgba {
    let alpha = u16::from(alpha);
    let channel = |i: usize| {
        let sum = u16::from(base[i]) * (255 - alpha) + u16::from(color[i]) * alpha;
        u8::try_from((sum + 127) / 255).expect("weighted color channel")
    };
    [channel(0), channel(1), channel(2), 255]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_colors_determine_tints_and_contrasting_accent() {
        let yellow = [255, 198, 0, 255];
        let cobalt = TabPalette::new([18, 39, 56, 255], [255; 4], yellow);
        assert_eq!(cobalt.surface, [14, 30, 44, 255]);
        assert_eq!(cobalt.label, [135, 143, 150, 255]);
        assert_eq!(cobalt.hovered, [212, 214, 217, 255]);
        assert_eq!(cobalt.digits(), [159, 165, 171, 255]);
        assert_eq!(cobalt.active_number, yellow);

        let light = TabPalette::new([255; 4], black(255), yellow);
        assert_eq!(light.surface, [245, 245, 245, 255]);
        assert_eq!(light.selected, black(255));
        assert_eq!(light.active_number, tokens::LIGHT.accent);

        let gray = [128, 128, 128, 255];
        assert_eq!(
            TabPalette::new(gray, [255; 4], gray).active_number,
            [255; 4]
        );
    }
}
