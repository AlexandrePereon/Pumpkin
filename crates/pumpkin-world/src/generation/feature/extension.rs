//! Features and placement modifiers of types Pumpkin does not know.
//!
//! A server that ports content registering its own `feature` or
//! `placement_modifier_type` registers a reader for the type's id here, before the
//! datapacks are read. The datapack feature reader (`feature/datapack.rs`) asks it for
//! any type it does not know itself, as the tree reader does for placers
//! (`features/tree/extension.rs`, whose [`DynCache`] gives the generic helpers back).
//!
//! [`DynCache`]: super::features::tree::extension::DynCache

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use pumpkin_util::math::position::BlockPos;
use pumpkin_util::random::RandomGenerator;

pub use super::datapack::heightmap as read_heightmap;
pub use super::features::tree::extension::{DynCache, Map, Value, read_int_provider};
use crate::generation::proto_chunk::GenerationCache;
use crate::world::WorldPortalExt;

/// What a feature of a registered type gets to place itself.
pub struct FeatureContext<'a> {
    pub chunk: &'a mut dyn GenerationCache,
    pub block_registry: &'a dyn WorldPortalExt,
    pub min_y: i8,
    pub height: u16,
    pub random: &'a mut RandomGenerator,
    /// The position the placed feature's modifiers gave.
    pub pos: BlockPos,
    /// The world's seed (vanilla's `WorldGenLevel.getSeed()`).
    pub seed: u64,
    /// The id of the dimension being decorated (`minecraft:overworld`…).
    pub dimension: &'static str,
}

/// A configured feature of a registered type (vanilla's `Feature.place`).
pub trait ExtensionFeature: Send + Sync {
    /// Places the feature; whether anything was placed.
    fn generate(&self, ctx: FeatureContext<'_>) -> bool;
}

/// What a placement modifier of a registered type gets to move or filter a position.
pub struct PlacementContext<'a> {
    pub chunk: &'a dyn GenerationCache,
    pub block_registry: &'a dyn WorldPortalExt,
    pub min_y: i8,
    pub height: u16,
    pub random: &'a mut RandomGenerator,
    pub pos: BlockPos,
    /// The world's seed (vanilla's `WorldGenLevel.getSeed()`).
    pub seed: u64,
    /// The id of the dimension being decorated (`minecraft:overworld`…).
    pub dimension: &'static str,
}

/// A placement modifier of a registered type (vanilla's `PlacementModifier.getPositions`).
pub trait ExtensionPlacementModifier: Send + Sync {
    /// The positions this modifier makes of `ctx.pos`: none filters it out.
    fn positions(&self, ctx: PlacementContext<'_>) -> Vec<BlockPos>;
}

pub type Reader<T> = fn(&Map<String, Value>) -> Result<Box<T>, String>;

struct Readers {
    feature: HashMap<String, Reader<dyn ExtensionFeature>>,
    placement: HashMap<String, Reader<dyn ExtensionPlacementModifier>>,
}

static READERS: LazyLock<RwLock<Readers>> = LazyLock::new(|| {
    RwLock::new(Readers {
        feature: HashMap::new(),
        placement: HashMap::new(),
    })
});

/// Registers the reader of a feature type's configuration, by its id (`namespace:path`).
pub fn register_feature(id: &str, reader: Reader<dyn ExtensionFeature>) {
    if let Ok(mut readers) = READERS.write() {
        readers.feature.insert(id.to_string(), reader);
    }
}

/// Registers the reader of a placement modifier type, by its id (`namespace:path`).
pub fn register_placement_modifier(id: &str, reader: Reader<dyn ExtensionPlacementModifier>) {
    if let Ok(mut readers) = READERS.write() {
        readers.placement.insert(id.to_string(), reader);
    }
}

/// Whether a reader is registered for this feature type.
#[must_use]
pub fn has_feature(id: &str) -> bool {
    READERS
        .read()
        .is_ok_and(|readers| readers.feature.contains_key(id))
}

/// Builds a feature of a registered type from its `config` object.
///
/// # Errors
/// When no reader is registered for `id`, or the reader refuses the configuration.
pub fn read_feature(
    id: &str,
    config: &Map<String, Value>,
) -> Result<Box<dyn ExtensionFeature>, String> {
    let reader = READERS
        .read()
        .ok()
        .and_then(|readers| readers.feature.get(id).copied())
        .ok_or_else(|| format!("unsupported feature type {id}"))?;
    reader(config).map_err(|e| format!("feature {id}: {e}"))
}

/// Builds a placement modifier of a registered type from its object.
///
/// # Errors
/// When no reader is registered for `id`, or the reader refuses the object.
pub fn read_placement_modifier(
    id: &str,
    object: &Map<String, Value>,
) -> Result<Box<dyn ExtensionPlacementModifier>, String> {
    let reader = READERS
        .read()
        .ok()
        .and_then(|readers| readers.placement.get(id).copied())
        .ok_or_else(|| format!("unsupported placement modifier {id}"))?;
    reader(object).map_err(|e| format!("placement modifier {id}: {e}"))
}
