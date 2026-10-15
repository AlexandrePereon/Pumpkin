//! Block entity types outside the generated vanilla set.
//!
//! Two halves, each installed once at startup:
//!
//! - **the types** — names in raw-id order, after the vanilla ones — from the
//!   same source as extension blocks, so their ids can be dictated to clients
//!   and written on the wire;
//! - **their implementations**, by name, from whoever implements the mod.
//!
//! The vanilla paths ([`super::create_block_entity`],
//! [`super::block_entity_from_nbt`], [`super::BlockEntity::get_id`]) only get
//! here when the vanilla lookup misses.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use pumpkin_data::block_properties::BLOCK_ENTITY_TYPES;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;

use super::BlockEntity;

/// Builds a fresh block entity, as placing its block does.
pub type NewFn = fn(BlockPos) -> Arc<dyn BlockEntity>;
/// Rebuilds a block entity from its saved NBT, as loading a chunk does.
pub type FromNbtFn = fn(&NbtCompound, BlockPos) -> Arc<dyn BlockEntity>;

/// How to build one extension block entity type.
#[derive(Clone, Copy)]
pub struct Factory {
    pub new: NewFn,
    pub from_nbt: FromNbtFn,
}

static TYPES: OnceLock<Vec<&'static str>> = OnceLock::new();
static FACTORIES: OnceLock<BTreeMap<&'static str, Factory>> = OnceLock::new();

#[derive(Debug, PartialEq, Eq)]
pub enum InstallError {
    /// A type name without a namespace, or in `minecraft`: not an extension.
    NotAnExtension(String),
    /// The same name twice.
    Duplicate(String),
    /// More types than raw ids.
    TooMany,
    /// A factory for a name that is not an installed type.
    UnknownType(String),
    /// This half was already installed; it is set once.
    AlreadyInstalled,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnExtension(name) => {
                write!(f, "{name} is not a namespaced, non-vanilla block entity type")
            }
            Self::Duplicate(name) => write!(f, "block entity type {name} is given twice"),
            Self::TooMany => write!(f, "too many block entity types for raw ids"),
            Self::UnknownType(name) => write!(f, "no block entity type is named {name}"),
            Self::AlreadyInstalled => write!(f, "already installed"),
        }
    }
}

impl std::error::Error for InstallError {}

/// Installs the extension types, in raw-id order: the first gets the id right
/// after the last vanilla type.
///
/// # Errors
///
/// Returns [`InstallError`] for a name that is bare, vanilla or repeated, or
/// when types are already installed. Nothing is installed on error.
pub fn install_types(names: Vec<&'static str>) -> Result<(), InstallError> {
    if BLOCK_ENTITY_TYPES.len() + names.len() > usize::from(u16::MAX) {
        return Err(InstallError::TooMany);
    }
    for (i, name) in names.iter().enumerate() {
        if !name.contains(':') || name.starts_with("minecraft:") {
            return Err(InstallError::NotAnExtension((*name).to_owned()));
        }
        if names[..i].contains(name) {
            return Err(InstallError::Duplicate((*name).to_owned()));
        }
    }
    TYPES.set(names).map_err(|_| InstallError::AlreadyInstalled)
}

/// Installs the implementations of extension types, by type name. Call after
/// [`install_types`].
///
/// # Errors
///
/// Returns [`InstallError`] for a name that is not an installed type or is
/// repeated, or when factories are already installed. Nothing is installed on
/// error.
pub fn install_factories(entries: Vec<(&'static str, Factory)>) -> Result<(), InstallError> {
    let mut factories = BTreeMap::new();
    for (name, factory) in entries {
        if id_of(name).is_none() {
            return Err(InstallError::UnknownType(name.to_owned()));
        }
        if factories.insert(name, factory).is_some() {
            return Err(InstallError::Duplicate(name.to_owned()));
        }
    }
    FACTORIES
        .set(factories)
        .map_err(|_| InstallError::AlreadyInstalled)
}

/// Total number of block entity types, vanilla plus extension.
#[must_use]
pub fn count() -> u16 {
    let extra = TYPES.get().map_or(0, Vec::len);
    // `install_types` rejects anything that would not fit.
    u16::try_from(BLOCK_ENTITY_TYPES.len() + extra).unwrap_or(u16::MAX)
}

/// The name of an extension type, by raw id. `None` for vanilla ids.
#[must_use]
pub fn name_of(id: u16) -> Option<&'static str> {
    let index = usize::from(id).checked_sub(BLOCK_ENTITY_TYPES.len())?;
    TYPES.get()?.get(index).copied()
}

/// The raw id of an extension type, by full name.
#[must_use]
pub fn id_of(name: &str) -> Option<u32> {
    let index = TYPES.get()?.iter().position(|n| *n == name)?;
    u32::try_from(BLOCK_ENTITY_TYPES.len() + index).ok()
}

// Off the vanilla path: only ids past the generated types get here.
#[cold]
#[inline(never)]
pub(crate) fn create(id: u16, position: BlockPos) -> Option<Arc<dyn BlockEntity>> {
    let factory = FACTORIES.get()?.get(name_of(id)?)?;
    Some((factory.new)(position))
}

// Off the vanilla path: only names no vanilla type has get here.
#[cold]
#[inline(never)]
pub(crate) fn from_nbt(
    name: &str,
    nbt: &NbtCompound,
    position: BlockPos,
) -> Option<Arc<dyn BlockEntity>> {
    let factory = FACTORIES.get()?.get(name)?;
    Some((factory.from_nbt)(nbt, position))
}

#[cfg(test)]
mod tests {
    use super::{InstallError, install_types, name_of};

    #[test]
    fn rejects_non_extension_names() {
        assert_eq!(
            install_types(vec!["chest"]),
            Err(InstallError::NotAnExtension("chest".into()))
        );
        assert_eq!(
            install_types(vec!["minecraft:chest"]),
            Err(InstallError::NotAnExtension("minecraft:chest".into()))
        );
        assert_eq!(
            install_types(vec!["verse:a", "verse:a"]),
            Err(InstallError::Duplicate("verse:a".into()))
        );
        assert_eq!(name_of(0), None);
    }
}
