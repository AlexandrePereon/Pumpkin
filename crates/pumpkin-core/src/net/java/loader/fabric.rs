//! Fabric's registry synchronisation.
//!
//! Fabric makes the *server* authoritative over numeric registry ids: it sends a
//! map of registry to entry to raw id during the configuration phase, and the
//! client remaps its own registries to obey. So Pumpkin does not have to guess
//! what ids a modded client assigned locally — it assigns them.
//!
//! Block state ids follow for free. Fabric rebuilds its state table by walking
//! blocks in raw-id order and appending each block's states in its own order, so
//! dictating block raw ids fixes every state id too, provided each block's
//! property order is known.
//!
//! The wire format mirrors `net.fabricmc.fabric.impl.registry.sync.packet.RegistrySyncPayload`.

use pumpkin_data::block_properties::BLOCK_ENTITY_TYPES;
use pumpkin_data::data_component::DataComponent;
use pumpkin_data::data_component_extension;
use pumpkin_data::item::Item;
use pumpkin_data::{Block, BlockId, block_extension, item_extension};
use pumpkin_protocol::VarInt;
use pumpkin_protocol::ser::{NetworkWriteExt, WritingError};

use crate::block::entities::extension as block_entity_extension;

/// The plugin channel the payload travels on.
pub const REGISTRY_SYNC_CHANNEL: &str = "fabric:registry/sync";

/// Fabric only ever sends the `OPTIONAL` attribute; the rest stay local.
const ATTRIBUTE_OPTIONAL: u8 = 0x1;

/// One registry to synchronise.
pub struct SyncedRegistry<'a> {
    /// Namespaced registry id, for example `minecraft:block`.
    pub id: &'a str,
    /// Whether a client lacking registry sync may still connect.
    pub optional: bool,
    /// Every entry of the registry, vanilla included. Fabric sends the whole
    /// map rather than a modded subset, and a client that received a partial map
    /// would remap against it.
    pub entries: &'a [(&'a str, u16)],
}

/// Splits a possibly unprefixed name, the way Pumpkin stores vanilla ids.
fn split_name(name: &str) -> (&str, &str) {
    match name.split_once(':') {
        Some((namespace, path)) => (namespace, path),
        None => ("minecraft", name),
    }
}

/// Fabric writes the default namespace as an empty string to save bytes.
fn optimize_namespace(namespace: &str) -> &str {
    if namespace == "minecraft" {
        ""
    } else {
        namespace
    }
}

fn count(len: usize) -> Result<VarInt, WritingError> {
    i32::try_from(len)
        .map(VarInt)
        .map_err(|_| WritingError::Message(format!("registry sync count {len} exceeds i32")))
}

/// Groups by a key, keeping first-seen order. Fabric groups entry namespaces
/// through a `LinkedHashMap` for the same reason: the order is part of the
/// format, because raw-id deltas are measured against whatever was written last.
fn group_by_first_seen<'a, T>(
    items: impl IntoIterator<Item = (&'a str, T)>,
) -> Vec<(&'a str, Vec<T>)> {
    let mut groups: Vec<(&'a str, Vec<T>)> = Vec::new();
    for (key, value) in items {
        if let Some(slot) = groups.iter_mut().find(|(seen, _)| *seen == key) {
            slot.1.push(value);
        } else {
            groups.push((key, vec![value]));
        }
    }
    groups
}

/// Encodes the `fabric:registry/sync` payload body, without the channel name.
///
/// # Errors
///
/// Returns [`WritingError`] if a count does not fit in an `i32`, or if raw ids
/// do not ascend across the registry. The format encodes each run of ids as a
/// delta from the last id written, so a run that moved backwards could not be
/// represented.
pub fn registry_sync_payload(registries: &[SyncedRegistry<'_>]) -> Result<Vec<u8>, WritingError> {
    let grouped = group_by_first_seen(registries.iter().map(|reg| (split_name(reg.id).0, reg)));

    let mut out = Vec::new();
    out.write_var_int(&count(grouped.len())?)?;

    for (namespace, regs) in grouped {
        out.write_string(optimize_namespace(namespace))?;
        out.write_var_int(&count(regs.len())?)?;

        for reg in regs {
            out.write_string(split_name(reg.id).1)?;
            out.write_u8(if reg.optional { ATTRIBUTE_OPTIONAL } else { 0 })?;
            write_entries(&mut out, reg.entries)?;
        }
    }

    Ok(out)
}

fn write_entries(out: &mut Vec<u8>, entries: &[(&str, u16)]) -> Result<(), WritingError> {
    let grouped = group_by_first_seen(entries.iter().map(|&(name, raw_id)| {
        let (namespace, path) = split_name(name);
        (namespace, (path, raw_id))
    }));

    out.write_var_int(&count(grouped.len())?)?;

    // Reset per registry, carried across namespace groups: Fabric measures each
    // delta against the last raw id written, whichever group it landed in.
    let mut previous_last_raw_id: u16 = 0;

    for (namespace, mut ids) in grouped {
        ids.sort_by_key(|&(_, raw_id)| raw_id);

        // One bulk per run of consecutive raw ids.
        let mut bulks: Vec<&[(&str, u16)]> = Vec::new();
        let mut start = 0;
        for index in 1..ids.len() {
            if ids[index].1 != ids[index - 1].1.saturating_add(1) {
                bulks.push(&ids[start..index]);
                start = index;
            }
        }
        if !ids.is_empty() {
            bulks.push(&ids[start..]);
        }

        out.write_string(optimize_namespace(namespace))?;
        out.write_var_int(&count(bulks.len())?)?;

        for bulk in bulks {
            let Some((_, first_raw_id)) = bulk.first() else {
                continue;
            };
            let delta = first_raw_id
                .checked_sub(previous_last_raw_id)
                .ok_or_else(|| {
                    WritingError::Message(format!(
                        "raw id {first_raw_id} is below the previous {previous_last_raw_id}; \
                     registry sync cannot encode a backwards run"
                    ))
                })?;

            out.write_var_int(&VarInt(i32::from(delta)))?;
            out.write_var_int(&count(bulk.len())?)?;
            for &(path, _) in bulk {
                out.write_string(path)?;
            }

            if let Some(&(_, last_raw_id)) = bulk.last() {
                previous_last_raw_id = last_raw_id;
            }
        }
    }

    Ok(())
}

/// Builds the payload for every registry the runtime extensions added to, or
/// `None` when none did.
///
/// Returning `None` is the safe default rather than an optimisation. The client
/// remaps its registries to whatever map it receives, so sending a vanilla-only
/// map could disturb the entries of a modded client that works against Pumpkin
/// today. Fabric reaches the same place from the other side: it skips registries
/// that carry no modded content, so a registry is sent only once it holds an
/// extension entry.
///
/// Each registry is sent whole, vanilla included, because that is what Fabric
/// sends. The vanilla half is a no-op for a client whose vanilla ids already
/// match, and omitting it would leave those entries unmapped.
///
/// # Errors
///
/// Propagates [`WritingError`] from the encoder.
pub fn extension_registry_sync_payload() -> Result<Option<Vec<u8>>, WritingError> {
    let block_total = block_extension::block_count();
    let blocks: Vec<(&'static str, u16)> = if block_total > BlockId::COUNT {
        (0..block_total)
            .filter_map(BlockId::registered)
            .map(|id| (Block::from_id(id).name, id.as_u16()))
            .collect()
    } else {
        Vec::new()
    };

    let item_total = item_extension::item_count();
    let items: Vec<(&'static str, u16)> = if item_total > Item::VANILLA_COUNT {
        (0..item_total)
            .filter_map(Item::from_id)
            .map(|item| (item.registry_key, item.id))
            .collect()
    } else {
        Vec::new()
    };

    let component_total = data_component_extension::count();
    let components: Vec<(&'static str, u16)> =
        if component_total > data_component_extension::VANILLA_COUNT {
            (0..component_total)
                .filter_map(|id| {
                    let name = u8::try_from(id)
                        .ok()
                        .and_then(DataComponent::try_from_id)
                        .map(DataComponent::to_name)
                        .or_else(|| data_component_extension::name_of(id))?;
                    Some((name, id))
                })
                .collect()
        } else {
            Vec::new()
        };

    let block_entity_total = block_entity_extension::count();
    let vanilla_block_entities = BLOCK_ENTITY_TYPES.len();
    let block_entities: Vec<(&'static str, u16)> =
        if usize::from(block_entity_total) > vanilla_block_entities {
            (0..block_entity_total)
                .filter_map(|id| {
                    let name = BLOCK_ENTITY_TYPES
                        .get(usize::from(id))
                        .copied()
                        .or_else(|| block_entity_extension::name_of(id))?;
                    Some((name, id))
                })
                .collect()
        } else {
            Vec::new()
        };

    let menus = crate::data::menu_extension::registry_entries();

    // None of these registries is OPTIONAL on a Fabric server: a client that cannot
    // receive the map has no business holding modded content.
    //
    // Order: Fabric walks `BuiltInRegistries.REGISTRY.keySet()`, and the keys of
    // a `MappedRegistry` live in a `java.util.HashMap`, so registries come in
    // hash-bucket order — item, block_entity_type, data_component_type, menu,
    // block on 26.3, as captured from a real Fabric server by the differential probe. The
    // client reads the payload into an insertion-ordered map, so matching the
    // order keeps it identical to what a Fabric client normally gets; the probe
    // flags a mismatch when another registry joins.
    let registries: Vec<SyncedRegistry<'_>> = [
        ("minecraft:item", items.as_slice()),
        ("minecraft:block_entity_type", block_entities.as_slice()),
        ("minecraft:data_component_type", components.as_slice()),
        ("minecraft:menu", menus.as_slice()),
        ("minecraft:block", blocks.as_slice()),
    ]
    .into_iter()
    .filter(|(_, entries)| !entries.is_empty())
    .map(|(id, entries)| SyncedRegistry {
        id,
        optional: false,
        entries,
    })
    .collect();

    if registries.is_empty() {
        return Ok(None);
    }
    registry_sync_payload(&registries).map(Some)
}

#[cfg(test)]
mod tests {
    use super::{SyncedRegistry, registry_sync_payload};

    /// Byte-for-byte against the format `RegistrySyncPayload::read` expects.
    /// A round trip would pass even if encoder and decoder shared a mistake, so
    /// this pins the actual bytes instead.
    #[test]
    fn payload_matches_fabric_wire_format() {
        let entries = [("stone", 0u16), ("dirt", 1), ("testmod:marker", 2)];
        let registries = [SyncedRegistry {
            id: "minecraft:block",
            optional: false,
            entries: &entries,
        }];

        #[rustfmt::skip]
        let expected: Vec<u8> = vec![
            1,                                  // one registry-namespace group
            0,                                  // namespace "" means minecraft
            1,                                  // one registry in the group
            5, b'b', b'l', b'o', b'c', b'k',    // registry path
            0,                                  // attributes: not OPTIONAL
            2,                                  // two entry-namespace groups
            0,                                  //   namespace "" means minecraft
            1,                                  //   one bulk
            0,                                  //     delta from 0
            2,                                  //     two consecutive ids
            5, b's', b't', b'o', b'n', b'e',    //     id 0
            4, b'd', b'i', b'r', b't',          //     id 1
            7, b't', b'e', b's', b't', b'm', b'o', b'd',
            1,                                  //   one bulk
            1,                                  //     delta from the last id, 1
            1,                                  //     one id
            6, b'm', b'a', b'r', b'k', b'e', b'r',
        ];

        let payload = registry_sync_payload(&registries).map_err(|error| error.to_string());
        assert_eq!(payload, Ok(expected));
    }

    /// Non-consecutive ids must split into separate bulks, each delta measured
    /// from the previous bulk's last id.
    #[test]
    fn gaps_split_into_bulks() {
        let entries = [("a", 0u16), ("b", 1), ("c", 5)];
        let registries = [SyncedRegistry {
            id: "minecraft:block",
            optional: true,
            entries: &entries,
        }];

        #[rustfmt::skip]
        let expected: Vec<u8> = vec![
            1, 0, 1,
            5, b'b', b'l', b'o', b'c', b'k',
            1,                      // attributes: OPTIONAL
            1,                      // one entry-namespace group
            0,                      //   minecraft
            2,                      //   two bulks
            0, 2, 1, b'a', 1, b'b', //     ids 0..1
            4, 1, 1, b'c',          //     id 5: delta 4 from last id 1
        ];

        let payload = registry_sync_payload(&registries).map_err(|error| error.to_string());
        assert_eq!(payload, Ok(expected));
    }

    /// The format cannot express a run that moves backwards, so say so instead
    /// of emitting a payload the client would misread.
    #[test]
    fn backwards_ids_are_rejected() {
        let entries = [("mod:late", 9u16), ("early", 1)];
        let registries = [SyncedRegistry {
            id: "minecraft:block",
            optional: false,
            entries: &entries,
        }];

        assert!(registry_sync_payload(&registries).is_err());
    }
}
