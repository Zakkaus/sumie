//! TOML and schema representation of explicit overrides for the core shortcut table.

use std::{borrow::Cow, collections::BTreeMap};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use sumie_core::{BINDINGS, Binding, Chord, Platform, Shortcut};

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(try_from = "BTreeMap<String, String>")]
pub struct Keybinds(BTreeMap<String, String>);

impl TryFrom<BTreeMap<String, String>> for Keybinds {
    type Error = String;

    fn try_from(values: BTreeMap<String, String>) -> Result<Self, Self::Error> {
        for (id, text) in &values {
            if binding(id).is_none() || Chord::parse(text).is_none() {
                return Err(format!("invalid key binding {id}: {text:?}"));
            }
        }
        Ok(Self(values))
    }
}

impl JsonSchema for Keybinds {
    fn schema_name() -> Cow<'static, str> {
        "Keybinds".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        let properties: serde_json::Map<_, _> = BINDINGS
            .iter()
            .map(|binding| {
                (
                    binding.id.to_owned(),
                    serde_json::json!({
                        "type": "string", "title": binding.label,
                        "description": "Override the application shortcut with a single key chord."
                    }),
                )
            })
            .collect();
        json_schema!({"type": "object", "properties": properties, "additionalProperties": false})
    }
}

impl Keybinds {
    /// The resolved overrides, in application table order. Defaults stay in `sumie-core`.
    pub fn overrides(&self) -> impl Iterator<Item = (Chord, Shortcut)> + '_ {
        BINDINGS.iter().filter_map(|binding| {
            let chord = Chord::parse(self.0.get(binding.id)?)?;
            Some((chord, binding.action))
        })
    }

    pub(super) fn overlay(&mut self, file: Self) {
        self.0.extend(file.0);
    }
}

pub(super) fn binding(id: &str) -> Option<&'static Binding> {
    BINDINGS.iter().find(|binding| binding.id == id)
}

pub(super) const fn platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::MacOS
    } else {
        Platform::Linux
    }
}
