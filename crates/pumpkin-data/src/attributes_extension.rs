//! Runtime extension of the generated attribute registry.
//!
//! The counterpart of [`crate::entity_type_extension`] for attributes. Vanilla
//! attributes are generated into `generated/attributes.rs` as
//! [`Attributes`] constants and listed in [`Attributes::ALL`]; attributes
//! contributed at startup get raw ids continuing the vanilla range. `Attributes`
//! is a plain struct, so an extension attribute is an ordinary value: only the
//! lookups by id and by name need to know about it, and they go through here.
//!
//! The extension is installed once, before the server accepts connections, and
//! is read-only afterwards. Reads are therefore lock-free.

use std::fmt;
use std::sync::OnceLock;

use crate::attributes::Attributes;

/// Why [`install`] rejected an extension.
#[derive(Debug)]
pub enum InstallError {
    /// An extension is already installed.
    AlreadyInstalled,
    /// Raw ids must continue the vanilla range without gaps.
    IdNotContiguous { expected: u8, found: u8 },
    /// The name is already taken, either by vanilla or earlier in this batch.
    DuplicateName { name: &'static str },
    /// Raw ids would exceed what `u8` can carry.
    TooMany,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => write!(f, "an attribute extension is already installed"),
            Self::IdNotContiguous { expected, found } => {
                write!(f, "expected attribute id {expected}, found {found}")
            }
            Self::DuplicateName { name } => write!(f, "attribute name {name} is already registered"),
            Self::TooMany => write!(f, "too many attributes to address with a u8 id"),
        }
    }
}

impl std::error::Error for InstallError {}

/// Indexed by `raw_id - vanilla_count()`.
static EXTENSION: OnceLock<Vec<&'static Attributes>> = OnceLock::new();

/// How many attributes the generated code knows.
#[must_use]
pub const fn vanilla_count() -> u8 {
    // 40 on 26.3: far from `u8::MAX`.
    Attributes::ALL.len() as u8
}

/// Installs the runtime attribute extension.
///
/// Every attribute must already be leaked to `'static`, carry a raw id
/// continuing the vanilla range, gapless and ascending, and a full
/// `namespace:path` name.
///
/// # Errors
///
/// Returns [`InstallError`] when an id is out of sequence, a name collides, or
/// an extension is already installed. Nothing is installed on error.
pub fn install(entries: Vec<&'static Attributes>) -> Result<(), InstallError> {
    if EXTENSION.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }

    let mut next_id = vanilla_count();
    for (index, attribute) in entries.iter().enumerate() {
        if attribute.id != next_id {
            return Err(InstallError::IdNotContiguous {
                expected: next_id,
                found: attribute.id,
            });
        }
        let taken = Attributes::ALL.iter().any(|a| a.name == attribute.name)
            || entries[..index].iter().any(|a| a.name == attribute.name);
        if taken {
            return Err(InstallError::DuplicateName {
                name: attribute.name,
            });
        }
        next_id = next_id.checked_add(1).ok_or(InstallError::TooMany)?;
    }

    EXTENSION
        .set(entries)
        .map_err(|_| InstallError::AlreadyInstalled)
}

/// Total number of attributes, vanilla plus extension.
#[must_use]
pub fn count() -> u8 {
    let extra = EXTENSION.get().map_or(0, Vec::len);
    // `install` rejects anything that would not fit.
    vanilla_count().saturating_add(extra.try_into().unwrap_or(u8::MAX))
}

/// The attribute with raw id `id`, vanilla or extension.
#[must_use]
pub fn by_id(id: u8) -> Option<&'static Attributes> {
    Attributes::ALL
        .iter()
        .find(|a| a.id == id)
        .or_else(|| extension_by_id(id))
}

#[cold]
#[inline(never)]
fn extension_by_id(id: u8) -> Option<&'static Attributes> {
    EXTENSION
        .get()?
        .get(usize::from(id.checked_sub(vanilla_count())?))
        .copied()
}

/// The attribute named `name`, vanilla or extension. A name without namespace
/// is a vanilla one, as everywhere in Minecraft.
#[must_use]
pub fn by_name(name: &str) -> Option<&'static Attributes> {
    let vanilla = name.strip_prefix("minecraft:").unwrap_or(name);
    Attributes::ALL
        .iter()
        .find(|a| a.name.strip_prefix("minecraft:") == Some(vanilla))
        .or_else(|| extension_by_name(name))
}

#[cold]
#[inline(never)]
fn extension_by_name(name: &str) -> Option<&'static Attributes> {
    EXTENSION.get()?.iter().copied().find(|a| a.name == name)
}

/// Every attribute, vanilla first, in raw-id order.
pub fn all() -> impl Iterator<Item = &'static Attributes> {
    Attributes::ALL
        .iter()
        .chain(EXTENSION.get().into_iter().flatten().copied())
}

#[cfg(test)]
mod tests {
    use super::{all, by_id, by_name, count, install, vanilla_count};
    use crate::attributes::Attributes;

    /// One test only: `install` writes a process-wide `OnceLock`.
    #[test]
    fn resolves_extension_attributes_by_id_and_name() {
        let resonance: &'static Attributes = Box::leak(Box::new(Attributes {
            id: vanilla_count(),
            default_value: 5.0,
            name: "verse:resonance",
        }));
        let clash: &'static Attributes = Box::leak(Box::new(Attributes {
            id: vanilla_count(),
            default_value: 0.0,
            name: "minecraft:armor",
        }));
        assert!(install(vec![clash]).is_err(), "a vanilla name was accepted");
        assert!(install(vec![resonance]).is_ok());

        assert_eq!(count(), vanilla_count() + 1);
        assert_eq!(by_id(vanilla_count()).map(|a| a.name), Some("verse:resonance"));
        assert_eq!(by_name("verse:resonance").map(|a| a.id), Some(vanilla_count()));
        // Vanilla still resolves, with or without its namespace.
        assert_eq!(by_name("armor").map(|a| a.id), Some(Attributes::ARMOR.id));
        assert_eq!(by_name("minecraft:armor").map(|a| a.id), Some(Attributes::ARMOR.id));
        assert_eq!(by_id(Attributes::ARMOR.id).map(|a| a.name), Some("minecraft:armor"));
        assert_eq!(by_name("verse:unknown"), None);
        assert_eq!(all().last().map(|a| a.name), Some("verse:resonance"));
        assert!(install(vec![]).is_err(), "installed twice");
    }
}
