//! Rust behaviour for blocks outside the generated vanilla set.
//!
//! [`BlockRegistry`](super::registry::BlockRegistry) finds a block's behaviour
//! through a table sized to the generated vanilla blocks and filled from
//! compile-time ids. A block installed at runtime by
//! `pumpkin_data::block_extension` has an id past that table, so its behaviour
//! is kept here instead: a second table indexed by `id - BlockId::COUNT`,
//! installed once at startup, after the block extension and before the server
//! starts. Only lookups that miss the vanilla table reach it, so vanilla blocks
//! pay nothing.

use std::fmt;
use std::sync::{Arc, OnceLock};

use pumpkin_data::{Block, BlockId, block_extension};

use super::BlockBehaviour;

static BEHAVIOURS: OnceLock<Vec<Option<Arc<dyn BlockBehaviour>>>> = OnceLock::new();

#[derive(Debug, PartialEq, Eq)]
pub enum InstallError {
    /// No block, vanilla or extension, has this name.
    UnknownBlock(String),
    /// The block is vanilla: its behaviour belongs in the registry itself.
    NotAnExtension(String),
    /// The same block was given two behaviours.
    Duplicate(String),
    /// Behaviours were already installed; the table is set once.
    AlreadyInstalled,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBlock(name) => write!(f, "no block is named {name}"),
            Self::NotAnExtension(name) => {
                write!(f, "{name} is a vanilla block, register it in the block registry")
            }
            Self::Duplicate(name) => write!(f, "block {name} was given two behaviours"),
            Self::AlreadyInstalled => write!(f, "extension block behaviours are already installed"),
        }
    }
}

impl std::error::Error for InstallError {}

/// Installs the behaviours of extension blocks, by block name.
///
/// Call it once, after `pumpkin_data::block_extension` is installed (names are
/// resolved against it) and before the server starts. Nothing is installed
/// when an entry is rejected.
///
/// # Errors
///
/// Returns [`InstallError`] for a name that is unknown, vanilla or repeated,
/// or when behaviours are already installed.
pub fn install(
    entries: Vec<(&str, Arc<dyn BlockBehaviour>)>,
) -> Result<(), InstallError> {
    let extension_blocks = block_extension::block_count().saturating_sub(BlockId::COUNT);
    let mut table: Vec<Option<Arc<dyn BlockBehaviour>>> =
        vec![None; usize::from(extension_blocks)];
    for (name, behaviour) in entries {
        let block = Block::from_name(name).ok_or_else(|| InstallError::UnknownBlock(name.into()))?;
        let index = block
            .id
            .as_u16()
            .checked_sub(BlockId::COUNT)
            .ok_or_else(|| InstallError::NotAnExtension(name.into()))?;
        // `from_name` only resolves extension ids that `block_count` counts.
        let slot = table
            .get_mut(usize::from(index))
            .ok_or_else(|| InstallError::UnknownBlock(name.into()))?;
        if slot.is_some() {
            return Err(InstallError::Duplicate(name.into()));
        }
        *slot = Some(behaviour);
    }
    BEHAVIOURS
        .set(table)
        .map_err(|_| InstallError::AlreadyInstalled)
}

// Off the vanilla hot path: only ids past the generated range get here.
#[cold]
#[inline(never)]
pub(crate) fn get(block: BlockId) -> Option<&'static Arc<dyn BlockBehaviour>> {
    let index = block.as_u16().checked_sub(BlockId::COUNT)?;
    BEHAVIOURS.get()?.get(usize::from(index))?.as_ref()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pumpkin_data::Block;

    use super::{InstallError, get, install};
    use crate::block::BlockBehaviour;

    struct Inert;
    impl BlockBehaviour for Inert {}

    #[test]
    fn rejected_entries_install_nothing() {
        assert_eq!(
            install(vec![("minecraft:stone", Arc::new(Inert))]),
            Err(InstallError::NotAnExtension("minecraft:stone".into()))
        );
        assert_eq!(
            install(vec![("verse:missing", Arc::new(Inert))]),
            Err(InstallError::UnknownBlock("verse:missing".into()))
        );
        assert!(get(Block::STONE.id).is_none());
    }
}
