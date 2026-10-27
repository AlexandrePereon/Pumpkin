//! Loads entity types that are not in the generated vanilla set.
//!
//! Fed by the same Extractor dump as [`super::block_extension`]: every
//! non-vanilla entry of its `entities.json` (names keep their namespace) is
//! built into an `EntityType` the way `pumpkin-codegen` builds vanilla ones and
//! installed into `pumpkin_data::entity_type_extension`.
//!
//! A living entity's attributes and hurt and death sounds are mapped by name
//! onto Pumpkin's vanilla tables, as codegen does. A hurt or death sound of a
//! mod's own has no `Sound` variant: it is kept apart, by raw id, in a side
//! table `LivingEntity` consults ([`hurt_sound_id`], [`death_sound_id`]). An
//! attribute of a mod's own resolves through `attributes_extension`, which
//! [`super::attribute_extension`] fills first.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use pumpkin_data::entity::{EntityType, MobCategory, SpawnLocation, SpawnRestriction};
use pumpkin_data::attributes::Attributes;
use pumpkin_data::entity_type_extension;
use pumpkin_data::sound::Sound;
use pumpkin_util::HeightMap;
use serde::Deserialize;
use tracing::{info, warn};

use super::name_list_extension::SOUND_EVENTS;

/// A hurt and a death sound outside `Sound`, by raw id.
#[derive(Clone, Copy, Default)]
struct ModSounds {
    hurt: Option<u16>,
    death: Option<u16>,
}

/// The mod sounds of extension entity types, by entity type id; only types
/// that have one are listed.
static MOD_SOUNDS: OnceLock<BTreeMap<u16, ModSounds>> = OnceLock::new();

/// The raw id of `entity_type`'s hurt sound if it is a mod's own.
#[must_use]
pub fn hurt_sound_id(entity_type: &EntityType) -> Option<u16> {
    MOD_SOUNDS.get()?.get(&entity_type.id)?.hurt
}

/// The raw id of `entity_type`'s death sound if it is a mod's own.
#[must_use]
pub fn death_sound_id(entity_type: &EntityType) -> Option<u16> {
    MOD_SOUNDS.get()?.get(&entity_type.id)?.death
}

#[derive(Deserialize)]
struct DumpEntity {
    id: u16,
    #[serde(default)]
    attributes: Option<Vec<BTreeMap<String, f64>>>,
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

    let mut mod_sounds = BTreeMap::new();
    let mut entries: Vec<&'static EntityType> = dump
        .into_iter()
        // The Extractor writes vanilla names bare and keeps every other namespace.
        .filter(|(name, _)| name.contains(':'))
        .filter_map(|(name, entity)| match build(&name, &entity) {
            Ok((entity_type, sounds)) => {
                if sounds.hurt.is_some() || sounds.death.is_some() {
                    mod_sounds.insert(entity_type.id, sounds);
                }
                Some(&*Box::leak(Box::new(entity_type)))
            }
            Err(error) => {
                warn!("Entity type extension skipped {name}: {error}");
                None
            }
        })
        .collect();
    if entries.is_empty() {
        return;
    }
    if !mod_sounds.is_empty() {
        let _ = MOD_SOUNDS.set(mod_sounds);
    }
    entries.sort_unstable_by_key(|entity_type| entity_type.id);

    let count = entries.len();
    match entity_type_extension::install(entries) {
        Ok(()) => info!("Installed {count} extension entity type(s)"),
        Err(error) => warn!("Could not install the entity type extension: {error}"),
    }
}

fn build(name: &str, entity: &DumpEntity) -> Result<(EntityType, ModSounds), String> {
    let attributes = entity
        .attributes
        .iter()
        .flatten()
        .flat_map(BTreeMap::iter)
        .map(|(key, value)| Ok((attribute(key)?, *value)))
        .collect::<Result<Vec<_>, String>>()?;
    let (hurt_sound, hurt) = entity_sound(entity.hurt_sound.as_deref())?;
    let (death_sound, death) = entity_sound(entity.death_sound.as_deref())?;
    let entity_type = EntityType {
        id: entity.id,
        attributes: if attributes.is_empty() {
            &[]
        } else {
            Box::leak(attributes.into_boxed_slice())
        },
        experience_reward: entity.experience_reward.unwrap_or(0),
        hurt_sound,
        death_sound,
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
    };
    Ok((entity_type, ModSounds { hurt, death }))
}

/// An attribute by its dumped name: vanilla bare, like codegen's input, or a
/// mod's own with its namespace.
fn attribute(key: &str) -> Result<Attributes, String> {
    pumpkin_data::attributes_extension::by_name(key)
        .cloned()
        .ok_or_else(|| format!("attribute {key} is neither vanilla nor an installed extension one"))
}

/// A vanilla sound by its dumped name, or else a mod's own by raw id — it has
/// a namespace and no `Sound` variant.
fn entity_sound(name: Option<&str>) -> Result<(Option<Sound>, Option<u16>), String> {
    let Some(name) = name else {
        return Ok((None, None));
    };
    if let Some(sound) = Sound::from_name(name) {
        return Ok((Some(sound), None));
    }
    SOUND_EVENTS
        .id_of(name)
        .map(|id| (None, Some(id)))
        .ok_or_else(|| format!("sound {name} is neither vanilla nor an installed extension one"))
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
