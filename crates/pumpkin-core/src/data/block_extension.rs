//! Loads blocks that are not in the generated vanilla set, and starts the
//! loading of items from the same dump (see [`super::item_extension`]).
//!
//! This is the link between a mod's content and `pumpkin_data::block_extension`.
//! The client already knows the block from its own jar; the server needs a
//! definition for it before it can place it, send it inside a chunk, or dictate
//! its id during configuration.
//!
//! Two sources, never mixed:
//!
//! - **An Extractor dump** (`extractor_dump`): the directory Pumpkin's Extractor
//!   writes when run with the mod loaded. Every non-vanilla block in it is
//!   installed with the ids, states and properties a real server assigned. This
//!   is the faithful path, and the only one that handles properties.
//! - **Hand-written entries** (`blocks`): a name plus a vanilla block whose
//!   physical state is copied. A stopgap for single-state, full-cube blocks.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use pumpkin_data::block_extension::{self, ExtensionBlock, PropertyDef};
use pumpkin_data::block_properties::{COLLISION_SHAPES, EnumVariants, NoteblockInstrument};
use pumpkin_data::block_state::PistonBehavior;
use pumpkin_data::{Block, BlockId, BlockState, BlockStateId};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::LoadJSONConfiguration;

#[derive(Deserialize, Serialize, Default)]
#[serde(default)]
pub struct BlockExtensionConfig {
    /// Directory written by the Extractor with the mod loaded, holding
    /// `blocks.json` and `properties.json`. Relative paths resolve against the
    /// server's working directory.
    pub extractor_dump: Option<PathBuf>,
    /// Hand-written blocks copying a vanilla block. Ignored when a dump is set.
    pub blocks: Vec<HandWrittenBlock>,
}

#[derive(Deserialize, Serialize)]
pub struct HandWrittenBlock {
    /// Namespaced id, which has to match what the mod registered on the client,
    /// for example `pumpkinverse_test:marker`.
    pub name: String,
    /// Vanilla block whose physical state this one copies.
    pub like: String,
}

impl LoadJSONConfiguration for BlockExtensionConfig {
    fn get_path() -> &'static Path {
        Path::new("block_extension.json")
    }

    fn validate(&self) {
        if self.extractor_dump.is_some() && !self.blocks.is_empty() {
            warn!(
                "block_extension.json sets both extractor_dump and blocks; the hand-written \
                 blocks are ignored, because ids come from the dump"
            );
        }
        for entry in &self.blocks {
            if !entry.name.contains(':') {
                warn!(
                    "Block extension entry {} has no namespace. Unprefixed names resolve against \
                     the vanilla table, so this block will never be reached.",
                    entry.name
                );
            }
        }
    }
}

/// Loads `data/block_extension.json` and installs what it declares.
///
/// Call once at startup, before any connection is accepted: the extension is
/// read-only afterwards, and ids have to be settled before a client can be told
/// about them.
pub fn load_and_install() {
    let config = BlockExtensionConfig::load();

    // Items come from the same dump; they do not depend on the blocks.
    if let Some(dir) = &config.extractor_dump {
        super::item_extension::install_from_dump(dir);
    }

    let entries = match &config.extractor_dump {
        Some(dir) => match from_dump(dir) {
            Ok(entries) => entries,
            Err(error) => {
                warn!(
                    "Could not load the Extractor dump at {}: {error}",
                    dir.display()
                );
                return;
            }
        },
        None => from_hand_written(&config.blocks),
    };

    if entries.is_empty() {
        return;
    }

    let count = entries.len();
    match block_extension::install(entries) {
        Ok(()) => info!("Installed {count} extension block(s)"),
        Err(error) => warn!("Could not install the block extension: {error}"),
    }
}

// ------------------------------------------------------------- Extractor dump

#[derive(Deserialize)]
struct DumpBlocks {
    shapes: Vec<DumpShape>,
    blocks: Vec<DumpBlock>,
}

#[derive(Deserialize)]
struct DumpShape {
    min: [f64; 3],
    max: [f64; 3],
}

#[derive(Deserialize)]
struct DumpBlock {
    id: u16,
    name: String,
    slipperiness: f32,
    velocity_multiplier: f32,
    jump_velocity_multiplier: f32,
    hardness: f32,
    blast_resistance: f32,
    map_color: u8,
    item_id: u16,
    properties: Vec<i64>,
    default_state_id: u16,
    states: Vec<DumpState>,
    #[serde(default)]
    loot_table: Option<String>,
}

#[derive(Deserialize)]
struct DumpState {
    id: u16,
    state_flags: u16,
    side_flags: u8,
    instrument: String,
    luminance: u8,
    piston_behavior: String,
    hardness: f32,
    opacity: u8,
    collision_shapes: Vec<u16>,
    outline_shapes: Vec<u16>,
    block_entity_type: Option<u16>,
}

#[derive(Deserialize)]
struct DumpProperty {
    hash_key: i64,
    serialized_name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    values: Vec<String>,
    min: Option<i32>,
    max: Option<i32>,
}

/// Reads every non-vanilla block from an Extractor dump.
///
/// Ids are taken from the dump rather than assigned here: they are what a real
/// server with the same mods assigned, so they are what the client expects.
fn from_dump(dir: &Path) -> Result<Vec<ExtensionBlock>, String> {
    let read =
        |file: &str| fs::read_to_string(dir.join(file)).map_err(|error| format!("{file}: {error}"));
    let blocks: DumpBlocks = serde_json::from_str(&read("blocks.json")?)
        .map_err(|error| format!("blocks.json: {error}"))?;
    let properties: Vec<DumpProperty> = serde_json::from_str(&read("properties.json")?)
        .map_err(|error| format!("properties.json: {error}"))?;

    // `hash_key`s are only meaningful inside one dump: they link the two files
    // and differ between Extractor runs.
    let properties: BTreeMap<i64, &DumpProperty> =
        properties.iter().map(|p| (p.hash_key, p)).collect();

    let mut entries = Vec::new();
    for block in &blocks.blocks {
        // The Extractor writes vanilla names bare and keeps every other namespace.
        if !block.name.contains(':') {
            continue;
        }
        match extension_from_dump(block, &properties, &blocks.shapes) {
            Ok(entry) => entries.push(entry),
            Err(error) => warn!("Block extension skipped {}: {error}", block.name),
        }
    }
    Ok(entries)
}

fn extension_from_dump(
    block: &DumpBlock,
    properties: &BTreeMap<i64, &DumpProperty>,
    shapes: &[DumpShape],
) -> Result<ExtensionBlock, String> {
    let mut defs = Vec::with_capacity(block.properties.len());
    for hash in &block.properties {
        let property = properties
            .get(hash)
            .ok_or_else(|| format!("unknown property hash {hash}"))?;
        defs.push(PropertyDef {
            name: leak(&property.serialized_name),
            values: leak_values(property_values(property)?),
        });
    }

    let mut states = Vec::with_capacity(block.states.len());
    for state in &block.states {
        states.push(BlockState {
            id: BlockStateId::extended(state.id)
                .ok_or_else(|| format!("state id {} is inside the vanilla range", state.id))?,
            state_flags: state.state_flags,
            side_flags: state.side_flags,
            instrument: instrument(&state.instrument)
                .ok_or_else(|| format!("unknown instrument {}", state.instrument))?,
            luminance: state.luminance,
            piston_behavior: piston_behavior(&state.piston_behavior)
                .ok_or_else(|| format!("unknown piston behaviour {}", state.piston_behavior))?,
            hardness: state.hardness,
            collision_shapes: shape_indices(&state.collision_shapes, shapes)?,
            outline_shapes: shape_indices(&state.outline_shapes, shapes)?,
            opacity: state.opacity,
            block_entity_type: state.block_entity_type.unwrap_or(u16::MAX),
        });
    }

    let default_index = block
        .states
        .iter()
        .position(|state| state.id == block.default_state_id)
        .ok_or("the default state is not among its states")?;
    let states: &'static [BlockState] = Box::leak(states.into_boxed_slice());

    let block = Box::leak(Box::new(Block {
        id: BlockId::extended(block.id)
            .ok_or_else(|| format!("block id {} is inside the vanilla range", block.id))?,
        name: leak(&block.name),
        hardness: block.hardness,
        blast_resistance: block.blast_resistance,
        map_color: block.map_color,
        slipperiness: block.slipperiness,
        velocity_multiplier: block.velocity_multiplier,
        jump_velocity_multiplier: block.jump_velocity_multiplier,
        item_id: block.item_id,
        default_state: &states[default_index],
        states,
        // Not in the dump; Pumpkin's codegen takes them from elsewhere.
        flammable: None,
        experience: None,
        loot_table: block.loot_table.as_deref().map(leak),
    }));

    Ok(ExtensionBlock {
        block,
        properties: Box::leak(defs.into_boxed_slice()),
    })
}

/// Values in the order Minecraft enumerates them, which is the order states
/// are built in. Booleans are `true` first.
fn property_values(property: &DumpProperty) -> Result<Vec<String>, String> {
    match property.kind.as_str() {
        "boolean" => Ok(vec!["true".to_owned(), "false".to_owned()]),
        "enum" => Ok(property.values.clone()),
        "int" => match (property.min, property.max) {
            (Some(min), Some(max)) => Ok((min..=max).map(|v| v.to_string()).collect()),
            _ => Err(format!(
                "int property {} has no range",
                property.serialized_name
            )),
        },
        other => Err(format!(
            "property {} has unknown type {other}",
            property.serialized_name
        )),
    }
}

/// Maps dump shape indices onto Pumpkin's shape table.
///
/// The two tables are built the same way, so vanilla shapes share indices. A
/// mod's new shapes would not exist in Pumpkin's table, which is a static
/// array, so any index whose shape differs is rejected rather than mapped to
/// the wrong box.
fn shape_indices(indices: &[u16], shapes: &[DumpShape]) -> Result<&'static [u16], String> {
    for &index in indices {
        let dump = shapes
            .get(usize::from(index))
            .ok_or_else(|| format!("shape {index} is missing from the dump"))?;
        let ours = COLLISION_SHAPES
            .get(usize::from(index))
            .ok_or_else(|| format!("shape {index} is not in Pumpkin's shape table"))?;
        let same = [ours.min.x, ours.min.y, ours.min.z] == dump.min
            && [ours.max.x, ours.max.y, ours.max.z] == dump.max;
        if !same {
            return Err(format!("shape {index} differs from Pumpkin's shape table"));
        }
    }
    Ok(Box::leak(indices.to_vec().into_boxed_slice()))
}

fn instrument(name: &str) -> Option<NoteblockInstrument> {
    (0..NoteblockInstrument::variant_count())
        .map(NoteblockInstrument::from_index)
        .find(|instrument| instrument.to_value() == name)
}

/// Same aliases as the codegen accepts.
fn piston_behavior(name: &str) -> Option<PistonBehavior> {
    Some(match name {
        "NORMAL" | "PUSH_PULL" => PistonBehavior::Normal,
        "DESTROY" | "POPPED" => PistonBehavior::Destroy,
        "BLOCK" => PistonBehavior::Block,
        "IGNORE" | "IMMOVEABLE" => PistonBehavior::Ignore,
        "PUSH_ONLY" | "PUSH" => PistonBehavior::PushOnly,
        _ => return None,
    })
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

fn leak_values(values: Vec<String>) -> &'static [&'static str] {
    let values: Vec<&'static str> = values
        .into_iter()
        .map(|v| &*Box::leak(v.into_boxed_str()))
        .collect();
    Box::leak(values.into_boxed_slice())
}

// --------------------------------------------------------- hand-written path

fn from_hand_written(entries: &[HandWrittenBlock]) -> Vec<ExtensionBlock> {
    let mut blocks = Vec::with_capacity(entries.len());
    let mut next_block_id = BlockId::COUNT;
    let mut next_state_id = BlockStateId::COUNT;

    for entry in entries {
        let Some(template) = Block::from_name(&entry.like) else {
            warn!(
                "Block extension skipped {}: no block is named {}",
                entry.name, entry.like
            );
            continue;
        };

        let Some(block_id) = BlockId::extended(next_block_id) else {
            warn!("Block extension skipped {}: raw ids exhausted", entry.name);
            continue;
        };

        let Some(block) = copy_of(&entry.name, template, block_id, next_state_id) else {
            warn!("Block extension skipped {}: could not be built", entry.name);
            continue;
        };

        let Some(states) = u16::try_from(block.states.len())
            .ok()
            .and_then(|len| next_state_id.checked_add(len))
        else {
            warn!(
                "Block extension skipped {}: state ids exhausted",
                entry.name
            );
            continue;
        };

        next_state_id = states;
        next_block_id = next_block_id.saturating_add(1);
        // A copied block has no properties of its own: `like` only makes sense
        // for single-state templates.
        blocks.push(ExtensionBlock {
            block,
            properties: &[],
        });
    }

    blocks
}

/// Builds a `'static` block copying `template`'s physical state under a new name
/// and a new id range.
fn copy_of(
    name: &str,
    template: &'static Block,
    block_id: BlockId,
    first_state_id: u16,
) -> Option<&'static Block> {
    // Without the template's properties the copy could not address several
    // states, so only single-state templates are accepted.
    if template.states.len() != 1 {
        warn!(
            "Block extension {name}: `like` needs a single-state block, and {} has {}",
            template.name,
            template.states.len()
        );
        return None;
    }

    let state = template.default_state;
    let states: &'static [BlockState] = Box::leak(Box::new([BlockState {
        id: BlockStateId::extended(first_state_id)?,
        state_flags: state.state_flags,
        side_flags: state.side_flags,
        instrument: state.instrument,
        luminance: state.luminance,
        piston_behavior: state.piston_behavior.clone(),
        hardness: state.hardness,
        collision_shapes: state.collision_shapes,
        outline_shapes: state.outline_shapes,
        opacity: state.opacity,
        // Never copied: claiming the template's block entity type would make
        // the server expect NBT this block does not carry.
        block_entity_type: u16::MAX,
    }]));

    Some(Box::leak(Box::new(Block {
        id: block_id,
        name: leak(name),
        hardness: template.hardness,
        blast_resistance: template.blast_resistance,
        map_color: template.map_color,
        slipperiness: template.slipperiness,
        velocity_multiplier: template.velocity_multiplier,
        jump_velocity_multiplier: template.jump_velocity_multiplier,
        // No item form: the item registry is not synchronised, so an item id here
        // would mean nothing to the client.
        item_id: 0,
        default_state: &states[0],
        states,
        // Left off rather than copied: a fixture block has no business burning,
        // dropping experience or loot.
        flammable: None,
        experience: None,
        loot_table: None,
    })))
}
