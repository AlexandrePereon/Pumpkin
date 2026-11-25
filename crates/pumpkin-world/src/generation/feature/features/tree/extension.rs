//! Trunk placers, foliage placers and tree decorators of types Pumpkin does not know.
//!
//! A server that ports content registering its own `trunk_placer_type`,
//! `foliage_placer_type` or `tree_decorator_type` registers a reader for the type's id
//! here, before the datapacks are read. The datapack tree reader
//! (`feature/datapack/tree.rs`) asks it for any type it does not know itself.
//!
//! The placers are trait objects, so they take the chunk as `&mut dyn GenerationCache`;
//! [`DynCache`] turns that back into a [`GenerationCache`] for the generic helpers
//! (`TrunkPlacer::place`, `FoliagePlacer::place_foliage_block`, block state providers…).

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use pumpkin_data::fluid::{Fluid, FluidState};
use pumpkin_data::{Block, BlockId, BlockState, BlockStateId, chunk::Biome};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use pumpkin_util::random::RandomGenerator;
/// The JSON types a [`Reader`] gets, so that a port needs no `serde_json` of its own.
pub use serde_json::{Map, Value};

/// Reads an int provider field (a number or a typed object), as Pumpkin's own readers do.
pub use crate::generation::feature::datapack::int_provider as read_int_provider;

use super::foliage::FoliagePlacer;
use super::trunk::TrunkPlacer;
use super::{TreeFeature, TreeNode};
use crate::ProtoChunk;
use crate::generation::height_limit::HeightLimitView;
use crate::generation::proto_chunk::GenerationCache;
use crate::world::{BlockAccessor, WorldPortalExt};

/// What a trunk placer gets: vanilla `TrunkPlacer.placeTrunk`'s arguments.
pub struct TrunkContext<'a> {
    pub chunk: &'a mut dyn GenerationCache,
    pub block_registry: &'a dyn WorldPortalExt,
    pub random: &'a mut RandomGenerator,
    /// The tree being placed (its providers and placers).
    pub tree: &'a TreeFeature,
    /// The trunk's height, clipped by the room above (`maxFreeTreeHeight`).
    pub height: u32,
    pub start_pos: BlockPos,
    /// The state the trunk provider gave for this tree (Pumpkin samples it once).
    pub trunk_state: &'static BlockState,
}

/// A trunk placer of a type Pumpkin does not know.
pub trait ExtensionTrunkPlacer: Send + Sync {
    /// Places the trunk: the foliage attachment points and the logs placed.
    fn place_trunk(&self, placer: &TrunkPlacer, ctx: TrunkContext<'_>)
    -> (Vec<TreeNode>, Vec<BlockPos>);

    /// Whether the trunk may grow through `block` besides vanilla's replaceable blocks
    /// and logs (an override of `TrunkPlacer.validTreePos`).
    fn can_grow_through(&self, _block: BlockId) -> bool {
        false
    }
}

/// What a foliage placer gets: vanilla `FoliagePlacer.createFoliage`'s arguments.
pub struct FoliageContext<'a> {
    pub chunk: &'a mut dyn GenerationCache,
    pub block_registry: &'a dyn WorldPortalExt,
    pub random: &'a mut RandomGenerator,
    pub tree: &'a TreeFeature,
    /// The trunk's height (`maxFreeTreeHeight`).
    pub trunk_height: i32,
    pub node: &'a TreeNode,
    pub foliage_height: i32,
    pub radius: i32,
    pub offset: i32,
    /// The foliage placed so far; a placer adds the positions it sets.
    pub foliage: &'a mut Vec<BlockPos>,
}

/// A foliage placer of a type Pumpkin does not know.
pub trait ExtensionFoliagePlacer: Send + Sync {
    /// `FoliagePlacer.foliageHeight(random, trunkHeight, tree)`.
    fn foliage_height(&self, random: &mut RandomGenerator, trunk_height: i32) -> i32;

    /// `FoliagePlacer.foliageRadius(random, trunkHeight)`: the placer's `radius` by default.
    fn foliage_radius(
        &self,
        placer: &FoliagePlacer,
        random: &mut RandomGenerator,
        _trunk_height: i32,
    ) -> i32 {
        placer.radius.get(random)
    }

    /// `FoliagePlacer.createFoliage`.
    fn create_foliage(&self, ctx: FoliageContext<'_>);
}

/// What a tree decorator gets: vanilla `TreeDecorator.Context`.
pub struct DecoratorContext<'a> {
    pub chunk: &'a mut dyn GenerationCache,
    pub block_registry: &'a dyn WorldPortalExt,
    pub random: &'a mut RandomGenerator,
    /// Sorted by height, as vanilla's.
    pub roots: &'a [BlockPos],
    pub logs: &'a [BlockPos],
    pub leaves: &'a [BlockPos],
    /// The decorator adds the positions it sets.
    pub decorations: &'a mut Vec<BlockPos>,
}

/// A tree decorator of a type Pumpkin does not know.
pub trait ExtensionTreeDecorator: Send + Sync {
    /// `TreeDecorator.place`.
    fn place(&self, ctx: DecoratorContext<'_>);
}

/// Reads a placer's configuration (the JSON object, `type` included).
pub type Reader<T> = fn(&Map<String, Value>) -> Result<Box<T>, String>;

struct Readers {
    trunk: HashMap<String, Reader<dyn ExtensionTrunkPlacer>>,
    foliage: HashMap<String, Reader<dyn ExtensionFoliagePlacer>>,
    decorator: HashMap<String, Reader<dyn ExtensionTreeDecorator>>,
}

static READERS: LazyLock<RwLock<Readers>> = LazyLock::new(|| {
    RwLock::new(Readers {
        trunk: HashMap::new(),
        foliage: HashMap::new(),
        decorator: HashMap::new(),
    })
});

/// Registers the reader of a trunk placer type (`namespace:path`).
pub fn register_trunk_placer(id: &str, reader: Reader<dyn ExtensionTrunkPlacer>) {
    if let Ok(mut readers) = READERS.write() {
        readers.trunk.insert(id.to_string(), reader);
    }
}

/// Registers the reader of a foliage placer type (`namespace:path`).
pub fn register_foliage_placer(id: &str, reader: Reader<dyn ExtensionFoliagePlacer>) {
    if let Ok(mut readers) = READERS.write() {
        readers.foliage.insert(id.to_string(), reader);
    }
}

/// Registers the reader of a tree decorator type (`namespace:path`).
pub fn register_tree_decorator(id: &str, reader: Reader<dyn ExtensionTreeDecorator>) {
    if let Ok(mut readers) = READERS.write() {
        readers.decorator.insert(id.to_string(), reader);
    }
}

fn read<T: ?Sized>(
    table: impl FnOnce(&Readers) -> Option<Reader<T>>,
    kind: &str,
    id: &str,
    config: &Map<String, Value>,
) -> Result<Box<T>, String> {
    let reader = READERS
        .read()
        .ok()
        .and_then(|readers| table(&readers))
        .ok_or_else(|| format!("unsupported {kind} {id}"))?;
    reader(config).map_err(|e| format!("{kind} {id}: {e}"))
}

/// A registered trunk placer type's placer, read from `config`.
pub fn read_trunk_placer(
    id: &str,
    config: &Map<String, Value>,
) -> Result<Box<dyn ExtensionTrunkPlacer>, String> {
    read(|r| r.trunk.get(id).copied(), "trunk placer", id, config)
}

/// A registered foliage placer type's placer, read from `config`.
pub fn read_foliage_placer(
    id: &str,
    config: &Map<String, Value>,
) -> Result<Box<dyn ExtensionFoliagePlacer>, String> {
    read(|r| r.foliage.get(id).copied(), "foliage placer", id, config)
}

/// A registered tree decorator type's decorator, read from `config`.
pub fn read_tree_decorator(
    id: &str,
    config: &Map<String, Value>,
) -> Result<Box<dyn ExtensionTreeDecorator>, String> {
    read(|r| r.decorator.get(id).copied(), "tree decorator", id, config)
}

/// A `&mut dyn GenerationCache` as a [`GenerationCache`], for the generic helpers.
pub struct DynCache<'a>(pub &'a mut dyn GenerationCache);

impl HeightLimitView for DynCache<'_> {
    fn height(&self) -> u16 {
        self.0.height()
    }

    fn bottom_y(&self) -> i8 {
        self.0.bottom_y()
    }
}

impl BlockAccessor for DynCache<'_> {
    fn get_block(&self, position: &BlockPos) -> &'static Block {
        BlockAccessor::get_block(self.0, position)
    }

    fn get_block_state(&self, position: &BlockPos) -> &'static BlockState {
        BlockAccessor::get_block_state(self.0, position)
    }

    fn get_block_state_id(&self, position: &BlockPos) -> BlockStateId {
        BlockAccessor::get_block_state_id(self.0, position)
    }

    fn get_block_and_state(&self, position: &BlockPos) -> (&'static Block, &'static BlockState) {
        BlockAccessor::get_block_and_state(self.0, position)
    }

    fn unlit_generation_brightness(&self) -> Option<u8> {
        BlockAccessor::unlit_generation_brightness(self.0)
    }
}

impl GenerationCache for DynCache<'_> {
    fn get_center_chunk_mut(&mut self) -> &mut ProtoChunk {
        self.0.get_center_chunk_mut()
    }

    fn get_center_chunk(&self) -> &ProtoChunk {
        self.0.get_center_chunk()
    }

    fn get_chunk_mut(&mut self, chunk_x: i32, chunk_z: i32) -> Option<&mut ProtoChunk> {
        self.0.get_chunk_mut(chunk_x, chunk_z)
    }

    fn get_chunk(&self, chunk_x: i32, chunk_z: i32) -> Option<&ProtoChunk> {
        self.0.get_chunk(chunk_x, chunk_z)
    }

    fn try_get_proto_chunk(&self, chunk_x: i32, chunk_z: i32) -> Option<&ProtoChunk> {
        self.0.try_get_proto_chunk(chunk_x, chunk_z)
    }

    fn get_block_state(&self, pos: &Vector3<i32>) -> BlockStateId {
        GenerationCache::get_block_state(self.0, pos)
    }

    fn get_fluid_and_fluid_state(&self, position: &Vector3<i32>) -> (&'static Fluid, FluidState) {
        self.0.get_fluid_and_fluid_state(position)
    }

    fn set_block_state(&mut self, pos: &Vector3<i32>, block_state: &BlockState) {
        self.0.set_block_state(pos, block_state);
    }

    fn add_block_entity(&mut self, pos: &Vector3<i32>, nbt: NbtCompound) {
        self.0.add_block_entity(pos, nbt);
    }

    fn top_motion_blocking_block_height_exclusive(&self, x: i32, z: i32) -> i32 {
        self.0.top_motion_blocking_block_height_exclusive(x, z)
    }

    fn top_motion_blocking_block_no_leaves_height_exclusive(&self, x: i32, z: i32) -> i32 {
        self.0.top_motion_blocking_block_no_leaves_height_exclusive(x, z)
    }

    fn get_top_y(&self, heightmap: &pumpkin_util::HeightMap, x: i32, z: i32) -> i32 {
        self.0.get_top_y(heightmap, x, z)
    }

    fn top_block_height_exclusive(&self, x: i32, z: i32) -> i32 {
        self.0.top_block_height_exclusive(x, z)
    }

    fn ocean_floor_height_exclusive(&self, x: i32, z: i32) -> i32 {
        self.0.ocean_floor_height_exclusive(x, z)
    }

    fn top_block_wg_height_exclusive(&self, x: i32, z: i32) -> i32 {
        self.0.top_block_wg_height_exclusive(x, z)
    }

    fn ocean_floor_wg_height_exclusive(&self, x: i32, z: i32) -> i32 {
        self.0.ocean_floor_wg_height_exclusive(x, z)
    }

    fn is_air(&self, local_pos: &Vector3<i32>) -> bool {
        self.0.is_air(local_pos)
    }

    fn get_biome_for_terrain_gen(&self, x: i32, y: i32, z: i32) -> &'static Biome {
        self.0.get_biome_for_terrain_gen(x, y, z)
    }

    fn get_blending_data(
        &self,
        chunk_x: i32,
        chunk_z: i32,
    ) -> Option<&crate::generation::blender::blending_data::BlendingData> {
        self.0.get_blending_data(chunk_x, chunk_z)
    }

    fn get_sea_level(&self) -> i32 {
        self.0.get_sea_level()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub;

    impl ExtensionTrunkPlacer for Stub {
        fn place_trunk(
            &self,
            _placer: &TrunkPlacer,
            _ctx: TrunkContext<'_>,
        ) -> (Vec<TreeNode>, Vec<BlockPos>) {
            (Vec::new(), Vec::new())
        }

        fn can_grow_through(&self, block: BlockId) -> bool {
            block == BlockId::MUD
        }
    }

    #[expect(clippy::unnecessary_wraps, reason = "the reader signature")]
    fn read_stub(_config: &Map<String, Value>) -> Result<Box<dyn ExtensionTrunkPlacer>, String> {
        Ok(Box::new(Stub))
    }

    #[test]
    fn registered_types_are_read_and_others_refused() {
        register_trunk_placer("test:stub_trunk_placer", read_stub);
        let config = Map::new();
        let placer = read_trunk_placer("test:stub_trunk_placer", &config);
        assert!(placer.is_ok_and(|p| p.can_grow_through(BlockId::MUD)));
        let unknown = read_foliage_placer("test:unknown", &config);
        assert!(unknown.is_err_and(|e| e == "unsupported foliage placer test:unknown"));
    }
}
