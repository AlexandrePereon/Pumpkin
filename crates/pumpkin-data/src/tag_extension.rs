//! Runtime extension of the generated tag tables.
//!
//! Vanilla's tags are generated at build time into `generated/tag.rs` and
//! cannot change. This module holds the tags datapacks define or extend — a
//! mod adding its blocks to `minecraft:mineable/pickaxe`, or declaring tags of
//! its own — each whole, as the datapacks leave it. The generated lookups by
//! name consult it first.
//!
//! The extension is installed once, after the datapacks first load, and is
//! read-only afterwards. Reads are therefore lock-free.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::sync::OnceLock;

use crate::tag::{RegistryKey, Tag, get_latest_map};

/// A tag as the datapacks leave it, every entry resolved.
pub struct ExtensionTag {
    pub registry: RegistryKey,
    /// Namespaced, `minecraft:` included, as the generated maps key them.
    pub name: String,
    /// Entry names, without `minecraft:`, as the generated tags write them.
    pub values: Vec<String>,
    /// Raw ids, in the same order as `values`.
    pub ids: Vec<u16>,
}

/// Why [`install`] rejected an extension.
#[derive(Debug)]
pub enum InstallError {
    /// An extension is already installed.
    AlreadyInstalled,
    /// A tag's names and ids do not pair up.
    LengthMismatch { name: String },
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => write!(f, "a tag extension is already installed"),
            Self::LengthMismatch { name } => {
                write!(f, "tag {name} has a different number of names and ids")
            }
        }
    }
}

impl std::error::Error for InstallError {}

#[derive(Default)]
struct Registry {
    tags: HashMap<&'static str, &'static Tag>,
    /// What extended vanilla tags gained, keyed by the vanilla tag's ids:
    /// `Taggable::has_tag` is handed a generated constant, not a name.
    gained: HashMap<&'static [u16], Vec<u16>>,
    /// Every id in `gained`, to dismiss the others without a lookup.
    gained_ids: BTreeSet<u16>,
}

/// A handful of registries at most: a linear search beats hashing the key.
static EXTENSION: OnceLock<Vec<(RegistryKey, Registry)>> = OnceLock::new();

/// Installs the tags the datapacks define or extend.
///
/// A tag that exists in vanilla replaces it for lookups by name, and what it
/// gained is also seen by [`Taggable::has_tag`](crate::tag::Taggable::has_tag).
/// Entries a datapack removes (`"replace": true`) still count for `has_tag`,
/// and so does what a vanilla tag gained for any other vanilla tag with the
/// very same entries.
///
/// # Errors
///
/// Returns [`InstallError`] when a tag's names and ids do not pair up, or an
/// extension is already installed. Nothing is installed on error.
pub fn install(tags: Vec<ExtensionTag>) -> Result<(), InstallError> {
    if EXTENSION.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }

    let mut registries: Vec<(RegistryKey, Registry)> = Vec::new();
    for tag in tags {
        if tag.values.len() != tag.ids.len() {
            return Err(InstallError::LengthMismatch { name: tag.name });
        }
        let index = registries
            .iter()
            .position(|(key, _)| *key == tag.registry)
            .unwrap_or_else(|| {
                registries.push((tag.registry, Registry::default()));
                registries.len() - 1
            });
        let registry = &mut registries[index].1;

        if let Some(vanilla) = get_latest_map(tag.registry).get(tag.name.as_str()) {
            let gained: Vec<u16> = tag
                .ids
                .iter()
                .copied()
                .filter(|id| !vanilla.1.contains(id))
                .collect();
            if !gained.is_empty() {
                registry.gained_ids.extend(&gained);
                registry.gained.entry(vanilla.1).or_default().extend(gained);
            }
        }

        let values: Vec<&'static str> = tag.values.into_iter().map(leak).collect();
        let entry: &'static Tag = Box::leak(Box::new((
            &*Box::leak(values.into_boxed_slice()),
            &*Box::leak(tag.ids.into_boxed_slice()),
        )));
        registry.tags.insert(leak(tag.name), entry);
    }

    EXTENSION
        .set(registries)
        .map_err(|_| InstallError::AlreadyInstalled)
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn registry(key: RegistryKey) -> Option<&'static Registry> {
    EXTENSION
        .get()?
        .iter()
        .find_map(|(k, registry)| (*k == key).then_some(registry))
}

/// The installed tag of that name, if a datapack defined or extended it.
#[inline]
pub(crate) fn tag(key: RegistryKey, name: &str) -> Option<&'static Tag> {
    registry(key)?.tags.get(name).copied()
}

/// Whether the vanilla tag with these ids gained `id` from a datapack.
#[inline]
pub(crate) fn gained(key: RegistryKey, vanilla_ids: &[u16], id: u16) -> bool {
    EXTENSION.get().is_some() && gained_cold(key, vanilla_ids, id)
}

// Off the vanilla path: only reached once a datapack's tags are installed,
// for an id the vanilla tag does not hold.
#[cold]
#[inline(never)]
fn gained_cold(key: RegistryKey, vanilla_ids: &[u16], id: u16) -> bool {
    registry(key).is_some_and(|registry| {
        registry.gained_ids.contains(&id)
            && registry
                .gained
                .get(vanilla_ids)
                .is_some_and(|ids| ids.contains(&id))
    })
}

/// Every tag of a registry as the client must be told: vanilla's, each
/// replaced by its installed version, then the datapacks' own, by name.
#[must_use]
pub fn network_tags(key: RegistryKey) -> Vec<(&'static str, &'static [u16])> {
    let vanilla = get_latest_map(key);
    let installed = registry(key);
    let mut tags: Vec<(&'static str, &'static [u16])> = vanilla
        .entries()
        .map(|(name, tag)| {
            let tag = installed
                .and_then(|r| r.tags.get(name).copied())
                .unwrap_or(*tag);
            (*name, tag.1)
        })
        .collect();
    if let Some(installed) = installed {
        let mut own: Vec<(&'static str, &'static [u16])> = installed
            .tags
            .iter()
            .filter(|(name, _)| !vanilla.contains_key(**name))
            .map(|(name, tag)| (*name, tag.1))
            .collect();
        own.sort_unstable_by_key(|(name, _)| *name);
        tags.extend(own);
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::{ExtensionTag, InstallError, install, network_tags};
    use crate::tag::{self, RegistryKey, Taggable};

    /// A block id past vanilla's, as a mod's would be.
    struct ModBlock(u16);

    impl Taggable for ModBlock {
        fn tag_key() -> RegistryKey {
            RegistryKey::Block
        }
        fn registry_key(&self) -> &'static str {
            "pumpkinverse_test:pedestal"
        }
        fn registry_id(&self) -> u16 {
            self.0
        }
    }

    #[test]
    fn datapack_tags_extend_and_add() {
        const PEDESTAL: u16 = 1291;
        let pickaxe = tag::get_tag_ids(RegistryKey::Block, "minecraft:mineable/pickaxe")
            .unwrap()
            .to_vec();
        let pedestal = ModBlock(PEDESTAL);
        assert!(!pedestal.has_tag(&tag::Block::MINECRAFT_MINEABLE_PICKAXE));

        let mut values: Vec<String> =
            tag::get_tag_values(RegistryKey::Block, "minecraft:mineable/pickaxe")
                .unwrap()
                .iter()
                .map(|v| (*v).to_owned())
                .collect();
        values.push("pumpkinverse_test:pedestal".to_owned());
        let mut ids = pickaxe.clone();
        ids.push(PEDESTAL);

        install(vec![
            ExtensionTag {
                registry: RegistryKey::Block,
                name: "minecraft:mineable/pickaxe".to_owned(),
                values,
                ids,
            },
            ExtensionTag {
                registry: RegistryKey::Block,
                name: "pumpkinverse_test:pedestals".to_owned(),
                values: vec!["pumpkinverse_test:pedestal".to_owned()],
                ids: vec![PEDESTAL],
            },
        ])
        .unwrap();
        assert!(matches!(
            install(Vec::new()),
            Err(InstallError::AlreadyInstalled)
        ));

        // Through the generated constant, by name, bare and namespaced.
        assert!(pedestal.has_tag(&tag::Block::MINECRAFT_MINEABLE_PICKAXE));
        assert!(!pedestal.has_tag(&tag::Block::MINECRAFT_MINEABLE_AXE));
        assert_eq!(pedestal.is_tagged_with("#mineable/pickaxe"), Some(true));
        assert_eq!(
            pedestal.is_tagged_with("pumpkinverse_test:pedestals"),
            Some(true)
        );
        assert_eq!(
            pedestal.is_tagged_with("minecraft:mineable/axe"),
            Some(false)
        );
        assert_eq!(
            tag::get_tag_ids(RegistryKey::Block, "minecraft:mineable/pickaxe")
                .unwrap()
                .len(),
            pickaxe.len() + 1
        );

        let synced = network_tags(RegistryKey::Block);
        let find = |name: &str| synced.iter().find(|(n, _)| *n == name).map(|(_, ids)| *ids);
        assert!(
            find("minecraft:mineable/pickaxe")
                .unwrap()
                .contains(&PEDESTAL)
        );
        assert_eq!(find("pumpkinverse_test:pedestals"), Some(&[PEDESTAL][..]));
        assert_eq!(
            synced.len(),
            tag::get_latest_map(RegistryKey::Block).len() + 1
        );
    }
}
