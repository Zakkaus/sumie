use std::{collections::HashMap, path::Path};

use skrifa::{MetadataProvider as _, font::Blob, raw::FileRef, string::StringId};

use super::FontError;

pub(super) struct FontFile {
    pub bytes: Blob,
    // Core Text identifies a face by name, not its TTC directory index.
    pub collection: Option<HashMap<String, u32>>,
}

impl FontFile {
    pub fn read(path: &Path) -> Result<Self, FontError> {
        let bytes: Blob = std::fs::read(path)
            .map_err(|source| FontError::ReadFont {
                path: path.to_owned(),
                source,
            })?
            .into();
        let file = FileRef::new(&bytes).map_err(|_| FontError::InvalidFont(path.to_owned()))?;
        let collection = match file {
            FileRef::Font(_) => None,
            FileRef::Collection(ttc) => {
                let mut names = HashMap::new();
                for (index, face) in (0..ttc.len()).zip(ttc.iter()) {
                    if let Ok(face) = face
                        && let Some(name) = face.localized_strings(StringId::POSTSCRIPT_NAME).next()
                    {
                        names.insert(name.to_string(), index);
                    }
                }
                Some(names)
            }
        };
        Ok(Self { bytes, collection })
    }
}
