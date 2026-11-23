//! Runtime extension of the generated structures and structure sets.
//!
//! Vanilla's structures and structure sets are generated at build time into
//! `generated/structures.rs`. A datapack can add structures and structure sets of its
//! own; this module holds them, so that a structure added at runtime has a key
//! ([`StructureKeys::Extension`]) that works everywhere a vanilla one does: structure
//! starts and references, the placement cache, the per-step lists and locate.
//!
//! The extension is installed once, before the worlds are created, and is read-only
//! afterwards. `StructureKeys::to_name` and `Structure::get` stay `const fn`: only an
//! extension key reaches the fallback here, and such a key exists only after
//! [`install`].
//!
//! Structure sets are numbered after vanilla's: index `i` below
//! `StructureSet::ALL.len()` is vanilla's `ALL[i]`, the rest are the installed ones in
//! order (see [`set`]).

use std::collections::BTreeMap;
use std::fmt;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::structures::{
    GenerationStep, Structure, StructureKeys, StructureSet, StructureType, TerrainAdaptation,
};

/// Why [`install`] rejected an extension.
#[derive(Debug)]
pub enum InstallError {
    /// An extension is already installed.
    AlreadyInstalled,
    /// The name is vanilla's, or given twice.
    DuplicateName { name: &'static str },
    /// More structures than a `u16` key can address.
    TooMany,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => write!(f, "a structure extension is already installed"),
            Self::DuplicateName { name } => write!(f, "{name} is already registered"),
            Self::TooMany => write!(f, "too many structures for a u16 key"),
        }
    }
}

impl std::error::Error for InstallError {}

struct Extension {
    /// Indexed by the key's `u16`: the full id and the structure.
    structures: Vec<(&'static str, &'static Structure)>,
    structures_by_name: BTreeMap<&'static str, u16>,
    /// The structure sets after vanilla's, in index order.
    sets: Vec<(&'static str, &'static StructureSet)>,
    sets_by_name: BTreeMap<&'static str, usize>,
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
    /// Only extension keys reach this. Such a key exists only after `install`, which
    /// runs once at startup before anything generates, and the pointer is never
    /// written again; reading it plainly then races with nothing.
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

/// What an extension key resolves to if nothing is installed, which cannot happen
/// for a key [`install`] handed out: a structure that generates nowhere.
static MISSING: Structure = Structure {
    biomes: "",
    step: GenerationStep::SurfaceStructures,
    start_pool: None,
    start_jigsaw_name: None,
    size: None,
    terrain_adaptation: TerrainAdaptation::None,
    start_height: None,
    project_start_to_heightmap: None,
    max_distance_from_center: None,
    liquid_settings: None,
    dimension_padding: None,
    use_expansion_hack: None,
    pool_aliases: &[],
    spawn_overrides: &[],
    is_beached: None,
    mineshaft_type: None,
    biome_temp: None,
    cluster_probability: None,
    large_probability: None,
    structure_type: StructureType::Unknown,
};

/// The name structures and sets are looked up by: the id, `minecraft:` left out (the
/// generated ones are named so).
fn key(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

/// Installs the datapacks' own structures and structure sets.
///
/// Structure `i` of `structures` gets the key `StructureKeys::Extension(i)`, which the
/// sets' entries name; each set gets the index `StructureSet::ALL.len()` plus its
/// position. Names are full ids (`namespace:path`) and everything is leaked to
/// `'static`.
///
/// # Errors
///
/// Returns [`InstallError`] when a name collides with vanilla's or another one, there
/// are too many structures, or an extension is already installed. Nothing is installed
/// on error.
pub fn install(
    structures: Vec<(&'static str, &'static Structure)>,
    sets: Vec<(&'static str, &'static StructureSet)>,
) -> Result<(), InstallError> {
    if EXTENSION.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }
    if structures.len() > usize::from(u16::MAX) + 1 {
        return Err(InstallError::TooMany);
    }
    let mut structures_by_name = BTreeMap::new();
    for (index, (name, _)) in structures.iter().enumerate() {
        // Only vanilla is visible to `from_name` before the install.
        if StructureKeys::from_name(name).is_some()
            || structures_by_name.insert(key(name), index as u16).is_some()
        {
            return Err(InstallError::DuplicateName { name });
        }
    }
    let mut sets_by_name = BTreeMap::new();
    for (offset, (name, _)) in sets.iter().enumerate() {
        if StructureSet::get(key(name)).is_some()
            || sets_by_name
                .insert(key(name), StructureSet::ALL.len() + offset)
                .is_some()
        {
            return Err(InstallError::DuplicateName { name });
        }
    }
    EXTENSION
        .set(Extension {
            structures,
            structures_by_name,
            sets,
            sets_by_name,
        })
        .map_err(|_| InstallError::AlreadyInstalled)
}

/// The installed structure of this name (`namespace:path`; `minecraft:` optional).
#[cold]
#[inline(never)]
#[must_use]
pub fn key_by_name(name: &str) -> Option<StructureKeys> {
    EXTENSION
        .get()?
        .structures_by_name
        .get(key(name))
        .map(|&index| StructureKeys::Extension(index))
}

/// An installed structure's full id.
#[cold]
#[inline(never)]
#[must_use]
pub const fn name(index: u16) -> &'static str {
    let Some(extension) = EXTENSION.get_unsynchronized() else {
        return "";
    };
    let index = index as usize;
    if index >= extension.structures.len() {
        return "";
    }
    extension.structures.as_slice()[index].0
}

/// An installed structure.
#[cold]
#[inline(never)]
#[must_use]
pub const fn structure(index: u16) -> &'static Structure {
    let Some(extension) = EXTENSION.get_unsynchronized() else {
        return &MISSING;
    };
    let index = index as usize;
    if index >= extension.structures.len() {
        return &MISSING;
    }
    extension.structures.as_slice()[index].1
}

/// A structure's full id: `minecraft:` and vanilla's name, or an installed one's id.
#[must_use]
pub fn full_name(key: StructureKeys) -> String {
    match key {
        StructureKeys::Extension(index) => name(index).to_string(),
        _ => format!("minecraft:{}", key.to_name()),
    }
}

/// Every structure key, vanilla's then the installed ones.
pub fn all_keys() -> impl Iterator<Item = StructureKeys> {
    let vanilla = StructureKeys::all_names()
        .iter()
        .filter_map(|name| StructureKeys::from_name(name));
    let installed = EXTENSION.get().map_or(0, |e| e.structures.len());
    vanilla.chain((0..installed).map(|index| StructureKeys::Extension(index as u16)))
}

/// The installed structures' full ids, in key order.
pub fn names() -> impl Iterator<Item = &'static str> {
    EXTENSION
        .get()
        .into_iter()
        .flat_map(|e| e.structures.iter().map(|(name, _)| *name))
}

/// How many structure sets there are, vanilla's and the installed ones.
#[must_use]
pub fn set_count() -> usize {
    StructureSet::ALL.len() + EXTENSION.get().map_or(0, |e| e.sets.len())
}

/// The structure set at `index`: vanilla's `ALL[index]`, then the installed ones.
#[must_use]
pub fn set(index: usize) -> Option<&'static StructureSet> {
    StructureSet::ALL.get(index).or_else(|| {
        EXTENSION
            .get()?
            .sets
            .get(index - StructureSet::ALL.len())
            .map(|(_, set)| *set)
    })
}

/// The name of the structure set at `index`: vanilla's without `minecraft:`, an
/// installed one's full id.
#[must_use]
pub fn set_name(index: usize) -> Option<&'static str> {
    StructureSet::NAMES.get(index).copied().or_else(|| {
        EXTENSION
            .get()?
            .sets
            .get(index - StructureSet::ALL.len())
            .map(|(name, _)| *name)
    })
}

/// A structure set by name (`minecraft:` optional), vanilla's or installed.
#[must_use]
pub fn set_by_name(name: &str) -> Option<&'static StructureSet> {
    let name = key(name);
    StructureSet::get(name).or_else(|| {
        let extension = EXTENSION.get()?;
        let index = *extension.sets_by_name.get(name)?;
        extension
            .sets
            .get(index - StructureSet::ALL.len())
            .map(|(_, set)| *set)
    })
}

/// Every structure set with its index and name, vanilla's then the installed ones.
pub fn all_sets() -> impl Iterator<Item = (usize, &'static str, &'static StructureSet)> {
    let vanilla = StructureSet::ALL
        .iter()
        .zip(StructureSet::NAMES)
        .map(|(set, name)| (*name, set));
    let installed = EXTENSION
        .get()
        .into_iter()
        .flat_map(|e| e.sets.iter().copied());
    vanilla
        .chain(installed)
        .enumerate()
        .map(|(index, (name, set))| (index, name, set))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structures::{
        RandomSpreadStructurePlacement, StructurePlacement, StructurePlacementType, WeightedEntry,
    };

    fn new_structure(biomes: &'static str) -> &'static Structure {
        Box::leak(Box::new(Structure {
            biomes,
            ..MISSING
        }))
    }

    fn new_set(structures: Vec<WeightedEntry>) -> &'static StructureSet {
        Box::leak(Box::new(StructureSet {
            placement: StructurePlacement {
                frequency_reduction_method: None,
                frequency: None,
                salt: 1,
                exclusion_zone: None,
                placement_type: StructurePlacementType::RandomSpread(
                    RandomSpreadStructurePlacement {
                        spacing: 8,
                        separation: 4,
                        spread_type: None,
                    },
                ),
            },
            structures: structures.leak(),
        }))
    }

    // One test: the extension is process-wide and installed once.
    #[test]
    fn extension_keys_and_sets() {
        assert!(matches!(
            install(vec![("minecraft:igloo", new_structure("#a"))], vec![]),
            Err(InstallError::DuplicateName { .. })
        ));
        assert!(matches!(
            install(vec![], vec![("villages", new_set(vec![]))]),
            Err(InstallError::DuplicateName { .. })
        ));
        assert!(StructureKeys::from_name("verse:arch").is_none());

        let arch = StructureKeys::Extension(0);
        install(
            vec![
                ("verse:arch", new_structure("#verse:arch_biomes")),
                ("verse:tower", new_structure("#verse:tower_biomes")),
            ],
            vec![(
                "verse:arches",
                new_set(vec![WeightedEntry {
                    structure: arch,
                    weight: 1,
                }]),
            )],
        )
        .unwrap();
        assert_eq!(StructureKeys::from_name("verse:arch"), Some(arch));
        assert_eq!(arch.to_name(), "verse:arch");
        assert_eq!(full_name(arch), "verse:arch");
        assert_eq!(full_name(StructureKeys::Igloo), "minecraft:igloo");
        assert_eq!(Structure::get(&arch).biomes, "#verse:arch_biomes");
        assert_eq!(
            Structure::get(&StructureKeys::Extension(1)).biomes,
            "#verse:tower_biomes"
        );
        assert_eq!(Structure::get(&StructureKeys::Extension(2)).biomes, "");
        assert_eq!(all_keys().count(), StructureKeys::all_names().len() + 2);

        let index = StructureSet::ALL.len();
        assert_eq!(set_count(), index + 1);
        assert_eq!(set_name(index), Some("verse:arches"));
        assert!(set(index + 1).is_none());
        assert_eq!(
            set_by_name("verse:arches").unwrap().structures[0].structure,
            arch
        );
        assert!(set_by_name("minecraft:villages").is_some());
        assert_eq!(all_sets().last().map(|(i, _, _)| i), Some(index));
        assert!(matches!(
            install(vec![], vec![]),
            Err(InstallError::AlreadyInstalled)
        ));
    }
}
