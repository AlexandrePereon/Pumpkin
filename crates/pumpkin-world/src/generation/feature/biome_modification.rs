//! Placed features added to existing biomes by code, at startup.
//!
//! This is how most mods add worldgen: not by overriding a biome's file (see
//! [`super::biome_datapack`]), but through their loader's biome modification API (Fabric's
//! `BiomeModifications`, phase `ADDITIONS`: "add placed feature K at step S to every biome
//! matching P"). A server that ports such content registers each modification here, before
//! the datapacks' worldgen files are read.
//!
//! The modifications apply while the biomes' feature lists are built
//! ([`super::super::noise::router::datapack::WorldgenJson::build_datapack_biomes`]): after the
//! datapack files, so a modification adds to the list a datapack left, and before the feature
//! order is computed, so the added features take their place in the global order of their step
//! (and shift the decoration seeds of the features after them) as on a modded server, whose
//! `FeatureSorter` runs on the modified lists. Within a biome, modifications apply in the order
//! of their ids, as Fabric sorts a phase's modifications; each adds its feature at the end of
//! the step's list.
//!
//! The feature is named by id: a vanilla placed feature (overridden by a datapack or not) or a
//! datapack's own. Its predicate is asked once per biome, vanilla's and the packs' own; biome
//! tags from datapacks are installed by then.

use std::sync::RwLock;

use pumpkin_data::chunk::Biome;

/// Which biomes a modification applies to.
pub type BiomePredicate = Box<dyn Fn(&'static Biome) -> bool + Send + Sync>;

/// One modification: a placed feature added at the end of a decoration step's list.
pub struct BiomeModification {
    /// Its id (`namespace:path`): modifications apply in id order.
    pub id: String,
    /// The decoration step, as vanilla's `GenerationStep.Decoration` ordinal (0 raw
    /// generation … 9 vegetal decoration, 10 top layer modification).
    pub step: usize,
    /// The placed feature's id.
    pub feature: String,
    /// The biomes it applies to.
    pub biomes: BiomePredicate,
}

static REGISTERED: RwLock<Vec<BiomeModification>> = RwLock::new(Vec::new());

/// Registers a modification. Call before the datapacks' worldgen files are read: one
/// registered later changes nothing until a restart.
pub fn register(modification: BiomeModification) {
    if let Ok(mut registered) = REGISTERED.write() {
        registered.push(modification);
        registered.sort_by(|a, b| a.id.cmp(&b.id));
    }
}

/// Whether any modification is registered.
#[must_use]
pub fn any() -> bool {
    REGISTERED.read().is_ok_and(|registered| !registered.is_empty())
}

/// Runs `f` on the registered modifications, in id order.
pub(crate) fn with_registered<R>(f: impl FnOnce(&[BiomeModification]) -> R) -> R {
    let registered = REGISTERED
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&registered)
}
