//! Rust behaviour for items outside the generated vanilla set.
//!
//! [`ItemRegistry`](super::registry::ItemRegistry) is built with the server,
//! from compile-time item ids. An item installed at runtime by
//! `pumpkin_data::item_extension` has an id past the generated range, so its
//! behaviour is kept here instead: a table indexed by `id - Item::VANILLA_COUNT`,
//! installed once at startup, after the item extension and before the server
//! starts. Only lookups that miss the registry reach it, so vanilla items pay
//! nothing.

use std::fmt;
use std::sync::{Arc, OnceLock};

use pumpkin_data::item::Item;
use pumpkin_data::item_extension;

use super::ItemBehaviour;

static BEHAVIOURS: OnceLock<Vec<Option<Arc<dyn ItemBehaviour>>>> = OnceLock::new();

#[derive(Debug, PartialEq, Eq)]
pub enum InstallError {
    /// No item, vanilla or extension, has this name.
    UnknownItem(String),
    /// The item is vanilla: its behaviour belongs in the registry itself.
    NotAnExtension(String),
    /// The same item was given two behaviours.
    Duplicate(String),
    /// Behaviours were already installed; the table is set once.
    AlreadyInstalled,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownItem(name) => write!(f, "no item is named {name}"),
            Self::NotAnExtension(name) => {
                write!(f, "{name} is a vanilla item, register it in the item registry")
            }
            Self::Duplicate(name) => write!(f, "item {name} was given two behaviours"),
            Self::AlreadyInstalled => write!(f, "extension item behaviours are already installed"),
        }
    }
}

impl std::error::Error for InstallError {}

/// Installs the behaviours of extension items, by item name.
///
/// Call it once, after `pumpkin_data::item_extension` is installed (names are
/// resolved against it) and before the server starts. Nothing is installed
/// when an entry is rejected.
///
/// # Errors
///
/// Returns [`InstallError`] for a name that is unknown, vanilla or repeated,
/// or when behaviours are already installed.
pub fn install(entries: Vec<(&str, Arc<dyn ItemBehaviour>)>) -> Result<(), InstallError> {
    let extension_items = item_extension::item_count().saturating_sub(Item::VANILLA_COUNT);
    let mut table: Vec<Option<Arc<dyn ItemBehaviour>>> = vec![None; usize::from(extension_items)];
    for (name, behaviour) in entries {
        let item =
            Item::from_registry_key(name).ok_or_else(|| InstallError::UnknownItem(name.into()))?;
        let index = item
            .id
            .checked_sub(Item::VANILLA_COUNT)
            .ok_or_else(|| InstallError::NotAnExtension(name.into()))?;
        // `from_registry_key` only resolves extension ids that `item_count` counts.
        let slot = table
            .get_mut(usize::from(index))
            .ok_or_else(|| InstallError::UnknownItem(name.into()))?;
        if slot.is_some() {
            return Err(InstallError::Duplicate(name.into()));
        }
        *slot = Some(behaviour);
    }
    BEHAVIOURS
        .set(table)
        .map_err(|_| InstallError::AlreadyInstalled)
}

// Off the vanilla path: only ids the registry does not know get here.
#[cold]
#[inline(never)]
pub(crate) fn get(item: u16) -> Option<&'static Arc<dyn ItemBehaviour>> {
    let index = item.checked_sub(Item::VANILLA_COUNT)?;
    BEHAVIOURS.get()?.get(usize::from(index))?.as_ref()
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::sync::Arc;

    use pumpkin_data::item::Item;

    use super::{InstallError, get, install};
    use crate::item::ItemBehaviour;

    struct Inert;
    impl ItemBehaviour for Inert {
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn rejected_entries_install_nothing() {
        assert_eq!(
            install(vec![("minecraft:stick", Arc::new(Inert))]),
            Err(InstallError::NotAnExtension("minecraft:stick".into()))
        );
        assert_eq!(
            install(vec![("verse:missing", Arc::new(Inert))]),
            Err(InstallError::UnknownItem("verse:missing".into()))
        );
        assert!(get(Item::STICK.id).is_none());
    }
}
