//! Runtime extension of the generated biome registry.
//!
//! Vanilla's biomes are generated at build time into `generated/biome.rs`,
//! numbered in registry order (alphabetical). A datapack can add biomes of its
//! own; the registry sent to clients appends them after vanilla's, sorted by
//! id, and the chunk palettes carry those indices. This module holds them so
//! that `Biome::from_id` and `Biome::from_name` resolve them too.
//!
//! The extension is installed once, before the worlds are created, and is
//! read-only afterwards. `Biome::from_id` stays a `const fn`: vanilla ids never
//! leave the generated match, and only ids past it reach the fallback here.

use std::collections::BTreeMap;
use std::fmt;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::biome::{Biome, SpawnCosts};

/// Why [`install`] rejected an extension.
#[derive(Debug)]
pub enum InstallError {
    /// An extension is already installed.
    AlreadyInstalled,
    /// Ids must continue the vanilla range without gaps, in order.
    IdNotContiguous { expected: usize, found: u8 },
    /// The name is vanilla's, or given twice.
    DuplicateName { name: &'static str },
    /// Ids would exceed what `u8` can carry.
    TooMany,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => write!(f, "a biome extension is already installed"),
            Self::IdNotContiguous { expected, found } => {
                write!(f, "expected biome id {expected}, found {found}")
            }
            Self::DuplicateName { name } => write!(f, "biome {name} is already registered"),
            Self::TooMany => write!(f, "too many biomes to address with a u8 id"),
        }
    }
}

impl std::error::Error for InstallError {}

struct Extension {
    /// Indexed by `id - Biome::ALL.len()`.
    biomes: Vec<&'static Biome>,
    /// By [`key`]: the full id, `minecraft:` left out.
    by_name: BTreeMap<&'static str, &'static Biome>,
}

/// A write-once slot that `const fn` lookups can also read (as the block extension's).
struct ExtensionCell(AtomicPtr<Extension>);

impl ExtensionCell {
    fn get(&self) -> Option<&'static Extension> {
        // SAFETY: the pointer is null or comes from a leaked `Box` set once by
        // `set`, never freed nor replaced.
        unsafe { self.0.load(Ordering::Acquire).as_ref() }
    }

    /// Reads the slot without synchronising, which `const fn` requires.
    ///
    /// Only ids past the generated biomes reach this. Such an id exists only
    /// after `install`, which runs once at startup before anything looks one
    /// up, and the pointer is never written again; reading it plainly then
    /// races with nothing.
    const fn get_unsynchronized(&'static self) -> Option<&'static Extension> {
        // SAFETY: see above; the pointer is null or a leaked `Box`.
        unsafe { (*self.0.as_ptr()).as_ref() }
    }

    fn set(&self, extension: Extension) -> Result<(), Extension> {
        let new = Box::into_raw(Box::new(extension));
        match self
            .0
            .compare_exchange(ptr::null_mut(), new, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(()),
            // SAFETY: `new` was not published, so this is its only owner.
            Err(_) => Err(*unsafe { Box::from_raw(new) }),
        }
    }
}

static EXTENSION: ExtensionCell = ExtensionCell(AtomicPtr::new(ptr::null_mut()));

/// The name biomes are looked up by: the id as given, without `minecraft:`
/// (the generated biomes are named so).
fn key(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

/// Installs the datapacks' own biomes.
///
/// Each must be leaked to `'static`, its `id` continuing the vanilla range
/// (the first is `Biome::ALL.len()`), in order and without gaps: the order of
/// the registry sent to clients. `registry_id` is the full id, `minecraft:`
/// left out as vanilla's are.
///
/// # Errors
///
/// Returns [`InstallError`] when an id is out of sequence or past `u8`, a name
/// collides, or an extension is already installed. Nothing is installed on
/// error.
pub fn install(biomes: Vec<&'static Biome>) -> Result<(), InstallError> {
    if EXTENSION.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }
    if Biome::ALL.len() + biomes.len() > usize::from(u8::MAX) + 1 {
        return Err(InstallError::TooMany);
    }
    let mut by_name = BTreeMap::new();
    for (offset, biome) in biomes.iter().enumerate() {
        let expected = Biome::ALL.len() + offset;
        if usize::from(biome.id) != expected {
            return Err(InstallError::IdNotContiguous {
                expected,
                found: biome.id,
            });
        }
        let name = key(biome.registry_id);
        // Only vanilla is visible to `from_name` before the install.
        if Biome::from_name(name).is_some() || by_name.insert(name, *biome).is_some() {
            return Err(InstallError::DuplicateName {
                name: biome.registry_id,
            });
        }
    }
    EXTENSION
        .set(Extension { biomes, by_name })
        .map_err(|_| InstallError::AlreadyInstalled)
}

/// The installed biome with this id, past the generated ones.
#[cold]
#[inline(never)]
#[must_use]
pub const fn by_id(id: u8) -> Option<&'static Biome> {
    let Some(extension) = EXTENSION.get_unsynchronized() else {
        return None;
    };
    let index = id as usize;
    if index < Biome::ALL.len() || index - Biome::ALL.len() >= extension.biomes.len() {
        return None;
    }
    Some(extension.biomes.as_slice()[index - Biome::ALL.len()])
}

/// The installed biome of this name (`ns:path`; `minecraft:` optional).
#[cold]
#[inline(never)]
#[must_use]
pub fn by_name(name: &str) -> Option<&'static Biome> {
    EXTENSION.get()?.by_name.get(key(name)).copied()
}

/// A biome by its full id (`minecraft:plains`, `verse:test_biome`), vanilla's or installed.
#[must_use]
pub fn from_full_id(id: &str) -> Option<&'static Biome> {
    Biome::from_name(key(id))
}

/// A biome's full id: `minecraft:` and vanilla's name, or a pack biome's id as installed.
#[must_use]
pub fn full_id(biome: &Biome) -> String {
    if biome.registry_id.contains(':') {
        biome.registry_id.to_string()
    } else {
        format!("minecraft:{}", biome.registry_id)
    }
}

/// The installed biomes, in id order.
#[must_use]
pub fn biomes() -> &'static [&'static Biome] {
    EXTENSION.get().map_or(&[], |e| e.biomes.as_slice())
}

/// Vanilla's biomes then the installed ones, in id order.
pub fn all() -> impl Iterator<Item = &'static Biome> {
    Biome::ALL.iter().copied().chain(biomes().iter().copied())
}

/// An empty spawn cost map, for a biome built at runtime.
#[must_use]
pub const fn no_spawn_costs() -> phf::Map<&'static str, SpawnCosts> {
    phf::phf_map! {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biome::SpawnGroups;

    fn biome(id: u8, registry_id: &'static str) -> &'static Biome {
        Box::leak(Box::new(Biome {
            id,
            registry_id,
            be_network_id: Biome::PLAINS.be_network_id,
            boolean_attributes: &[],
            weather: Biome::PLAINS.weather.clone(),
            carvers: &[],
            features: &[],
            creature_spawn_probability: 0.1,
            spawners: SpawnGroups {
                monster: &[],
                ambient: &[],
                axolotls: &[],
                creature: &[],
                misc: &[],
                underground_water_creature: &[],
                water_ambient: &[],
                water_creature: &[],
            },
            spawn_costs: no_spawn_costs(),
        }))
    }

    // One test: the extension is process-wide and installed once.
    #[test]
    fn extension_ids_and_names() {
        let first = u8::try_from(Biome::ALL.len()).unwrap();
        assert!(matches!(
            install(vec![biome(first + 1, "verse:gap")]),
            Err(InstallError::IdNotContiguous { .. })
        ));
        assert!(matches!(
            install(vec![biome(first, "plains")]),
            Err(InstallError::DuplicateName { .. })
        ));
        assert!(matches!(
            install(vec![biome(first, "verse:a"), biome(first + 1, "verse:a")]),
            Err(InstallError::DuplicateName { .. })
        ));
        assert!(Biome::from_id(first).is_none());

        install(vec![biome(first, "verse:a"), biome(first + 1, "b")]).unwrap();
        assert_eq!(Biome::from_id(first).unwrap().registry_id, "verse:a");
        assert_eq!(Biome::from_id(first + 1).unwrap().registry_id, "b");
        assert!(Biome::from_id(first + 2).is_none());
        assert_eq!(Biome::from_id(0).unwrap().registry_id, "badlands");
        assert_eq!(Biome::from_name("verse:a").unwrap().id, first);
        assert_eq!(Biome::from_name("b").unwrap().id, first + 1);
        assert_eq!(from_full_id("minecraft:b").unwrap().id, first + 1);
        assert_eq!(from_full_id("minecraft:plains").unwrap().id, Biome::PLAINS.id);
        assert_eq!(full_id(Biome::from_id(first).unwrap()), "verse:a");
        assert_eq!(full_id(&Biome::PLAINS), "minecraft:plains");
        assert_eq!(all().count(), Biome::ALL.len() + 2);
        assert!(matches!(
            install(vec![biome(first + 2, "verse:c")]),
            Err(InstallError::AlreadyInstalled)
        ));
    }
}
