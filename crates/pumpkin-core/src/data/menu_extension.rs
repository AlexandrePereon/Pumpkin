//! Loads menu types that are not in the generated vanilla set.
//!
//! Fed by the same Extractor dump as [`super::block_extension`]: its
//! `screens.json` lists every menu type in raw-id order, vanilla names bare and
//! any other namespace kept. A mod's menu type has to be dictated to Fabric
//! clients (`minecraft:menu` is synced) and sent by raw id when its screen
//! opens — see `ScreenHandler::menu_type_id`.

use std::path::Path;

use pumpkin_data::screen::WindowType;

use super::name_list_extension::NameListExtension;

/// All menu types, vanilla first, in raw-id order.
static MENUS: NameListExtension = NameListExtension::new(
    "screens.json",
    "menu type",
    WindowType::Stonecutter as usize + 1,
);

/// Reads `screens.json` from an Extractor dump and installs its menu types if
/// any lies outside the vanilla set.
pub fn install_from_dump(dir: &Path) {
    MENUS.install_from_dump(dir);
}

/// The raw id of an extension menu type, by full name.
#[must_use]
pub fn id_of(name: &str) -> Option<i32> {
    MENUS.id_of(name).map(i32::from)
}

/// Every menu type with its raw id, for registry sync; empty without extension.
#[must_use]
pub fn registry_entries() -> Vec<(&'static str, u16)> {
    MENUS.registry_entries()
}
