use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Default cursor appearance; terminal applications can override it with DECSCUSR.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Cursor {
    /// Cursor shape: block, bar, or underline.
    #[schemars(with = "CursorStyle", default = "CursorStyle::default")]
    style: Option<CursorStyle>,
    /// Blink while the window is focused.
    #[schemars(with = "bool", default = "default_cursor_blink")]
    blink: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    #[default]
    Block,
    Bar,
    Underline,
}

impl Cursor {
    #[must_use]
    pub fn style(self) -> CursorStyle {
        self.style.unwrap_or_default()
    }

    #[must_use]
    pub fn blink(self) -> bool {
        self.blink.unwrap_or_else(default_cursor_blink)
    }

    pub(super) fn overlay(&mut self, file: Self) {
        self.style = file.style.or(self.style);
        self.blink = file.blink.or(self.blink);
    }
}

const fn default_cursor_blink() -> bool {
    true
}
