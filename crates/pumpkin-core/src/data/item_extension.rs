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
//! reported, as the generated vanilla items skip it too. A default of a
//! component type of a mod's own has no reader: its value is kept as NBT, as the
//! component's codec writes it, for [`mod_component_default`].

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use pumpkin_data::data_component::DataComponent;
use pumpkin_data::data_component_extension;
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
    #[serde(default)]
    crafting_remainder: Option<DumpRemainder>,
}

/// `Item.getCraftingRemainder`, an `ItemStackTemplate`; only its item is kept,
/// as codegen does.
#[derive(Deserialize)]
struct DumpRemainder {
    id: String,
}

/// A component type's entry: its raw id, alone in older dumps, or with its
/// metadata (`persistent`, `ignore_swap_animation`), of which only the id is
/// read for now.
#[derive(Deserialize)]
#[serde(untagged)]
enum DumpComponentType {
    Id(u16),
    WithMetadata { id: u16 },
}

impl DumpComponentType {
    const fn id(&self) -> u16 {
        match self {
            Self::Id(id) | Self::WithMetadata { id } => *id,
        }
    }
}

/// Reads `data_component.json` from an Extractor dump and installs the
/// component types a mod declared, so registry sync can name them and item
/// stacks carrying them are not rejected.
pub fn install_component_types_from_dump(dir: &Path) {
    let path = dir.join("data_component.json");
    let dump: BTreeMap<String, DumpComponentType> = match fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(dump) => dump,
        Err(error) => {
            warn!("Could not load {}: {error}", path.display());
            return;
        }
    };

    let mut entries: Vec<(u16, &'static str)> = dump
        .into_iter()
        .filter(|(name, _)| !name.starts_with("minecraft:"))
        .map(|(name, entry)| (entry.id(), &*Box::leak(name.into_boxed_str())))
        .collect();
    if entries.is_empty() {
        return;
    }
    entries.sort_unstable_by_key(|(id, _)| *id);

    let count = entries.len();
    match data_component_extension::install(entries) {
        Ok(()) => info!("Installed {count} extension data component type(s)"),
        Err(error) => warn!("Could not install the data component extension: {error}"),
    }
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

    let mut defaults = BTreeMap::new();
    let items = entries
        .into_iter()
        .map(|(name, item)| leak_item(name, item, &mut defaults))
        .collect();
    if MOD_COMPONENT_DEFAULTS.set(defaults).is_err() {
        warn!("Mod component defaults were already installed");
    }
    Ok(items)
}

/// Defaults of mod component types on mod items, by (item id, component type id).
static MOD_COMPONENT_DEFAULTS: OnceLock<BTreeMap<(u16, u16), NbtTag>> = OnceLock::new();

/// The default of a mod's own component type on a mod item, as NBT.
///
/// That is `Item.Properties.component(type, value)`, in the shape the
/// component's codec writes; `None` when the item has none. Pumpkin cannot read
/// the value: code that knows the mod's codec can.
#[must_use]
pub fn mod_component_default(item: u16, component: u16) -> Option<&'static NbtTag> {
    MOD_COMPONENT_DEFAULTS.get()?.get(&(item, component))
}

fn leak_item(
    name: String,
    item: DumpItem,
    defaults: &mut BTreeMap<(u16, u16), NbtTag>,
) -> &'static Item {
    let DumpItem {
        id: raw_id,
        components: dumped,
        crafting_remainder,
    } = item;

    let mut components: Vec<(DataComponent, &'static dyn DataComponentImpl)> = Vec::new();
    for (component_name, value) in dumped {
        let Some(id) = DataComponent::try_from_name(&component_name) else {
            // A mod's own type: kept for code that knows its codec.
            if let Some(mod_id) = data_component_extension::id_of(&component_name) {
                defaults.insert((raw_id, mod_id), json_to_nbt(&value));
            }
            // Otherwise also what codegen does for vanilla: Pumpkin has no such
            // component.
            continue;
        };
        if let Some(component) = read_data(id, &json_to_nbt(&value)) {
            components.push((id, Box::leak(component)));
        } else {
            warn!("Item {name}: component {component_name} could not be read and is skipped");
        }
    }

    let crafting_remainder = crafting_remainder.and_then(|remainder| {
        // Only a vanilla item can be named: the extension is not installed yet.
        let item = Item::from_registry_key(&remainder.id);
        if item.is_none() {
            warn!("Item {name}: crafting remainder {} is unknown and is skipped", remainder.id);
        }
        item
    });

    Box::leak(Box::new(Item {
        id: raw_id,
        registry_key: Box::leak(name.into_boxed_str()),
        crafting_remainder,
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
