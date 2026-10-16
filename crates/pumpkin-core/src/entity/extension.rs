//! Rust implementations of entity types outside the generated vanilla set.
//!
//! [`super::r#type::from_type`] builds an entity's Rust object by matching its
//! type against the vanilla ones. A type installed at runtime by
//! `pumpkin_data::entity_type_extension` matches none of them, so whoever
//! implements it registers a factory here, by type name, once at startup.
//! `from_type` consults this table only when no vanilla type matched.

use std::fmt;
use std::sync::{Arc, OnceLock};

use pumpkin_data::entity::EntityType;
use pumpkin_data::entity_type_extension;

use super::{Entity, EntityBase};

/// Builds the Rust object of an extension entity around its base [`Entity`].
pub type Factory = fn(Entity) -> Arc<dyn EntityBase>;

/// Indexed by `id - entity_type_extension::vanilla_count()`.
static FACTORIES: OnceLock<Vec<Option<Factory>>> = OnceLock::new();

#[derive(Debug, PartialEq, Eq)]
pub enum InstallError {
    /// No entity type, vanilla or extension, has this name.
    UnknownType(String),
    /// The type is vanilla: its implementation belongs in `from_type`.
    NotAnExtension(String),
    /// The same type was given two factories.
    Duplicate(String),
    /// Factories were already installed; the table is set once.
    AlreadyInstalled,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownType(name) => write!(f, "no entity type is named {name}"),
            Self::NotAnExtension(name) => write!(f, "{name} is a vanilla entity type"),
            Self::Duplicate(name) => write!(f, "entity type {name} was given two factories"),
            Self::AlreadyInstalled => write!(f, "extension entity factories are already installed"),
        }
    }
}

impl std::error::Error for InstallError {}

/// Installs the factories of extension entity types, by type name. Call after
/// the entity type extension is installed and before the server starts.
///
/// # Errors
///
/// Returns [`InstallError`] for a name that is unknown, vanilla or repeated,
/// or when factories are already installed. Nothing is installed on error.
pub fn install(entries: Vec<(&str, Factory)>) -> Result<(), InstallError> {
    let vanilla = entity_type_extension::vanilla_count();
    let extension = entity_type_extension::count().saturating_sub(vanilla);
    let mut table: Vec<Option<Factory>> = vec![None; usize::from(extension)];
    for (name, factory) in entries {
        let entity_type =
            EntityType::from_name(name).ok_or_else(|| InstallError::UnknownType(name.into()))?;
        let index = entity_type
            .id
            .checked_sub(vanilla)
            .ok_or_else(|| InstallError::NotAnExtension(name.into()))?;
        let slot = table
            .get_mut(usize::from(index))
            .ok_or_else(|| InstallError::UnknownType(name.into()))?;
        if slot.is_some() {
            return Err(InstallError::Duplicate(name.into()));
        }
        *slot = Some(factory);
    }
    FACTORIES
        .set(table)
        .map_err(|_| InstallError::AlreadyInstalled)
}

// Off the vanilla path: only types no vanilla arm matched get here.
#[cold]
#[inline(never)]
pub(crate) fn factory(entity_type: &EntityType) -> Option<Factory> {
    let index = entity_type
        .id
        .checked_sub(entity_type_extension::vanilla_count())?;
    *FACTORIES.get()?.get(usize::from(index))?
}

#[cfg(test)]
mod tests {
    use super::{InstallError, install};
    use crate::entity::{Entity, EntityBase};
    use std::sync::Arc;

    fn plain(entity: Entity) -> Arc<dyn EntityBase> {
        Arc::new(entity)
    }

    #[test]
    fn rejected_entries_install_nothing() {
        assert_eq!(
            install(vec![("minecraft:pig", plain)]),
            Err(InstallError::NotAnExtension("minecraft:pig".into()))
        );
        assert_eq!(
            install(vec![("verse:missing", plain)]),
            Err(InstallError::UnknownType("verse:missing".into()))
        );
    }
}
