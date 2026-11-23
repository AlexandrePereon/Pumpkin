//! Biomes of a datapack's own namespace, and biome sources from datapack dimension files.
//!
//! A datapack biome that is not vanilla's becomes a [`Biome`] built at runtime from its file:
//! its climate (temperature, downfall, precipitation, temperature modifier), its boolean
//! environment attributes and its natural spawns. Its features and carvers are left empty
//! here and come from the biome overlay (`generation::feature::biome_datapack`), as for an
//! overridden vanilla biome. Ids continue vanilla's, in the order the synced registry appends
//! a pack's new entries: sorted by id. Installed into `pumpkin_data::biome_extension`.
//!
//! A dimension file (`data/minecraft/dimension/overworld.json`) whose generator has a
//! `multi_noise` source with an inline `biomes` list gives that dimension's biome source
//! ([`super::install_datapack_source`]); a `preset` one keeps the compiled tree.
//!
//! Not read: spawn costs (empty), weights of the natural spawns (Pumpkin's spawners have
//! none), a dimension's other fields (its noise settings and dimension type stay those of
//! the world).

use pumpkin_data::biome_extension;
use pumpkin_data::chunk::{Biome, Parameter, ParameterPoint, SpawnGroups, Spawner};
use pumpkin_data::environment_attribute::EnvironmentAttribute;
use pumpkin_util::biome::{TemperatureModifier, Weather};
use serde_json::Value;

use crate::generation::noise::router::datapack::{Result, WorldgenJson, WorldgenRegistry};

/// The ids of the datapacks' biomes that are not vanilla's, in the order the registry sent
/// to clients appends them: sorted by full id.
#[must_use]
pub fn own_biome_ids(json: &WorldgenJson) -> Vec<String> {
    let mut ids: Vec<String> = json
        .datapack_ids(WorldgenRegistry::Biome)
        .into_iter()
        .filter(|id| {
            id.strip_prefix("minecraft:")
                .is_none_or(|name| Biome::from_name(name).is_none())
        })
        .collect();
    ids.sort_unstable();
    ids
}

/// Builds the datapacks' own biomes, numbered from vanilla's count in [`own_biome_ids`] order.
///
/// Returns them, leaked, and one message per biome built with a field left out. A
/// biome is never skipped (that would renumber the ones after it, which the client's
/// registry still lists): an unreadable field falls back to plains'.
#[must_use]
pub fn build_own_biomes(json: &WorldgenJson) -> (Vec<&'static Biome>, Vec<String>) {
    let mut biomes = Vec::new();
    let mut warnings = Vec::new();
    for (offset, id) in own_biome_ids(json).into_iter().enumerate() {
        let Ok(number) = u8::try_from(Biome::ALL.len() + offset) else {
            warnings.push(format!("{id}: past 255 biomes, not built"));
            break;
        };
        let value = json
            .get(WorldgenRegistry::Biome, &id)
            .map_or(&Value::Null, |(value, _)| value);
        let biome = build_biome(number, &id, value, &mut |field: &str| {
            warnings.push(format!("{id}: {field}"));
        });
        biomes.push(&*Box::leak(Box::new(biome)));
    }
    (biomes, warnings)
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn build_biome(id: u8, name: &str, value: &Value, warn: &mut impl FnMut(&str)) -> Biome {
    let plains = &Biome::PLAINS;
    let number = |key: &str| value.get(key).and_then(Value::as_f64).map(|v| v as f32);
    let temperature = number("temperature").unwrap_or_else(|| {
        warn("no temperature, plains'");
        0.8
    });
    let downfall = number("downfall").unwrap_or(0.4);
    let has_precipitation = value
        .get("has_precipitation")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let temperature_modifier = match value.get("temperature_modifier").and_then(Value::as_str) {
        None | Some("none") => TemperatureModifier::None,
        Some("frozen") => TemperatureModifier::Frozen,
        Some(other) => {
            warn(&format!("temperature modifier {other} unknown, none"));
            TemperatureModifier::None
        }
    };
    let attributes = value.get("attributes").and_then(Value::as_object);
    let attribute = |key: &str| {
        attributes
            .and_then(|a| a.get(key))
            .map(|v| v.get("argument").unwrap_or(v))
    };
    let boolean_attributes: Vec<(EnvironmentAttribute, bool)> = attributes
        .into_iter()
        .flatten()
        .filter_map(|(key, v)| {
            let flag = v
                .as_bool()
                .or_else(|| v.get("argument").and_then(Value::as_bool))?;
            Some((EnvironmentAttribute::from_name(key)?, flag))
        })
        .collect();
    let creature_spawn_probability =
        attribute("minecraft:gameplay/creature_world_gen_spawn_probability")
            .or_else(|| value.get("creature_spawn_probability"))
            .and_then(Value::as_f64)
            .map_or(0.1, |v| v as f32);
    let spawns = attribute("minecraft:gameplay/natural_mob_spawns")
        .and_then(|a| a.get("spawns_by_category"));
    let group = |category: &str| -> &'static [Spawner] {
        let list = spawns
            .and_then(|s| s.get(category))
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(spawner).collect::<Vec<_>>())
            .unwrap_or_default();
        Box::leak(list.into_boxed_slice())
    };
    Biome {
        id,
        registry_id: leak(name.strip_prefix("minecraft:").unwrap_or(name).to_string()),
        be_network_id: plains.be_network_id,
        boolean_attributes: Box::leak(boolean_attributes.into_boxed_slice()),
        weather: Weather::new(
            has_precipitation,
            temperature,
            temperature_modifier,
            downfall,
        ),
        carvers: &[],
        features: &[],
        creature_spawn_probability,
        spawners: SpawnGroups {
            monster: group("monster"),
            ambient: group("ambient"),
            axolotls: group("axolotls"),
            creature: group("creature"),
            misc: group("misc"),
            underground_water_creature: group("underground_water_creature"),
            water_ambient: group("water_ambient"),
            water_creature: group("water_creature"),
        },
        spawn_costs: biome_extension::no_spawn_costs(),
    }
}

/// One natural spawn entry: `{type, count, weight}`, `count` a number or a uniform range.
fn spawner(entry: &Value) -> Option<Spawner> {
    let r#type = entry.get("type")?.as_str()?;
    let int = |v: &Value| v.as_i64().and_then(|n| i32::try_from(n).ok());
    let (min_count, max_count) = match entry.get("count") {
        Some(Value::Object(range)) => {
            let min = range.get("min_inclusive").and_then(int).unwrap_or(1);
            (min, range.get("max_inclusive").and_then(int).unwrap_or(min))
        }
        Some(count) => {
            let n = int(count).unwrap_or(1);
            (n, n)
        }
        None => (
            entry.get("minCount").and_then(int).unwrap_or(1),
            entry.get("maxCount").and_then(int).unwrap_or(1),
        ),
    };
    Some(Spawner {
        r#type: leak(r#type.to_string()),
        min_count,
        max_count,
    })
}

/// A climate parameter: one value, or `[min, max]`; quantized as vanilla's codec does.
fn parameter(value: Option<&Value>, key: &str) -> Result<Parameter> {
    let float = |v: &Value| {
        v.as_f64()
            .map(|f| f as f32)
            .ok_or_else(|| format!("{key}: not a number: {v}"))
    };
    match value {
        Some(Value::Array(range)) if range.len() == 2 => {
            let (min, max) = (float(&range[0])?, float(&range[1])?);
            if min > max {
                return Err(format!("{key}: min {min} above max {max}"));
            }
            Ok(Parameter::span(min, max))
        }
        Some(v) => Ok(Parameter::point(float(v)?)),
        None => Err(format!("{key} missing")),
    }
}

/// A `multi_noise` source's `biomes` list: each entry's parameter point and biome, in order.
/// The biomes must exist, vanilla's or installed.
///
/// # Errors
///
/// A message naming the first entry that cannot be read.
pub fn parse_multi_noise_biomes(list: &Value) -> Result<Vec<(ParameterPoint, &'static Biome)>> {
    let entries = list
        .as_array()
        .ok_or_else(|| "\"biomes\" is not a list".to_string())?;
    let mut out = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let name = entry
            .get("biome")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("entry {i}: no biome"))?;
        let biome = Biome::from_name(name.strip_prefix("minecraft:").unwrap_or(name))
            .ok_or_else(|| format!("entry {i}: unknown biome {name}"))?;
        let p = entry
            .get("parameters")
            .ok_or_else(|| format!("entry {i}: no parameters"))?;
        let get = |key: &str| parameter(p.get(key), key).map_err(|e| format!("entry {i}: {e}"));
        let offset = p
            .get("offset")
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("entry {i}: offset missing"))?;
        out.push((
            ParameterPoint::new(
                get("temperature")?,
                get("humidity")?,
                get("continentalness")?,
                get("erosion")?,
                get("depth")?,
                get("weirdness")?,
                pumpkin_data::chunk::quantize_coord(offset as f32),
            ),
            biome,
        ));
    }
    if out.is_empty() {
        return Err("empty biome list".to_string());
    }
    Ok(out)
}

/// A dimension file's biome list, if its generator is `noise` with an inline `multi_noise`
/// list; `None` for a preset or another source (the compiled one stays).
///
/// # Errors
///
/// When the inline list cannot be read.
pub fn dimension_biome_list(
    dimension: &Value,
) -> Result<Option<Vec<(ParameterPoint, &'static Biome)>>> {
    let Some(source) = dimension
        .get("generator")
        .and_then(|g| g.get("biome_source"))
    else {
        return Ok(None);
    };
    let kind = source.get("type").and_then(Value::as_str).unwrap_or("");
    if kind != "minecraft:multi_noise" && kind != "multi_noise" {
        return Ok(None);
    }
    source
        .get("biomes")
        .map_or(Ok(None), |list| parse_multi_noise_biomes(list).map(Some))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biome::RTree;
    use pumpkin_data::chunk::{BiomeTree, OVERWORLD_BIOME_SOURCE};

    const OVERWORLD_PARAMETERS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/multi_noise_biome_parameters/overworld.json"
    ));

    /// Vanilla's overworld list, as `--reports` writes it (`biome_parameters`).
    fn vanilla_list() -> Vec<(ParameterPoint, &'static Biome)> {
        let value: Value = serde_json::from_str(OVERWORLD_PARAMETERS).unwrap();
        parse_multi_noise_biomes(&value["biomes"]).unwrap()
    }

    fn same_tree(a: &BiomeTree, b: &BiomeTree, path: &str) {
        match (a, b) {
            (
                BiomeTree::Leaf {
                    parameters: pa,
                    biome: ba,
                },
                BiomeTree::Leaf {
                    parameters: pb,
                    biome: bb,
                },
            ) => {
                assert_eq!(pa, pb, "{path}");
                assert_eq!(ba.id, bb.id, "{path}");
            }
            (
                BiomeTree::Branch {
                    parameters: pa,
                    nodes: na,
                },
                BiomeTree::Branch {
                    parameters: pb,
                    nodes: nb,
                },
            ) => {
                assert_eq!(pa, pb, "{path}");
                assert_eq!(na.len(), nb.len(), "{path}");
                for (i, (x, y)) in na.iter().zip(nb.iter()).enumerate() {
                    same_tree(x, y, &format!("{path}/{i}"));
                }
            }
            _ => panic!("{path}: a leaf against a branch"),
        }
    }

    /// Vanilla's `Climate.RTree.create` on vanilla's list builds the very tree the
    /// Extractor dumped: same nodes, same order, same leaves.
    #[test]
    fn vanilla_list_builds_the_compiled_tree() {
        let list = vanilla_list();
        assert_eq!(list.len(), 7594);
        let built = RTree::create(list).leak_biome_tree();
        same_tree(built, &OVERWORLD_BIOME_SOURCE, "root");
    }

    #[test]
    fn dimension_files() {
        let preset = serde_json::json!({"type": "minecraft:overworld", "generator": {
            "type": "minecraft:noise", "settings": "minecraft:overworld",
            "biome_source": {"type": "minecraft:multi_noise", "preset": "minecraft:overworld"}}});
        assert!(dimension_biome_list(&preset).unwrap().is_none());
        let inline = serde_json::json!({"generator": {"biome_source": {
            "type": "minecraft:multi_noise",
            "biomes": [{"biome": "minecraft:plains", "parameters": {
                "temperature": [-1.0, 1.0], "humidity": 0.5, "continentalness": 0.0,
                "erosion": 0.0, "depth": 0.0, "weirdness": 0.0, "offset": 0.0}}]}}});
        let list = dimension_biome_list(&inline).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].1.id, Biome::PLAINS.id);
        assert_eq!(list[0].0.temperature, Parameter::span(-1.0, 1.0));
        assert_eq!(list[0].0.humidity, Parameter::point(0.5));
        let unknown = serde_json::json!({"generator": {"biome_source": {
            "type": "minecraft:multi_noise", "biomes": [{"biome": "verse:nowhere", "parameters": {}}]}}});
        assert!(dimension_biome_list(&unknown).is_err());
    }

    #[test]
    fn a_biome_from_its_file() {
        let value = serde_json::json!({
            "temperature": -0.5, "downfall": 0.9, "has_precipitation": true,
            "temperature_modifier": "frozen",
            "attributes": {
                "minecraft:gameplay/snow_golem_melts": true,
                "minecraft:gameplay/natural_mob_spawns": {"argument": {"spawns_by_category": {
                    "creature": [{"type": "minecraft:cow", "count": {"type": "minecraft:uniform",
                        "min_inclusive": 2, "max_inclusive": 4}, "weight": 8}]}}, "modifier": "overlay"}
            }
        });
        let mut warnings = Vec::new();
        let biome = build_biome(70, "verse:cold", &value, &mut |w: &str| {
            warnings.push(w.to_string());
        });
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(biome.id, 70);
        assert_eq!(biome.registry_id, "verse:cold");
        assert_eq!(biome.spawners.creature.len(), 1);
        assert_eq!(biome.spawners.creature[0].r#type, "minecraft:cow");
        assert_eq!(
            (
                biome.spawners.creature[0].min_count,
                biome.spawners.creature[0].max_count
            ),
            (2, 4)
        );
        assert_eq!(biome.boolean_attributes.len(), 1);
        assert!(biome.features.is_empty() && biome.carvers.is_empty());
        let plains = build_biome(71, "minecraft:my_plains", &Value::Null, &mut |_: &str| {});
        assert_eq!(plains.registry_id, "my_plains");
    }
}
