use std::{borrow::Cow, mem::discriminant};

use sumie_config::POINT_TO_PX;
use sumie_core::FontZoom;
use sumie_font::{FontError, FontOptions, FontSystem, Hinting};

use crate::Result;

pub(crate) struct FontSettings {
    family: Cow<'static, str>,
    px: f32,
    options: FontOptions,
}

impl FontSettings {
    pub(crate) fn from_config(config: sumie_config::Font) -> Result<Self> {
        let px = std::env::var("SUMIE_FONT_PX")
            .map_or_else(|_| Ok(config.logical_px()), |px| px.parse::<f32>())?;
        let hinting = match std::env::var("SUMIE_FONT_HINTING").as_deref() {
            Err(std::env::VarError::NotPresent) => match config.hinting() {
                sumie_config::Hinting::Off => Hinting::Off,
                sumie_config::Hinting::Light => Hinting::Light,
                sumie_config::Hinting::Normal => Hinting::Normal,
            },
            Ok("off") => Hinting::Off,
            Ok("light") => Hinting::Light,
            Ok("normal") => Hinting::Normal,
            _ => return Err("SUMIE_FONT_HINTING must be off, light, or normal".into()),
        };
        let weight = match std::env::var("SUMIE_FONT_WEIGHT") {
            Err(std::env::VarError::NotPresent) => config.weight(),
            value => Some(value?.parse().map_err(|_| FontError::InvalidWeight)?),
        };
        let family = std::env::var("SUMIE_FONT").map_or_else(|_| config.into_family(), Cow::Owned);
        Ok(Self {
            family,
            px,
            options: FontOptions { hinting, weight },
        })
    }

    pub(super) fn matches(&self, other: &Self, zoom_points: Option<f32>) -> bool {
        self.family == other.family
            && self.px.to_bits() == other.px(zoom_points).to_bits()
            && self.options.weight == other.options.weight
            && discriminant(&self.options.hinting) == discriminant(&other.options.hinting)
    }

    pub(super) fn zoom(&self, current: Option<f32>, action: FontZoom) -> Option<f32> {
        let delta = match action {
            FontZoom::Increase => 1.0,
            FontZoom::Decrease => -1.0,
            FontZoom::Reset => return None,
        };
        let points = current.unwrap_or(self.px / POINT_TO_PX);
        Some((points + delta).clamp(4.0, 200.0))
    }

    pub(super) fn px(&self, zoom_points: Option<f32>) -> f32 {
        zoom_points.map_or(self.px, |points| points * POINT_TO_PX)
    }

    pub(super) fn at_scale(&self, scale: u32, zoom_points: Option<f32>) -> Result<FontSystem> {
        let scale_units = u16::try_from(scale)?;
        let font = FontSystem::new(
            &self.family,
            self.px(zoom_points),
            f32::from(scale_units) / 120.0,
            self.options,
        )?;
        let metrics = font.metrics();
        tracing::info!(
            family = font.family(font.primary_face()),
            scale_units,
            cell_width = metrics.width,
            cell_height = metrics.height,
            "terminal font"
        );
        Ok(font)
    }
}
