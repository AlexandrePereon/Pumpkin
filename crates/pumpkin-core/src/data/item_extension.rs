//! Loads items that are not in the generated vanilla set.
//!
//! The item counterpart of [`super::block_extension`], fed by the same
//! Extractor dump. Every non-vanilla entry of its `items.json` is installed into
//! `pumpkin_data::item_extension` with the id and default components a real
//! server assigned.
//!
//! Components are read through `data_component_impl::read_data`, the same
//! readers that load item stacks from saves: the dump's JSON is converted to the
//! NBT shape they expect. A component Pumpkin has no reader for is skipped and
//! reported, as the generated vanilla items skip it too.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use pumpkin_data::data_component::DataComponent;
use pumpkin_data::data_component_impl::{DataComponentImpl, read_data};
use pumpkin_data::item::Item;
use pumpkin_data::item_extension;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use serde::Deserialize;
use serde_json::Value;
use tracing::{info, warn};

#[derive(Deserialize)]
struct DumpItem {
    id: u16,
    components: BTreeMap<String, Value>,
}

/// Reads `items.json` from an Extractor dump and installs its non-vanilla items.
pub fn install_from_dump(dir: &Path) {
    let items = match from_dump(dir) {
        Ok(items) => items,
        Err(error) => {
            warn!(
                "Could not load the items of the Extractor dump at {}: {error}",
                dir.display()
            );
            return;
        }
    };
    if items.is_empty() {
        return;
    }

    let count = items.len();
    match item_extension::install(items) {
        Ok(()) => info!("Installed {count} extension item(s)"),
        Err(error) => warn!("Could not install the item extension: {error}"),
    }
}

fn from_dump(dir: &Path) -> Result<Vec<&'static Item>, String> {
    let path = dir.join("items.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let dump: BTreeMap<String, DumpItem> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;

    // Vanilla names are bare paths; anything namespaced came from a mod.
    let mut entries: Vec<(String, DumpItem)> = dump
        .into_iter()
        .filter(|(name, _)| name.contains(':'))
        .collect();
    entries.sort_by_key(|(_, item)| item.id);

    Ok(entries
        .into_iter()
        .map(|(name, item)| leak_item(name, item))
        .collect())
}

fn leak_item(name: String, item: DumpItem) -> &'static Item {
    let DumpItem {
        id: raw_id,
        components: dumped,
    } = item;

    let mut components: Vec<(DataComponent, &'static dyn DataComponentImpl)> = Vec::new();
    for (component_name, value) in dumped {
        let Some(id) = DataComponent::try_from_name(&component_name) else {
            // Also what codegen does for vanilla: Pumpkin has no such component.
            continue;
        };
        if let Some(component) = read_data(id, &json_to_nbt(&value)) {
            components.push((id, Box::leak(component)));
        } else {
            warn!("Item {name}: component {component_name} could not be read and is skipped");
        }
    }

    Box::leak(Box::new(Item {
        id: raw_id,
        registry_key: Box::leak(name.into_boxed_str()),
        components: Box::leak(components.into_boxed_slice()),
    }))
}

/// Converts a component's JSON, as Minecraft's codecs write it, into the NBT
/// they would write for the same value. JSON has one number type, so integers
/// become `Int` (`Long` if they do not fit) and other numbers `Double` — what
/// the component readers expect for the components mods commonly declare.
fn json_to_nbt(value: &Value) -> NbtTag {
    match value {
        Value::Null => NbtTag::End,
        Value::Bool(b) => NbtTag::Byte(i8::from(*b)),
        Value::Number(number) => number.as_i64().map_or_else(
            || NbtTag::Double(number.as_f64().unwrap_or_default()),
            |int| i32::try_from(int).map_or(NbtTag::Long(int), NbtTag::Int),
        ),
        Value::String(string) => NbtTag::String(string.as_str().into()),
        Value::Array(values) => NbtTag::List(values.iter().map(json_to_nbt).collect()),
        Value::Object(fields) => {
            let mut compound = NbtCompound::new();
            for (key, field) in fields {
                compound.put(key, json_to_nbt(field));
            }
            NbtTag::Compound(compound)
        }
    }
}
