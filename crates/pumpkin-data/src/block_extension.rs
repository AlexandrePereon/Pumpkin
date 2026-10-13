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

use crate::block_properties::{BlockProperties, Property};
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
    /// The product of the property value counts is not the number of states.
    PropertyLayout {
        name: &'static str,
        expected_states: usize,
        found_states: usize,
    },
    /// Properties must come in Minecraft's order, sorted by name, without repeats.
    PropertiesNotSorted { name: &'static str },
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
            Self::PropertyLayout {
                name,
                expected_states,
                found_states,
            } => write!(
                f,
                "block {name} has {found_states} states but its properties describe {expected_states}"
            ),
            Self::PropertiesNotSorted { name } => {
                write!(f, "properties of block {name} are not sorted by name")
            }
        }
    }
}

impl std::error::Error for InstallError {}

/// One block-state property: its name and its values, in Minecraft's order.
#[derive(Debug)]
pub struct PropertyDef {
    pub name: &'static str,
    pub values: &'static [&'static str],
}

/// A block contributed to the extension, with the properties its states span.
pub struct ExtensionBlock {
    pub block: &'static Block,
    /// Sorted by name — the order Minecraft's `StateDefinition` uses — so that
    /// states are their cartesian product with the last property varying
    /// fastest. Empty for a block with a single state.
    pub properties: &'static [PropertyDef],
}

struct Extension {
    /// Indexed by `raw_id - BlockId::COUNT`.
    blocks: Vec<&'static Block>,
    /// Same indexing as `blocks`.
    properties: Vec<&'static [PropertyDef]>,
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
/// Returns [`InstallError`] when an id is out of sequence, a name collides, a
/// block's properties do not describe its states, or an extension is already
/// installed. Nothing is installed on error.
pub fn install(entries: Vec<ExtensionBlock>) -> Result<(), InstallError> {
    if EXTENSION.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }

    let mut blocks = Vec::with_capacity(entries.len());
    let mut properties = Vec::with_capacity(entries.len());
    let mut states = Vec::new();
    let mut owners = Vec::new();
    let mut by_name = BTreeMap::new();

    let mut next_block_id = BlockId::COUNT;
    let mut next_state_id = BlockStateId::COUNT;

    for entry in entries {
        let block = entry.block;
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

        if !entry
            .properties
            .windows(2)
            .all(|pair| pair[0].name < pair[1].name)
        {
            return Err(InstallError::PropertiesNotSorted { name: block.name });
        }
        let expected_states = entry
            .properties
            .iter()
            .map(|def| def.values.len())
            .product::<usize>();
        if expected_states != block.states.len() {
            return Err(InstallError::PropertyLayout {
                name: block.name,
                expected_states,
                found_states: block.states.len(),
            });
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
        if Block::from_name(block.name).is_some() || by_name.insert(block.name, block).is_some() {
            return Err(InstallError::DuplicateName { name: block.name });
        }

        blocks.push(block);
        properties.push(entry.properties);

        next_block_id = next_block_id.checked_add(1).ok_or(InstallError::TooMany)?;
    }

    let extension = Extension {
        blocks,
        properties,
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

// Off the vanilla hot path: only ids past the generated range get here.
#[cold]
#[inline(never)]
pub(crate) fn block_by_raw_id(raw: u16) -> Option<&'static Block> {
    let index = usize::from(raw.checked_sub(BlockId::COUNT)?);
    EXTENSION.get()?.blocks.get(index).copied()
}

#[inline]
pub(crate) fn block_by_name(name: &str) -> Option<&'static Block> {
    EXTENSION.get()?.by_name.get(name).copied()
}

// Off the vanilla hot path: only ids past the generated range get here.
#[cold]
#[inline(never)]
pub(crate) fn owner_of_state_id(raw: u16) -> Option<BlockId> {
    let index = usize::from(raw.checked_sub(BlockStateId::COUNT)?);
    EXTENSION.get()?.owners.get(index).copied()
}

// Off the vanilla hot path: only ids past the generated range get here.
#[cold]
#[inline(never)]
pub(crate) fn state_by_state_id(raw: u16) -> Option<&'static BlockState> {
    let index = usize::from(raw.checked_sub(BlockStateId::COUNT)?);
    EXTENSION.get()?.states.get(index).copied()
}

fn properties_of_block(block: &Block) -> Option<&'static [PropertyDef]> {
    let index = usize::from(block.id.as_u16().checked_sub(BlockId::COUNT)?);
    EXTENSION.get()?.properties.get(index).copied()
}

/// Properties of an extension block, computed at runtime.
///
/// Vanilla blocks get a generated type per property layout. Extension blocks
/// cannot, so this one carries the layout with it and keeps the state's index
/// within its block, from which every value can be decoded.
pub struct ExtensionProperties {
    properties: &'static [PropertyDef],
    index: u16,
}

impl ExtensionProperties {
    fn new(block: &Block, index: u16) -> Self {
        Self {
            properties: properties_of_block(block).unwrap_or_default(),
            index,
        }
    }

    /// The value index of each property, the last property varying fastest.
    fn value_indices(&self) -> Vec<usize> {
        let mut rest = usize::from(self.index);
        let mut indices = vec![0; self.properties.len()];
        for (slot, def) in indices.iter_mut().zip(self.properties).rev() {
            let len = def.values.len().max(1);
            *slot = rest % len;
            rest /= len;
        }
        indices
    }

    fn from_value_indices(properties: &'static [PropertyDef], indices: &[usize]) -> Self {
        let mut index = 0usize;
        for (def, value) in properties.iter().zip(indices) {
            index = index * def.values.len() + value;
        }
        Self {
            properties,
            index: u16::try_from(index).unwrap_or(0),
        }
    }
}

impl BlockProperties for ExtensionProperties {
    fn to_index(&self) -> u16 {
        self.index
    }

    /// Without a block there is no layout to decode against, so the result
    /// only round-trips through [`Self::to_index`] and [`Self::to_state_id`].
    fn from_index(index: u16) -> Self {
        Self {
            properties: &[],
            index,
        }
    }

    fn handles_block_id(id: BlockId) -> bool {
        block_by_raw_id(id.as_u16()).is_some()
    }

    fn to_state_id(&self, block: &Block) -> BlockStateId {
        block
            .states
            .get(usize::from(self.index))
            .map_or(block.default_state.id, |state| state.id)
    }

    fn from_state_id(id: BlockStateId, block: &Block) -> Self {
        let index = block
            .states
            .first()
            .and_then(|first| id.as_u16().checked_sub(first.id.as_u16()))
            .filter(|index| usize::from(*index) < block.states.len())
            .unwrap_or(0);
        Self::new(block, index)
    }

    fn default(block: &Block) -> Self {
        Self::from_state_id(block.default_state.id, block)
    }

    /// A vanilla name and value become their typed `Property`, so code written
    /// for vanilla properties (rotation, the debug stick) works on them; anything
    /// else is a `Property::Extension`.
    fn to_props(&self) -> Vec<Property> {
        self.properties
            .iter()
            .zip(self.value_indices())
            .filter_map(|(def, value)| {
                let value = *def.values.get(value)?;
                Some(
                    Property::from_str_pair(def.name, value).unwrap_or(Property::Extension {
                        key: def.name,
                        value,
                    }),
                )
            })
            .collect()
    }

    /// Starts from the default state and applies each recognised property,
    /// matched by name and value string whichever variant carries it. Unknown
    /// ones are ignored, as the generated types do; validating them is the
    /// caller's job.
    fn from_props(props: &[Property], block: &Block) -> Self {
        let default = Self::default(block);
        let mut indices = default.value_indices();
        for prop in props {
            let (name, value) = (prop.key_str(), prop.value_str());
            let found = default
                .properties
                .iter()
                .enumerate()
                .find(|(_, def)| def.name == name)
                .and_then(|(slot, def)| Some((slot, def.values.iter().position(|v| *v == value)?)));
            if let Some((slot, value_index)) = found {
                indices[slot] = value_index;
            }
        }
        Self::from_value_indices(default.properties, &indices)
    }
}

/// The generated `Block::properties` falls back here for blocks it does not know.
pub(crate) fn properties_of(
    block: &Block,
    state: BlockStateId,
) -> Option<Box<dyn BlockProperties>> {
    properties_of_block(block)?;
    Some(Box::new(ExtensionProperties::from_state_id(state, block)))
}

/// The generated `Block::from_properties` falls back here for blocks it does not know.
pub(crate) fn properties_from(
    block: &Block,
    props: &[Property],
) -> Option<Box<dyn BlockProperties>> {
    properties_of_block(block)?;
    Some(Box::new(ExtensionProperties::from_props(props, block)))
}

/// The generated `Property::from_str_pair` falls back here for a pair no
/// vanilla block has: a `Property::Extension` if an installed extension block
/// declares that name with that value. Off the vanilla path — only reached once
/// vanilla parsing failed.
pub(crate) fn extension_property(key: &str, value: &str) -> Option<Property> {
    EXTENSION
        .get()?
        .properties
        .iter()
        .flat_map(|defs| defs.iter())
        .filter(|def| def.name == key)
        .find_map(|def| {
            let value = def.values.iter().find(|v| **v == value)?;
            Some(Property::Extension {
                key: def.name,
                value,
            })
        })
}

#[cfg(test)]
mod tests {
    use super::{ExtensionBlock, PropertyDef, block_count, install, state_count};
    use crate::block_properties::{Property, PropertyKey};
    use crate::block_properties::NoteblockInstrument;
    use crate::block_state::PistonBehavior;
    use crate::{Block, BlockId, BlockState, BlockStateId};

    const MARKER: &str = "pumpkinverse_test:marker";
    const LAMP: &str = "pumpkinverse_test:lamp";
    const DIAL: &str = "pumpkinverse_test:dial";

    /// A property no vanilla block has, name and values alike.
    static DIAL_PROPERTIES: [PropertyDef; 1] = [PropertyDef {
        name: "colour",
        values: &["red", "blue"],
    }];

    /// The lamp from `testmod`, laid out as Minecraft's `StateDefinition` does:
    /// properties sorted by name, booleans as `[true, false]`.
    static LAMP_PROPERTIES: [PropertyDef; 2] = [
        PropertyDef {
            name: "facing",
            values: &["north", "south", "west", "east"],
        },
        PropertyDef {
            name: "lit",
            values: &["true", "false"],
        },
    ];

    fn leak_block(
        name: &'static str,
        id: BlockId,
        first_state: u16,
        states: u16,
        default: usize,
    ) -> &'static Block {
        let states: &'static [BlockState] = Box::leak(
            (0..states)
                .map(|offset| BlockState {
                    id: BlockStateId::extended(first_state + offset).unwrap_or(BlockStateId::AIR),
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
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        Box::leak(Box::new(Block {
            id,
            name,
            hardness: 1.0,
            blast_resistance: 1.0,
            map_color: 0,
            slipperiness: 0.6,
            velocity_multiplier: 1.0,
            jump_velocity_multiplier: 1.0,
            item_id: 0,
            default_state: &states[default],
            states,
            flammable: None,
            experience: None,
            loot_table: None,
        }))
    }

    /// There is deliberately one test here. `install` writes a process-wide
    /// `OnceLock`, so a second installing test would pass or fail depending on
    /// thread order.
    #[test]
    fn resolves_ids_and_properties_beyond_the_vanilla_range() {
        let vanilla_blocks = BlockId::COUNT;
        let vanilla_states = BlockStateId::COUNT;

        let marker_id = BlockId::extended(vanilla_blocks).unwrap_or(BlockId::AIR);
        let lamp_id = BlockId::extended(vanilla_blocks + 1).unwrap_or(BlockId::AIR);
        let marker_state = BlockStateId::extended(vanilla_states).unwrap_or(BlockStateId::AIR);
        let lamp_first = vanilla_states + 1;

        let marker = leak_block(MARKER, marker_id, vanilla_states, 1, 0);
        // The Extractor reports the lamp's default as its second state:
        // facing=north, lit=false.
        let lamp = leak_block(LAMP, lamp_id, lamp_first, 8, 1);
        let dial_id = BlockId::extended(vanilla_blocks + 2).unwrap_or(BlockId::AIR);
        let dial_first = lamp_first + 8;
        let dial = leak_block(DIAL, dial_id, dial_first, 2, 0);

        let installed = install(vec![
            ExtensionBlock {
                block: marker,
                properties: &[],
            },
            ExtensionBlock {
                block: lamp,
                properties: &LAMP_PROPERTIES,
            },
            ExtensionBlock {
                block: dial,
                properties: &DIAL_PROPERTIES,
            },
        ]);
        assert!(installed.is_ok(), "install rejected the blocks");

        assert_eq!(block_count(), vanilla_blocks + 3);
        assert_eq!(state_count(), vanilla_states + 11);

        // Every lookup path reaches the extension.
        assert_eq!(Block::from_name(MARKER).map(|b| b.name), Some(MARKER));
        assert_eq!(
            Block::from_registry_key(MARKER).map(|b| b.name),
            Some(MARKER)
        );
        assert_eq!(Block::from_id(marker_id).name, MARKER);
        assert_eq!(Block::from_state_id(marker_state).name, MARKER);
        assert_eq!(BlockState::from_id(marker_state).id, marker_state);
        assert_eq!(BlockId::from_state_id(marker_state), marker_id);

        // Vanilla still resolves through the generated tables.
        assert_eq!(Block::from_name("stone").map(|b| b.name), Some("stone"));
        assert_eq!(Block::from_id(BlockId::AIR).name, Block::AIR.name);
        assert_eq!(BlockId::from_state_id(BlockStateId::AIR), BlockId::AIR);

        // An id the extension does not declare falls back instead of indexing
        // out of bounds, which is what the removed `assert_unchecked` risked.
        let unknown = BlockId::extended(vanilla_blocks + 3).unwrap_or(BlockId::AIR);
        assert_eq!(Block::from_id(unknown).name, Block::AIR.name);

        // Properties go through the generated dispatch and fall back to the
        // extension. Expected ids are the Extractor's, from a real server:
        // the lamp spans 35724..=35731, last property varying fastest.
        let pairs_of = |props: Vec<Property>| -> Vec<(&str, &str)> {
            props.iter().map(|p| (p.key_str(), p.value_str())).collect()
        };
        let props = |offset: u16| {
            let id = BlockStateId::extended(lamp_first + offset).unwrap_or(BlockStateId::AIR);
            lamp.properties(id).map(|p| pairs_of(p.to_props()))
        };
        assert_eq!(props(0), Some(vec![("facing", "north"), ("lit", "true")]));
        assert_eq!(props(1), Some(vec![("facing", "north"), ("lit", "false")]));
        assert_eq!(props(6), Some(vec![("facing", "east"), ("lit", "true")]));
        assert_eq!(props(7), Some(vec![("facing", "east"), ("lit", "false")]));

        // Vanilla names and values come out typed, so vanilla tools apply.
        let typed = lamp.properties(lamp.default_state.id).map(|p| p.to_props());
        assert_eq!(
            typed.and_then(|props| props.first().map(|p| p.key())),
            Some(PropertyKey::Facing)
        );

        // Strings reach the lamp the way callers parse them, through
        // `Property::from_str_pair`; a pair no block declares parses to nothing.
        assert_eq!(Property::from_str_pair("colour", "green"), None);
        let state_of = |pairs: &[(&str, &str)]| {
            let props: Vec<Property> = pairs
                .iter()
                .filter_map(|(key, value)| Property::from_str_pair(key, value))
                .collect();
            lamp.from_properties(&props).to_state_id(lamp).as_u16()
        };
        assert_eq!(
            state_of(&[("facing", "east"), ("lit", "true")]),
            lamp_first + 6
        );
        assert_eq!(state_of(&[("facing", "south")]), lamp_first + 3);
        assert_eq!(state_of(&[]), lamp_first + 1);
        // Unknown names and values are ignored, leaving the default in place.
        assert_eq!(
            state_of(&[("facing", "up"), ("colour", "red")]),
            lamp_first + 1
        );

        // Every state round-trips through its properties.
        for offset in 0..8 {
            let id = BlockStateId::extended(lamp_first + offset).unwrap_or(BlockStateId::AIR);
            let props = lamp
                .properties(id)
                .map(|p| p.to_props())
                .unwrap_or_default();
            assert_eq!(
                lamp.from_properties(&props).to_state_id(lamp).as_u16(),
                lamp_first + offset
            );
        }

        // A property only an extension block has parses, through the vanilla
        // entry point, to `Property::Extension`, and round-trips.
        let blue = Property::from_str_pair("colour", "blue");
        assert_eq!(
            blue,
            Some(Property::Extension {
                key: "colour",
                value: "blue"
            })
        );
        let blue: Vec<Property> = blue.into_iter().collect();
        assert_eq!(
            dial.from_properties(&blue).to_state_id(dial).as_u16(),
            dial_first + 1
        );
        let dial_props = dial
            .properties(dial.default_state.id)
            .map(|p| p.to_props());
        assert_eq!(
            dial_props,
            Some(vec![Property::Extension {
                key: "colour",
                value: "red"
            }])
        );

        // A block with a single state has no properties to report.
        assert_eq!(
            marker.properties(marker_state).map(|p| p.to_props()),
            Some(vec![])
        );

        assert!(
            install(vec![ExtensionBlock {
                block: marker,
                properties: &[],
            }])
            .is_err(),
            "installed twice"
        );
    }
}
