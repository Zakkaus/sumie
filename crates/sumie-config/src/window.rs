use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::Problem;

/// Window background and pane padding in logical pixels.
#[derive(Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Window {
    /// Left and right padding. Set to 0 to remove it.
    #[schemars(
        with = "u16",
        default = "default_padding_x",
        title = "Horizontal padding"
    )]
    padding_x: Option<u16>,
    /// Top and bottom padding. Set to 0 to remove it.
    #[schemars(
        with = "u16",
        default = "default_padding_y",
        title = "Vertical padding"
    )]
    padding_y: Option<u16>,
    /// Opacity of the default terminal background, including pane padding and split gutters.
    #[serde(deserialize_with = "deserialize_opacity")]
    #[schemars(
        with = "f32",
        range(min = 0.0, max = 1.0),
        default = "opaque",
        title = "Background opacity"
    )]
    background_opacity: Option<f32>,
    /// Ask the Wayland compositor to blur the area behind a translucent window.
    #[schemars(with = "bool", default = "bool::default", title = "Background blur")]
    background_blur: Option<bool>,
}

impl Window {
    #[must_use]
    pub fn padding(&self) -> [u16; 2] {
        [
            self.padding_x.unwrap_or_else(default_padding_x),
            self.padding_y.unwrap_or_else(default_padding_y),
        ]
    }

    #[must_use]
    pub fn background_opacity(&self) -> f32 {
        self.background_opacity.unwrap_or_else(opaque)
    }

    #[must_use]
    pub fn background_blur(&self) -> bool {
        self.background_blur.unwrap_or_default()
    }

    pub(super) fn overlay(&mut self, file: &Self) {
        self.padding_x = file.padding_x.or(self.padding_x);
        self.padding_y = file.padding_y.or(self.padding_y);
        self.background_opacity = file.background_opacity.or(self.background_opacity);
        self.background_blur = file.background_blur.or(self.background_blur);
    }
}

const fn default_padding_x() -> u16 {
    8
}

const fn default_padding_y() -> u16 {
    6
}

const fn opaque() -> f32 {
    1.0
}

fn deserialize_opacity<'de, D: Deserializer<'de>>(input: D) -> Result<Option<f32>, D::Error> {
    let opacity = crate::checked(input, opacity)?;
    #[expect(clippy::cast_possible_truncation, reason = "validated unit interval")]
    Ok(opacity.map(|opacity| opacity as f32))
}

pub(crate) fn opacity(opacity: f64) -> Result<f64, Problem> {
    if (0.0..=1.0).contains(&opacity) {
        Ok(opacity)
    } else {
        Err(Problem::range("window.background-opacity", 0.0, 1.0))
    }
}
