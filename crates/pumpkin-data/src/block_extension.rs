//! Runtime extension of the generated block registry.
//!
//! The vanilla block set is generated at build time into
//! `generated/block.rs` and cannot change. This module holds blocks contributed
//! at startup, whose raw ids continue the vanilla range, so lookups can resolve
//! ids the generated tables know nothing about.
//!
//! The extension is installed once, before the server accepts connections, and
//! is read-only afterwards. Reads are therefore lock-free.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use crate::{Block, BlockId, BlockState, BlockStateId};

/// Why [`install`] rejected an extension.
#[derive(Debug)]
pub enum InstallError {
    /// An extension is already installed.
    AlreadyInstalled,
    /// Raw block ids must continue the vanilla range without gaps.
    BlockIdNotContiguous { expected: u16, found: u16 },
    /// Block state ids must continue the vanilla range without gaps.
    StateIdNotContiguous { expected: u16, found: u16 },
    /// A block declares no states, so it has no default to serialize.
    NoStates { name: &'static str },
    /// The name is already taken, either by vanilla or earlier in this batch.
    DuplicateName { name: &'static str },
    /// Raw ids would exceed what `u16` can carry.
    TooMany,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => write!(f, "a block extension is already installed"),
            Self::BlockIdNotContiguous { expected, found } => {
                write!(f, "expected block id {expected}, found {found}")
            }
            Self::StateIdNotContiguous { expected, found } => {
                write!(f, "expected block state id {expected}, found {found}")
            }
            Self::NoStates { name } => write!(f, "block {name} declares no states"),
            Self::DuplicateName { name } => write!(f, "block name {name} is already registered"),
            Self::TooMany => write!(f, "too many blocks to address with a u16 id"),
        }
    }
}

impl std::error::Error for InstallError {}

struct Extension {
    /// Indexed by `raw_id - BlockId::COUNT`.
    blocks: Vec<&'static Block>,
    /// Indexed by `state_id - BlockStateId::COUNT`.
    states: Vec<&'static BlockState>,
    /// Owning block for each extended state, same indexing as `states`.
    owners: Vec<BlockId>,
    by_name: BTreeMap<&'static str, &'static Block>,
}

static EXTENSION: OnceLock<Extension> = OnceLock::new();

/// Installs the runtime block extension.
///
/// Every block must already be leaked to `'static`, carry a raw id continuing
/// the vanilla range, and own states whose ids continue the vanilla state range.
/// Both sequences must be gapless and in ascending order, because lookups index
/// into them directly.
///
/// # Errors
///
/// Returns [`InstallError`] when an id is out of sequence, a name collides, or
/// an extension is already installed. Nothing is installed on error.
pub fn install(blocks: Vec<&'static Block>) -> Result<(), InstallError> {
    if EXTENSION.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }

    let mut states = Vec::new();
    let mut owners = Vec::new();
    let mut by_name = BTreeMap::new();

    let mut next_block_id = BlockId::COUNT;
    let mut next_state_id = BlockStateId::COUNT;

    for block in &blocks {
        let raw = block.id.as_u16();
        if raw != next_block_id {
            return Err(InstallError::BlockIdNotContiguous {
                expected: next_block_id,
                found: raw,
            });
        }

        if block.states.is_empty() {
            return Err(InstallError::NoStates { name: block.name });
        }

        for state in block.states {
            let state_raw = state.id.as_u16();
            if state_raw != next_state_id {
                return Err(InstallError::StateIdNotContiguous {
                    expected: next_state_id,
                    found: state_raw,
                });
            }
            states.push(state);
            owners.push(block.id);
            next_state_id = next_state_id.checked_add(1).ok_or(InstallError::TooMany)?;
        }

        // `Block::from_name` can only see vanilla here, because the extension is
        // not installed yet. A collision would otherwise be silent: the
        // generated map is consulted first and the extension never reached.
        if Block::from_name(block.name).is_some() || by_name.insert(block.name, *block).is_some() {
            return Err(InstallError::DuplicateName { name: block.name });
        }

        next_block_id = next_block_id.checked_add(1).ok_or(InstallError::TooMany)?;
    }

    let extension = Extension {
        blocks,
        states,
        owners,
        by_name,
    };

    EXTENSION
        .set(extension)
        .map_err(|_| InstallError::AlreadyInstalled)
}

/// Total number of blocks, vanilla plus extension.
#[must_use]
pub fn block_count() -> u16 {
    let extra = EXTENSION.get().map_or(0, |e| e.blocks.len());
    // `install` rejects anything that would not fit.
    BlockId::COUNT.saturating_add(extra.try_into().unwrap_or(u16::MAX))
}

/// Total number of block states, vanilla plus extension.
#[must_use]
pub fn state_count() -> u16 {
    let extra = EXTENSION.get().map_or(0, |e| e.states.len());
    BlockStateId::COUNT.saturating_add(extra.try_into().unwrap_or(u16::MAX))
}

#[inline]
pub(crate) fn block_by_raw_id(raw: u16) -> Option<&'static Block> {
    let index = usize::from(raw.checked_sub(BlockId::COUNT)?);
    EXTENSION.get()?.blocks.get(index).copied()
}

#[inline]
pub(crate) fn block_by_name(name: &str) -> Option<&'static Block> {
    EXTENSION.get()?.by_name.get(name).copied()
}

#[inline]
pub(crate) fn owner_of_state_id(raw: u16) -> Option<BlockId> {
    let index = usize::from(raw.checked_sub(BlockStateId::COUNT)?);
    EXTENSION.get()?.owners.get(index).copied()
}

#[inline]
pub(crate) fn state_by_state_id(raw: u16) -> Option<&'static BlockState> {
    let index = usize::from(raw.checked_sub(BlockStateId::COUNT)?);
    EXTENSION.get()?.states.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::{block_count, install, state_count};
    use crate::block_properties::NoteblockInstrument;
    use crate::block_state::PistonBehavior;
    use crate::{Block, BlockId, BlockState, BlockStateId};

    const NAME: &str = "pumpkinverse_test:marker";

    /// There is deliberately one test here. `install` writes a process-wide
    /// `OnceLock`, so a second installing test would pass or fail depending on
    /// thread order.
    #[test]
    fn resolves_ids_beyond_the_vanilla_range() {
        let vanilla_blocks = BlockId::COUNT;
        let vanilla_states = BlockStateId::COUNT;

        let block_id = BlockId::extended(vanilla_blocks).unwrap_or(BlockId::AIR);
        let state_id = BlockStateId::extended(vanilla_states).unwrap_or(BlockStateId::AIR);

        let states: &'static [BlockState] = Box::leak(
            vec![BlockState {
                id: state_id,
                state_flags: 0,
                side_flags: 0,
                instrument: NoteblockInstrument::Harp,
                luminance: 0,
                piston_behavior: PistonBehavior::Normal,
                hardness: 1.0,
                collision_shapes: &[],
                outline_shapes: &[],
                opacity: 0,
                block_entity_type: u16::MAX,
            }]
            .into_boxed_slice(),
        );

        let block: &'static Block = Box::leak(Box::new(Block {
            id: block_id,
            name: NAME,
            hardness: 1.0,
            blast_resistance: 1.0,
            map_color: 0,
            slipperiness: 0.6,
            velocity_multiplier: 1.0,
            jump_velocity_multiplier: 1.0,
            item_id: 0,
            default_state: &states[0],
            states,
            flammable: None,
            experience: None,
        }));

        assert!(install(vec![block]).is_ok(), "install rejected the block");

        assert_eq!(block_count(), vanilla_blocks + 1);
        assert_eq!(state_count(), vanilla_states + 1);

        // Every lookup path reaches the extension.
        assert_eq!(Block::from_name(NAME).map(|b| b.name), Some(NAME));
        assert_eq!(Block::from_registry_key(NAME).map(|b| b.name), Some(NAME));
        assert_eq!(Block::from_id(block_id).name, NAME);
        assert_eq!(Block::from_state_id(state_id).name, NAME);
        assert_eq!(BlockState::from_id(state_id).id, state_id);
        assert_eq!(BlockId::from_state_id(state_id), block_id);

        // Vanilla still resolves through the generated tables.
        assert_eq!(Block::from_name("stone").map(|b| b.name), Some("stone"));
        assert_eq!(Block::from_id(BlockId::AIR).name, Block::AIR.name);
        assert_eq!(BlockId::from_state_id(BlockStateId::AIR), BlockId::AIR);

        // An id the extension does not declare falls back instead of indexing
        // out of bounds, which is what the removed `assert_unchecked` risked.
        let unknown = BlockId::extended(vanilla_blocks + 1).unwrap_or(BlockId::AIR);
        assert_eq!(Block::from_id(unknown).name, Block::AIR.name);

        assert!(install(vec![block]).is_err(), "installed twice");
    }
}
