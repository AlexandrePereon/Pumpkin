//! Callbacks run first when a player uses a block, as mod loaders run theirs
//! (Fabric's `UseBlockCallback`, Balm's `BlockCallback.Use`).
//!
//! Vanilla skips a block's own use for a sneaking player holding an item, so a
//! [`BlockBehaviour`](super::BlockBehaviour) never sees that click. A callback
//! does, whatever the player holds and whether they sneak. Installed once,
//! before the server starts; with none installed a use costs one load.

use std::fmt;
use std::sync::{Arc, OnceLock};

use pumpkin_util::Hand;
use pumpkin_util::math::position::BlockPos;

use super::BlockHitResult;
use super::registry::BlockActionResult;
use crate::entity::player::Player;
use crate::server::Server;
use crate::world::World;

/// What a [`UseBlockCallback`] is told about the use.
pub struct UseBlockArgs<'a> {
    pub server: &'a Arc<Server>,
    pub world: &'a Arc<World>,
    pub player: &'a Arc<Player>,
    pub hand: Hand,
    pub position: &'a BlockPos,
    pub hit: &'a BlockHitResult<'a>,
}

/// Returns [`BlockActionResult::Pass`] to let the use go on; any other result
/// ends it there, the hand swung for [`BlockActionResult::SuccessServer`].
pub type UseBlockCallback = fn(&UseBlockArgs<'_>) -> BlockActionResult;

static CALLBACKS: OnceLock<Vec<UseBlockCallback>> = OnceLock::new();

/// [`install`] was already called.
#[derive(Debug)]
pub struct AlreadyInstalled;

impl fmt::Display for AlreadyInstalled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "block use callbacks are already installed")
    }
}

impl std::error::Error for AlreadyInstalled {}

/// Installs the callbacks, run in this order.
///
/// # Errors
///
/// Returns [`AlreadyInstalled`] on a second call; the first callbacks stay.
pub fn install(callbacks: Vec<UseBlockCallback>) -> Result<(), AlreadyInstalled> {
    CALLBACKS.set(callbacks).map_err(|_| AlreadyInstalled)
}

/// The first result other than [`BlockActionResult::Pass`], if any.
pub(crate) fn run(args: &UseBlockArgs<'_>) -> Option<BlockActionResult> {
    CALLBACKS
        .get()?
        .iter()
        .map(|callback| callback(args))
        .find(|result| !matches!(result, BlockActionResult::Pass))
}
