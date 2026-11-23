//! A datapack's own structures and structure sets, built from its `worldgen/structure`
//! and `worldgen/structure_set` files at startup.
//!
//! A structure of the pack's own is built when its `type` is a registered one (see
//! [`super::extension`]); it gets an extension key in `pumpkin_data::structure_extension`.
//! A structure set of the pack's own may name those and vanilla's structures; random
//! spread placements are supported (every vanilla field but `locate_offset`).
//!
//! Not supported yet, refused with a message (the file is skipped): a pack structure of
//! a vanilla type (jigsaw and the rest are compiled), a `biomes` list rather than a tag,
//! concentric ring placements of a pack's own, and files that override vanilla's
//! structures or sets.

use std::collections::HashMap;

use pumpkin_data::structure_extension;
use pumpkin_data::structures::{
    ExclusionZone, FrequencyReductionMethod, GenerationStep, RandomSpreadStructurePlacement,
    SpreadType, Structure, StructureKeys, StructurePlacement, StructurePlacementType,
    StructureSet, StructureType, TerrainAdaptation, WeightedEntry,
};
use serde_json::{Map, Value};

use super::extension::{self, ExtensionStructure};
use crate::generation::noise::router::datapack::{WorldgenJson, WorldgenRegistry, full_id};

/// The structures and structure sets built from the enabled datapacks.
#[derive(Default)]
pub struct DatapackStructures {
    structures: Vec<(&'static str, &'static Structure, Box<dyn ExtensionStructure>)>,
    sets: Vec<(&'static str, &'static StructureSet)>,
}

impl DatapackStructures {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.structures.is_empty() && self.sets.is_empty()
    }

    #[must_use]
    pub fn structure_names(&self) -> Vec<&'static str> {
        self.structures.iter().map(|(name, _, _)| *name).collect()
    }

    #[must_use]
    pub fn set_names(&self) -> Vec<&'static str> {
        self.sets.iter().map(|(name, _)| *name).collect()
    }
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_string().into_boxed_str())
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{what} is not an object"))
}

fn int(obj: &Map<String, Value>, field: &str) -> Result<i64, String> {
    obj.get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("missing or non-integer `{field}`"))
}

fn step(name: &str) -> Result<GenerationStep, String> {
    Ok(match name {
        "raw_generation" => GenerationStep::RawGeneration,
        "lakes" => GenerationStep::Lakes,
        "local_modifications" => GenerationStep::LocalModifications,
        "underground_structures" => GenerationStep::UndergroundStructures,
        "surface_structures" => GenerationStep::SurfaceStructures,
        "strongholds" => GenerationStep::Strongholds,
        "underground_ores" => GenerationStep::UndergroundOres,
        "underground_decoration" => GenerationStep::UndergroundDecoration,
        "fluid_springs" => GenerationStep::FluidSprings,
        "vegetal_decoration" => GenerationStep::VegetalDecoration,
        "top_layer_modification" => GenerationStep::TopLayerModification,
        other => return Err(format!("unknown step {other}")),
    })
}

fn terrain_adaptation(name: Option<&str>) -> Result<TerrainAdaptation, String> {
    Ok(match name.unwrap_or("none") {
        "none" => TerrainAdaptation::None,
        "beard_thin" => TerrainAdaptation::BeardThin,
        "beard_box" => TerrainAdaptation::BeardBox,
        "bury" => TerrainAdaptation::Bury,
        "encapsulate" => TerrainAdaptation::Encapsulate,
        other => return Err(format!("unknown terrain adaptation {other}")),
    })
}

/// The settings every structure shares (vanilla `Structure.StructureSettings`), for a
/// structure of a registered type.
fn structure_settings(obj: &Map<String, Value>) -> Result<Structure, String> {
    let biomes = match obj.get("biomes") {
        Some(Value::String(tag)) if tag.starts_with('#') => leak(tag),
        Some(_) => return Err("`biomes` other than a tag are not supported".to_string()),
        None => return Err("missing `biomes`".to_string()),
    };
    let step = step(
        obj.get("step")
            .and_then(Value::as_str)
            .ok_or("missing `step`")?,
    )?;
    let terrain_adaptation =
        terrain_adaptation(obj.get("terrain_adaptation").and_then(Value::as_str))?;
    Ok(Structure {
        biomes,
        step,
        start_pool: None,
        start_jigsaw_name: None,
        size: None,
        terrain_adaptation,
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
    })
}

fn placement(value: &Value) -> Result<StructurePlacement, String> {
    let obj = object(value, "placement")?;
    let kind = full_id(
        obj.get("type")
            .and_then(Value::as_str)
            .ok_or("placement without a type")?,
    );
    let placement_type = match kind.as_str() {
        "minecraft:random_spread" => {
            let spacing = int(obj, "spacing")? as i32;
            let separation = int(obj, "separation")? as i32;
            if separation >= spacing {
                return Err("spacing has to be larger than separation".to_string());
            }
            let spread_type = match obj.get("spread_type").and_then(Value::as_str) {
                None | Some("linear") => None,
                Some("triangular") => Some(SpreadType::Triangular),
                Some(other) => return Err(format!("unknown spread type {other}")),
            };
            StructurePlacementType::RandomSpread(RandomSpreadStructurePlacement {
                spacing,
                separation,
                spread_type,
            })
        }
        other => return Err(format!("placement {other} is not supported for a pack's set")),
    };
    if obj
        .get("locate_offset")
        .and_then(Value::as_array)
        .is_some_and(|offset| offset.iter().any(|v| v.as_i64() != Some(0)))
    {
        return Err("`locate_offset` is not supported".to_string());
    }
    let frequency_reduction_method =
        match obj.get("frequency_reduction_method").and_then(Value::as_str) {
            None => None,
            Some("default") => Some(FrequencyReductionMethod::Default),
            Some("legacy_type_1") => Some(FrequencyReductionMethod::LegacyType1),
            Some("legacy_type_2") => Some(FrequencyReductionMethod::LegacyType2),
            Some("legacy_type_3") => Some(FrequencyReductionMethod::LegacyType3),
            Some(other) => return Err(format!("unknown frequency reduction method {other}")),
        };
    let frequency = obj
        .get("frequency")
        .and_then(Value::as_f64)
        .map(|f| f as f32);
    let exclusion_zone = match obj.get("exclusion_zone") {
        None => None,
        Some(zone) => {
            let zone = object(zone, "exclusion_zone")?;
            Some(ExclusionZone {
                other_set: leak(
                    zone.get("other_set")
                        .and_then(Value::as_str)
                        .ok_or("exclusion zone without `other_set`")?,
                ),
                chunk_count: int(zone, "chunk_count")? as i32,
            })
        }
    };
    Ok(StructurePlacement {
        frequency_reduction_method,
        frequency,
        salt: int(obj, "salt")? as u32,
        exclusion_zone,
        placement_type,
    })
}

impl WorldgenJson {
    /// Builds the datapacks' own structures (of registered types) and structure sets.
    /// Returns them, and one message per file that was skipped.
    #[must_use]
    pub fn build_datapack_structures(&self) -> (DatapackStructures, Vec<String>) {
        let mut built = DatapackStructures::default();
        let mut errors = Vec::new();
        let mut own_keys: HashMap<String, StructureKeys> = HashMap::new();

        for id in self.datapack_ids(WorldgenRegistry::Structure) {
            if StructureKeys::from_name(&id).is_some() {
                errors.push(format!(
                    "{id}: overriding a vanilla structure is not supported (kept compiled)"
                ));
                continue;
            }
            let Some((value, _)) = self.get(WorldgenRegistry::Structure, &id) else {
                continue;
            };
            let result = object(value, "structure").and_then(|obj| {
                let kind = full_id(obj.get("type").and_then(Value::as_str).ok_or("no type")?);
                if !extension::has_structure_type(&kind) {
                    return Err(format!("structure type {kind} is not supported"));
                }
                let settings = structure_settings(obj)?;
                let structure = extension::read_structure(&kind, obj)?;
                Ok((settings, structure))
            });
            match result {
                Ok((settings, structure)) => {
                    let index = built.structures.len();
                    let Ok(key) = u16::try_from(index) else {
                        errors.push(format!("{id}: too many structures"));
                        continue;
                    };
                    let name = leak(&id);
                    own_keys.insert(id, StructureKeys::Extension(key));
                    built
                        .structures
                        .push((name, Box::leak(Box::new(settings)), structure));
                }
                Err(error) => errors.push(format!("{id}: {error}")),
            }
        }

        for id in self.datapack_ids(WorldgenRegistry::StructureSet) {
            if StructureSet::get(id.strip_prefix("minecraft:").unwrap_or(&id)).is_some() {
                errors.push(format!(
                    "{id}: overriding a vanilla structure set is not supported (kept compiled)"
                ));
                continue;
            }
            let Some((value, _)) = self.get(WorldgenRegistry::StructureSet, &id) else {
                continue;
            };
            let result = object(value, "structure set").and_then(|obj| {
                let mut entries = Vec::new();
                for entry in obj
                    .get("structures")
                    .and_then(Value::as_array)
                    .ok_or("no `structures`")?
                {
                    let entry = object(entry, "structure entry")?;
                    let name = full_id(
                        entry
                            .get("structure")
                            .and_then(Value::as_str)
                            .ok_or("entry without a structure")?,
                    );
                    let key = own_keys
                        .get(&name)
                        .copied()
                        .or_else(|| {
                            name.starts_with("minecraft:")
                                .then(|| StructureKeys::from_name(&name))
                                .flatten()
                        })
                        .ok_or_else(|| format!("structure {name} was not built"))?;
                    entries.push(WeightedEntry {
                        structure: key,
                        weight: int(entry, "weight")? as u32,
                    });
                }
                let placement = placement(obj.get("placement").ok_or("no `placement`")?)?;
                Ok(StructureSet {
                    placement,
                    structures: entries.leak(),
                })
            });
            match result {
                Ok(set) => built.sets.push((leak(&id), Box::leak(Box::new(set)))),
                Err(error) => errors.push(format!("{id}: {error}")),
            }
        }
        (built, errors)
    }
}

/// Installs the structures and structure sets built from datapacks.
///
/// Once per process, before the worlds are created: worldgen registries are fixed for a
/// server's lifetime.
///
/// # Errors
/// When structures were installed already.
pub fn install(built: DatapackStructures) -> Result<(), String> {
    let DatapackStructures { structures, sets } = built;
    let mut keyed = Vec::with_capacity(structures.len());
    let mut types = Vec::with_capacity(structures.len());
    for (name, settings, structure) in structures {
        keyed.push((name, settings));
        types.push(structure);
    }
    structure_extension::install(keyed, sets).map_err(|e| e.to_string())?;
    if !extension::install(types) {
        return Err("structures of registered types were installed already".to_string());
    }
    Ok(())
}
