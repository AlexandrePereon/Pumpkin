//! Runtime extension of the data component type registry.
//!
//! Vanilla component types are the generated, closed [`DataComponent`] enum. A
//! mod declares its own types, with raw ids continuing the vanilla range; the
//! server cannot know their value format, which the mod defines. So this module
//! only holds their identities — names and ids — for registry sync and for
//! recognising them on the wire. Their values travel as opaque bytes in
//! [`crate::item_stack::ItemStack::opaque_patch`].
//!
//! Installed once, before the server accepts connections, and read-only
//! afterwards. Reads are therefore lock-free.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use crate::data_component::DataComponent;

/// Number of vanilla component types. Their raw ids are `0..VANILLA_COUNT`.
pub const VANILLA_COUNT: u16 = vanilla_count();

const fn vanilla_count() -> u16 {
    let mut count = 0u16;
    while count <= u8::MAX as u16 && DataComponent::try_from_id(count as u8).is_some() {
        count += 1;
    }
    count
}

/// Why [`install`] rejected an extension.
#[derive(Debug)]
pub enum InstallError {
    /// An extension is already installed.
    AlreadyInstalled,
    /// Raw ids must continue the vanilla range without gaps.
    IdNotContiguous { expected: u16, found: u16 },
    /// The name is already taken, by vanilla or earlier in this batch.
    DuplicateName { name: &'static str },
    /// Raw ids would exceed what `u16` can carry.
    TooMany,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => {
                write!(f, "a data component extension is already installed")
            }
            Self::IdNotContiguous { expected, found } => {
                write!(f, "expected component type id {expected}, found {found}")
            }
            Self::DuplicateName { name } => {
                write!(f, "component type {name} is already registered")
            }
            Self::TooMany => write!(f, "too many component types to address with a u16 id"),
        }
    }
}

impl std::error::Error for InstallError {}

struct Extension {
    /// Indexed by `raw_id - VANILLA_COUNT`.
    names: Vec<&'static str>,
    by_name: BTreeMap<&'static str, u16>,
}

static EXTENSION: OnceLock<Extension> = OnceLock::new();

/// Installs the extension: `(raw id, full namespaced name)` pairs, ids ascending
/// and continuing the vanilla range without gaps.
///
/// # Errors
///
/// Returns [`InstallError`] when an id is out of sequence, a name collides, or
/// an extension is already installed. Nothing is installed on error.
pub fn install(entries: Vec<(u16, &'static str)>) -> Result<(), InstallError> {
    if EXTENSION.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }

    let mut names = Vec::with_capacity(entries.len());
    let mut by_name = BTreeMap::new();
    let mut next_id = VANILLA_COUNT;

    for (id, name) in entries {
        if id != next_id {
            return Err(InstallError::IdNotContiguous {
                expected: next_id,
                found: id,
            });
        }
        if DataComponent::try_from_name(name).is_some() || by_name.insert(name, id).is_some() {
            return Err(InstallError::DuplicateName { name });
        }
        names.push(name);
        next_id = next_id.checked_add(1).ok_or(InstallError::TooMany)?;
    }

    EXTENSION
        .set(Extension { names, by_name })
        .map_err(|_| InstallError::AlreadyInstalled)
}

/// Total number of component types, vanilla plus extension.
#[must_use]
pub fn count() -> u16 {
    let extra = EXTENSION.get().map_or(0, |e| e.names.len());
    // `install` rejects anything that would not fit.
    VANILLA_COUNT.saturating_add(extra.try_into().unwrap_or(u16::MAX))
}

/// The name of an extension component type, or `None` for a vanilla or unknown id.
#[must_use]
pub fn name_of(id: u16) -> Option<&'static str> {
    let index = usize::from(id.checked_sub(VANILLA_COUNT)?);
    EXTENSION.get()?.names.get(index).copied()
}

/// The raw id of an extension component type, by its full namespaced name.
#[must_use]
pub fn id_of(name: &str) -> Option<u16> {
    EXTENSION.get()?.by_name.get(name).copied()
}

/// Whether a raw id off the wire names an installed extension component type.
#[must_use]
pub fn is_extension(id: i32) -> bool {
    u16::try_from(id).is_ok_and(|id| name_of(id).is_some())
}

#[cfg(test)]
mod tests {
    use super::{VANILLA_COUNT, count, id_of, install, is_extension, name_of};
    use crate::data_component::DataComponent;

    #[test]
    fn vanilla_count_covers_the_generated_enum() {
        assert!(DataComponent::try_from_id((VANILLA_COUNT - 1) as u8).is_some());
        assert!(DataComponent::try_from_id(VANILLA_COUNT as u8).is_none());
    }

    /// One test for the module: the extension is a process-wide `OnceLock`.
    #[test]
    fn resolves_component_types_beyond_the_vanilla_range() {
        let id = VANILLA_COUNT;
        assert!(!is_extension(i32::from(id)));

        assert!(install(vec![(id + 1, "pumpkinverse_test:charge")]).is_err());
        assert!(install(vec![(id, "minecraft:max_stack_size")]).is_err());
        install(vec![(id, "pumpkinverse_test:charge")]).expect("contiguous ids install");

        assert_eq!(count(), id + 1);
        assert_eq!(name_of(id), Some("pumpkinverse_test:charge"));
        assert_eq!(id_of("pumpkinverse_test:charge"), Some(id));
        assert!(is_extension(i32::from(id)));
        assert!(!is_extension(i32::from(id) + 1));
        assert!(!is_extension(1));
        assert!(!is_extension(-1));
        assert!(install(vec![]).is_err());
    }
}
