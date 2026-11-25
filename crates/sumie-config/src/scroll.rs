use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scroll {
    /// Scroll history by pixels with kinetic touchpad motion. Set to false to scroll whole rows.
    #[schemars(with = "bool", default = "enabled", title = "Smooth scrolling")]
    smooth: Option<bool>,
}

const fn enabled() -> bool {
    true
}

impl Scroll {
    #[must_use]
    pub fn smooth(&self) -> bool {
        self.smooth.unwrap_or_else(enabled)
    }

    pub(super) fn overlay(&mut self, file: &Self) {
        self.smooth = file.smooth.or(self.smooth);
    }
}
