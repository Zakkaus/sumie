use std::{collections::BTreeMap, path::Path};

use super::{Snapshot, walk_sources};
use crate::Value;

/// Parseable settings and the file snapshots they came from, in loader precedence.
/// Invalid values remain visible so the editor can repair them via `save`.
#[derive(Default)]
pub struct EditValues {
    files: Vec<Snapshot>,
    keys: BTreeMap<String, (usize, Value)>,
}

impl EditValues {
    /// Read the main file and its includes without applying settings validation.
    /// Unreadable or unparseable files contribute no values; use `load` for diagnostics.
    #[must_use]
    pub fn read(main: &Path) -> Self {
        let mut values = Self::default();
        walk_sources(main, &mut Vec::new(), &mut |snapshot, table| {
            let file = values.files.len();
            values.files.push(snapshot);
            for (key, value) in table {
                if let Value::Table(entries) = value {
                    for (field, value) in entries {
                        values
                            .keys
                            .entry(format!("{key}.{field}"))
                            .or_insert((file, value));
                    }
                } else {
                    values.keys.entry(key).or_insert((file, value));
                }
            }
            None::<()>
        });
        values
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.keys.get(key).map(|(_, value)| value)
    }

    /// The snapshot to pass to `save`. An undefined key belongs to the main file.
    #[must_use]
    pub fn source(&self, key: &str) -> Option<&Snapshot> {
        self.files
            .get(self.keys.get(key).map_or(0, |(file, _)| *file))
    }
}
