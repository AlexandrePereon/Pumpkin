//! Loads attributes that are not in the generated vanilla set.
//!
//! Fed by the same Extractor dump as [`super::block_extension`]: its
//! `attributes.json` maps every attribute name (vanilla bare, any other
//! namespace kept) to its raw id and default value. The vanilla entries must
//! match the generated ones; the rest are installed into
//! `pumpkin_data::attributes_extension`. Load before entity types, whose
//! default attributes may name them.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use pumpkin_data::attributes::Attributes;
use pumpkin_data::attributes_extension;
use serde::Deserialize;
use tracing::{info, warn};

#[derive(Deserialize)]
struct DumpAttribute {
    id: u8,
    default_value: f64,
}

/// Reads `attributes.json` from an Extractor dump and installs its
/// non-vanilla attributes.
pub fn install_from_dump(dir: &Path) {
    let path = dir.join("attributes.json");
    let dump: BTreeMap<String, DumpAttribute> = match fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(dump) => dump,
        Err(error) => {
            warn!("Could not load {}: {error}", path.display());
            return;
        }
    };
    match build(dump) {
        Ok(entries) if entries.is_empty() => {}
        Ok(entries) => {
            let count = entries.len();
            match attributes_extension::install(entries) {
                Ok(()) => info!("Installed {count} extension attribute(s)"),
                Err(error) => warn!("Could not install the attribute extension: {error}"),
            }
        }
        Err(error) => warn!("Attributes not installed: {error}"),
    }
}

/// The extension attributes in raw-id order, if every vanilla entry matches
/// the generated table.
fn build(dump: BTreeMap<String, DumpAttribute>) -> Result<Vec<&'static Attributes>, String> {
    let mut extension = Vec::new();
    for (name, attribute) in dump {
        // The Extractor writes vanilla names bare and keeps every other namespace.
        if name.contains(':') {
            extension.push(&*Box::leak(Box::new(Attributes {
                id: attribute.id,
                default_value: attribute.default_value,
                name: Box::leak(name.into_boxed_str()),
            })));
        } else {
            let full = format!("minecraft:{name}");
            if !Attributes::ALL
                .iter()
                .any(|a| a.name == full && a.id == attribute.id)
            {
                return Err(format!(
                    "vanilla attribute {name} (id {}) differs from the generated table",
                    attribute.id
                ));
            }
        }
    }
    extension.sort_unstable_by_key(|a| a.id);
    Ok(extension)
}

/// Every attribute with its raw id, for registry sync; empty without extension.
#[must_use]
pub fn registry_entries() -> Vec<(&'static str, u16)> {
    if attributes_extension::count() == attributes_extension::vanilla_count() {
        return Vec::new();
    }
    attributes_extension::all()
        .map(|a| (a.name, u16::from(a.id)))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{DumpAttribute, build};
    use pumpkin_data::attributes::Attributes;

    #[test]
    fn keeps_namespaced_entries_and_checks_vanilla() {
        let mut dump = BTreeMap::new();
        dump.insert(
            "armor".to_owned(),
            DumpAttribute {
                id: Attributes::ARMOR.id,
                default_value: 0.0,
            },
        );
        dump.insert(
            "verse:resonance".to_owned(),
            DumpAttribute {
                id: 40,
                default_value: 5.0,
            },
        );
        let built = build(dump).map(|v| v.iter().map(|a| (a.name, a.id)).collect::<Vec<_>>());
        assert_eq!(built, Ok(vec![("verse:resonance", 40)]));

        let mut wrong = BTreeMap::new();
        wrong.insert(
            "armor".to_owned(),
            DumpAttribute {
                id: Attributes::ARMOR.id + 1,
                default_value: 0.0,
            },
        );
        assert!(build(wrong).is_err());
    }
}
