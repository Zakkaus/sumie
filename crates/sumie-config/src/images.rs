use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Images {
    /// Maximum height of new natural-size placements, in rows. Set to 0 to disable thumbnails.
    #[schemars(
        with = "u16",
        default = "default_thumbnail_rows",
        title = "Thumbnail rows"
    )]
    thumbnail_rows: Option<u16>,
}

const fn default_thumbnail_rows() -> u16 {
    3
}

impl Images {
    #[must_use]
    pub fn thumbnail_rows(&self) -> u16 {
        self.thumbnail_rows.unwrap_or_else(default_thumbnail_rows)
    }

    pub(super) fn overlay(&mut self, file: &Self) {
        self.thumbnail_rows = file.thumbnail_rows.or(self.thumbnail_rows);
    }
}
