//! Structures of types Pumpkin does not know.
//!
//! A server that ports content registering its own `structure_type` registers a reader
//! for the type's id here, before the datapacks are read. The datapack structure reader
//! (`structure/datapack.rs`) builds every `worldgen/structure` file of such a type with
//! it; the structure then gets an extension key (`StructureKeys::Extension`) and is
//! started, referenced, placed and located like a vanilla one.
//!
//! A structure of a registered type follows vanilla's `Structure.generate`:
//! [`ExtensionStructure::find_generation_point`] (`findGenerationPoint`), the biome check
//! at the position it returns, then [`ExtensionStructure::add_pieces`] (the generation
//! stub's pieces builder). Its pieces are ordinary [`StructurePieceBase`]s, placed by the
//! structure pass of the structure's decoration step.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, OnceLock, RwLock};

use pumpkin_data::chunk::Biome;
use pumpkin_util::HeightMap;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::random::RandomGenerator;

pub use crate::ProtoChunk;
pub use crate::generation::feature::features::tree::extension::{Map, Value, read_int_provider};
pub use crate::generation::structure::piece::StructurePieceType;
pub use crate::generation::structure::structures::{
    StructurePiece, StructurePieceBase, StructurePiecesCollector,
};
pub use crate::world::WorldPortalExt;
pub use pumpkin_util::math::block_box::BlockBox;
use crate::generation::structure::structures::{
    HeightSampler, StructureGeneratorContext, StructurePosition,
};
use crate::generation::{biome_coords, positions::chunk_pos};

/// What a structure of a registered type gets: vanilla's `Structure.GenerationContext`.
pub struct StructureContext<'a> {
    pub seed: i64,
    pub chunk_x: i32,
    pub chunk_z: i32,
    /// The structure's random, seeded from the world seed and the chunk
    /// (`setLargeFeatureSeed`), shared by both steps.
    pub random: RandomGenerator,
    pub sea_level: i32,
    pub min_y: i32,
    pub height: u16,
    heights: Option<&'a mut dyn HeightSampler>,
    biomes: &'a mut dyn FnMut(i32, i32, i32) -> &'static Biome,
}

impl StructureContext<'_> {
    /// The highest occupied block of a column for this heightmap, from the noise
    /// (`ChunkGenerator.getFirstOccupiedHeight`). Only the world generation heightmaps
    /// exist before the chunk does; any other reads as `WORLD_SURFACE_WG`.
    pub fn first_occupied_height(&mut self, x: i32, z: i32, heightmap: HeightMap) -> i32 {
        let Some(heights) = self.heights.as_deref_mut() else {
            return self.sea_level;
        };
        match heightmap {
            HeightMap::OceanFloorWg | HeightMap::OceanFloor => {
                heights.estimate_ocean_floor_height(x, z)
            }
            _ => heights.estimate_height(x, z) - 1,
        }
    }

    /// The biome source's biome at a quart position, before any blending
    /// (`GenerationContext.biomeResolver().getNoiseBiome`).
    pub fn noise_biome(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> &'static Biome {
        (self.biomes)(quart_x, quart_y, quart_z)
    }

    /// The chunk's middle block (`ChunkPos.getMiddleBlockX` / `Z`).
    #[must_use]
    pub const fn chunk_middle(&self) -> (i32, i32) {
        (
            chunk_pos::get_center_x(self.chunk_x),
            chunk_pos::get_center_z(self.chunk_z),
        )
    }

    /// The chunk's middle block on top of the given heightmap
    /// (`Structure.onTopOfChunkCenter`'s position).
    pub fn on_top_of_chunk_center(&mut self, heightmap: HeightMap) -> BlockPos {
        let (x, z) = self.chunk_middle();
        let y = self.first_occupied_height(x, z, heightmap);
        BlockPos::new(x, y, z)
    }
}

/// A structure of a registered type (vanilla's `Structure` subclass).
pub trait ExtensionStructure: Send + Sync {
    /// Where the structure starts in this chunk, if it does (`findGenerationPoint`).
    fn find_generation_point(&self, ctx: &mut StructureContext<'_>) -> Option<BlockPos>;

    /// Adds the structure's pieces, once the start passed the biome check (the
    /// generation stub's pieces builder). No piece means no structure.
    fn add_pieces(
        &self,
        ctx: &mut StructureContext<'_>,
        start: BlockPos,
        collector: &mut StructurePiecesCollector,
    );
}

/// Builds a structure of a registered type from its whole `worldgen/structure` object
/// (the shared settings and the type's own fields).
pub type Reader = fn(&Map<String, Value>) -> Result<Box<dyn ExtensionStructure>, String>;

static READERS: LazyLock<RwLock<HashMap<String, Reader>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// The installed structures of registered types, indexed like their extension keys.
static INSTALLED: OnceLock<Vec<Box<dyn ExtensionStructure>>> = OnceLock::new();

/// Registers the reader of a structure type, by its id (`namespace:path`).
pub fn register_structure_type(id: &str, reader: Reader) {
    if let Ok(mut readers) = READERS.write() {
        readers.insert(id.to_string(), reader);
    }
}

/// Whether a reader is registered for this structure type.
#[must_use]
pub fn has_structure_type(id: &str) -> bool {
    READERS
        .read()
        .is_ok_and(|readers| readers.contains_key(id))
}

/// Builds a structure of a registered type from its object.
///
/// # Errors
/// When no reader is registered for `id`, or the reader refuses the object.
pub fn read_structure(
    id: &str,
    object: &Map<String, Value>,
) -> Result<Box<dyn ExtensionStructure>, String> {
    let reader = READERS
        .read()
        .ok()
        .and_then(|readers| readers.get(id).copied())
        .ok_or_else(|| format!("unsupported structure type {id}"))?;
    reader(object).map_err(|e| format!("structure type {id}: {e}"))
}

/// Installs the structures of registered types, in extension key order. Returns false
/// if they were installed already.
pub(crate) fn install(structures: Vec<Box<dyn ExtensionStructure>>) -> bool {
    INSTALLED.set(structures).is_ok()
}

/// Generates the structure of extension key `index` in the context's chunk: vanilla's
/// `Structure.generate`, with `is_valid_biome` deciding on the biome at the start.
pub(crate) fn generate(
    index: u16,
    context: StructureGeneratorContext<'_>,
    biomes: &mut dyn FnMut(i32, i32, i32) -> &'static Biome,
    is_valid_biome: impl FnOnce(&'static Biome) -> bool,
) -> Option<StructurePosition> {
    let structure = INSTALLED.get()?.get(usize::from(index))?;
    let StructureGeneratorContext {
        seed,
        chunk_x,
        chunk_z,
        random,
        sea_level,
        min_y,
        height,
        height_sampler,
        ..
    } = context;
    let mut ctx = StructureContext {
        seed,
        chunk_x,
        chunk_z,
        random,
        sea_level,
        min_y,
        height,
        heights: height_sampler.map(|heights| heights as &mut dyn HeightSampler),
        biomes,
    };
    let start = structure.find_generation_point(&mut ctx)?;
    let biome = ctx.noise_biome(
        biome_coords::from_block(start.0.x),
        biome_coords::from_block(start.0.y),
        biome_coords::from_block(start.0.z),
    );
    if !is_valid_biome(biome) {
        return None;
    }
    let mut collector = StructurePiecesCollector::new();
    structure.add_pieces(&mut ctx, start, &mut collector);
    (!collector.is_empty()).then(|| StructurePosition {
        start_pos: start,
        collector: Arc::new(Mutex::new(collector)),
    })
}
