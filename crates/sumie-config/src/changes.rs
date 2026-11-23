use crate::{Config, Font, default_family};

/// Subsystems affected by a change to the resolved settings.
#[expect(
    clippy::struct_excessive_bools,
    reason = "subsystems change independently"
)]
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub font: bool,
    pub terminal_colors: bool,
    pub selection_colors: bool,
    pub minimum_contrast: bool,
}

impl Config {
    /// Compare effective values, not how defaults and includes were spelled.
    #[must_use]
    pub fn changes(&self, next: &Self) -> Changes {
        fn family(font: &Font) -> &str {
            font.family.as_deref().unwrap_or_else(|| default_family())
        }

        let old = &self.colors;
        let new = &next.colors;
        Changes {
            font: family(&self.font) != family(&next.font)
                || self.font.logical_px().to_bits() != next.font.logical_px().to_bits()
                || self.font.weight() != next.font.weight()
                || self.font.hinting() != next.font.hinting(),
            terminal_colors: old.foreground() != new.foreground()
                || old.background() != new.background()
                || old.cursor() != new.cursor()
                || old.palette() != new.palette(),
            selection_colors: old.selection_foreground() != new.selection_foreground()
                || old.selection_background() != new.selection_background(),
            minimum_contrast: self.minimum_contrast().to_bits()
                != next.minimum_contrast().to_bits(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_only_changed_settings() {
        let old = Config::default();
        let defaults = crate::defaults_toml();
        for (text, expected) in [
            (&*defaults, [false; 4]),
            ("[font]\nsize = 18", [true, false, false, false]),
            ("[font]\nhinting = 'off'", [true, false, false, false]),
            (
                "[colors]\nbackground = '#123456'",
                [false, true, false, false],
            ),
            (
                "[colors]\nselection-background = '#123456'",
                [false, false, true, false],
            ),
            (
                "minimum-contrast = 7\n[font]\nweight = 500",
                [true, false, false, true],
            ),
        ] {
            let new = toml::from_str(text).unwrap();
            let Changes {
                font,
                terminal_colors,
                selection_colors,
                minimum_contrast,
            } = old.changes(&new);
            assert_eq!(
                [font, terminal_colors, selection_colors, minimum_contrast],
                expected,
                "{text}"
            );
        }
    }
}
