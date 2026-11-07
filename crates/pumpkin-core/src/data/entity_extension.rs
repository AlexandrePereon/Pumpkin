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

use pumpkin_data::entity::{
    BoatSeats, EntityAttachments, EntityPose, EntityType, MobCategory, MovementEmission,
    PoseDimensions, SpawnLocation, SpawnRestriction,
};
use pumpkin_data::attributes::Attributes;
use pumpkin_data::entity_type_extension;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_util::HeightMap;
use pumpkin_util::math::boundingbox::EntityDimensions;
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
    // The fields below are absent from dumps older than them.
    #[serde(default)]
    ambient_sound: Option<String>,
    #[serde(default)]
    eating_sound: Option<String>,
    #[serde(default)]
    angry_sound: Option<String>,
    #[serde(default)]
    sit_sound: Option<String>,
    #[serde(default)]
    stand_sound: Option<String>,
    #[serde(default)]
    dash_sound: Option<String>,
    #[serde(default)]
    dash_ready_sound: Option<String>,
    #[serde(default)]
    saddle_sound: Option<String>,
    #[serde(default)]
    passenger_attachments: Option<Vec<[f64; 3]>>,
    #[serde(default)]
    vehicle_attachment: Option<[f64; 3]>,
    #[serde(default)]
    baby: Option<DumpBaby>,
    #[serde(default)]
    poses: BTreeMap<String, DumpPose>,
    #[serde(default)]
    boat: Option<DumpBoat>,
    #[serde(default)]
    sound_source: Option<String>,
    #[serde(default)]
    movement_emission: Option<String>,
    #[serde(default)]
    swim_sound: Option<String>,
    #[serde(default)]
    swim_splash_sound: Option<String>,
    #[serde(default)]
    swim_high_speed_splash_sound: Option<String>,
    #[serde(default)]
    small_fall_sound: Option<String>,
    #[serde(default)]
    big_fall_sound: Option<String>,
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

/// A pose's size and attachment points, where they differ from standing.
#[derive(Deserialize)]
struct DumpPose {
    dimension: [f32; 2],
    eye_height: f32,
    passenger_attachments: Vec<[f64; 3]>,
    vehicle_attachment: [f64; 3],
}

#[derive(Deserialize)]
struct DumpBaby {
    dimension: [f32; 2],
    eye_height: f32,
    passenger_attachments: Vec<[f64; 3]>,
    vehicle_attachment: [f64; 3],
    eating_sound: Option<String>,
    angry_sound: Option<String>,
    #[serde(default)]
    poses: BTreeMap<String, DumpPose>,
}

#[derive(Deserialize)]
struct DumpBoat {
    ride_height: f64,
    single_passenger_x_offset: f32,
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
        ambient_sound: vanilla_sound(entity.ambient_sound.as_deref()),
        eating_sound: vanilla_sound(entity.eating_sound.as_deref()),
        baby_eating_sound: vanilla_sound(entity.baby.as_ref().and_then(|b| b.eating_sound.as_deref())),
        angry_sound: vanilla_sound(entity.angry_sound.as_deref()),
        baby_angry_sound: vanilla_sound(entity.baby.as_ref().and_then(|b| b.angry_sound.as_deref())),
        sit_sound: vanilla_sound(entity.sit_sound.as_deref()),
        stand_sound: vanilla_sound(entity.stand_sound.as_deref()),
        dash_sound: vanilla_sound(entity.dash_sound.as_deref()),
        dash_ready_sound: vanilla_sound(entity.dash_ready_sound.as_deref()),
        saddle_sound: vanilla_sound(entity.saddle_sound.as_deref()),
        sound_source: sound_source(entity.sound_source.as_deref())?,
        movement_emission: movement_emission(entity.movement_emission.as_deref())?,
        swim_sound: vanilla_sound(entity.swim_sound.as_deref()),
        swim_splash_sound: vanilla_sound(entity.swim_splash_sound.as_deref()),
        swim_high_speed_splash_sound: vanilla_sound(
            entity.swim_high_speed_splash_sound.as_deref(),
        ),
        small_fall_sound: vanilla_sound(entity.small_fall_sound.as_deref()),
        big_fall_sound: vanilla_sound(entity.big_fall_sound.as_deref()),
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
        // `EntityAttachments.createDefault` when the dump has none.
        attachments: attachments(
            entity.passenger_attachments.as_deref().unwrap_or(&[[0.0, f64::from(entity.dimension[1]), 0.0]]),
            entity.vehicle_attachment.unwrap_or_default(),
        ),
        baby_attachments: entity.baby.as_ref().map(|b| attachments(&b.passenger_attachments, b.vehicle_attachment)),
        baby_dimensions: entity.baby.as_ref().map(|b| EntityDimensions {
            width: b.dimension[0],
            height: b.dimension[1],
            eye_height: b.eye_height,
        }),
        poses: poses(&entity.poses)?,
        baby_poses: entity.baby.as_ref().map_or(Ok(&[][..]), |b| poses(&b.poses))?,
        boat: entity.boat.as_ref().map(|b| BoatSeats {
            ride_height: b.ride_height,
            single_passenger_x_offset: b.single_passenger_x_offset,
        }),
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

fn attachments(passenger: &[[f64; 3]], vehicle: [f64; 3]) -> EntityAttachments {
    EntityAttachments {
        passenger: Box::leak(passenger.to_vec().into_boxed_slice()),
        vehicle,
    }
}

/// The dumped poses, as codegen writes them: by name, in name order.
fn poses(dumped: &BTreeMap<String, DumpPose>) -> Result<&'static [PoseDimensions], String> {
    let poses = dumped
        .iter()
        .map(|(name, pose)| {
            Ok(PoseDimensions {
                pose: entity_pose(name)?,
                dimensions: EntityDimensions {
                    width: pose.dimension[0],
                    height: pose.dimension[1],
                    eye_height: pose.eye_height,
                },
                attachments: attachments(&pose.passenger_attachments, pose.vehicle_attachment),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(if poses.is_empty() { &[] } else { Box::leak(poses.into_boxed_slice()) })
}

/// A pose by its dumped name (`Pose` in lower case).
fn entity_pose(name: &str) -> Result<EntityPose, String> {
    Ok(match name {
        "standing" => EntityPose::Standing,
        "fall_flying" => EntityPose::FallFlying,
        "sleeping" => EntityPose::Sleeping,
        "swimming" => EntityPose::Swimming,
        "spin_attack" => EntityPose::SpinAttack,
        "crouching" => EntityPose::Crouching,
        "long_jumping" => EntityPose::LongJumping,
        "dying" => EntityPose::Dying,
        "croaking" => EntityPose::Croaking,
        "using_tongue" => EntityPose::UsingTongue,
        "sitting" => EntityPose::Sitting,
        "roaring" => EntityPose::Roaring,
        "sniffing" => EntityPose::Sniffing,
        "emerging" => EntityPose::Emerging,
        "digging" => EntityPose::Digging,
        "sliding" => EntityPose::Sliding,
        "shooting" => EntityPose::Shooting,
        "inhaling" => EntityPose::Inhaling,
        other => return Err(format!("unknown pose {other}")),
    })
}

/// A vanilla movement sound by its dumped name. A mod's own is dropped: only
/// hurt and death sounds have a side table.
fn vanilla_sound(name: Option<&str>) -> Option<Sound> {
    name.and_then(Sound::from_name)
}

/// The dumped `getSoundSource`, `NEUTRAL` when absent, as vanilla's default.
fn sound_source(name: Option<&str>) -> Result<SoundCategory, String> {
    name.map_or(Ok(SoundCategory::Neutral), |name| {
        SoundCategory::from_name(&name.to_ascii_lowercase())
            .ok_or_else(|| format!("unknown sound source {name}"))
    })
}

/// The dumped `getMovementEmission`, `ALL` when absent, as vanilla's default.
fn movement_emission(name: Option<&str>) -> Result<MovementEmission, String> {
    Ok(match name {
        None | Some("ALL") => MovementEmission::All,
        Some("NONE") => MovementEmission::None,
        Some("SOUNDS") => MovementEmission::Sounds,
        Some("EVENTS") => MovementEmission::Events,
        Some(other) => return Err(format!("unknown movement emission {other}")),
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
