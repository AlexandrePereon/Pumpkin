//! `minecraft:tree` and `minecraft:fallen_tree` from datapack JSON: trunk, foliage and root
//! placers, decorators and feature sizes, read as `tools/pumpkin-codegen/src/configured_feature.rs`
//! reads vanilla's files.

use pumpkin_data::BlockDirection;
use pumpkin_data::tag::RegistryKey;
use pumpkin_util::math::int_provider::UniformIntProvider;
use serde_json::{Map, Value};

use super::{
    FeatureParser, Result, array, block, direction, f32_or, full_id, i32_or, int_provider, object,
    str_of,
};
use crate::generation::feature::features::fallen_tree::FallenTreeFeature;
use crate::generation::feature::features::tree::{TreeFeature, extension};
use crate::generation::feature::features::tree::decorator::{
    TreeDecorator, alter_ground::AlterGroundTreeDecorator,
    attached_to_leaves::AttachedToLeavesTreeDecorator,
    attached_to_logs::AttachedToLogsTreeDecorator, beehive::BeehiveTreeDecorator,
    cocoa::CocoaTreeDecorator, creaking_heart::CreakingHeartTreeDecorator,
    leave_vine::LeavesVineTreeDecorator, pale_moss::PaleMossTreeDecorator,
    place_on_ground::PlaceOnGroundTreeDecorator, trunk_vine::TrunkVineTreeDecorator,
};
use crate::generation::feature::features::tree::foliage::{
    FoliagePlacer, FoliageType, acacia::AcaciaFoliagePlacer, blob::BlobFoliagePlacer,
    bush::BushFoliagePlacer, cherry::CherryFoliagePlacer, dark_oak::DarkOakFoliagePlacer,
    fancy::LargeOakFoliagePlacer, jungle::JungleFoliagePlacer, mega_pine::MegaPineFoliagePlacer,
    pine::PineFoliagePlacer, poplar::PoplarFoliagePlacer, random_spread::RandomSpreadFoliagePlacer,
    spruce::SpruceFoliagePlacer,
};
use crate::generation::feature::features::tree::root::RootPlacer;
use crate::generation::feature::features::tree::root::mangrove::{
    AboveRootPlacement, MangroveRootPlacement, MangroveRootPlacer,
};
use crate::generation::feature::features::tree::trunk::{
    TrunkPlacer, TrunkType, bending::BendingTrunkPlacer, cherry::CherryTrunkPlacer,
    dark_oak::DarkOakTrunkPlacer, fancy::FancyTrunkPlacer, forking::ForkingTrunkPlacer,
    giant::GiantTrunkPlacer, mega_jungle::MegaJungleTrunkPlacer, poplar::PoplarTrunkPlacer,
    straight::StraightTrunkPlacer, upwards_branching::UpwardsBranchingTrunkPlacer,
};
use crate::generation::feature::size::{
    FeatureSize, FeatureSizeType, ThreeLayersFeatureSize, TwoLayersFeatureSize,
};

fn get<'v>(obj: &'v Map<String, Value>, key: &str) -> &'v Value {
    obj.get(key).unwrap_or(&Value::Null)
}

/// A small unsigned field (vanilla's `Codec.intRange` bounds them well below 256).
fn u8_or(obj: &Map<String, Value>, key: &str, default: Option<u8>) -> Result<u8> {
    obj.get(key).map_or_else(
        || default.ok_or_else(|| format!("missing \"{key}\"")),
        |v| {
            v.as_u64()
                .and_then(|n| u8::try_from(n).ok())
                .ok_or_else(|| format!("\"{key}\" out of range: {v}"))
        },
    )
}

/// A block list: `#tag` or block ids. Lists are leaked: the features they belong to are built
/// once per process and kept for its lifetime.
fn block_list(value: &Value) -> Result<&'static [u16]> {
    match value {
        Value::String(name) => match name.strip_prefix('#') {
            Some(tag) => pumpkin_data::tag::get_tag(RegistryKey::Block, &full_id(tag))
                .map(|tag| tag.1)
                .ok_or_else(|| format!("unknown block tag {tag}")),
            None => Ok(Vec::leak(vec![block(name)?.id.as_u16()])),
        },
        Value::Array(names) => {
            let ids = names
                .iter()
                .map(|n| {
                    n.as_str()
                        .ok_or_else(|| format!("bad block id {n}"))
                        .and_then(block)
                        .map(|b| b.id.as_u16())
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Vec::leak(ids))
        }
        other => Err(format!("bad block list {other}")),
    }
}

fn directions(obj: &Map<String, Value>) -> Result<Vec<BlockDirection>> {
    array(obj, "directions")?
        .iter()
        .map(|d| {
            d.as_str()
                .ok_or_else(|| format!("bad direction {d}"))
                .and_then(direction)
        })
        .collect()
}

fn type_of(obj: &Map<String, Value>) -> Result<&str> {
    let ty = str_of(obj, "type")?;
    Ok(ty.strip_prefix("minecraft:").unwrap_or(ty))
}

fn trunk_placer(value: &Value) -> Result<TrunkPlacer> {
    let obj = object(value)?;
    let r#type = match type_of(obj)? {
        "straight_trunk_placer" => TrunkType::Straight(StraightTrunkPlacer),
        "forking_trunk_placer" => TrunkType::Forking(ForkingTrunkPlacer),
        "giant_trunk_placer" => TrunkType::Giant(GiantTrunkPlacer),
        "mega_jungle_trunk_placer" => TrunkType::MegaJungle(MegaJungleTrunkPlacer),
        "dark_oak_trunk_placer" => TrunkType::DarkOak(DarkOakTrunkPlacer),
        "fancy_trunk_placer" => TrunkType::Fancy(FancyTrunkPlacer),
        "bending_trunk_placer" => TrunkType::Bending(BendingTrunkPlacer {
            min_height_for_leaves: u32::from(u8_or(obj, "min_height_for_leaves", Some(1))?),
            bend_length: int_provider(get(obj, "bend_length"))?,
        }),
        "upwards_branching_trunk_placer" => {
            TrunkType::UpwardsBranching(UpwardsBranchingTrunkPlacer {
                extra_branch_steps: int_provider(get(obj, "extra_branch_steps"))?,
                place_branch_per_log_probability: f32_or(
                    obj,
                    "place_branch_per_log_probability",
                    0.0,
                )?,
                extra_branch_length: int_provider(get(obj, "extra_branch_length"))?,
                can_grow_through: block_list(get(obj, "can_grow_through"))?,
            })
        }
        "cherry_trunk_placer" => {
            let start = object(get(obj, "branch_start_offset_from_top"))?;
            TrunkType::Cherry(CherryTrunkPlacer {
                count: int_provider(get(obj, "branch_count"))?,
                horizontal_length: int_provider(get(obj, "branch_horizontal_length"))?,
                start_offset_from_top: UniformIntProvider {
                    min_inclusive: i32_or(start, "min_inclusive", 0)?,
                    max_inclusive: i32_or(start, "max_inclusive", 0)?,
                },
                end_offset_from_top: int_provider(get(obj, "branch_end_offset_from_top"))?,
            })
        }
        "poplar_trunk_placer" => TrunkType::Poplar(PoplarTrunkPlacer {
            trunk_height_above_branches: int_provider(get(obj, "trunk_height_above_branches"))?,
            branch_amount: int_provider(get(obj, "branch_amount"))?,
        }),
        other => TrunkType::Extension(extension::read_trunk_placer(&full_id(other), obj)?),
    };
    Ok(TrunkPlacer {
        base_height: u8_or(obj, "base_height", None)?,
        height_rand_a: u8_or(obj, "height_rand_a", None)?,
        height_rand_b: u8_or(obj, "height_rand_b", None)?,
        r#type,
    })
}

fn foliage_placer(value: &Value) -> Result<FoliagePlacer> {
    let obj = object(value)?;
    let height = |default| i32_or(obj, "height", default);
    let r#type = match type_of(obj)? {
        "blob_foliage_placer" => FoliageType::Blob(BlobFoliagePlacer { height: height(3)? }),
        "spruce_foliage_placer" => FoliageType::Spruce(SpruceFoliagePlacer {
            trunk_height: int_provider(get(obj, "trunk_height"))?,
        }),
        "pine_foliage_placer" => FoliageType::Pine(PineFoliagePlacer {
            height: int_provider(get(obj, "height"))?,
        }),
        "acacia_foliage_placer" => FoliageType::Acacia(AcaciaFoliagePlacer),
        "bush_foliage_placer" => FoliageType::Bush(BushFoliagePlacer { height: height(2)? }),
        "fancy_foliage_placer" => FoliageType::Fancy(LargeOakFoliagePlacer { height: height(4)? }),
        "jungle_foliage_placer" => FoliageType::Jungle(JungleFoliagePlacer { height: height(2)? }),
        "mega_pine_foliage_placer" => FoliageType::MegaPine(MegaPineFoliagePlacer {
            crown_height: int_provider(get(obj, "crown_height"))?,
        }),
        "dark_oak_foliage_placer" => FoliageType::DarkOak(DarkOakFoliagePlacer),
        "random_spread_foliage_placer" => FoliageType::RandomSpread(RandomSpreadFoliagePlacer {
            foliage_height: int_provider(get(obj, "foliage_height"))?,
            leaf_placement_attempts: i32_or(obj, "leaf_placement_attempts", 128)?,
        }),
        "cherry_foliage_placer" => FoliageType::Cherry(CherryFoliagePlacer {
            height: int_provider(get(obj, "height"))?,
            wide_bottom_layer_hole_chance: f32_or(obj, "wide_bottom_layer_hole_chance", 0.0)?,
            corner_hole_chance: f32_or(obj, "corner_hole_chance", 0.0)?,
            hanging_leaves_chance: f32_or(obj, "hanging_leaves_chance", 0.0)?,
            hanging_leaves_extension_chance: f32_or(obj, "hanging_leaves_extension_chance", 0.0)?,
        }),
        "poplar_foliage_placer" => FoliageType::Poplar(PoplarFoliagePlacer {
            height: int_provider(get(obj, "height"))?,
            side_hole_chance: f32_or(obj, "side_hole_chance", 0.0)?,
        }),
        other => FoliageType::Extension(extension::read_foliage_placer(&full_id(other), obj)?),
    };
    Ok(FoliagePlacer {
        radius: int_provider(get(obj, "radius"))?,
        offset: int_provider(get(obj, "offset"))?,
        r#type,
    })
}

fn feature_size(value: &Value) -> Result<FeatureSize> {
    let obj = object(value)?;
    let min_clipped_height = match obj.get("min_clipped_height") {
        None | Some(Value::Null) => None,
        Some(_) => Some(u8_or(obj, "min_clipped_height", None)?),
    };
    let r#type = match type_of(obj)? {
        "two_layers_feature_size" => FeatureSizeType::TwoLayersFeatureSize(TwoLayersFeatureSize {
            limit: u8_or(obj, "limit", Some(1))?,
            lower_size: u8_or(obj, "lower_size", Some(0))?,
            upper_size: u8_or(obj, "upper_size", Some(1))?,
        }),
        "three_layers_feature_size" => {
            FeatureSizeType::ThreeLayersFeatureSize(ThreeLayersFeatureSize {
                limit: u8_or(obj, "limit", Some(1))?,
                upper_limit: u8_or(obj, "upper_limit", Some(1))?,
                lower_size: u8_or(obj, "lower_size", Some(0))?,
                middle_size: u8_or(obj, "middle_size", Some(1))?,
                upper_size: u8_or(obj, "upper_size", Some(1))?,
            })
        }
        other => return Err(format!("unsupported feature size {other}")),
    };
    Ok(FeatureSize {
        min_clipped_height,
        r#type,
    })
}

impl FeatureParser<'_> {
    pub(super) fn tree(&mut self, config: &Map<String, Value>) -> Result<TreeFeature> {
        Ok(TreeFeature {
            trunk_provider: self.block_state_provider(get(config, "trunk_provider"))?,
            trunk_placer: trunk_placer(get(config, "trunk_placer"))?,
            foliage_provider: self.block_state_provider(get(config, "foliage_provider"))?,
            foliage_placer: foliage_placer(get(config, "foliage_placer"))?,
            minimum_size: feature_size(get(config, "minimum_size"))?,
            // Required: vanilla's files write it either way.
            ignore_vines: config
                .get("ignore_vines")
                .and_then(Value::as_bool)
                .ok_or("missing boolean \"ignore_vines\"")?,
            decorators: self.tree_decorators(config, "decorators")?,
            below_trunk_provider: self.block_state_provider(get(config, "below_trunk_provider"))?,
            root_placer: match config.get("root_placer") {
                None | Some(Value::Null) => None,
                Some(v) => Some(self.root_placer(v)?),
            },
        })
    }

    pub(super) fn fallen_tree(&mut self, config: &Map<String, Value>) -> Result<FallenTreeFeature> {
        Ok(FallenTreeFeature {
            trunk_provider: self.block_state_provider(get(config, "trunk_provider"))?,
            log_length: int_provider(get(config, "log_length"))?,
            stump_decorators: self.tree_decorators(config, "stump_decorators")?,
            log_decorators: self.tree_decorators(config, "log_decorators")?,
        })
    }

    fn root_placer(&mut self, value: &Value) -> Result<RootPlacer> {
        let obj = object(value)?;
        match type_of(obj)? {
            "mangrove_root_placer" => {
                let placement = object(get(obj, "mangrove_root_placement"))?;
                Ok(RootPlacer::Mangrove(MangroveRootPlacer {
                    trunk_offset_y: int_provider(get(obj, "trunk_offset_y"))?,
                    root_provider: self.block_state_provider(get(obj, "root_provider"))?,
                    above_root_placement: match obj.get("above_root_placement") {
                        None | Some(Value::Null) => None,
                        Some(v) => {
                            let above = object(v)?;
                            Some(AboveRootPlacement {
                                above_root_provider: self
                                    .block_state_provider(get(above, "above_root_provider"))?,
                                above_root_placement_chance: f32_or(
                                    above,
                                    "above_root_placement_chance",
                                    0.0,
                                )?,
                            })
                        }
                    },
                    mangrove_root_placement: MangroveRootPlacement {
                        can_grow_through: block_list(get(placement, "can_grow_through"))?,
                        muddy_roots_in: block_list(get(placement, "muddy_roots_in"))?,
                        muddy_roots_provider: self
                            .block_state_provider(get(placement, "muddy_roots_provider"))?,
                        max_root_width: i32_or(placement, "max_root_width", 8)?,
                        max_root_length: i32_or(placement, "max_root_length", 15)?,
                        random_skew_chance: f32_or(placement, "random_skew_chance", 0.0)?,
                    },
                }))
            }
            other => Err(format!("unsupported root placer {other}")),
        }
    }

    fn tree_decorators(
        &mut self,
        config: &Map<String, Value>,
        key: &str,
    ) -> Result<Vec<TreeDecorator>> {
        let mut decorators = Vec::new();
        for value in array(config, key)? {
            if let Some(decorator) = self.tree_decorator(value)? {
                decorators.push(decorator);
            }
        }
        Ok(decorators)
    }

    /// One decorator; `None` for `shelf_mushroom`, which Pumpkin's trees do not place (the
    /// compiled ones drop it too).
    fn tree_decorator(&mut self, value: &Value) -> Result<Option<TreeDecorator>> {
        let obj = object(value)?;
        let probability = || f32_or(obj, "probability", 0.0);
        Ok(Some(match type_of(obj)? {
            "trunk_vine" => TreeDecorator::TrunkVine(TrunkVineTreeDecorator),
            "leave_vine" => TreeDecorator::LeaveVine(LeavesVineTreeDecorator {
                probability: probability()?,
            }),
            "cocoa" => TreeDecorator::Cocoa(CocoaTreeDecorator {
                probability: probability()?,
            }),
            "beehive" => TreeDecorator::Beehive(BeehiveTreeDecorator {
                probability: probability()?,
            }),
            "alter_ground" => TreeDecorator::AlterGround(AlterGroundTreeDecorator {
                provider: self.block_state_provider(get(obj, "provider"))?,
            }),
            "attached_to_logs" => TreeDecorator::AttachedToLogs(AttachedToLogsTreeDecorator {
                probability: probability()?,
                block_provider: self.block_state_provider(get(obj, "block_provider"))?,
                directions: directions(obj)?,
            }),
            "attached_to_leaves" => {
                TreeDecorator::AttachedToLeaves(AttachedToLeavesTreeDecorator {
                    probability: probability()?,
                    exclusion_radius_xz: i32_or(obj, "exclusion_radius_xz", 0)?,
                    exclusion_radius_y: i32_or(obj, "exclusion_radius_y", 0)?,
                    block_provider: self.block_state_provider(get(obj, "block_provider"))?,
                    required_empty_blocks: i32_or(obj, "required_empty_blocks", 1)?,
                    directions: directions(obj)?,
                })
            }
            "place_on_ground" => TreeDecorator::PlaceOnGround(PlaceOnGroundTreeDecorator {
                tries: i32_or(obj, "tries", 128)?,
                radius: i32_or(obj, "radius", 2)?,
                height: i32_or(obj, "height", 1)?,
                block_state_provider: self
                    .block_state_provider(get(obj, "block_state_provider"))?,
            }),
            "creaking_heart" => TreeDecorator::CreakingHeart(CreakingHeartTreeDecorator {
                probability: probability()?,
            }),
            "pale_moss" => TreeDecorator::PaleMoss(PaleMossTreeDecorator {
                leaves_probability: f32_or(obj, "leaves_probability", 0.0)?,
                trunk_probability: f32_or(obj, "trunk_probability", 0.0)?,
                ground_probability: f32_or(obj, "ground_probability", 0.0)?,
            }),
            "shelf_mushroom" => return Ok(None),
            other => TreeDecorator::Extension(extension::read_tree_decorator(&full_id(other), obj)?),
        }))
    }
}
