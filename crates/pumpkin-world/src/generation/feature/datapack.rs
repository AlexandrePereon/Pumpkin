//! Placed and configured features built at runtime from datapack JSON.
//!
//! The vanilla features are compiled (`placed_features_generated.rs`,
//! `configured_features_generated.rs`) and keyed by the generated enums. A datapack can override
//! any `worldgen/placed_feature` or `worldgen/feature` file, or add its own; such a pack changes
//! the decoration step only if the server builds those files at startup. This module does that,
//! into the same [`PlacedFeature`] and [`ConfiguredFeature`] values the codegen emits, and
//! installs them beside the compiled maps: [`placed_feature`](super::placed_features::placed_feature)
//! and [`configured_feature`](super::configured_features::configured_feature) ask it first.
//!
//! The reading follows `tools/pumpkin-codegen/src/{placed_feature,configured_feature}.rs`.
//! Only the files a datapack gives are built, and only those that override a vanilla id are
//! installed: biomes list their features compiled, so a feature of a datapack's own namespace
//! is reached through a vanilla one that names it, where it is built inline. A file using a
//! feature type this reader does not handle keeps the compiled feature.

use std::collections::HashMap;
use std::num::NonZero;
use std::sync::OnceLock;

use pumpkin_data::configured_feature::ConfiguredFeature as ConfiguredFeatureKey;
use pumpkin_data::placed_feature::PlacedFeature as PlacedFeatureKey;
use pumpkin_data::tag::RegistryKey;
use pumpkin_data::{Block, BlockDirection, BlockState};
use pumpkin_util::DoublePerlinNoiseParametersCodec;
use pumpkin_util::HeightMap;
use pumpkin_util::math::int_provider::{
    BiasedToBottomIntProvider, ClampedIntProvider, ClampedNormalIntProvider, ConstantIntProvider,
    IntProvider, NormalIntProvider, TrapezoidIntProvider, UniformIntProvider, WeightedEntry,
    WeightedListIntProvider,
};
use pumpkin_util::math::pool::Weighted;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::math::vertical_surface_type::VerticalSurfaceType;
use pumpkin_util::y_offset::{AboveBottom, Absolute, BelowTop, RelativeToSeaLevel, YOffset};
use serde_json::{Map, Value};

use super::configured_features::ConfiguredFeature;
use super::features::block_column::{BlockColumnFeature, Layer};
use super::features::block_pile::BlockPileFeature;
use super::features::disk::DiskFeature;
use super::features::forest_rock::ForestRockFeature;
use super::features::netherrack_replace_blobs::ReplaceBlobsFeature;
use super::features::ore::{OreFeature, OreTarget};
use super::features::random_boolean_selector::RandomBooleanFeature;
use super::features::random_selector::{RandomFeature, RandomFeatureEntry};
use super::features::replace_single_block::ReplaceSingleBlockFeature;
use super::features::scattered_ore::ScatteredOreFeature;
use super::features::simple_block::SimpleBlockFeature;
use super::features::simple_random_selector::SimpleRandomFeature;
use super::features::spring_feature::{BlockWrapper, SpringFeatureFeature};
use super::features::weighted_random_selector::{
    WeightedRandomFeature, WeightedRandomFeatureEntry,
};
use super::features::{
    bamboo::BambooFeature, blue_ice::BlueIceFeature, bonus_chest::BonusChestFeature,
    chorus_plant::ChorusPlantFeature, end_gateway::EndGatewayFeature,
    end_island::EndIslandFeature, end_platform::EndPlatformFeature,
    end_podium::EndPodiumFeature, freeze_top_layer::FreezeTopLayerFeature,
    huge_fungus::HugeFungusFeature, iceberg::IcebergFeature, lake::LakeFeature,
    monster_room::DungeonFeature, multiface_growth::MultifaceGrowthFeature,
    overlay::OverlayFeature, projected_random_patchy_square::ProjectedRandomPatchySquareFeature,
    random_neighbor_spread::RandomNeighborSpreadFeature, root_system::RootSystemFeature,
    sea_pickle::SeaPickleFeature, seagrass::SeagrassFeature,
    single_block_pillar::SingleBlockPillarFeature,
    stepped_column_cluster::SteppedColumnClusterFeature, underwater_magma::UnderwaterMagmaFeature,
    vegetation_patch::VegetationPatchFeature, vines::VinesFeature,
    void_start_platform::VoidStartPlatformFeature,
    waterlogged_vegetation_patch::WaterloggedVegetationPatchFeature,
};
use super::placed_features::{
    BiomePlacementModifier, BlockFilterPlacementModifier, CountOnEveryLayerPlacementModifier,
    CountPlacementModifier, EnvironmentScanPlacementModifier, Feature,
    HeightRangePlacementModifier, HeightmapPlacementModifier, NoiseBasedCountPlacementModifier,
    NoiseThresholdCountPlacementModifier, OffsetPlacementModifier, PlacedFeature,
    PlacedFeatureWrapper, PlacementModifier, RarityFilterPlacementModifier,
    SquarePlacementModifier, SurfaceThresholdFilterPlacementModifier,
    SurfaceWaterDepthFilterPlacementModifier,
};
use crate::block::BlockStateCodec;
use crate::generation::block_predicate::{
    AllOfBlockPredicate, AnyOfBlockPredicate, BlockPredicate, HasSturdyFacePredicate,
    HeightRangeBlockPredicate, InsideWorldBoundsBlockPredicate, MatchingBlockTagPredicate,
    MatchingBlocksBlockPredicate, MatchingBlocksWrapper, MatchingFluidsBlockPredicate,
    NotBlockPredicate, OffsetBlocksBlockPredicate, ReplaceableBlockPredicate, SolidBlockPredicate,
    WouldSurviveBlockPredicate,
};
use crate::generation::block_state_provider::{
    BlockStateProvider, BlockStateRule, DualNoiseBlockStateProvider, NoiseBlockStateProvider,
    NoiseBlockStateProviderBase, NoiseThresholdBlockStateProvider, PillarBlockStateProvider,
    RandomizedIntBlockStateProvider, RuleBasedBlockStateProvider, SimpleStateProvider,
    WeightedBlockStateProvider,
};
use crate::generation::height_provider::{
    HeightProvider, TrapezoidHeightProvider, UniformHeightProvider,
    VeryBiasedToBottomHeightProvider,
};
use crate::generation::noise::router::datapack::{
    Result, WorldgenJson, WorldgenRegistry, block_state, full_id,
};
use crate::generation::rule::RuleTest;
use crate::generation::rule::block_match::BlockMatchRuleTest;
use crate::generation::rule::block_state_match::BlockStateMatchRuleTest;
use crate::generation::rule::random_block_match::RandomBlockMatchRuleTest;
use crate::generation::rule::random_block_state_match::RandomBlockStateMatchRuleTest;
use crate::generation::rule::tag_match::TagMatchRuleTest;

mod tree;

/// Feature references nested deeper than this are taken for a cycle.
const MAX_DEPTH: usize = 64;

/// The vanilla features a datapack overrides, built from its files.
#[derive(Default)]
pub struct DatapackFeatures {
    placed: HashMap<PlacedFeatureKey, PlacedFeature>,
    configured: HashMap<ConfiguredFeatureKey, ConfiguredFeature>,
}

impl DatapackFeatures {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.placed.is_empty() && self.configured.is_empty()
    }

    /// The overridden placed features' names, sorted.
    #[must_use]
    pub fn placed_names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.placed.keys().map(PlacedFeatureKey::to_name).collect();
        names.sort_unstable();
        names
    }

    /// The overridden configured features' names, sorted.
    #[must_use]
    pub fn configured_names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self
            .configured
            .keys()
            .map(ConfiguredFeatureKey::to_name)
            .collect();
        names.sort_unstable();
        names
    }
}

/// The vanilla path of a `minecraft:` id, `None` for any other namespace.
fn vanilla_path(id: &str) -> Option<&str> {
    id.strip_prefix("minecraft:")
}

impl WorldgenJson {
    /// Builds every datapack `worldgen/feature` and `worldgen/placed_feature` file that
    /// overrides a vanilla one. Returns them, and one message per file that failed (that
    /// feature stays compiled).
    #[must_use]
    pub fn build_datapack_features(&self) -> (DatapackFeatures, Vec<String>) {
        let mut built = DatapackFeatures::default();
        let mut errors = Vec::new();
        let mut parser = FeatureParser {
            json: self,
            depth: 0,
        };
        for id in self.datapack_ids(WorldgenRegistry::ConfiguredFeature) {
            let Some(key) = vanilla_path(&id).and_then(ConfiguredFeatureKey::from_name) else {
                continue;
            };
            let Some((value, _)) = self.get(WorldgenRegistry::ConfiguredFeature, &id) else {
                continue;
            };
            match parser.configured_feature(value) {
                Ok(feature) => {
                    built.configured.insert(key, feature);
                }
                Err(error) => errors.push(format!("{id}: {error}")),
            }
        }
        for id in self.datapack_ids(WorldgenRegistry::PlacedFeature) {
            let Some(key) = vanilla_path(&id).and_then(PlacedFeatureKey::from_name) else {
                continue;
            };
            let Some((value, _)) = self.get(WorldgenRegistry::PlacedFeature, &id) else {
                continue;
            };
            match parser.placed_feature(value) {
                Ok(feature) => {
                    built.placed.insert(key, feature);
                }
                Err(error) => errors.push(format!("{id}: {error}")),
            }
        }
        (built, errors)
    }

    pub(crate) fn datapack_ids(&self, registry: WorldgenRegistry) -> Vec<String> {
        self.ids(registry)
            .filter(|id| self.get(registry, id).is_some_and(|(_, ours)| *ours))
            .map(str::to_string)
            .collect()
    }
}

static INSTALLED: OnceLock<DatapackFeatures> = OnceLock::new();

/// Installs the features built from datapacks; returns false if some were already.
///
/// Once per process, before the worlds are created: worldgen registries are fixed for a
/// server's lifetime, as in vanilla.
pub fn install(features: DatapackFeatures) -> bool {
    INSTALLED.set(features).is_ok()
}

/// The placed feature a datapack built for `key`, if any.
#[must_use]
pub fn installed_placed(key: PlacedFeatureKey) -> Option<&'static PlacedFeature> {
    INSTALLED.get()?.placed.get(&key)
}

/// The configured feature a datapack built for `key`, if any.
#[must_use]
pub fn installed_configured(key: ConfiguredFeatureKey) -> Option<&'static ConfiguredFeature> {
    INSTALLED.get()?.configured.get(&key)
}

// ------------------------------------------------------------------------- parsing

fn object(value: &Value) -> Result<&Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| format!("expected an object, got {value}"))
}

fn str_of<'v>(obj: &'v Map<String, Value>, key: &str) -> Result<&'v str> {
    obj.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string \"{key}\""))
}

fn i32_or(obj: &Map<String, Value>, key: &str, default: i32) -> Result<i32> {
    obj.get(key).map_or(Ok(default), |v| {
        v.as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| format!("\"{key}\" is not an int: {v}"))
    })
}

fn f64_or(obj: &Map<String, Value>, key: &str, default: f64) -> Result<f64> {
    obj.get(key).map_or(Ok(default), |v| {
        v.as_f64()
            .ok_or_else(|| format!("\"{key}\" is not a number: {v}"))
    })
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "vanilla reads these as floats"
)]
fn f32_or(obj: &Map<String, Value>, key: &str, default: f32) -> Result<f32> {
    f64_or(obj, key, f64::from(default)).map(|v| v as f32)
}

fn bool_or(obj: &Map<String, Value>, key: &str, default: bool) -> Result<bool> {
    obj.get(key).map_or(Ok(default), |v| {
        v.as_bool()
            .ok_or_else(|| format!("\"{key}\" is not a boolean: {v}"))
    })
}

fn array<'v>(obj: &'v Map<String, Value>, key: &str) -> Result<&'v [Value]> {
    obj.get(key).map_or(Ok(&[]), |v| {
        v.as_array()
            .map(Vec::as_slice)
            .ok_or_else(|| format!("\"{key}\" is not a list: {v}"))
    })
}

fn vec3(value: &Value) -> Result<Vector3<i32>> {
    let coords = value
        .as_array()
        .filter(|a| a.len() == 3)
        .ok_or_else(|| format!("expected [x, y, z], got {value}"))?;
    let c = |i: usize| {
        coords[i]
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| format!("bad coordinate in {value}"))
    };
    Ok(Vector3::new(c(0)?, c(1)?, c(2)?))
}

fn block(name: &str) -> Result<&'static Block> {
    Block::from_name(name).ok_or_else(|| format!("unknown block {name}"))
}

/// One block id or a list of them, as `HolderSet<Block>` writes them inline.
fn block_ids(value: &Value) -> Result<Vec<pumpkin_data::BlockId>> {
    match value {
        Value::String(name) => Ok(vec![block(name)?.id]),
        Value::Array(names) => names
            .iter()
            .map(|n| {
                n.as_str()
                    .ok_or_else(|| format!("bad block id {n}"))
                    .and_then(block)
                    .map(|b| b.id)
            })
            .collect(),
        other => Err(format!("bad block list {other}")),
    }
}

fn block_tag(name: &str) -> Result<pumpkin_data::tag::Tag> {
    pumpkin_data::tag::get_tag(RegistryKey::Block, &full_id(name))
        .copied()
        .ok_or_else(|| format!("unknown block tag {name}"))
}

fn direction(name: &str) -> Result<BlockDirection> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "down" => BlockDirection::Down,
        "up" => BlockDirection::Up,
        "north" => BlockDirection::North,
        "south" => BlockDirection::South,
        "west" => BlockDirection::West,
        "east" => BlockDirection::East,
        _ => return Err(format!("unknown direction {name}")),
    })
}

fn heightmap(name: &str) -> Result<HeightMap> {
    Ok(match name {
        "WORLD_SURFACE_WG" => HeightMap::WorldSurfaceWg,
        "WORLD_SURFACE" => HeightMap::WorldSurface,
        "OCEAN_FLOOR_WG" => HeightMap::OceanFloorWg,
        "OCEAN_FLOOR" => HeightMap::OceanFloor,
        "MOTION_BLOCKING" => HeightMap::MotionBlocking,
        "MOTION_BLOCKING_NO_LEAVES" => HeightMap::MotionBlockingNoLeaves,
        _ => return Err(format!("unknown heightmap {name}")),
    })
}

fn y_offset(value: &Value) -> Result<YOffset> {
    let obj = object(value)?;
    let int = |key: &str| obj.get(key).and_then(Value::as_i64);
    let range = || format!("Y offset out of range: {value}");
    if let Some(n) = int("absolute") {
        let absolute = i16::try_from(n).map_err(|_| range())?;
        Ok(YOffset::Absolute(Absolute { absolute }))
    } else if let Some(n) = int("above_bottom") {
        let above_bottom = i8::try_from(n).map_err(|_| range())?;
        Ok(YOffset::AboveBottom(AboveBottom { above_bottom }))
    } else if let Some(n) = int("below_top") {
        let below_top = i8::try_from(n).map_err(|_| range())?;
        Ok(YOffset::BelowTop(BelowTop { below_top }))
    } else if let Some(n) = int("relative_to_sea_level") {
        let relative_to_sea_level = i16::try_from(n).map_err(|_| range())?;
        Ok(YOffset::RelativeToSeaLevel(RelativeToSeaLevel {
            relative_to_sea_level,
        }))
    } else {
        Err(format!("unknown Y offset {value}"))
    }
}

fn int_provider(value: &Value) -> Result<IntProvider> {
    let obj = match value {
        Value::Null => return Ok(IntProvider::Constant(0)),
        Value::Number(n) => {
            return n
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .map(IntProvider::Constant)
                .ok_or_else(|| format!("bad int {value}"));
        }
        Value::Object(obj) => obj,
        other => return Err(format!("bad int provider {other}")),
    };
    let ty = str_of(obj, "type")?;
    let provider = match ty.strip_prefix("minecraft:").unwrap_or(ty) {
        "constant" => NormalIntProvider::Constant(ConstantIntProvider {
            value: i32_or(obj, "value", 0)?,
        }),
        "uniform" => NormalIntProvider::Uniform(UniformIntProvider {
            min_inclusive: i32_or(obj, "min_inclusive", 0)?,
            max_inclusive: i32_or(obj, "max_inclusive", 0)?,
        }),
        "biased_to_bottom" => NormalIntProvider::BiasedToBottom(BiasedToBottomIntProvider {
            min_inclusive: i32_or(obj, "min_inclusive", 0)?,
            max_inclusive: i32_or(obj, "max_inclusive", 0)?,
        }),
        "clamped" => NormalIntProvider::Clamped(ClampedIntProvider {
            source: Box::new(int_provider(obj.get("source").unwrap_or(&Value::Null))?),
            min_inclusive: i32_or(obj, "min_inclusive", 0)?,
            max_inclusive: i32_or(obj, "max_inclusive", 0)?,
        }),
        "trapezoid" => NormalIntProvider::Trapezoid(TrapezoidIntProvider {
            min_inclusive: i32_or(obj, "min", 0)?,
            max_inclusive: i32_or(obj, "max", 0)?,
            plateau: i32_or(obj, "plateau", 0)?,
        }),
        "clamped_normal" => NormalIntProvider::ClampedNormal(ClampedNormalIntProvider {
            mean: f32_or(obj, "mean", 0.0)?,
            deviation: f32_or(obj, "deviation", 1.0)?,
            min_inclusive: i32_or(obj, "min_inclusive", 0)?,
            max_inclusive: i32_or(obj, "max_inclusive", 0)?,
        }),
        "weighted_list" => NormalIntProvider::WeightedList(WeightedListIntProvider {
            distribution: array(obj, "distribution")?
                .iter()
                .map(|entry| {
                    let entry = object(entry)?;
                    Ok(WeightedEntry {
                        data: int_provider(entry.get("data").unwrap_or(&Value::Null))?,
                        weight: i32_or(entry, "weight", 1)?,
                    })
                })
                .collect::<Result<_>>()?,
        }),
        other => return Err(format!("unsupported int provider {other}")),
    };
    Ok(IntProvider::Object(provider))
}

fn height_provider(value: &Value) -> Result<HeightProvider> {
    let obj = object(value)?;
    let ty = str_of(obj, "type")?;
    let offset = |key: &str| y_offset(obj.get(key).unwrap_or(&Value::Null));
    Ok(match ty.strip_prefix("minecraft:").unwrap_or(ty) {
        "uniform" => HeightProvider::Uniform(UniformHeightProvider {
            min_inclusive: offset("min_inclusive")?,
            max_inclusive: offset("max_inclusive")?,
        }),
        "trapezoid" => HeightProvider::Trapezoid(TrapezoidHeightProvider {
            min_inclusive: offset("min_inclusive")?,
            max_inclusive: offset("max_inclusive")?,
            plateau: obj
                .get("plateau")
                .and_then(Value::as_i64)
                .and_then(|n| i32::try_from(n).ok()),
        }),
        "very_biased_to_bottom" => {
            HeightProvider::VeryBiasedToBottom(VeryBiasedToBottomHeightProvider {
                min_inclusive: offset("min_inclusive")?,
                max_inclusive: offset("max_inclusive")?,
                inner: obj
                    .get("inner")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .and_then(NonZero::new),
            })
        }
        // As the codegen: a constant is a uniform range of one value.
        "constant" => {
            let y = offset("value")?;
            HeightProvider::Uniform(UniformHeightProvider {
                min_inclusive: y,
                max_inclusive: y,
            })
        }
        other => return Err(format!("unsupported height provider {other}")),
    })
}

fn offset_predicate(obj: &Map<String, Value>) -> Result<OffsetBlocksBlockPredicate> {
    Ok(OffsetBlocksBlockPredicate {
        offset: match obj.get("offset") {
            Some(v @ Value::Array(_)) => Some(vec3(v)?),
            _ => None,
        },
    })
}

fn matching_blocks(value: Option<&Value>) -> MatchingBlocksWrapper {
    match value {
        Some(Value::String(name)) => MatchingBlocksWrapper::Single(name.clone()),
        Some(Value::Array(names)) => MatchingBlocksWrapper::Multiple(
            names
                .iter()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect(),
        ),
        _ => MatchingBlocksWrapper::Single(String::new()),
    }
}

fn block_state_codec(value: &Value) -> Result<BlockStateCodec> {
    let (name, properties) = match value {
        Value::String(name) => (name.as_str(), None),
        Value::Object(obj) => (
            obj.get("id")
                .or_else(|| obj.get("Name"))
                .and_then(Value::as_str)
                .ok_or("block state without id")?,
            obj.get("properties")
                .or_else(|| obj.get("Properties"))
                .and_then(Value::as_object),
        ),
        other => return Err(format!("unsupported block state {other}")),
    };
    Ok(BlockStateCodec {
        name: block(name)?,
        properties: properties.map(|props| {
            props
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        }),
    })
}

fn block_predicate(value: &Value) -> Result<BlockPredicate> {
    // A bare string: `#tag` matches the tag, anything else is always true (as the codegen).
    if let Some(s) = value.as_str() {
        return Ok(match s.strip_prefix('#') {
            Some(tag) => BlockPredicate::MatchingBlockTag(MatchingBlockTagPredicate {
                offset: OffsetBlocksBlockPredicate { offset: None },
                tag: block_tag(tag)?,
            }),
            None => BlockPredicate::AlwaysTrue,
        });
    }
    let obj = match value {
        Value::Null => return Ok(BlockPredicate::AlwaysTrue),
        other => object(other)?,
    };
    let ty = obj.get("type").and_then(Value::as_str).unwrap_or("");
    Ok(match ty.strip_prefix("minecraft:").unwrap_or(ty) {
        "true" | "volume_match" | "" => BlockPredicate::AlwaysTrue,
        "matching_blocks" => BlockPredicate::MatchingBlocks(MatchingBlocksBlockPredicate {
            offset: offset_predicate(obj)?,
            blocks: matching_blocks(obj.get("blocks")),
        }),
        "matching_block_tag" => BlockPredicate::MatchingBlockTag(MatchingBlockTagPredicate {
            offset: offset_predicate(obj)?,
            tag: block_tag(str_of(obj, "tag")?)?,
        }),
        "matching_fluids" => BlockPredicate::MatchingFluids(MatchingFluidsBlockPredicate {
            offset: offset_predicate(obj)?,
            fluids: matching_blocks(obj.get("fluids")),
        }),
        "has_sturdy_face" => BlockPredicate::HasSturdyFace(HasSturdyFacePredicate {
            offset: offset_predicate(obj)?,
            direction: direction(
                obj.get("direction")
                    .and_then(Value::as_str)
                    .unwrap_or("down"),
            )?,
        }),
        "solid" => BlockPredicate::Solid(SolidBlockPredicate {
            offset: offset_predicate(obj)?,
        }),
        "replaceable" => BlockPredicate::Replaceable(ReplaceableBlockPredicate {
            offset: offset_predicate(obj)?,
        }),
        "would_survive" => BlockPredicate::WouldSurvive(WouldSurviveBlockPredicate {
            offset: offset_predicate(obj)?,
            state: block_state_codec(obj.get("state").unwrap_or(&Value::Null))?,
        }),
        "inside_world_bounds" => {
            BlockPredicate::InsideWorldBounds(InsideWorldBoundsBlockPredicate {
                offset: match obj.get("offset") {
                    Some(v @ Value::Array(_)) => vec3(v)?,
                    _ => Vector3::new(0, 0, 0),
                },
            })
        }
        "any_of" => BlockPredicate::AnyOf(AnyOfBlockPredicate {
            predicates: array(obj, "predicates")?
                .iter()
                .map(block_predicate)
                .collect::<Result<_>>()?,
        }),
        "all_of" => BlockPredicate::AllOf(AllOfBlockPredicate {
            predicates: array(obj, "predicates")?
                .iter()
                .map(block_predicate)
                .collect::<Result<_>>()?,
        }),
        "not" => BlockPredicate::Not(NotBlockPredicate {
            predicate: Box::new(block_predicate(
                obj.get("predicate").unwrap_or(&Value::Null),
            )?),
        }),
        "height_range" => BlockPredicate::HeightRange(HeightRangeBlockPredicate {
            min_inclusive: y_offset(obj.get("min_inclusive").unwrap_or(&Value::Null))?,
            max_inclusive: y_offset(obj.get("max_inclusive").unwrap_or(&Value::Null))?,
        }),
        other => return Err(format!("unsupported block predicate {other}")),
    })
}

fn rule_test(value: &Value) -> Result<RuleTest> {
    let obj = object(value)?;
    let ty = obj
        .get("predicate_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let state_id =
        |key: &str| -> Result<_> { Ok(block_state(obj.get(key).unwrap_or(&Value::Null))?.id) };
    Ok(match ty.strip_prefix("minecraft:").unwrap_or(ty) {
        "always_true" | "" => RuleTest::AlwaysTrue,
        "block_match" => RuleTest::BlockMatch(BlockMatchRuleTest {
            block: block(str_of(obj, "block")?)?.id,
        }),
        "blockstate_match" => RuleTest::BlockStateMatch(BlockStateMatchRuleTest {
            block_state: state_id("block_state")?,
        }),
        "tag_match" => RuleTest::TagMatch(TagMatchRuleTest {
            tag: block_tag(str_of(obj, "tag")?)?,
        }),
        "random_block_match" => RuleTest::RandomBlockMatch(RandomBlockMatchRuleTest {
            block: block(str_of(obj, "block")?)?.id,
            probability: f32_or(obj, "probability", 0.5)?,
        }),
        "random_blockstate_match" => {
            RuleTest::RandomBlockStateMatch(RandomBlockStateMatchRuleTest {
                block_state: state_id("block_state")?,
                probability: f32_or(obj, "probability", 0.5)?,
            })
        }
        "any_of" => RuleTest::AnyOf(
            array(obj, "rules")?
                .iter()
                .map(rule_test)
                .collect::<Result<_>>()?,
        ),
        "all_of" => RuleTest::AllOf(
            array(obj, "rules")?
                .iter()
                .map(rule_test)
                .collect::<Result<_>>()?,
        ),
        "not" => RuleTest::Not(Box::new(rule_test(
            obj.get("rule").unwrap_or(&Value::Null),
        )?)),
        "height_match" => RuleTest::HeightMatch {
            min_inclusive: i32_or(obj, "min_inclusive", i32::MIN)?,
            max_inclusive: i32_or(obj, "max_inclusive", i32::MAX)?,
        },
        other => return Err(format!("unsupported rule test {other}")),
    })
}

fn noise_parameters(value: &Value) -> Result<DoublePerlinNoiseParametersCodec> {
    let obj = object(value)?;
    let first_octave = obj
        .get("firstOctave")
        .or_else(|| obj.get("base_octave"))
        .and_then(Value::as_i64)
        .map_or(Ok(-7), i32::try_from)
        .map_err(|_| format!("bad first octave in {value}"))?;
    let amplitudes = obj
        .get("amplitudes")
        .or_else(|| obj.get("amplitude_modifiers"))
        .and_then(Value::as_array)
        .map_or_else(
            || {
                let count = obj.get("octave_count").and_then(Value::as_u64).unwrap_or(1);
                vec![1.0; usize::try_from(count.max(1)).unwrap_or(1)]
            },
            |list| list.iter().filter_map(Value::as_f64).collect(),
        );
    Ok(DoublePerlinNoiseParametersCodec {
        first_octave,
        amplitudes,
    })
}

fn noise_base(obj: &Map<String, Value>) -> Result<NoiseBlockStateProviderBase> {
    Ok(NoiseBlockStateProviderBase {
        seed: obj.get("seed").and_then(Value::as_i64).unwrap_or(0),
        noise: noise_parameters(obj.get("noise").unwrap_or(&Value::Null))?,
        scale: f32_or(obj, "scale", 1.0)?,
    })
}

fn states(obj: &Map<String, Value>, key: &str) -> Result<Vec<&'static BlockState>> {
    array(obj, key)?.iter().map(block_state).collect()
}

fn ore_targets(obj: &Map<String, Value>) -> Result<Vec<OreTarget>> {
    array(obj, "targets")?
        .iter()
        .map(|target| {
            let target = object(target)?;
            Ok(OreTarget {
                target: rule_test(target.get("target").unwrap_or(&Value::Null))?,
                state: block_state(target.get("state").unwrap_or(&Value::Null))?,
            })
        })
        .collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per modifier type, as the codegen"
)]
fn placement_modifier(value: &Value) -> Result<PlacementModifier> {
    let obj = object(value)?;
    let ty = str_of(obj, "type")?;
    let get = |key: &str| obj.get(key).unwrap_or(&Value::Null);
    Ok(match ty.strip_prefix("minecraft:").unwrap_or(ty) {
        "biome" => PlacementModifier::Biome(BiomePlacementModifier),
        "in_square" => PlacementModifier::InSquare(SquarePlacementModifier),
        "fixed_placement" => PlacementModifier::FixedPlacement(
            array(obj, "positions")?
                .iter()
                .map(|p| vec3(p).map(BlockPos))
                .collect::<Result<_>>()?,
        ),
        "heightmap" => PlacementModifier::Heightmap(HeightmapPlacementModifier {
            heightmap: heightmap(
                obj.get("heightmap")
                    .and_then(Value::as_str)
                    .unwrap_or("MOTION_BLOCKING"),
            )?,
        }),
        "height_range" => PlacementModifier::HeightRange(HeightRangePlacementModifier {
            height: height_provider(get("height"))?,
        }),
        "count" => PlacementModifier::Count(CountPlacementModifier {
            count: int_provider(get("count"))?,
        }),
        "count_on_every_layer" => {
            PlacementModifier::CountOnEveryLayer(CountOnEveryLayerPlacementModifier {
                count: int_provider(get("count"))?,
            })
        }
        "rarity_filter" => PlacementModifier::RarityFilter(RarityFilterPlacementModifier {
            chance: obj
                .get("chance")
                .and_then(Value::as_u64)
                .map_or(Ok(1), u32::try_from)
                .map_err(|_| format!("bad rarity chance in {value}"))?,
        }),
        "block_predicate_filter" => {
            PlacementModifier::BlockPredicateFilter(BlockFilterPlacementModifier {
                predicate: block_predicate(get("predicate"))?,
            })
        }
        "surface_relative_threshold_filter" => PlacementModifier::SurfaceRelativeThresholdFilter(
            SurfaceThresholdFilterPlacementModifier {
                heightmap: heightmap(
                    obj.get("heightmap")
                        .and_then(Value::as_str)
                        .unwrap_or("MOTION_BLOCKING"),
                )?,
                min_inclusive: obj
                    .get("min_inclusive")
                    .and_then(Value::as_i64)
                    .and_then(|n| i32::try_from(n).ok()),
                max_inclusive: obj
                    .get("max_inclusive")
                    .and_then(Value::as_i64)
                    .and_then(|n| i32::try_from(n).ok()),
            },
        ),
        "surface_water_depth_filter" => {
            PlacementModifier::SurfaceWaterDepthFilter(SurfaceWaterDepthFilterPlacementModifier {
                max_water_depth: i32_or(obj, "max_water_depth", 0)?,
            })
        }
        "noise_based_count" => {
            PlacementModifier::NoiseBasedCount(NoiseBasedCountPlacementModifier {
                to_count_ratio: i32_or(obj, "noise_to_count_ratio", 0)?,
                factor: f64_or(obj, "noise_factor", 1.0)?,
                offset: f64_or(obj, "noise_offset", 0.0)?,
            })
        }
        "noise_threshold_count" => {
            PlacementModifier::NoiseThresholdCount(NoiseThresholdCountPlacementModifier {
                noise_level: f64_or(obj, "noise_level", 0.0)?,
                below_noise: i32_or(obj, "below_noise", 0)?,
                above_noise: i32_or(obj, "above_noise", 0)?,
            })
        }
        "environment_scan" => {
            PlacementModifier::EnvironmentScan(EnvironmentScanPlacementModifier {
                direction_of_search: direction(
                    obj.get("direction_of_search")
                        .and_then(Value::as_str)
                        .unwrap_or("down"),
                )?,
                target_condition: block_predicate(get("target_condition"))?,
                allowed_search_condition: match obj.get("allowed_search_condition") {
                    Some(v @ Value::Object(_)) => Some(block_predicate(v)?),
                    _ => None,
                },
                max_steps: i32_or(obj, "max_steps", 1)?,
            })
        }
        // `RandomOffsetPlacement` samples x, y, then z: the same provider serves x and z.
        "random_offset" => {
            let xz = int_provider(get("xz_spread"))?;
            PlacementModifier::Offset(OffsetPlacementModifier {
                x: xz.clone(),
                y: int_provider(get("y_spread"))?,
                z: xz,
            })
        }
        "offset" => PlacementModifier::Offset(OffsetPlacementModifier {
            x: int_provider(get("x"))?,
            y: int_provider(get("y"))?,
            z: int_provider(get("z"))?,
        }),
        other => return Err(format!("unsupported placement modifier {other}")),
    })
}

pub(super) struct FeatureParser<'j> {
    json: &'j WorldgenJson,
    depth: usize,
}

impl<'j> FeatureParser<'j> {
    pub(super) const fn new(json: &'j WorldgenJson) -> Self {
        Self { json, depth: 0 }
    }

    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.depth >= MAX_DEPTH {
            return Err("feature references nested too deep (a cycle?)".to_string());
        }
        self.depth += 1;
        let result = f(self);
        self.depth -= 1;
        result
    }

    /// A datapack's own entry of `registry`: vanilla ids are left to the keyed lookups.
    fn own_entry(&self, registry: WorldgenRegistry, id: &str) -> Result<&'j Value> {
        let json: &'j WorldgenJson = self.json;
        json.get(registry, id)
            .map(|(value, _)| value)
            .ok_or_else(|| format!("unknown {} {id}", registry.folder()))
    }

    pub(super) fn placed_feature(&mut self, value: &Value) -> Result<PlacedFeature> {
        let obj = object(value)?;
        Ok(PlacedFeature {
            feature: self.feature_ref(obj.get("feature").unwrap_or(&Value::Null))?,
            placement: array(obj, "placement")?
                .iter()
                .map(placement_modifier)
                .collect::<Result<_>>()?,
        })
    }

    fn feature_ref(&mut self, value: &Value) -> Result<Feature> {
        match value {
            Value::String(id) => {
                let id = full_id(id);
                if let Some(key) = vanilla_path(&id).and_then(ConfiguredFeatureKey::from_name) {
                    return Ok(Feature::Named(key));
                }
                let value = self.own_entry(WorldgenRegistry::ConfiguredFeature, &id)?;
                self.nested(|p| p.configured_feature(value))
                    .map(|f| Feature::Inlined(Box::new(f)))
                    .map_err(|e| format!("{id}: {e}"))
            }
            Value::Object(_) => self
                .nested(|p| p.configured_feature(value))
                .map(|f| Feature::Inlined(Box::new(f))),
            other => Err(format!("bad feature reference {other}")),
        }
    }

    fn placed_ref(&mut self, value: &Value) -> Result<PlacedFeatureWrapper> {
        match value {
            Value::String(id) => {
                let id = full_id(id);
                if let Some(key) = vanilla_path(&id).and_then(PlacedFeatureKey::from_name) {
                    return Ok(PlacedFeatureWrapper::Named(key));
                }
                self.own_placed(&id).map(PlacedFeatureWrapper::Direct)
            }
            Value::Object(_) => self
                .nested(|p| p.placed_feature(value))
                .map(PlacedFeatureWrapper::Direct),
            other => Err(format!("bad placed feature reference {other}")),
        }
    }

    pub(super) fn own_placed(&mut self, id: &str) -> Result<PlacedFeature> {
        let value = self.own_entry(WorldgenRegistry::PlacedFeature, id)?;
        self.nested(|p| p.placed_feature(value))
            .map_err(|e| format!("{id}: {e}"))
    }

    /// A placed feature held by value: written inline, or one of the datapack's own by id.
    fn placed_value(&mut self, value: &Value) -> Result<PlacedFeature> {
        match value {
            Value::String(id) => self.own_placed(&full_id(id)),
            other => self.nested(|p| p.placed_feature(other)),
        }
    }

    fn vegetation_patch(&mut self, config: &Map<String, Value>) -> Result<VegetationPatchFeature> {
        let get = |key: &str| config.get(key).unwrap_or(&Value::Null);
        Ok(VegetationPatchFeature {
            replaceable: block_predicate(get("replaceable"))?,
            ground_state: self.block_state_provider(get("ground_state"))?,
            vegetation_feature: Box::new(self.placed_value(get("vegetation_feature"))?),
            surface: match get("surface").as_str().unwrap_or("floor") {
                "floor" => VerticalSurfaceType::Floor,
                "ceiling" => VerticalSurfaceType::Ceiling,
                other => return Err(format!("unknown surface {other}")),
            },
            depth: int_provider(get("depth"))?,
            extra_bottom_block_chance: f32_or(config, "extra_bottom_block_chance", 0.0)?,
            vertical_range: i32_or(config, "vertical_range", 0)?,
            vegetation_chance: f32_or(config, "vegetation_chance", 0.0)?,
            xz_radius: int_provider(get("xz_radius"))?,
            extra_edge_column_chance: f32_or(config, "extra_edge_column_chance", 0.0)?,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per provider type, as the codegen"
    )]
    fn block_state_provider(&mut self, value: &Value) -> Result<BlockStateProvider> {
        if let Value::String(id) = value {
            let id = full_id(id);
            let value = self.own_entry(WorldgenRegistry::BlockStateProvider, &id)?;
            return self.nested(|p| p.block_state_provider(value));
        }
        let obj = object(value)?;
        if !obj.contains_key("type") && (obj.contains_key("id") || obj.contains_key("Name")) {
            return Ok(BlockStateProvider::Simple(SimpleStateProvider {
                state: block_state(value)?,
            }));
        }
        let ty = str_of(obj, "type")?;
        Ok(match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "simple_state_provider" | "simple" => BlockStateProvider::Simple(SimpleStateProvider {
                state: block_state(obj.get("state").unwrap_or(value))?,
            }),
            "weighted_state_provider" | "weighted" => {
                BlockStateProvider::Weighted(WeightedBlockStateProvider {
                    entries: array(obj, "entries")?
                        .iter()
                        .map(|entry| {
                            let entry = object(entry)?;
                            Ok(Weighted {
                                data: block_state(entry.get("data").unwrap_or(&Value::Null))?,
                                weight: i32_or(entry, "weight", 1)?,
                            })
                        })
                        .collect::<Result<_>>()?,
                })
            }
            "rotated_block_provider" | "rotated" => {
                BlockStateProvider::Pillar(PillarBlockStateProvider {
                    state: block_state(obj.get("state").unwrap_or(&Value::Null))?,
                })
            }
            "noise_provider" | "noise" => {
                BlockStateProvider::NoiseProvider(NoiseBlockStateProvider {
                    base: noise_base(obj)?,
                    states: states(obj, "states")?,
                })
            }
            "dual_noise_provider" | "dual_noise" => {
                let variety = obj.get("variety").and_then(Value::as_array);
                let variety_at = |i: usize, default: u32| {
                    variety
                        .and_then(|v| v.get(i))
                        .and_then(Value::as_u64)
                        .and_then(|n| u32::try_from(n).ok())
                        .unwrap_or(default)
                };
                BlockStateProvider::DualNoise(DualNoiseBlockStateProvider {
                    base: NoiseBlockStateProvider {
                        base: noise_base(obj)?,
                        states: states(obj, "states")?,
                    },
                    variety: [variety_at(0, 2), variety_at(1, 4)],
                    slow_noise: noise_parameters(obj.get("slow_noise").unwrap_or(&Value::Null))?,
                    slow_scale: f64_or(obj, "slow_scale", 1.0)?,
                })
            }
            "noise_threshold_provider" | "noise_threshold" => {
                BlockStateProvider::NoiseThreshold(NoiseThresholdBlockStateProvider {
                    base: noise_base(obj)?,
                    threshold: f32_or(obj, "threshold", 0.0)?,
                    high_chance: f32_or(obj, "high_chance", 0.0)?,
                    default_state: block_state(obj.get("default_state").unwrap_or(&Value::Null))?,
                    low_states: states(obj, "low_states")?,
                    high_states: states(obj, "high_states")?,
                })
            }
            "rule_based_state_provider" | "rule_based" => {
                BlockStateProvider::Rule(RuleBasedBlockStateProvider {
                    fallback: match obj.get("fallback") {
                        None | Some(Value::Null) => None,
                        Some(v) => Some(Box::new(self.nested(|p| p.block_state_provider(v))?)),
                    },
                    rules: array(obj, "rules")?
                        .iter()
                        .map(|rule| {
                            let rule = object(rule)?;
                            Ok(BlockStateRule {
                                if_true: block_predicate(
                                    rule.get("if_true").unwrap_or(&Value::Null),
                                )?,
                                then: self.nested(|p| {
                                    p.block_state_provider(rule.get("then").unwrap_or(&Value::Null))
                                })?,
                            })
                        })
                        .collect::<Result<_>>()?,
                })
            }
            "randomized_int_state_provider" | "randomized_int" => {
                let source = self.nested(|p| {
                    p.block_state_provider(obj.get("source").unwrap_or(&Value::Null))
                })?;
                let name = str_of(obj, "property")?;
                // The property is looked up on the source's block, as vanilla's codec does.
                let state = match &source {
                    BlockStateProvider::Simple(p) => Some(p.state),
                    BlockStateProvider::Pillar(p) => Some(p.state),
                    BlockStateProvider::Weighted(p) => p.entries.first().map(|e| e.data),
                    _ => None,
                }
                .ok_or_else(|| format!("randomized_int over an unsupported source: {name}"))?;
                let block = state.id.to_block();
                let property = block
                    .properties(state.id)
                    .map(|p| p.to_props())
                    .into_iter()
                    .flatten()
                    .find(|p| p.key_str() == name)
                    .ok_or_else(|| format!("{} has no property {name}", block.name))?
                    .key();
                BlockStateProvider::RandomizedInt(RandomizedIntBlockStateProvider {
                    source: Box::new(source),
                    property,
                    values: int_provider(obj.get("values").unwrap_or(&Value::Null))?,
                })
            }
            other => return Err(format!("unsupported block state provider {other}")),
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per feature type, as the codegen"
    )]
    fn configured_feature(&mut self, value: &Value) -> Result<ConfiguredFeature> {
        let obj = object(value)?;
        let ty = str_of(obj, "type")?;
        let config = match obj.get("config") {
            Some(Value::Object(config)) => config,
            _ => obj,
        };
        let get = |key: &str| config.get(key).unwrap_or(&Value::Null);
        Ok(match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "no_op" => ConfiguredFeature::NoOp,
            "ore" => ConfiguredFeature::Ore(OreFeature {
                size: i32_or(config, "size", 0)?,
                discard_chance_on_air_exposure: f32_or(
                    config,
                    "discard_chance_on_air_exposure",
                    0.0,
                )?,
                targets: ore_targets(config)?,
            }),
            "scattered_ore" => ConfiguredFeature::ScatteredOre(ScatteredOreFeature {
                size: i32_or(config, "size", 0)?,
                discard_chance_on_air_exposure: f32_or(
                    config,
                    "discard_chance_on_air_exposure",
                    0.0,
                )?,
                targets: ore_targets(config)?,
            }),
            "replace_single_block" => {
                ConfiguredFeature::ReplaceSingleBlock(ReplaceSingleBlockFeature {
                    targets: ore_targets(config)?,
                })
            }
            "simple_block" => ConfiguredFeature::SimpleBlock(SimpleBlockFeature {
                to_place: self.block_state_provider(get("to_place"))?,
                schedule_tick: config.get("schedule_tick").and_then(Value::as_bool),
            }),
            "block_pile" => ConfiguredFeature::BlockPile(BlockPileFeature {
                state_provider: self.block_state_provider(get("state_provider"))?,
            }),
            "block_blob" => ConfiguredFeature::ForestRock(ForestRockFeature {
                state: block_state(get("state"))?,
            }),
            "disk" => ConfiguredFeature::Disk(DiskFeature {
                state_provider: self.block_state_provider(get("state_provider"))?,
                target: block_predicate(get("target"))?,
                radius: int_provider(get("radius"))?,
                half_height: i32_or(config, "half_height", 1)?,
            }),
            "block_column" => ConfiguredFeature::BlockColumn(BlockColumnFeature {
                layers: array(config, "layers")?
                    .iter()
                    .map(|layer| {
                        let layer = object(layer)?;
                        Ok(Layer {
                            height: int_provider(layer.get("height").unwrap_or(&Value::Null))?,
                            provider: self.block_state_provider(
                                layer.get("provider").unwrap_or(&Value::Null),
                            )?,
                        })
                    })
                    .collect::<Result<_>>()?,
                direction: direction(
                    config
                        .get("direction")
                        .and_then(Value::as_str)
                        .unwrap_or("up"),
                )?,
                allowed_placement: block_predicate(get("allowed_placement"))?,
                prioritize_tip: bool_or(config, "prioritize_tip", false)?,
            }),
            "spring_feature" => ConfiguredFeature::SpringFeature(SpringFeatureFeature {
                state: block_state(get("state"))?,
                requires_block_below: bool_or(config, "requires_block_below", true)?,
                rock_count: i32_or(config, "rock_count", 4)?,
                hole_count: i32_or(config, "hole_count", 1)?,
                valid_blocks: match get("valid_blocks") {
                    Value::String(name) => BlockWrapper::Single(name.clone()),
                    Value::Array(names) => BlockWrapper::Multi(
                        names
                            .iter()
                            .filter_map(|n| n.as_str().map(str::to_string))
                            .collect(),
                    ),
                    _ => BlockWrapper::Single(String::new()),
                },
            }),
            "netherrack_replace_blobs" => {
                ConfiguredFeature::NetherrackReplaceBlobs(ReplaceBlobsFeature {
                    target: block_state(get("target"))?,
                    state: block_state(get("state"))?,
                    radius: int_provider(get("radius"))?,
                })
            }
            "sea_pickle" => ConfiguredFeature::SeaPickle(SeaPickleFeature {
                count: int_provider(get("count"))?,
            }),
            "seagrass" => ConfiguredFeature::Seagrass(SeagrassFeature {
                probability: f32_or(config, "probability", 0.0)?,
            }),
            "bamboo" => ConfiguredFeature::Bamboo(BambooFeature {
                probability: f32_or(config, "probability", 0.0)?,
            }),
            "random_selector" => ConfiguredFeature::RandomSelector(RandomFeature {
                features: array(config, "features")?
                    .iter()
                    .map(|entry| {
                        let entry = object(entry)?;
                        Ok(RandomFeatureEntry {
                            feature: self
                                .placed_ref(entry.get("feature").unwrap_or(&Value::Null))?,
                            chance: f32_or(entry, "chance", 0.1)?,
                        })
                    })
                    .collect::<Result<_>>()?,
                default: Box::new(self.placed_ref(get("default"))?),
            }),
            "weighted_random_selector" => {
                let features = array(config, "features")?
                    .iter()
                    .map(|entry| {
                        let entry = object(entry)?;
                        Ok(WeightedRandomFeatureEntry {
                            feature: self.placed_ref(entry.get("data").unwrap_or(&Value::Null))?,
                            weight: i32_or(entry, "weight", 1)?.max(0),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                ConfiguredFeature::WeightedRandomSelector(WeightedRandomFeature {
                    total_weight: features.iter().map(|f| f.weight).sum(),
                    features,
                })
            }
            "simple_random_selector" => {
                ConfiguredFeature::SimpleRandomSelector(SimpleRandomFeature {
                    features: array(config, "features")?
                        .iter()
                        .map(|feature| match feature {
                            Value::String(id) => self.own_placed(&full_id(id)),
                            other => self.nested(|p| p.placed_feature(other)),
                        })
                        .collect::<Result<_>>()?,
                })
            }
            "random_boolean_selector" => {
                ConfiguredFeature::RandomBooleanSelector(RandomBooleanFeature {
                    feature_true: Box::new(self.placed_ref(get("feature_true"))?),
                    feature_false: Box::new(self.placed_ref(get("feature_false"))?),
                })
            }
            "tree" => ConfiguredFeature::Tree(Box::new(self.tree(config)?)),
            "fallen_tree" => ConfiguredFeature::FallenTree(self.fallen_tree(config)?),
            // A list of placed features, or a single one written alone.
            "overlay" => ConfiguredFeature::Overlay(OverlayFeature {
                features: match get("features") {
                    Value::Array(list) => list
                        .iter()
                        .map(|f| self.placed_ref(f))
                        .collect::<Result<_>>()?,
                    single => vec![self.placed_ref(single)?],
                },
            }),
            "stepped_column_cluster" => {
                ConfiguredFeature::SteppedColumnCluster(SteppedColumnClusterFeature {
                    block: self.block_state_provider(get("block"))?,
                    continue_through: block_predicate(get("continue_through"))?,
                    can_replace: block_predicate(get("can_replace"))?,
                    cannot_place_on: block_tag(
                        str_of(config, "cannot_place_on")?
                            .strip_prefix('#')
                            .ok_or("\"cannot_place_on\" must be a block tag")?,
                    )?,
                    cluster_reach: int_provider(get("cluster_reach"))?,
                    column_count: int_provider(get("column_count"))?,
                    column_reach: int_provider(get("column_reach"))?,
                    height: int_provider(get("height"))?,
                })
            }
            "single_block_pillar" => {
                ConfiguredFeature::SingleBlockPillar(SingleBlockPillarFeature {
                    block: self.block_state_provider(get("block"))?,
                    can_replace: block_predicate(get("can_replace"))?,
                    direction: direction(
                        config
                            .get("direction")
                            .and_then(Value::as_str)
                            .unwrap_or("down"),
                    )?,
                    chance_to_continue: f32_or(config, "chance_to_continue", 1.0)?,
                    cap_feature: match get("cap_feature") {
                        Value::Null => None,
                        v => Some(self.placed_ref(v)?),
                    },
                })
            }
            "projected_random_patchy_square" => ConfiguredFeature::ProjectedRandomPatchySquare(
                ProjectedRandomPatchySquareFeature {
                    block: self.block_state_provider(get("block"))?,
                    project_through: block_predicate(get("project_through"))?,
                    size: int_provider(get("size"))?,
                    max_projection_height: i32_or(config, "max_projection_height", 0)?,
                },
            ),
            "vegetation_patch" => ConfiguredFeature::VegetationPatch(self.vegetation_patch(config)?),
            "waterlogged_vegetation_patch" => {
                ConfiguredFeature::WaterloggedVegetationPatch(WaterloggedVegetationPatchFeature {
                    base: self.vegetation_patch(config)?,
                })
            }
            "root_system" => ConfiguredFeature::RootSystem(RootSystemFeature {
                feature: Box::new(self.placed_value(get("feature"))?),
                required_vertical_space_for_tree: i32_or(
                    config,
                    "required_vertical_space_for_tree",
                    0,
                )?,
                root_radius: i32_or(config, "root_radius", 0)?,
                root_replaceable: block_predicate(get("root_replaceable"))?,
                root_state_provider: self.block_state_provider(get("root_state_provider"))?,
                root_placement_attempts: i32_or(config, "root_placement_attempts", 0)?,
                root_column_max_height: i32_or(config, "root_column_max_height", 0)?,
                hanging_root_radius: i32_or(config, "hanging_root_radius", 0)?,
                hanging_roots_vertical_span: i32_or(config, "hanging_roots_vertical_span", 0)?,
                hanging_root_state_provider: self
                    .block_state_provider(get("hanging_root_state_provider"))?,
                hanging_root_placement_attempts: i32_or(
                    config,
                    "hanging_root_placement_attempts",
                    0,
                )?,
                allowed_vertical_water_for_tree: i32_or(
                    config,
                    "allowed_vertical_water_for_tree",
                    0,
                )?,
                allowed_tree_position: block_predicate(get("allowed_tree_position"))?,
            }),
            "huge_fungus" => ConfiguredFeature::HugeFungus(HugeFungusFeature {
                valid_base_block: block_state(get("valid_base_block"))?,
                stem_state: block_state(get("stem_state"))?,
                hat_state: block_state(get("hat_state"))?,
                decor_state: block_state(get("decor_state"))?,
                replaceable_blocks: block_predicate(get("replaceable_blocks"))?,
                planted: bool_or(config, "planted", false)?,
            }),
            "multiface_growth" => ConfiguredFeature::MultifaceGrowth(MultifaceGrowthFeature {
                place_block: block(str_of(config, "block")?)?.id,
                search_range: i32_or(config, "search_range", 10)?,
                can_place_on_floor: bool_or(config, "can_place_on_floor", false)?,
                can_place_on_ceiling: bool_or(config, "can_place_on_ceiling", false)?,
                can_place_on_wall: bool_or(config, "can_place_on_wall", false)?,
                chance_of_spreading: f32_or(config, "chance_of_spreading", 0.5)?,
                can_be_placed_on: block_ids(get("can_be_placed_on"))?,
            }),
            "random_neighbor_spread" => {
                ConfiguredFeature::RandomNeighborSpread(RandomNeighborSpreadFeature {
                    block: self.block_state_provider(get("block"))?,
                    accepted_neighbors: block_ids(get("accepted_neighbors"))?,
                    can_replace: block_predicate(get("can_replace"))?,
                    attempts: int_provider(get("attempts"))?,
                    xz_offset: int_provider(get("xz_offset"))?,
                    y_offset: int_provider(get("y_offset"))?,
                })
            }
            "underwater_magma" => ConfiguredFeature::UnderwaterMagma(UnderwaterMagmaFeature {
                floor_search_range: i32_or(config, "floor_search_range", 0)?,
                placement_radius: i32_or(config, "placement_radius_around_floor", 0)?,
                placement_probability: f32_or(
                    config,
                    "placement_probability_per_valid_position",
                    0.0,
                )?,
            }),
            // Pumpkin's lake reads only its two providers; the predicates are not ported.
            "lake" => ConfiguredFeature::Lake(LakeFeature {
                fluid: self.block_state_provider(get("fluid"))?,
                barrier: self.block_state_provider(get("barrier"))?,
            }),
            "iceberg" => ConfiguredFeature::Iceberg(IcebergFeature {
                main_block: block_state_codec(get("state"))?,
            }),
            "end_podium" => {
                ConfiguredFeature::EndPodium(EndPodiumFeature::new(bool_or(config, "active", false)?))
            }
            "end_gateway" => ConfiguredFeature::EndGateway(EndGatewayFeature {
                exit: match get("exit") {
                    Value::Null => None,
                    v => Some(BlockPos(vec3(v)?)),
                },
                exact: bool_or(config, "exact", false)?,
            }),
            // Types without a configuration.
            "monster_room" => ConfiguredFeature::MonsterRoom(DungeonFeature),
            "freeze_top_layer" => ConfiguredFeature::FreezeTopLayer(FreezeTopLayerFeature),
            "blue_ice" => ConfiguredFeature::BlueIce(BlueIceFeature),
            "chorus_plant" => ConfiguredFeature::ChorusPlant(ChorusPlantFeature),
            "end_platform" => ConfiguredFeature::EndPlatform(EndPlatformFeature),
            "end_island" => ConfiguredFeature::EndIsland(EndIslandFeature),
            "void_start_platform" => ConfiguredFeature::VoidStartPlatform(VoidStartPlatformFeature),
            "bonus_chest" => ConfiguredFeature::BonusChest(BonusChestFeature),
            "vines" => ConfiguredFeature::Vines(VinesFeature),
            other => return Err(format!("unsupported feature type {other}")),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::mem::discriminant;
    use std::path::Path;

    use super::*;
    use crate::generation::feature::configured_features::CONFIGURED_FEATURES;
    use crate::generation::feature::placed_features::PLACED_FEATURES;

    const WORLDGEN: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/datapack/data/minecraft/worldgen"
    );

    /// Vanilla's own files, given as if a datapack overrode every one of them.
    pub fn vanilla_as_datapack() -> WorldgenJson {
        let mut json = WorldgenJson::vanilla();
        for registry in [
            WorldgenRegistry::ConfiguredFeature,
            WorldgenRegistry::PlacedFeature,
        ] {
            let base = Path::new(WORLDGEN).join(registry.folder());
            let mut dirs = vec![base.clone()];
            while let Some(dir) = dirs.pop() {
                for entry in fs::read_dir(&dir).unwrap().flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        dirs.push(path);
                        continue;
                    }
                    let relative = path.strip_prefix(&base).unwrap().with_extension("");
                    let id = format!("minecraft:{}", relative.to_string_lossy());
                    let value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
                    json.insert_from_datapack(registry, &id, value);
                }
            }
        }
        json
    }

    #[test]
    fn vanilla_json_rebuilds_the_compiled_features() {
        let (built, errors) = vanilla_as_datapack().build_datapack_features();

        // Every placed feature is read, modifier for modifier as the codegen reads it.
        assert_eq!(built.placed.len(), PLACED_FEATURES.len());
        for (key, feature) in &built.placed {
            let compiled = &PLACED_FEATURES[key];
            assert_eq!(feature.placement.len(), compiled.placement.len(), "{key:?}");
            for (a, b) in feature.placement.iter().zip(&compiled.placement) {
                assert_eq!(discriminant(a), discriminant(b), "{key:?}");
            }
            match (&feature.feature, &compiled.feature) {
                (Feature::Named(a), Feature::Named(b)) => assert_eq!(a, b, "{key:?}"),
                _ => panic!("{key:?}: a vanilla placed feature names its feature"),
            }
        }

        // Configured features: the supported types are read as the codegen reads them, the
        // others are reported, never misread.
        for (key, feature) in &built.configured {
            assert_eq!(
                discriminant(feature),
                discriminant(&CONFIGURED_FEATURES[key]),
                "{key:?}"
            );
        }
        // `random_block` is a provider Pumpkin lacks (the codegen places air for it).
        assert!(
            errors.iter().all(|e| e.contains("unsupported feature type")
                || e.ends_with("unsupported block state provider random_block")),
            "{errors:?}"
        );
        // What is still left compiled, by the type that stops it (its own or a nested one).
        let mut refused: Vec<&str> = errors
            .iter()
            .filter_map(|e| e.split_once("unsupported feature type ").map(|(_, ty)| ty))
            .collect();
        refused.sort_unstable();
        refused.dedup();
        assert_eq!(
            refused,
            [
                "coral_tree",
                "delta_feature",
                "end_spike",
                "fossil",
                "geode",
                "huge_brown_mushroom",
                "huge_red_mushroom",
                "large_dripstone",
                "sequence",
                "speleothem",
                "speleothem_cluster",
                "spike",
                "template",
            ],
            "{errors:?}"
        );
        assert!(built.configured.len() > 200, "{}", built.configured.len());

        // Ores, field for field.
        let (Some(ConfiguredFeature::Ore(ours)), Some(ConfiguredFeature::Ore(compiled))) = (
            built.configured.get(&ConfiguredFeatureKey::OreDiamondSmall),
            CONFIGURED_FEATURES.get(&ConfiguredFeatureKey::OreDiamondSmall),
        ) else {
            panic!("ore_diamond_small is an ore");
        };
        assert_eq!(ours.size, compiled.size);
        assert_eq!(
            ours.discard_chance_on_air_exposure.to_bits(),
            compiled.discard_chance_on_air_exposure.to_bits()
        );
        let states = |f: &OreFeature| f.targets.iter().map(|t| t.state.id).collect::<Vec<_>>();
        assert_eq!(states(ours), states(compiled));
    }

    #[test]
    fn vanilla_json_rebuilds_the_compiled_trees() {
        let (built, _) = vanilla_as_datapack().build_datapack_features();
        let mut trees = 0;
        for (key, feature) in &built.configured {
            let (ConfiguredFeature::Tree(ours), Some(ConfiguredFeature::Tree(compiled))) =
                (feature, CONFIGURED_FEATURES.get(key))
            else {
                continue;
            };
            trees += 1;
            let (a, b) = (&ours.trunk_placer, &compiled.trunk_placer);
            assert_eq!(
                (a.base_height, a.height_rand_a, a.height_rand_b),
                (b.base_height, b.height_rand_a, b.height_rand_b),
                "{key:?}"
            );
            assert_eq!(discriminant(&a.r#type), discriminant(&b.r#type), "{key:?}");
            assert_eq!(
                discriminant(&ours.foliage_placer.r#type),
                discriminant(&compiled.foliage_placer.r#type),
                "{key:?}"
            );
            assert_eq!(
                discriminant(&ours.minimum_size.r#type),
                discriminant(&compiled.minimum_size.r#type),
                "{key:?}"
            );
            assert_eq!(
                ours.minimum_size.min_clipped_height,
                compiled.minimum_size.min_clipped_height,
                "{key:?}"
            );
            assert_eq!(ours.ignore_vines, compiled.ignore_vines, "{key:?}");
            assert_eq!(
                ours.decorators.iter().map(discriminant).collect::<Vec<_>>(),
                compiled.decorators.iter().map(discriminant).collect::<Vec<_>>(),
                "{key:?}"
            );
            assert_eq!(
                ours.root_placer.is_some(),
                compiled.root_placer.is_some(),
                "{key:?}"
            );
            let state = |p: &BlockStateProvider| match p {
                BlockStateProvider::Simple(s) => Some(s.state.id),
                _ => None,
            };
            assert_eq!(
                state(&ours.trunk_provider),
                state(&compiled.trunk_provider),
                "{key:?}"
            );
            assert_eq!(
                state(&ours.foliage_provider),
                state(&compiled.foliage_provider),
                "{key:?}"
            );
        }
        assert_eq!(trees, 45);
    }

    #[test]
    fn an_override_reaches_a_datapack_feature_of_its_own() {
        let mut json = WorldgenJson::vanilla();
        json.insert_from_datapack(
            WorldgenRegistry::PlacedFeature,
            "minecraft:ore_diamond",
            serde_json::json!({
                "feature": "verse:gold_ore",
                "placement": [{"type": "minecraft:count", "count": 40}, {"type": "minecraft:in_square"}]
            }),
        );
        json.insert_from_datapack(
            WorldgenRegistry::ConfiguredFeature,
            "verse:gold_ore",
            serde_json::json!({
                "type": "minecraft:ore",
                "size": 9,
                "discard_chance_on_air_exposure": 0.0,
                "targets": [{
                    "target": {"predicate_type": "minecraft:tag_match", "tag": "minecraft:stone_ore_replaceables"},
                    "state": {"id": "minecraft:gold_block"}
                }]
            }),
        );
        // A feature type this reader does not handle keeps the compiled one.
        json.insert_from_datapack(
            WorldgenRegistry::ConfiguredFeature,
            "minecraft:amethyst_geode",
            serde_json::json!({"type": "minecraft:geode", "config": {}}),
        );
        let (built, errors) = json.build_datapack_features();

        assert_eq!(built.placed_names(), ["ore_diamond"]);
        assert!(built.configured.is_empty());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].starts_with("minecraft:amethyst_geode: unsupported feature type"));

        let placed = &built.placed[&PlacedFeatureKey::OreDiamond];
        assert_eq!(placed.placement.len(), 2);
        let Feature::Inlined(feature) = &placed.feature else {
            panic!("a datapack's own feature is built inline");
        };
        let ConfiguredFeature::Ore(ore) = feature.as_ref() else {
            panic!("an ore");
        };
        assert_eq!(ore.size, 9);
        assert_eq!(ore.targets[0].state.id, Block::GOLD_BLOCK.default_state.id);
    }
}
