//! Loads blocks that are not in the generated vanilla set.
//!
//! This is the link between a mod's content and `pumpkin_data::block_extension`.
//! The client already knows the block from its own jar; the server needs a
//! definition for it before it can place it, send it inside a chunk, or dictate
//! its id during configuration.
//!
//! Each entry copies the physical state of a vanilla block instead of describing
//! its own. That is a deliberate stopgap for simple full-cube blocks: a block
//! state stores *indices* into a global voxel-shape table, so borrowing a
//! vanilla block's indices avoids inventing shapes. A block with its own
//! properties needs the Extractor's dump, which is the next facet.

use std::path::Path;

use pumpkin_data::{Block, BlockId, BlockState, BlockStateId, block_extension};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::LoadJSONConfiguration;

#[derive(Deserialize, Serialize, Default)]
#[serde(default)]
pub struct BlockExtensionConfig {
    pub blocks: Vec<ExtensionBlock>,
}

#[derive(Deserialize, Serialize)]
pub struct ExtensionBlock {
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
    if config.blocks.is_empty() {
        return;
    }

    let mut blocks = Vec::with_capacity(config.blocks.len());
    let mut next_block_id = BlockId::COUNT;
    let mut next_state_id = BlockStateId::COUNT;

    for entry in &config.blocks {
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

        let Some(block) = extend(&entry.name, template, block_id, next_state_id) else {
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
        blocks.push(block);
    }

    let count = blocks.len();
    match block_extension::install(blocks) {
        Ok(()) => info!("Installed {count} extension block(s)"),
        Err(error) => warn!("Could not install the block extension: {error}"),
    }
}

/// Builds a `'static` block copying `template`'s physical state under a new name
/// and a new id range.
fn extend(
    name: &str,
    template: &'static Block,
    block_id: BlockId,
    first_state_id: u16,
) -> Option<&'static Block> {
    let default_index = template
        .states
        .iter()
        .position(|state| state.id == template.default_state.id)?;

    let mut states = Vec::with_capacity(template.states.len());
    for (offset, state) in template.states.iter().enumerate() {
        let raw = first_state_id.checked_add(u16::try_from(offset).ok()?)?;
        states.push(BlockState {
            id: BlockStateId::extended(raw)?,
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
        });
    }

    let states: &'static [BlockState] = Box::leak(states.into_boxed_slice());

    Some(Box::leak(Box::new(Block {
        id: block_id,
        name: Box::leak(name.to_owned().into_boxed_str()),
        hardness: template.hardness,
        blast_resistance: template.blast_resistance,
        map_color: template.map_color,
        slipperiness: template.slipperiness,
        velocity_multiplier: template.velocity_multiplier,
        jump_velocity_multiplier: template.jump_velocity_multiplier,
        // No item form: the item registry is not synchronised, so an item id here
        // would mean nothing to the client.
        item_id: 0,
        default_state: &states[default_index],
        states,
        // Left off rather than copied: a fixture block has no business burning or
        // dropping experience.
        flammable: None,
        experience: None,
    })))
}
