use rustc_hash::FxHashSet;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::sync::{LazyLock, OnceLock};

use pumpkin_data::chunk::{Biome, BiomeTree, NETHER_BIOME_SOURCE, OVERWORLD_BIOME_SOURCE};

use crate::generation::noise::router::multi_noise_sampler::MultiNoiseSampler;
pub mod datapack;
pub mod end;
pub mod multi_noise;
pub mod position_finder;

pub use position_finder::{
    Climate, ClimateSampler, DistanceMetric, FittestPositionFinder, FittestPositionFinderResult,
    ParameterList, RTree, RTreeLeaf, RTreeNode, RTreeSubTree, SpawnFinder, SpawnFinderResult,
};
pub use pumpkin_data::chunk::{
    Parameter, ParameterPoint, ParameterRange, TargetPoint, quantize_coord, unquantize_coord,
};

#[repr(C, align(64))]
#[derive(Clone, Copy, Default)]
struct FlatNode {
    min: [i32; 7],
    max: [i32; 7],
    child_or_leaf: i32,
    len: u32,
}

impl FlatNode {
    #[inline]
    fn squared_distance(&self, p: &[i32; 7]) -> i64 {
        let d0 = (p[0] - self.max[0]).max(self.min[0] - p[0]).max(0) as i64;
        let d1 = (p[1] - self.max[1]).max(self.min[1] - p[1]).max(0) as i64;
        let d2 = (p[2] - self.max[2]).max(self.min[2] - p[2]).max(0) as i64;
        let d3 = (p[3] - self.max[3]).max(self.min[3] - p[3]).max(0) as i64;
        let d4 = (p[4] - self.max[4]).max(self.min[4] - p[4]).max(0) as i64;
        let d5 = (p[5] - self.max[5]).max(self.min[5] - p[5]).max(0) as i64;
        let d6 = (p[6] - self.max[6]).max(self.min[6] - p[6]).max(0) as i64;
        d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3 + d4 * d4 + d5 * d5 + d6 * d6
    }
}

struct FlatBiomeTree {
    nodes: Box<[FlatNode]>,
    leaves: Box<[&'static Biome]>,
    root_len: usize,
}

#[derive(Clone, Copy)]
struct CachedFlatLeaf {
    kind: FlatTreeKind,
    node_idx: usize,
    point: [i32; 7],
    biome: &'static Biome,
}

impl FlatBiomeTree {
    fn from_tree(root: &'static BiomeTree) -> Self {
        fn build_children(
            children: &'static [BiomeTree],
            nodes: &mut Vec<FlatNode>,
            leaves: &mut Vec<&'static Biome>,
        ) -> usize {
            let start = nodes.len();
            nodes.resize(start + children.len(), FlatNode::default());
            for (i, child) in children.iter().enumerate() {
                let params = match child {
                    BiomeTree::Leaf { parameters, .. } | BiomeTree::Branch { parameters, .. } => {
                        parameters
                    }
                };
                let min = std::array::from_fn(|d| params[d].min as i32);
                let max = std::array::from_fn(|d| params[d].max as i32);
                match child {
                    BiomeTree::Leaf { biome, .. } => {
                        let leaf_idx = leaves.len();
                        leaves.push(biome);
                        nodes[start + i] = FlatNode {
                            min,
                            max,
                            child_or_leaf: !(leaf_idx as i32),
                            len: 0,
                        };
                    }
                    BiomeTree::Branch {
                        nodes: grand_children,
                        ..
                    } => {
                        let child_start = build_children(grand_children, nodes, leaves);
                        nodes[start + i] = FlatNode {
                            min,
                            max,
                            child_or_leaf: child_start as i32,
                            len: grand_children.len() as u32,
                        };
                    }
                }
            }
            start
        }

        let BiomeTree::Branch {
            nodes: root_children,
            ..
        } = root
        else {
            unreachable!()
        };
        let mut nodes = Vec::with_capacity(8192);
        let mut leaves = Vec::with_capacity(8192);
        build_children(root_children, &mut nodes, &mut leaves);
        Self {
            nodes: nodes.into_boxed_slice(),
            leaves: leaves.into_boxed_slice(),
            root_len: root_children.len(),
        }
    }

    #[inline]
    fn get(
        &self,
        kind: FlatTreeKind,
        p64: &[i64; 7],
        cache: &mut Option<CachedFlatLeaf>,
    ) -> &'static Biome {
        let p: [i32; 7] = [
            p64[0].clamp(-1_000_000_000, 1_000_000_000) as i32,
            p64[1].clamp(-1_000_000_000, 1_000_000_000) as i32,
            p64[2].clamp(-1_000_000_000, 1_000_000_000) as i32,
            p64[3].clamp(-1_000_000_000, 1_000_000_000) as i32,
            p64[4].clamp(-1_000_000_000, 1_000_000_000) as i32,
            p64[5].clamp(-1_000_000_000, 1_000_000_000) as i32,
            p64[6].clamp(-1_000_000_000, 1_000_000_000) as i32,
        ];
        if let Some(prev) = cache
            && prev.kind == kind
            && p == prev.point
        {
            return prev.biome;
        }
        let (mut best_dist, mut best_idx) = if let Some(prev) = *cache
            && prev.kind == kind
        {
            let node = &self.nodes[prev.node_idx];
            let dist = node.squared_distance(&p);
            if dist == 0 {
                if let Some(c) = cache {
                    c.point = p;
                }
                return prev.biome;
            }
            (dist, prev.node_idx)
        } else {
            (i64::MAX, 0)
        };
        self.search(0, self.root_len, &p, &mut best_dist, &mut best_idx);
        let leaf_idx = (!self.nodes[best_idx].child_or_leaf) as usize;
        let biome = self.leaves[leaf_idx];
        *cache = Some(CachedFlatLeaf {
            kind,
            node_idx: best_idx,
            point: p,
            biome,
        });
        biome
    }

    #[inline]
    fn search(
        &self,
        start: usize,
        len: usize,
        p: &[i32; 7],
        best_dist: &mut i64,
        best_node: &mut usize,
    ) {
        let children = &self.nodes[start..start + len];
        for (i, node) in children.iter().enumerate() {
            let dist = node.squared_distance(p);
            if dist >= *best_dist {
                continue;
            }
            if node.child_or_leaf < 0 {
                *best_dist = dist;
                *best_node = start + i;
                if dist == 0 {
                    return;
                }
            } else {
                self.search(
                    node.child_or_leaf as usize,
                    node.len as usize,
                    p,
                    best_dist,
                    best_node,
                );
                if *best_dist == 0 {
                    return;
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FlatTreeKind {
    Overworld,
    Nether,
    DatapackOverworld,
    DatapackNether,
}

static OVERWORLD_FLAT_TREE: LazyLock<FlatBiomeTree> =
    LazyLock::new(|| FlatBiomeTree::from_tree(&OVERWORLD_BIOME_SOURCE));
static NETHER_FLAT_TREE: LazyLock<FlatBiomeTree> =
    LazyLock::new(|| FlatBiomeTree::from_tree(&NETHER_BIOME_SOURCE));

/// A dimension whose biome source a datapack can give (`dimension/<name>.json`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatapackDimension {
    Overworld,
    Nether,
}

/// A `multi_noise` biome source built from a datapack's parameter list.
struct DatapackSource {
    tree: &'static BiomeTree,
    flat: FlatBiomeTree,
    /// The biomes in the order the list first names them: vanilla's `possibleBiomes`, the
    /// order the feature sorter visits.
    biomes: Vec<&'static Biome>,
}

static DATAPACK_OVERWORLD: OnceLock<DatapackSource> = OnceLock::new();
static DATAPACK_NETHER: OnceLock<DatapackSource> = OnceLock::new();

const fn datapack_slot(dimension: DatapackDimension) -> &'static OnceLock<DatapackSource> {
    match dimension {
        DatapackDimension::Overworld => &DATAPACK_OVERWORLD,
        DatapackDimension::Nether => &DATAPACK_NETHER,
    }
}

/// Installs a datapack's biome source for `dimension`: vanilla's `Climate.RTree` built from
/// the parameter list, in list order. Returns false if one was already installed.
///
/// Once per process, before the worlds are created: as in vanilla, a datapack's dimension
/// replaces the generator the world was saved with.
///
/// # Panics
///
/// If `list` is empty.
#[must_use]
pub fn install_datapack_source(
    dimension: DatapackDimension,
    list: Vec<(ParameterPoint, &'static Biome)>,
) -> bool {
    let mut biomes: Vec<&'static Biome> = Vec::new();
    for (_, biome) in &list {
        if !biomes.contains(biome) {
            biomes.push(biome);
        }
    }
    let tree = RTree::create(list).leak_biome_tree();
    datapack_slot(dimension)
        .set(DatapackSource {
            tree,
            flat: FlatBiomeTree::from_tree(tree),
            biomes,
        })
        .is_ok()
}

/// The biomes of a datapack's source for `dimension`, in the order its list first names
/// them, if one is installed.
#[must_use]
pub fn datapack_biomes(dimension: DatapackDimension) -> Option<&'static [&'static Biome]> {
    datapack_slot(dimension)
        .get()
        .map(|source| source.biomes.as_slice())
}

/// Replaces the biome the overworld's multi-noise source picked, position by position.
///
/// A biome placement library's hook (Biolith hooks vanilla's `MultiNoiseBiomeSource` the same
/// way, after its parameter search). Registered once, early; started once the packs' biomes
/// are installed and before the worlds are created.
pub trait BiomeReplacer: Send + Sync {
    /// The world's seed and folder, once, before any lookup. Returns the biomes it may place:
    /// they join the overworld's possible biomes (and so the feature order) after the
    /// source's own, in this order, as a placement library writes them into vanilla's
    /// parameter list. An empty list leaves the source as it is (the hook stays inactive).
    fn start(&self, seed: u64, level_folder: &std::path::Path) -> Vec<&'static Biome>;

    /// The biome at quart position (`x`, `y`, `z`) where the source picked `biome`.
    fn replace(&self, x: i32, y: i32, z: i32, biome: &'static Biome) -> &'static Biome;
}

static OVERWORLD_REPLACER: OnceLock<Box<dyn BiomeReplacer>> = OnceLock::new();
/// What the started replacer may place; empty when none is registered or it declined.
static OVERWORLD_REPLACEMENT_BIOMES: OnceLock<Vec<&'static Biome>> = OnceLock::new();

/// Registers the overworld's biome replacer. Returns false if one already is.
pub fn register_overworld_replacer(replacer: Box<dyn BiomeReplacer>) -> bool {
    OVERWORLD_REPLACER.set(replacer).is_ok()
}

/// Starts the registered replacer, if any: once, after the packs' biomes are installed and
/// before the worlds (and the feature order) are built.
pub fn start_overworld_replacer(seed: u64, level_folder: &std::path::Path) {
    OVERWORLD_REPLACEMENT_BIOMES.get_or_init(|| {
        OVERWORLD_REPLACER
            .get()
            .map(|replacer| replacer.start(seed, level_folder))
            .unwrap_or_default()
    });
}

/// The biomes the overworld's replacer may place (empty without an active one): possible
/// biomes of the overworld beyond its source's.
#[must_use]
pub fn overworld_replacement_biomes() -> &'static [&'static Biome] {
    OVERWORLD_REPLACEMENT_BIOMES.get().map_or(&[], Vec::as_slice)
}

fn replace_overworld_biome(x: i32, y: i32, z: i32, biome: &'static Biome) -> &'static Biome {
    if overworld_replacement_biomes().is_empty() {
        return biome;
    }
    OVERWORLD_REPLACER
        .get()
        .map_or(biome, |replacer| replacer.replace(x, y, z, biome))
}

thread_local! {
    /// A shortcut; check if last used biome is what we should use.
    ///
    /// Keyed by the tree the leaf came from. `BiomeTree::get` seeds its search distance from
    /// this leaf and only accepts strictly closer nodes, so a leaf cached from one dimension's
    /// tree would otherwise beat every node of another's and be returned unchanged. Vanilla
    /// stores `lastResult` on the `RTree` itself rather than sharing one across all of them.
    static LAST_FLAT_LEAF: RefCell<Option<CachedFlatLeaf>> =
        const { RefCell::new(None) };
}

pub trait BiomeSupplier {
    fn biome(&self, x: i32, y: i32, z: i32, noise: &mut MultiNoiseSampler<'_>) -> &'static Biome;
}

#[derive(Clone, Copy)]
pub struct MultiNoiseBiomeSupplier {
    source: &'static BiomeTree,
    flat_kind: FlatTreeKind,
}

impl MultiNoiseBiomeSupplier {
    pub const OVERWORLD: Self = Self::new(&OVERWORLD_BIOME_SOURCE, FlatTreeKind::Overworld);
    pub const NETHER: Self = Self::new(&NETHER_BIOME_SOURCE, FlatTreeKind::Nether);

    #[must_use]
    pub fn from_preset(preset: &str) -> Option<Self> {
        let preset = preset.strip_prefix("minecraft:").unwrap_or(preset);
        match preset {
            "overworld" | "large_biomes" | "amplified" => Some(Self::OVERWORLD),
            "nether" => Some(Self::NETHER),
            _ => None,
        }
    }

    const fn new(source: &'static BiomeTree, flat_kind: FlatTreeKind) -> Self {
        Self { source, flat_kind }
    }

    /// The overworld's source: a datapack's if one is installed, else vanilla's.
    #[must_use]
    pub fn overworld() -> Self {
        Self::from_datapack(DatapackDimension::Overworld).unwrap_or(Self::OVERWORLD)
    }

    /// The nether's source: a datapack's if one is installed, else vanilla's.
    #[must_use]
    pub fn nether() -> Self {
        Self::from_datapack(DatapackDimension::Nether).unwrap_or(Self::NETHER)
    }

    /// The datapack's source for `dimension`, if one is installed.
    #[must_use]
    pub fn from_datapack(dimension: DatapackDimension) -> Option<Self> {
        let source = datapack_slot(dimension).get()?;
        let kind = match dimension {
            DatapackDimension::Overworld => FlatTreeKind::DatapackOverworld,
            DatapackDimension::Nether => FlatTreeKind::DatapackNether,
        };
        Some(Self::new(source.tree, kind))
    }
}

impl BiomeSupplier for MultiNoiseBiomeSupplier {
    fn biome(&self, x: i32, y: i32, z: i32, noise: &mut MultiNoiseSampler<'_>) -> &'static Biome {
        let point = noise.sample(x, y, z);
        let point_list = point.convert_to_list();
        let flat_tree = match self.flat_kind {
            FlatTreeKind::Overworld => &*OVERWORLD_FLAT_TREE,
            FlatTreeKind::Nether => &*NETHER_FLAT_TREE,
            FlatTreeKind::DatapackOverworld => DATAPACK_OVERWORLD
                .get()
                .map_or_else(|| &*OVERWORLD_FLAT_TREE, |source| &source.flat),
            FlatTreeKind::DatapackNether => DATAPACK_NETHER
                .get()
                .map_or_else(|| &*NETHER_FLAT_TREE, |source| &source.flat),
        };
        let biome = LAST_FLAT_LEAF
            .with_borrow_mut(|last_result| flat_tree.get(self.flat_kind, &point_list, last_result));
        match self.flat_kind {
            FlatTreeKind::Overworld | FlatTreeKind::DatapackOverworld => {
                replace_overworld_biome(x, y, z, biome)
            }
            FlatTreeKind::Nether | FlatTreeKind::DatapackNether => biome,
        }
    }
}

#[derive(Clone, Copy)]
pub struct FixedBiomeSupplier {
    pub biome: &'static Biome,
}

impl FixedBiomeSupplier {
    #[must_use]
    pub const fn new(biome: &'static Biome) -> Self {
        Self { biome }
    }
}

impl BiomeSupplier for FixedBiomeSupplier {
    fn biome(
        &self,
        _x: i32,
        _y: i32,
        _z: i32,
        _noise: &mut MultiNoiseSampler<'_>,
    ) -> &'static Biome {
        self.biome
    }
}

#[derive(Clone, Copy)]
pub enum ActiveBiomeSupplier {
    MultiNoise(MultiNoiseBiomeSupplier),
    End(end::TheEndBiomeSupplier),
    Fixed(FixedBiomeSupplier),
}

impl BiomeSupplier for ActiveBiomeSupplier {
    fn biome(&self, x: i32, y: i32, z: i32, noise: &mut MultiNoiseSampler<'_>) -> &'static Biome {
        match self {
            Self::MultiNoise(supplier) => supplier.biome(x, y, z, noise),
            Self::End(supplier) => supplier.biome(x, y, z, noise),
            Self::Fixed(supplier) => supplier.biome(x, y, z, noise),
        }
    }
}

impl ActiveBiomeSupplier {
    #[must_use]
    pub fn possible_biomes(&self) -> FxHashSet<u16> {
        fn collect(tree: &BiomeTree, biomes: &mut FxHashSet<u16>) {
            match tree {
                BiomeTree::Leaf { biome, .. } => {
                    biomes.insert(u16::from(biome.id));
                }
                BiomeTree::Branch { nodes, .. } => {
                    for node in *nodes {
                        collect(node, biomes);
                    }
                }
            }
        }

        match self {
            Self::MultiNoise(supplier) => {
                let mut biomes = FxHashSet::default();
                collect(supplier.source, &mut biomes);
                if matches!(
                    supplier.flat_kind,
                    FlatTreeKind::Overworld | FlatTreeKind::DatapackOverworld
                ) {
                    biomes.extend(
                        overworld_replacement_biomes()
                            .iter()
                            .map(|biome| u16::from(biome.id)),
                    );
                }
                biomes
            }
            Self::End(_) => end::TheEndBiomeSupplier::POSSIBLE_BIOMES
                .iter()
                .map(|biome| u16::from(biome.id))
                .collect(),
            Self::Fixed(supplier) => std::iter::once(u16::from(supplier.biome.id)).collect(),
        }
    }

    #[must_use]
    pub fn from_biome_source(
        source: Option<&crate::world_info::BiomeSource>,
        dimension: &pumpkin_data::dimension::Dimension,
    ) -> Self {
        // A datapack's dimension replaces the saved source, as vanilla does (its
        // `world_gen_settings.dat` keeps the preset while the pack's list generates).
        let datapack = if dimension == &pumpkin_data::dimension::Dimension::OVERWORLD {
            MultiNoiseBiomeSupplier::from_datapack(DatapackDimension::Overworld)
        } else if dimension == &pumpkin_data::dimension::Dimension::THE_NETHER {
            MultiNoiseBiomeSupplier::from_datapack(DatapackDimension::Nether)
        } else {
            None
        };
        if let Some(supplier) = datapack {
            return Self::MultiNoise(supplier);
        }
        match source {
            Some(crate::world_info::BiomeSource::Fixed { biome, .. }) => {
                let clean = biome.strip_prefix("minecraft:").unwrap_or(biome);
                let resolved = pumpkin_data::biome::Biome::from_name(clean)
                    .unwrap_or(&pumpkin_data::biome::Biome::PLAINS);
                Self::Fixed(FixedBiomeSupplier::new(resolved))
            }
            Some(crate::world_info::BiomeSource::WithPreset { preset, .. }) => {
                let clean = preset.strip_prefix("minecraft:").unwrap_or(preset);
                if clean == "nether" {
                    Self::MultiNoise(MultiNoiseBiomeSupplier::NETHER)
                } else {
                    Self::MultiNoise(MultiNoiseBiomeSupplier::OVERWORLD)
                }
            }
            Some(crate::world_info::BiomeSource::Simple { biome_type }) => {
                let clean = biome_type.strip_prefix("minecraft:").unwrap_or(biome_type);
                if clean == "the_end" {
                    Self::End(end::TheEndBiomeSupplier)
                } else if clean == "the_nether" {
                    Self::MultiNoise(MultiNoiseBiomeSupplier::NETHER)
                } else {
                    Self::from_dimension(dimension)
                }
            }
            None => Self::from_dimension(dimension),
        }
    }

    #[must_use]
    pub fn from_dimension(dimension: &pumpkin_data::dimension::Dimension) -> Self {
        if dimension == &pumpkin_data::dimension::Dimension::THE_END {
            Self::End(end::TheEndBiomeSupplier)
        } else if dimension == &pumpkin_data::dimension::Dimension::THE_NETHER {
            Self::MultiNoise(MultiNoiseBiomeSupplier::nether())
        } else {
            Self::MultiNoise(MultiNoiseBiomeSupplier::overworld())
        }
    }
}

#[must_use]
pub fn hash_seed(seed: u64) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(seed.to_le_bytes());
    let result = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&result[..8]);
    i64::from_le_bytes(bytes)
}

#[cfg(test)]
mod test {
    use pumpkin_data::{chunk::Biome, dimension::Dimension};
    use serde::Deserialize;

    use crate::{
        ProtoChunk, chunk::palette::BIOME_NETWORK_MAX_BITS,
        generation::noise::router::multi_noise_sampler::MultiNoiseSampler,
    };

    use super::{BiomeSupplier, MultiNoiseBiomeSupplier, hash_seed};

    #[test]
    fn biome_desert() {
        use crate::generation::generator::{GeneratorInit, VanillaGenerator};
        use pumpkin_util::world_seed::Seed;
        let seed = 13579;
        let generator = VanillaGenerator::new(Seed(seed as u64), Dimension::OVERWORLD);
        let mut sampler = MultiNoiseSampler::generate(&generator.base_router.multi_noise);
        let biome = MultiNoiseBiomeSupplier::OVERWORLD.biome(-24, 1, 8, &mut sampler);
        assert_eq!(biome, &Biome::DESERT);
    }

    #[test]
    fn wide_area_surface() {
        use crate::generation::generator::{GeneratorInit, VanillaGenerator, WorldGenerator};
        use crate::generation::noise::router::multi_noise_sampler::MultiNoiseSampler;
        use pumpkin_util::world_seed::Seed;
        #[derive(Deserialize)]
        struct BiomeData {
            x: i32,
            z: i32,
            data: Vec<(i32, i32, i32, u8)>,
        }

        let expected_data: Vec<BiomeData> =
            include_json!("../../../../assets/tests/biome_no_blend_no_beard_0.json");

        let seed = 0;
        let world_gen = WorldGenerator::Noise(Box::new(VanillaGenerator::new(
            Seed(seed as u64),
            Dimension::OVERWORLD,
        )));
        let WorldGenerator::Noise(generator) = &world_gen else {
            unreachable!()
        };

        for data in expected_data {
            let chunk_x = data.x;
            let chunk_z = data.z;

            let mut chunk = ProtoChunk::new(chunk_x, chunk_z, &world_gen);

            let mut multi_noise_sampler =
                MultiNoiseSampler::generate(&generator.base_router.multi_noise);

            chunk.populate_biomes(generator, &mut multi_noise_sampler);

            for (biome_x, biome_y, biome_z, biome_id) in data.data {
                let calculated_biome = chunk.get_biome(biome_x, biome_y, biome_z);

                assert_eq!(
                    biome_id,
                    calculated_biome.id,
                    "Expected {:?} was {:?} at {},{},{} ({},{})",
                    Biome::from_id(biome_id),
                    calculated_biome,
                    biome_x,
                    biome_y,
                    biome_z,
                    data.x,
                    data.z
                );
            }
        }
    }

    #[test]
    fn hash_seed_test() {
        let hashed_seed = hash_seed(0);
        assert_eq!(8794265229978523055, hashed_seed);

        let hashed_seed = hash_seed((-777i64) as u64);
        assert_eq!(-1087248400229165450, hashed_seed);
    }

    #[test]
    fn proper_network_bits_per_entry() {
        let id_to_test = 1 << BIOME_NETWORK_MAX_BITS;
        assert!(
            Biome::from_id(id_to_test).is_none(),
            "We need to update our constants!"
        );
    }

    /// A leaf cached from one dimension's biome tree must never seed another dimension's
    /// search, or the Nether fills with overworld biomes (and their mobs and features).
    #[test]
    fn nether_chunks_only_contain_nether_biomes() {
        use crate::generation::generator::{GeneratorInit, VanillaGenerator, WorldGenerator};
        use crate::generation::noise::router::multi_noise_sampler::MultiNoiseSampler;
        use pumpkin_util::world_seed::Seed;

        let fill = |dimension: Dimension, chunk_x: i32, chunk_z: i32| -> Vec<u8> {
            let world_gen =
                WorldGenerator::Noise(Box::new(VanillaGenerator::new(Seed(0), dimension)));
            let WorldGenerator::Noise(generator) = &world_gen else {
                unreachable!()
            };
            let mut chunk = ProtoChunk::new(chunk_x, chunk_z, &world_gen);
            let mut sampler = MultiNoiseSampler::generate(&generator.base_router.multi_noise);
            chunk.populate_biomes(generator, &mut sampler);
            let mut biomes = Vec::new();
            for y in 0..8 {
                for x in 0..4 {
                    for z in 0..4 {
                        biomes.push(chunk.get_biome(x, y, z).id);
                    }
                }
            }
            biomes
        };

        // Warm the thread-local leaf cache with overworld leaves, then generate a Nether
        // chunk on the same thread.
        let _ = fill(Dimension::OVERWORLD, 0, 0);
        let nether = fill(Dimension::THE_NETHER, 0, 0);

        let nether_ids: Vec<u8> = [
            "nether_wastes",
            "soul_sand_valley",
            "crimson_forest",
            "warped_forest",
            "basalt_deltas",
        ]
        .iter()
        .map(|name| Biome::from_name(name).unwrap().id)
        .collect();

        let leaked: Vec<&str> = nether
            .iter()
            .filter(|id| !nether_ids.contains(id))
            .filter_map(|id| Biome::from_id(*id).map(|biome| biome.registry_id))
            .collect();
        assert!(
            leaked.is_empty(),
            "non-Nether biomes generated in a Nether chunk: {leaked:?}"
        );
    }
}
