//! Runtime extension of the generated entity type registry.
//!
//! The counterpart of [`crate::block_extension`] for entity types. Vanilla
//! types are generated at build time into `generated/entity_type.rs`; types
//! contributed at startup get raw ids continuing the vanilla range, so
//! `EntityType::from_raw` and `EntityType::from_name` resolve ids and names the
//! generated code knows nothing about.
//!
//! The extension is installed once, before the server accepts connections, and
//! is read-only afterwards. Reads are therefore lock-free.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use crate::entity_type::EntityType;

/// Why [`install`] rejected an extension.
#[derive(Debug)]
pub enum InstallError {
    /// An extension is already installed.
    AlreadyInstalled,
    /// Raw ids must continue the vanilla range without gaps.
    IdNotContiguous { expected: u16, found: u16 },
    /// The name is already taken, either by vanilla or earlier in this batch.
    DuplicateName { name: &'static str },
    /// Raw ids would exceed what `u16` can carry.
    TooMany,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => write!(f, "an entity type extension is already installed"),
            Self::IdNotContiguous { expected, found } => {
                write!(f, "expected entity type id {expected}, found {found}")
            }
            Self::DuplicateName { name } => {
                write!(f, "entity type name {name} is already registered")
            }
            Self::TooMany => write!(f, "too many entity types to address with a u16 id"),
        }
    }
}

impl std::error::Error for InstallError {}

struct Extension {
    /// Indexed by `raw_id - VANILLA_COUNT`.
    types: Vec<&'static EntityType>,
    by_name: BTreeMap<&'static str, &'static EntityType>,
}

static EXTENSION: OnceLock<Extension> = OnceLock::new();

/// How many entity types the generated code knows.
#[must_use]
pub const fn vanilla_count() -> u16 {
    // 161 on 26.3: far from `u16::MAX`.
    EntityType::ALL.len() as u16
}

/// Installs the runtime entity type extension.
///
/// Every type must already be leaked to `'static` and carry a raw id
/// continuing the vanilla range, gapless and ascending. A type's
/// `resource_name` is its full `namespace:path`.
///
/// # Errors
///
/// Returns [`InstallError`] when an id is out of sequence, a name collides, or
/// an extension is already installed. Nothing is installed on error.
pub fn install(entries: Vec<&'static EntityType>) -> Result<(), InstallError> {
    if EXTENSION.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }

    let mut by_name = BTreeMap::new();
    let mut next_id = vanilla_count();

    for &entity_type in &entries {
        if entity_type.id != next_id {
            return Err(InstallError::IdNotContiguous {
                expected: next_id,
                found: entity_type.id,
            });
        }
        // `EntityType::from_name` only sees vanilla here, as the extension is
        // not installed yet; a collision would otherwise be silent.
        if EntityType::from_name(entity_type.resource_name).is_some()
            || by_name
                .insert(entity_type.resource_name, entity_type)
                .is_some()
        {
            return Err(InstallError::DuplicateName {
                name: entity_type.resource_name,
            });
        }
        next_id = next_id.checked_add(1).ok_or(InstallError::TooMany)?;
    }

    EXTENSION
        .set(Extension {
            types: entries,
            by_name,
        })
        .map_err(|_| InstallError::AlreadyInstalled)
}

/// Total number of entity types, vanilla plus extension.
#[must_use]
pub fn count() -> u16 {
    let extra = EXTENSION.get().map_or(0, |e| e.types.len());
    // `install` rejects anything that would not fit.
    vanilla_count().saturating_add(extra.try_into().unwrap_or(u16::MAX))
}

// Off the vanilla path: only ids past the generated range get here.
#[cold]
#[inline(never)]
pub(crate) fn by_raw(raw: u16) -> Option<&'static EntityType> {
    let index = usize::from(raw.checked_sub(vanilla_count())?);
    EXTENSION.get()?.types.get(index).copied()
}

// Off the vanilla path: only names the generated match misses get here.
#[cold]
#[inline(never)]
pub(crate) fn by_name(name: &str) -> Option<&'static EntityType> {
    EXTENSION.get()?.by_name.get(name).copied()
}

#[cfg(test)]
mod tests {
    use super::{count, install, vanilla_count};
    use crate::entity_type::EntityType;

    fn leak_copy(id: u16, name: &'static str) -> &'static EntityType {
        Box::leak(Box::new(EntityType {
            id,
            resource_name: name,
            ..EntityType::MARKER.clone()
        }))
    }

    /// One test for the whole module: the extension is a process-wide
    /// `OnceLock`, so it can only be installed once per test binary.
    #[test]
    fn resolves_types_beyond_the_vanilla_range() {
        let vanilla = vanilla_count();
        assert_eq!(EntityType::from_raw(vanilla).map(|t| t.id), None);

        let wisp = leak_copy(vanilla, "pumpkinverse_test:wisp");
        assert!(install(vec![leak_copy(vanilla + 1, "pumpkinverse_test:gap")]).is_err());
        install(vec![wisp]).expect("contiguous ids install");

        assert_eq!(count(), vanilla + 1);
        assert_eq!(EntityType::from_raw(vanilla).map(|t| t.id), Some(vanilla));
        assert_eq!(
            EntityType::from_name("pumpkinverse_test:wisp").map(|t| t.id),
            Some(vanilla)
        );
        assert_eq!(EntityType::from_name("wisp").map(|t| t.id), None);
        // Vanilla lookups are unchanged.
        assert_eq!(
            EntityType::from_name("minecraft:pig").map(|t| t.id),
            Some(EntityType::PIG.id)
        );
        assert!(install(vec![]).is_err());
    }
}
