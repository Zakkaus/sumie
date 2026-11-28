use std::{collections::BTreeMap, path::Path};

use super::{MAX_INCLUDE_DEPTH, Snapshot, includes, parse};
use crate::Value;

/// Parseable settings and the file snapshots they came from, in loader precedence.
/// Invalid values remain visible so the editor can repair them via `save`.
#[derive(Default)]
pub struct EditValues {
    files: Vec<Snapshot>,
    keys: BTreeMap<String, Definition>,
}

struct Definition {
    value: Value,
    file: usize,
    shadowed: Vec<usize>,
}

impl EditValues {
    /// Read the main file and its includes without applying settings validation.
    /// Unreadable or unparseable files contribute no values; use `load` for diagnostics.
    #[must_use]
    pub fn read(main: &Path) -> Self {
        let mut values = Self::default();
        values.visit(main, &mut Vec::new());
        values
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.keys.get(key).map(|entry| &entry.value)
    }

    /// The snapshot to pass to `save`. An undefined key belongs to the main file.
    #[must_use]
    pub fn source(&self, key: &str) -> Option<&Snapshot> {
        self.definitions(key).next().or_else(|| self.files.first())
    }

    /// Every file defining `key`, highest precedence first. Used to unset a key entirely.
    pub fn definitions(&self, key: &str) -> impl Iterator<Item = &Snapshot> {
        self.keys
            .get(key)
            .into_iter()
            .flat_map(|entry| std::iter::once(entry.file).chain(entry.shadowed.iter().copied()))
            .map(|file| &self.files[file])
    }

    fn visit(&mut self, path: &Path, active: &mut Vec<usize>) {
        if active.len() == MAX_INCLUDE_DEPTH || self.files.iter().any(|file| file.path == path) {
            return;
        }
        let Ok(snapshot) = Snapshot::read(path) else {
            return;
        };
        if active
            .iter()
            .any(|&file| self.files[file].real_path == snapshot.real_path)
        {
            return;
        }
        let table = parse(&snapshot.bytes);
        let file = self.files.len();
        self.files.push(snapshot);
        let Some(table) = table else { return };
        let children: Vec<_> = includes(&table)
            .rev()
            .map(|entry| path.with_file_name(entry))
            .collect();
        for (key, value) in table {
            if let Value::Table(entries) = value {
                for (field, value) in entries {
                    self.record(format!("{key}.{field}"), file, value);
                }
            } else {
                self.record(key, file, value);
            }
        }
        active.push(file);
        for child in children {
            self.visit(&child, active);
        }
        active.pop();
    }

    fn record(&mut self, key: String, file: usize, value: Value) {
        use std::collections::btree_map::Entry;

        match self.keys.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(Definition {
                    value,
                    file,
                    shadowed: Vec::new(),
                });
            }
            Entry::Occupied(mut entry) => {
                let entry = entry.get_mut();
                let real_path = &self.files[file].real_path;
                if !std::iter::once(entry.file)
                    .chain(entry.shadowed.iter().copied())
                    .any(|source| self.files[source].real_path == *real_path)
                {
                    entry.shadowed.push(file);
                }
            }
        }
    }
}
