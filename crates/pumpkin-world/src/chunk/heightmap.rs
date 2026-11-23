//! What raises each heightmap, as vanilla's `Heightmap.Types` decides it.
//!
//! Since 26.1 vanilla no longer asks whether a block is solid: `OCEAN_FLOOR`
//! (and its worldgen twin) is the block tag `#minecraft:blocks_motion_in_heightmap`,
//! `MOTION_BLOCKING` that tag or a fluid, `MOTION_BLOCKING_NO_LEAVES`
//! `#minecraft:blocks_motion_in_heightmap_no_leaves` or a fluid. The legacy
//! "solid" flag agrees with the tags for vanilla blocks except signs (solid,
//! but in neither tag), and not at all for a block a datapack leaves out of
//! them, such as a mod's: a canopy of solid mod blocks raises Pumpkin's
//! heightmaps where vanilla's ignore it, and the features that read them
//! (tree decorators, placement modifiers) then diverge.
//!
//! The tags are read once, as the datapacks left them (the tag extension is
//! installed before any world is generated), into one bit per block id.

use std::sync::LazyLock;

use pumpkin_data::tag::{RegistryKey, get_tag_ids};
use pumpkin_data::{BlockId, BlockState};

struct BlockBits(Box<[u64]>);

impl BlockBits {
    fn of_tag(name: &str) -> Self {
        let ids = get_tag_ids(RegistryKey::Block, name).unwrap_or_default();
        let len = ids.iter().max().map_or(0, |max| usize::from(*max) / 64 + 1);
        let mut bits = vec![0u64; len].into_boxed_slice();
        for id in ids {
            bits[usize::from(*id) / 64] |= 1 << (id % 64);
        }
        Self(bits)
    }

    #[inline]
    fn contains(&self, id: u16) -> bool {
        self.0
            .get(usize::from(id) / 64)
            .is_some_and(|word| word >> (id % 64) & 1 != 0)
    }
}

static MOTION: LazyLock<BlockBits> =
    LazyLock::new(|| BlockBits::of_tag("minecraft:blocks_motion_in_heightmap"));
static MOTION_NO_LEAVES: LazyLock<BlockBits> =
    LazyLock::new(|| BlockBits::of_tag("minecraft:blocks_motion_in_heightmap_no_leaves"));

/// Vanilla's `!state.getFluidState().isEmpty()`: a fluid block or a
/// waterlogged one. Blocks that are always water inside (kelp, seagrass,
/// bubble columns) are not counted: they sit under water, which counts.
#[inline]
const fn has_fluid(state: &BlockState) -> bool {
    state.is_liquid() || state.is_waterlogged()
}

/// `OCEAN_FLOOR` and `OCEAN_FLOOR_WG`.
#[inline]
#[must_use]
pub fn raises_ocean_floor(block: BlockId) -> bool {
    MOTION.contains(block.as_u16())
}

/// `MOTION_BLOCKING`.
#[inline]
#[must_use]
pub fn raises_motion_blocking(state: &BlockState, block: BlockId) -> bool {
    MOTION.contains(block.as_u16()) || has_fluid(state)
}

/// `MOTION_BLOCKING_NO_LEAVES`.
#[inline]
#[must_use]
pub fn raises_motion_blocking_no_leaves(state: &BlockState, block: BlockId) -> bool {
    MOTION_NO_LEAVES.contains(block.as_u16()) || has_fluid(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_data::Block;

    const fn state(block: &Block) -> &'static BlockState {
        block.default_state
    }

    #[test]
    fn heightmaps_follow_the_tags() {
        for block in [&Block::STONE, &Block::WHITE_WOOL, &Block::OAK_LOG] {
            assert!(raises_ocean_floor(block.id), "{}", block.name);
            assert!(raises_motion_blocking_no_leaves(state(block), block.id));
        }
        // Leaves: everything but the no-leaves map.
        assert!(raises_motion_blocking(
            state(&Block::OAK_LEAVES),
            Block::OAK_LEAVES.id
        ));
        assert!(!raises_motion_blocking_no_leaves(
            state(&Block::OAK_LEAVES),
            Block::OAK_LEAVES.id
        ));
        // Solid, but in neither tag.
        assert!(state(&Block::OAK_SIGN).is_solid());
        assert!(!raises_ocean_floor(Block::OAK_SIGN.id));
        assert!(!raises_motion_blocking(
            state(&Block::OAK_SIGN),
            Block::OAK_SIGN.id
        ));
        // A fluid raises the motion maps only.
        assert!(!raises_ocean_floor(Block::WATER.id));
        assert!(raises_motion_blocking_no_leaves(
            state(&Block::WATER),
            Block::WATER.id
        ));
        assert!(!raises_motion_blocking(state(&Block::AIR), Block::AIR.id));
    }
}
