use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

/// A six-digit sRGB color in `#rrggbb` notation.
#[derive(Clone, Copy, Debug, JsonSchema)]
#[schemars(transparent)]
struct Color(#[schemars(with = "String", regex(pattern = r"^#[0-9a-fA-F]{6}$"))] [u8; 3]);

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        struct HexColor;

        impl de::Visitor<'_> for HexColor {
            type Value = Color;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a color in #rrggbb notation")
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Color, E> {
                let rgb = text
                    .strip_prefix('#')
                    .filter(|hex| hex.len() == 6 && !hex.starts_with('+'))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .ok_or_else(|| E::invalid_value(de::Unexpected::Str(text), &self))?
                    .to_be_bytes();
                Ok(Color([rgb[1], rgb[2], rgb[3]]))
            }
        }

        input.deserialize_str(HexColor)
    }
}

impl Serialize for Color {
    fn serialize<S: serde::Serializer>(&self, output: S) -> Result<S::Ok, S::Error> {
        let [r, g, b] = self.0;
        output.collect_str(&format_args!("#{r:02x}{g:02x}{b:02x}"))
    }
}

/// Terminal colors in `#rrggbb` notation.
#[derive(Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Colors {
    /// Default text color.
    #[schemars(with = "Color", default = "default_foreground")]
    foreground: Option<Color>,
    /// Default terminal background color.
    #[schemars(with = "Color", default = "default_background")]
    background: Option<Color>,
    /// Cursor color. Omit to use the foreground color.
    #[schemars(with = "Color", default = "default_foreground")]
    cursor: Option<Color>,
    /// Selected text color.
    #[schemars(with = "Color", default = "default_foreground")]
    selection_foreground: Option<Color>,
    /// Selection background color.
    #[schemars(with = "Color", default = "default_selection_background")]
    selection_background: Option<Color>,
    /// ANSI colors 0 through 15, in index order.
    #[schemars(with = "Vec<Color>", length(equal = 16), default = "default_palette")]
    palette: Option<[Color; 16]>,
}

impl Colors {
    #[must_use]
    pub fn foreground(&self) -> [u8; 3] {
        self.foreground.unwrap_or_else(default_foreground).0
    }

    #[must_use]
    pub fn background(&self) -> [u8; 3] {
        self.background.unwrap_or_else(default_background).0
    }

    #[must_use]
    pub fn cursor(&self) -> Option<[u8; 3]> {
        Some(
            self.cursor
                .map_or_else(|| self.foreground(), |color| color.0),
        )
    }

    #[must_use]
    pub fn selection_foreground(&self) -> Option<[u8; 3]> {
        Some(
            self.selection_foreground
                .unwrap_or_else(default_foreground)
                .0,
        )
    }

    #[must_use]
    pub fn selection_background(&self) -> Option<[u8; 3]> {
        Some(
            self.selection_background
                .unwrap_or_else(default_selection_background)
                .0,
        )
    }

    #[must_use]
    pub fn palette(&self) -> [[u8; 3]; 16] {
        self.palette
            .unwrap_or_else(default_palette)
            .map(|color| color.0)
    }

    pub(super) fn overlay(&mut self, file: &Self) {
        self.foreground = file.foreground.or(self.foreground);
        self.background = file.background.or(self.background);
        self.cursor = file.cursor.or(self.cursor);
        self.selection_foreground = file.selection_foreground.or(self.selection_foreground);
        self.selection_background = file.selection_background.or(self.selection_background);
        self.palette = file.palette.or(self.palette);
    }
}

const fn default_foreground() -> Color {
    Color([0xff; 3])
}

const fn default_background() -> Color {
    Color([0x12, 0x27, 0x38])
}

const fn default_selection_background() -> Color {
    Color([0x00, 0x50, 0xa4])
}

const fn default_palette() -> [Color; 16] {
    [
        Color([0x00, 0x00, 0x00]),
        Color([0xff, 0x62, 0x8c]),
        Color([0x3a, 0xd9, 0x00]),
        Color([0xff, 0xc6, 0x00]),
        Color([0x00, 0x88, 0xff]),
        Color([0xfb, 0x94, 0xff]),
        Color([0x80, 0xfc, 0xff]),
        Color([0xff; 3]),
        Color([0x00, 0x50, 0xa4]),
        Color([0xff, 0x62, 0x8c]),
        Color([0x3a, 0xd9, 0x00]),
        Color([0xff, 0xc6, 0x00]),
        Color([0x00, 0x88, 0xff]),
        Color([0xfb, 0x94, 0xff]),
        Color([0x80, 0xfc, 0xff]),
        Color([0xff; 3]),
    ]
}
