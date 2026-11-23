use crate::{Config, Font, default_family};

/// Subsystems affected by a change to the resolved settings.
#[expect(
    clippy::struct_excessive_bools,
    reason = "subsystems change independently"
)]
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub font: bool,
    pub cursor: bool,
    pub terminal_colors: bool,
    pub selection_colors: bool,
    pub minimum_contrast: bool,
    pub scrollback_limit: bool,
    pub thumbnail_rows: bool,
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
            cursor: self.cursor.style() != next.cursor.style()
                || self.cursor.blink() != next.cursor.blink(),
            terminal_colors: old.foreground() != new.foreground()
                || old.background() != new.background()
                || old.cursor() != new.cursor()
                || old.palette() != new.palette(),
            selection_colors: old.selection_foreground() != new.selection_foreground()
                || old.selection_background() != new.selection_background(),
            minimum_contrast: self.minimum_contrast().to_bits()
                != next.minimum_contrast().to_bits(),
            scrollback_limit: self.scrollback_limit() != next.scrollback_limit(),
            thumbnail_rows: self.images.thumbnail_rows() != next.images.thumbnail_rows(),
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
            (&*defaults, [false; 7]),
            (
                "[font]\nsize = 18",
                [true, false, false, false, false, false, false],
            ),
            (
                "[font]\nhinting = 'off'",
                [true, false, false, false, false, false, false],
            ),
            (
                "[cursor]\nstyle = 'bar'",
                [false, true, false, false, false, false, false],
            ),
            (
                "[cursor]\nblink = false",
                [false, true, false, false, false, false, false],
            ),
            (
                "[colors]\nbackground = '#123456'",
                [false, false, true, false, false, false, false],
            ),
            (
                "[colors]\nselection-background = '#123456'",
                [false, false, false, true, false, false, false],
            ),
            (
                "minimum-contrast = 7\n[font]\nweight = 500",
                [true, false, false, false, true, false, false],
            ),
            (
                "scrollback-limit = 0",
                [false, false, false, false, false, true, false],
            ),
            (
                "[images]\nthumbnail-rows = 0",
                [false, false, false, false, false, false, true],
            ),
        ] {
            let new = toml::from_str(text).unwrap();
            let Changes {
                font,
                cursor,
                terminal_colors,
                selection_colors,
                minimum_contrast,
                scrollback_limit,
                thumbnail_rows,
            } = old.changes(&new);
            assert_eq!(
                [
                    font,
                    cursor,
                    terminal_colors,
                    selection_colors,
                    minimum_contrast,
                    scrollback_limit,
                    thumbnail_rows,
                ],
                expected,
                "{text}"
            );
        }
    }
}
