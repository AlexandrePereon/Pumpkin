//! Surface builders of a server that ports content (a mod's own code run per column).
//!
//! Vanilla's surface pass (`MaterialSystem.buildSurface`) runs code of its own around the
//! material rules for two biomes: eroded badlands pillars before the rules, icebergs after.
//! A biome placement library hooks those two spots for any biome (Biolith's
//! `BiolithSurfaceBuilder`: `generate` where badlands are checked, `generateLate` where
//! frozen oceans are), so a ported mod registers its builders here, before any chunk is
//! generated. Each column calls every builder whose [`SurfaceBuilder::applies_to`] accepts
//! the column's biome, in registration order.

use std::sync::{LazyLock, RwLock};

use pumpkin_data::BlockState;
use pumpkin_data::chunk::Biome;
use pumpkin_util::random::xoroshiro128::Xoroshiro;

use crate::ProtoChunk;
use crate::generation::noise::perlin::DoublePerlinNoiseSampler;

/// One column of the chunk being surfaced, as vanilla's `BlockColumn` gives it.
pub struct SurfaceColumn<'a> {
    pub(crate) chunk: &'a mut ProtoChunk,
    pub(crate) x: i32,
    pub(crate) z: i32,
    pub(crate) min_y: i32,
    pub(crate) max_y: i32,
}

impl SurfaceColumn<'_> {
    /// The block at height `y` of this column (`BlockColumn.getBlock`).
    #[must_use]
    pub fn block(&self, y: i32) -> &'static BlockState {
        self.chunk
            .get_block_state(&pumpkin_util::math::vector3::Vector3::new(self.x, y, self.z))
            .to_state()
    }

    /// Sets the block at height `y`, ignored outside the generation height
    /// (`BlockColumn.setBlock`).
    pub fn set_block(&mut self, y: i32, state: &BlockState) {
        if y < self.min_y || y > self.max_y {
            return;
        }
        self.chunk.set_block_state(self.x, y, self.z, state);
    }

    /// The highest block that raises `OCEAN_FLOOR_WG` in this column, as vanilla's
    /// `ChunkAccess.getHeight` gives it (the block's height, not the one above).
    #[must_use]
    pub const fn ocean_floor_wg(&self) -> i32 {
        // During the surface pass the live map is the one kept up to date, as vanilla's
        // worldgen heightmaps are while the chunk is at its noise status.
        self.chunk.ocean_floor_height_exclusive(self.x, self.z) - 1
    }

    /// The highest non-air block in this column (`WORLD_SURFACE_WG`), as
    /// `ChunkAccess.getHeight` gives it.
    #[must_use]
    pub const fn world_surface_wg(&self) -> i32 {
        self.chunk.top_block_height_exclusive(self.x, self.z) - 1
    }
}

/// What a builder gets for one column.
pub struct SurfaceContext<'a, 'b> {
    pub column: &'b mut SurfaceColumn<'a>,
    /// The column's biome (vanilla's, at the column's start height).
    pub biome: &'static Biome,
    /// Block coordinates of the column.
    pub x: i32,
    pub z: i32,
    /// One above the column's highest block before the surface pass touched it
    /// (vanilla's `startHeight`).
    pub start_height: i32,
    pub sea_level: i32,
    /// `noiseRandom.at(x, start_height, z)`: one per column and hook, shared by the
    /// builders called there in turn.
    pub random: &'b mut Xoroshiro,
    /// The dimension's `minecraft:surface` noise (`RandomState.getOrCreateNoise`).
    pub surface_noise: &'b DoublePerlinNoiseSampler,
    /// The biome source's biome for the quart holding a block position, with no fuzzy
    /// zoom (`ServerLevel.getUncachedNoiseBiome`, what Biolith reads through the
    /// surface pass's `WorldGenRegion` biome manager): any position, not only this
    /// chunk's.
    pub biome_at: &'b mut dyn FnMut(i32, i32, i32) -> &'static Biome,
}

/// A surface builder (Biolith's `BiolithSurfaceBuilder`).
pub trait SurfaceBuilder: Send + Sync {
    /// Whether this builder runs in columns of `biome`.
    fn applies_to(&self, biome: &'static Biome) -> bool;

    /// Before the material rules (where vanilla raises eroded badlands pillars).
    fn build_early(&self, _ctx: &mut SurfaceContext<'_, '_>) {}

    /// After the material rules (where vanilla raises icebergs). `min_surface_level` is
    /// the material rule context's estimate (`getMinSurfaceLevel`).
    fn build_late(&self, _ctx: &mut SurfaceContext<'_, '_>, _min_surface_level: i32) {}
}

static BUILDERS: LazyLock<RwLock<Vec<Box<dyn SurfaceBuilder>>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Registers a surface builder, before any chunk is generated.
pub fn register(builder: Box<dyn SurfaceBuilder>) {
    if let Ok(mut builders) = BUILDERS.write() {
        builders.push(builder);
    }
}

/// Whether any builder is registered (the surface pass then looks up every column's biome).
pub(in crate::generation) fn any() -> bool {
    BUILDERS.read().is_ok_and(|builders| !builders.is_empty())
}

/// Whether a registered builder applies to `biome`.
pub(in crate::generation) fn applies(biome: &'static Biome) -> bool {
    BUILDERS
        .read()
        .is_ok_and(|builders| builders.iter().any(|builder| builder.applies_to(biome)))
}

/// Runs the builders that apply to the context's biome, early or late.
pub(in crate::generation) fn run(ctx: &mut SurfaceContext<'_, '_>, late: Option<i32>) {
    let Ok(builders) = BUILDERS.read() else {
        return;
    };
    let biome = ctx.biome;
    for builder in builders.iter().filter(|builder| builder.applies_to(biome)) {
        match late {
            None => builder.build_early(ctx),
            Some(min_surface_level) => builder.build_late(ctx, min_surface_level),
        }
    }
}
