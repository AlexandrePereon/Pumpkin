//! Loads menu types that are not in the generated vanilla set.
//!
//! Fed by the same Extractor dump as [`super::block_extension`]: its
//! `screens.json` lists every menu type in raw-id order, vanilla names bare and
//! any other namespace kept. A mod's menu type has to be dictated to Fabric
//! clients (`minecraft:menu` is synced) and sent by raw id when its screen
//! opens — see `ScreenHandler::menu_type_id`.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use pumpkin_data::screen::WindowType;
use tracing::{info, warn};

/// All menu types, vanilla first, in raw-id order; set only when the dump has
/// some outside the vanilla set.
static MENUS: OnceLock<Vec<&'static str>> = OnceLock::new();

/// How many menu types the generated `WindowType` knows.
const VANILLA_COUNT: usize = WindowType::Stonecutter as usize + 1;

/// Reads `screens.json` from an Extractor dump and installs its menu types if
/// any lies outside the vanilla set.
pub fn install_from_dump(dir: &Path) {
    let path = dir.join("screens.json");
    let names: Vec<String> = match fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(names) => names,
        Err(error) => {
            warn!("Could not load {}: {error}", path.display());
            return;
        }
    };
    match validate(&names) {
        Ok(0) => {}
        Ok(count) => {
            let names = names.into_iter().map(|n| &*Box::leak(n.into_boxed_str())).collect();
            if MENUS.set(names).is_ok() {
                info!("Installed {count} extension menu type(s)");
            }
        }
        Err(error) => warn!("Menu types not installed: {error}"),
    }
}

/// The number of extension menu types, if the vanilla ones come first, bare,
/// and the rest are namespaced and fit a raw id.
fn validate(names: &[String]) -> Result<usize, String> {
    let (vanilla, extension) = names.split_at(VANILLA_COUNT.min(names.len()));
    if vanilla.len() != VANILLA_COUNT || vanilla.iter().any(|name| name.contains(':')) {
        return Err("screens.json does not start with the vanilla menu types".into());
    }
    if let Some(name) = extension.iter().find(|name| !name.contains(':')) {
        return Err(format!("{name} comes after the vanilla menu types without a namespace"));
    }
    if names.len() > usize::from(u16::MAX) {
        return Err("too many menu types".into());
    }
    Ok(extension.len())
}

/// The raw id of an extension menu type, by full name.
#[must_use]
pub fn id_of(name: &str) -> Option<i32> {
    let index = MENUS.get()?.iter().skip(VANILLA_COUNT).position(|n| *n == name)?;
    i32::try_from(VANILLA_COUNT + index).ok()
}

/// Every menu type with its raw id, for registry sync; empty without extension.
#[must_use]
pub fn registry_entries() -> Vec<(&'static str, u16)> {
    MENUS.get().map_or_else(Vec::new, |names| {
        names
            .iter()
            .zip(0u16..)
            .map(|(name, id)| (*name, id))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::{VANILLA_COUNT, validate};

    fn vanilla() -> Vec<String> {
        (0..VANILLA_COUNT).map(|i| format!("menu_{i}")).collect()
    }

    #[test]
    fn counts_namespaced_menus_after_vanilla() {
        let mut names = vanilla();
        assert_eq!(validate(&names), Ok(0));
        names.push("verse:box".into());
        assert_eq!(validate(&names), Ok(1));
    }

    #[test]
    fn rejects_bad_dumps() {
        assert!(validate(&vanilla()[1..]).is_err());
        let mut names = vanilla();
        names.push("box".into());
        assert!(validate(&names).is_err());
    }
}
