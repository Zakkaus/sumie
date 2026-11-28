//! TOML and schema representation of explicit overrides for the core shortcut table.

use std::{borrow::Cow, collections::BTreeMap};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use sumie_core::{BINDINGS, Binding, Chord, Shortcut};

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Keybinds(BTreeMap<String, String>);

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
