//! Loads entity types that are not in the generated vanilla set.
//!
//! Fed by the same Extractor dump as [`super::block_extension`]: every
//! non-vanilla entry of its `entities.json` (names keep their namespace) is
//! built into an `EntityType` the way `pumpkin-codegen` builds vanilla ones and
//! installed into `pumpkin_data::entity_type_extension`.
//!
//! Not yet handled, and skipped with a warning: attributes and hurt or death
//! sounds — what a living entity has. Mapping them needs the attribute and
//! sound tables by name.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use pumpkin_data::entity::{EntityType, MobCategory, SpawnLocation, SpawnRestriction};
use pumpkin_data::entity_type_extension;
use pumpkin_util::HeightMap;
use serde::Deserialize;
use serde_json::Value;
use tracing::{info, warn};

#[derive(Deserialize)]
struct DumpEntity {
    id: u16,
    #[serde(default)]
    attributes: Option<Vec<Value>>,
    #[serde(default)]
    experience_reward: Option<u32>,
    #[serde(default)]
    hurt_sound: Option<String>,
    #[serde(default)]
    death_sound: Option<String>,
    #[serde(default)]
    attackable: Option<bool>,
    #[serde(default)]
    mob: Option<bool>,
    #[serde(default)]
    is_enemy: bool,
    #[serde(default)]
    limit_per_chunk: Option<i32>,
    summonable: bool,
    fire_immune: bool,
    saveable: bool,
    category: String,
    can_spawn_far_from_player: bool,
    client_tracking_range: u32,
    update_interval: u32,
    track_deltas: bool,
    dimension: [f32; 2],
    eye_height: f32,
    spawn_dimensions_scale: f32,
    spawn_restriction: DumpSpawnRestriction,
}

#[derive(Deserialize)]
struct DumpSpawnRestriction {
    location: String,
    heightmap: String,
}

/// Reads `entities.json` from an Extractor dump and installs its non-vanilla
/// entity types.
pub fn install_from_dump(dir: &Path) {
    let path = dir.join("entities.json");
    let dump: BTreeMap<String, DumpEntity> = match fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(dump) => dump,
        Err(error) => {
            warn!("Could not load {}: {error}", path.display());
            return;
        }
    };

    let mut entries: Vec<&'static EntityType> = dump
        .into_iter()
        // The Extractor writes vanilla names bare and keeps every other namespace.
        .filter(|(name, _)| name.contains(':'))
        .filter_map(|(name, entity)| match build(&name, &entity) {
            Ok(entity_type) => Some(&*Box::leak(Box::new(entity_type))),
            Err(error) => {
                warn!("Entity type extension skipped {name}: {error}");
                None
            }
        })
        .collect();
    if entries.is_empty() {
        return;
    }
    entries.sort_unstable_by_key(|entity_type| entity_type.id);

    let count = entries.len();
    match entity_type_extension::install(entries) {
        Ok(()) => info!("Installed {count} extension entity type(s)"),
        Err(error) => warn!("Could not install the entity type extension: {error}"),
    }
}

fn build(name: &str, entity: &DumpEntity) -> Result<EntityType, String> {
    if entity.attributes.as_ref().is_some_and(|a| !a.is_empty()) {
        return Err("attributes are not supported yet".into());
    }
    if entity.hurt_sound.is_some() || entity.death_sound.is_some() {
        return Err("entity sounds are not supported yet".into());
    }
    Ok(EntityType {
        id: entity.id,
        attributes: &[],
        experience_reward: entity.experience_reward.unwrap_or(0),
        hurt_sound: None,
        death_sound: None,
        attackable: entity.attackable,
        mob: entity.mob.unwrap_or(false),
        is_enemy: entity.is_enemy,
        saveable: entity.saveable,
        limit_per_chunk: entity.limit_per_chunk.unwrap_or(0),
        summonable: entity.summonable,
        fire_immune: entity.fire_immune,
        category: category(&entity.category)?,
        can_spawn_far_from_player: entity.can_spawn_far_from_player,
        client_tracking_range: entity.client_tracking_range,
        update_interval: entity.update_interval,
        track_deltas: entity.track_deltas,
        dimension: entity.dimension,
        eye_height: entity.eye_height,
        spawn_dimensions_scale: entity.spawn_dimensions_scale,
        spawn_restriction: SpawnRestriction {
            location: location(&entity.spawn_restriction.location)?,
            heightmap: heightmap(&entity.spawn_restriction.heightmap)?,
        },
        resource_name: Box::leak(name.to_owned().into_boxed_str()),
    })
}

fn category(name: &str) -> Result<&'static MobCategory, String> {
    Ok(match name {
        "MONSTER" => &MobCategory::MONSTER,
        "CREATURE" => &MobCategory::CREATURE,
        "AMBIENT" => &MobCategory::AMBIENT,
        "AXOLOTLS" => &MobCategory::AXOLOTLS,
        "UNDERGROUND_WATER_CREATURE" => &MobCategory::UNDERGROUND_WATER_CREATURE,
        "WATER_CREATURE" => &MobCategory::WATER_CREATURE,
        "WATER_AMBIENT" => &MobCategory::WATER_AMBIENT,
        "MISC" => &MobCategory::MISC,
        other => return Err(format!("unknown mob category {other}")),
    })
}

fn location(name: &str) -> Result<SpawnLocation, String> {
    Ok(match name {
        "IN_LAVA" => SpawnLocation::InLava,
        "IN_WATER" => SpawnLocation::InWater,
        "ON_GROUND" => SpawnLocation::OnGround,
        "UNRESTRICTED" => SpawnLocation::Unrestricted,
        other => return Err(format!("unknown spawn location {other}")),
    })
}

fn heightmap(name: &str) -> Result<HeightMap, String> {
    Ok(match name {
        "WORLD_SURFACE_WG" => HeightMap::WorldSurfaceWg,
        "WORLD_SURFACE" => HeightMap::WorldSurface,
        "OCEAN_FLOOR_WG" => HeightMap::OceanFloorWg,
        "OCEAN_FLOOR" => HeightMap::OceanFloor,
        "MOTION_BLOCKING" => HeightMap::MotionBlocking,
        "MOTION_BLOCKING_NO_LEAVES" => HeightMap::MotionBlockingNoLeaves,
        other => return Err(format!("unknown heightmap {other}")),
    })
}
