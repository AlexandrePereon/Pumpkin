//! Biomes from datapacks: what a `worldgen/biome` file overriding a vanilla biome changes in
//! the terrain, its feature list (per decoration step) and its carvers.
//!
//! A biome lists placed features by id: a vanilla one (the generated key, overridden or not
//! by a datapack `worldgen/placed_feature` file, see [`super::datapack`]) or one of the packs'
//! own, which the generated enum cannot name. Both are a [`FeatureKey`]; the packs' own
//! features are built here and numbered in the order they are first met.
//!
//! The decoration order follows vanilla's `FeatureSorter` on the effective lists (see
//! [`crate::generation::feature_order`]): a feature added to a biome takes its place in the
//! global order of its step, shifting the indices, hence the decoration seeds, of those after
//! it, as in vanilla.
//!
//! A biome's other fields (climate, effects, spawners) are not read here: an overridden
//! vanilla biome keeps its compiled climate (`freeze_top_layer` reads it); a biome of a pack's
//! own namespace gets its climate from its file when it is built and installed beforehand
//! (see [`crate::biome::datapack`]), and its lists here like any other.
//!
//! Like the other worldgen overlays, installed once before the worlds are created.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::OnceLock;

use pumpkin_data::carver::CarverConfig;
use pumpkin_data::chunk::Biome;
use pumpkin_data::placed_feature::PlacedFeature as PlacedFeatureKey;
use serde_json::Value;

use super::datapack::FeatureParser;
use super::placed_features::PlacedFeature;
use crate::generation::noise::router::datapack::{Result, WorldgenJson, WorldgenRegistry, full_id};

/// A placed feature as a biome lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FeatureKey {
    /// A vanilla placed feature, by its generated key.
    Vanilla(PlacedFeatureKey),
    /// One of the datapacks' own placed features, by its number (see [`own_placed`]).
    Pack(u16),
}

/// The vanilla biomes datapacks override, built from their files.
#[derive(Default)]
pub struct DatapackBiomes {
    /// Per overridden biome (by id), its features per decoration step.
    features: HashMap<u8, Vec<Vec<FeatureKey>>>,
    /// Per overridden biome (by id), its carvers.
    carvers: HashMap<u8, Vec<&'static CarverConfig>>,
    /// The packs' own placed features, by [`FeatureKey::Pack`] number, with their ids
    /// (`None` for one written inline in a biome file).
    own: Vec<(Option<String>, PlacedFeature)>,
    /// The overridden biomes' ids, sorted.
    names: Vec<String>,
}

impl DatapackBiomes {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The overridden biomes' ids, sorted.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// How many of the packs' own placed features the biomes list.
    #[must_use]
    pub const fn own_count(&self) -> usize {
        self.own.len()
    }
}

/// The biome a datapack file is for: vanilla's, or one of the packs' own once installed
/// (`pumpkin_data::biome_extension`, see [`crate::biome::datapack`]).
fn known_biome(id: &str) -> Option<&'static Biome> {
    Biome::from_name(id.strip_prefix("minecraft:").unwrap_or(id))
}

/// The vanilla configured carvers, by id: those of the compiled plains (`cave`,
/// `cave_extra_underground`, `canyon`) and nether wastes (`nether_cave`), so that overridden biomes
/// listing the same carvers share them (the generator's `uniform_carvers` compares pointers).
fn carver(id: &str) -> Option<&'static CarverConfig> {
    let plains = Biome::PLAINS.carvers;
    match full_id(id).as_str() {
        "minecraft:cave" => plains.first().copied(),
        "minecraft:cave_extra_underground" => plains.get(1).copied(),
        "minecraft:canyon" => plains.get(2).copied(),
        "minecraft:nether_cave" => Biome::NETHER_WASTES.carvers.first().copied(),
        _ => None,
    }
}

/// A holder set as vanilla's codecs accept it: one id, or a list of ids and inline entries.
fn holder_list(value: &Value) -> Result<Vec<&Value>> {
    match value {
        Value::String(id) if id.starts_with('#') => {
            Err(format!("tag {id}: tags of worldgen entries are not read"))
        }
        Value::String(_) | Value::Object(_) => Ok(vec![value]),
        Value::Array(entries) => Ok(entries.iter().collect()),
        other => Err(format!("bad holder list {other}")),
    }
}

struct Builder<'j> {
    json: &'j WorldgenJson,
    parser: FeatureParser<'j>,
    own: Vec<(Option<String>, PlacedFeature)>,
    own_ids: HashMap<String, u16>,
    /// Building a pack's own biome: a named placed feature that cannot be read keeps its
    /// place in the list as a feature placing nothing (its error noted in `stubbed`), rather
    /// than failing the biome. A pack's own biome has no compiled list to fall back on, and
    /// every feature counts in the global feature order and so in the others' seeds.
    placeholders: bool,
    stubbed: Vec<String>,
}

impl Builder<'_> {
    fn new_own(&mut self, id: Option<String>, feature: PlacedFeature) -> Result<FeatureKey> {
        let number = u16::try_from(self.own.len())
            .map_err(|_| "too many datapack placed features".to_string())?;
        if let Some(id) = &id {
            self.own_ids.insert(id.clone(), number);
        }
        self.own.push((id, feature));
        Ok(FeatureKey::Pack(number))
    }

    fn feature(&mut self, entry: &Value) -> Result<FeatureKey> {
        match entry {
            Value::String(id) if id.starts_with('#') => {
                Err(format!("tag {id}: tags of placed features are not read"))
            }
            Value::String(id) => {
                let id = full_id(id);
                if let Some(key) = id
                    .strip_prefix("minecraft:")
                    .and_then(PlacedFeatureKey::from_name)
                {
                    return Ok(FeatureKey::Vanilla(key));
                }
                if let Some(number) = self.own_ids.get(&id) {
                    return Ok(FeatureKey::Pack(*number));
                }
                let feature = match self.parser.own_placed(&id) {
                    Ok(feature) => feature,
                    Err(error) if self.placeholders => {
                        self.stubbed.push(format!("{id}: {error}"));
                        PlacedFeature {
                            feature: super::placed_features::Feature::Inlined(Box::new(
                                super::configured_features::ConfiguredFeature::NoOp,
                            )),
                            placement: Vec::new(),
                        }
                    }
                    Err(error) => return Err(format!("{id}: {error}")),
                };
                self.new_own(Some(id), feature)
            }
            Value::Object(_) => {
                let feature = self.parser.placed_feature(entry)?;
                self.new_own(None, feature)
            }
            other => Err(format!("bad placed feature reference {other}")),
        }
    }

    fn biome(
        &mut self,
        value: &Value,
    ) -> Result<(Vec<Vec<FeatureKey>>, Vec<&'static CarverConfig>)> {
        let obj = value
            .as_object()
            .ok_or_else(|| format!("expected an object, got {value}"))?;
        let steps = match obj.get("features") {
            None => &[][..],
            Some(Value::Array(steps)) => steps.as_slice(),
            Some(other) => return Err(format!("\"features\" is not a list: {other}")),
        };
        let mut features = Vec::with_capacity(steps.len());
        for step in steps {
            let mut keys = Vec::new();
            for entry in holder_list(step)? {
                keys.push(self.feature(entry)?);
            }
            features.push(keys);
        }
        let mut carvers = Vec::new();
        if let Some(list) = obj.get("carvers") {
            for entry in holder_list(list)? {
                let id = entry
                    .as_str()
                    .ok_or_else(|| format!("inline carver {entry}: not read"))?;
                carvers.push(
                    carver(id).ok_or_else(|| format!("carver {id}: only vanilla's are known"))?,
                );
            }
        }
        Ok((features, carvers))
    }
}

impl WorldgenJson {
    /// Builds every datapack `worldgen/biome` file that overrides a vanilla biome. Returns
    /// them, and one message per file that failed or was skipped (that biome stays compiled).
    #[must_use]
    pub fn build_datapack_biomes(&self) -> (DatapackBiomes, Vec<String>) {
        let mut built = DatapackBiomes::default();
        let mut errors = Vec::new();
        let mut builder = Builder {
            json: self,
            parser: FeatureParser::new(self),
            own: Vec::new(),
            own_ids: HashMap::new(),
            placeholders: false,
            stubbed: Vec::new(),
        };
        for id in self.datapack_ids(WorldgenRegistry::Biome) {
            let Some(biome) = known_biome(&id) else {
                errors.push(format!(
                    "{id}: not a known biome (its own were not installed)"
                ));
                continue;
            };
            let Some((value, _)) = builder.json.get(WorldgenRegistry::Biome, &id) else {
                continue;
            };
            // A failed file leaves no feature of its own behind.
            let (own_before, ids_before) = (builder.own.len(), builder.own_ids.clone());
            builder.placeholders = !id.starts_with("minecraft:");
            match builder.biome(value) {
                Ok((features, carvers)) => {
                    built.features.insert(biome.id, features);
                    built.carvers.insert(biome.id, carvers);
                    built.names.push(id);
                }
                Err(error) => {
                    builder.own.truncate(own_before);
                    builder.own_ids = ids_before;
                    errors.push(format!("{id}: {error}"));
                }
            }
        }
        built.own = builder.own;
        built.names.sort_unstable();
        errors.extend(
            builder
                .stubbed
                .into_iter()
                .map(|e| format!("placed feature kept as one placing nothing: {e}")),
        );
        (built, errors)
    }
}

static INSTALLED: OnceLock<DatapackBiomes> = OnceLock::new();

/// Installs the biomes built from datapacks; returns false if some were already.
///
/// Once per process, before the worlds are created: worldgen registries are fixed for a
/// server's lifetime, as in vanilla.
pub fn install(biomes: DatapackBiomes) -> bool {
    INSTALLED.set(biomes).is_ok()
}

/// A datapack's feature list for `biome`, per step, if one overrides it.
#[must_use]
pub fn overridden_features(biome: &Biome) -> Option<&'static [Vec<FeatureKey>]> {
    INSTALLED.get()?.features.get(&biome.id).map(Vec::as_slice)
}

/// `biome`'s features per step: a datapack's list if one overrides it, else the compiled one.
#[must_use]
pub fn feature_keys(biome: &Biome) -> Vec<Vec<FeatureKey>> {
    overridden_features(biome).map_or_else(
        || {
            biome
                .features
                .iter()
                .map(|step| step.iter().copied().map(FeatureKey::Vanilla).collect())
                .collect()
        },
        <[Vec<FeatureKey>]>::to_vec,
    )
}

/// Whether `biome` lists `key` in any step (vanilla's `BiomeFilter`).
#[must_use]
pub fn has_feature(biome: &Biome, key: FeatureKey) -> bool {
    match (overridden_features(biome), key) {
        (Some(steps), key) => steps.iter().any(|step| step.contains(&key)),
        (None, FeatureKey::Vanilla(key)) => biome.features.iter().any(|step| step.contains(&key)),
        (None, FeatureKey::Pack(_)) => false,
    }
}

/// `biome`'s carvers: a datapack's if one overrides it, else the compiled ones.
#[must_use]
pub fn carvers(biome: &'static Biome) -> &'static [&'static CarverConfig] {
    INSTALLED
        .get()
        .and_then(|b| b.carvers.get(&biome.id))
        .map_or(biome.carvers, Vec::as_slice)
}

/// One of the datapacks' own placed features.
#[must_use]
pub fn own_placed(number: u16) -> Option<&'static PlacedFeature> {
    INSTALLED
        .get()?
        .own
        .get(usize::from(number))
        .map(|(_, feature)| feature)
}

thread_local! {
    /// The pack's own placed feature being decorated on this thread, if any: the generated
    /// enum the placement code carries as "this feature" cannot name it.
    static TOP_PACK_FEATURE: Cell<Option<u16>> = const { Cell::new(None) };
}

/// The key a placement passes as "this feature" for one of the packs' own features, which
/// the generated enum cannot name: never read for it, see [`top_feature`].
pub const PACK_FEATURE_STAND_IN: PlacedFeatureKey = PlacedFeatureKey::Acacia;

/// Runs `f`, the decoration of the top-level feature `key`, so that a biome filter inside it
/// tests `key` (vanilla's `PlacementContext.topFeature`).
pub fn decorating<R>(key: FeatureKey, f: impl FnOnce() -> R) -> R {
    let pack = match key {
        FeatureKey::Pack(number) => Some(number),
        FeatureKey::Vanilla(_) => None,
    };
    let previous = TOP_PACK_FEATURE.replace(pack);
    let result = f();
    TOP_PACK_FEATURE.set(previous);
    result
}

/// The top-level feature a biome filter tests: the pack's own one being decorated, if any,
/// else `this`, the vanilla key the placement carries.
#[must_use]
pub fn top_feature(this: PlacedFeatureKey) -> FeatureKey {
    TOP_PACK_FEATURE
        .get()
        .map_or(FeatureKey::Vanilla(this), FeatureKey::Pack)
}

#[cfg(test)]
pub mod tests {
    use std::fs;
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::generation::feature_order::{OVERWORLD_BIOMES, sort_features_per_step_with};

    const BIOMES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/datapack/data/minecraft/worldgen/biome"
    );

    /// Vanilla's biome files, given as if a datapack overrode every one of them.
    #[must_use]
    pub fn vanilla_biomes_as_datapack() -> WorldgenJson {
        let mut json = WorldgenJson::vanilla();
        for entry in fs::read_dir(Path::new(BIOMES)).unwrap().flatten() {
            let path = entry.path();
            let id = format!("minecraft:{}", path.file_stem().unwrap().to_string_lossy());
            let value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            json.insert_from_datapack(WorldgenRegistry::Biome, &id, value);
        }
        json
    }

    fn compiled(biome: &Biome) -> Vec<Vec<FeatureKey>> {
        biome
            .features
            .iter()
            .map(|step| step.iter().copied().map(FeatureKey::Vanilla).collect())
            .collect()
    }

    #[test]
    fn vanilla_json_rebuilds_the_compiled_lists() {
        let (built, errors) = vanilla_biomes_as_datapack().build_datapack_biomes();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(built.own_count(), 0);
        assert_eq!(built.names().len(), Biome::ALL.len());
        for biome in Biome::ALL {
            assert_eq!(
                built.features[&biome.id],
                compiled(biome),
                "{}",
                biome.registry_id
            );
            let carvers = &built.carvers[&biome.id];
            assert_eq!(carvers.len(), biome.carvers.len(), "{}", biome.registry_id);
            for (a, b) in carvers.iter().zip(biome.carvers) {
                assert_eq!(format!("{a:?}"), format!("{b:?}"), "{}", biome.registry_id);
            }
        }
    }

    /// Vanilla's `biome`, plus `extra` at the end of step 6 (underground ores).
    fn biome_with(biome: &str, extra: Value) -> Value {
        let text = fs::read_to_string(Path::new(BIOMES).join(format!("{biome}.json"))).unwrap();
        let mut value: Value = serde_json::from_str(&text).unwrap();
        value["features"][6].as_array_mut().unwrap().push(extra);
        value
    }

    fn pack_ore() -> Value {
        json!({
            "feature": {
                "type": "minecraft:ore",
                "discard_chance_on_air_exposure": 0.0,
                "size": 8,
                "targets": [{
                    "state": "minecraft:diamond_block",
                    "target": {"predicate_type": "minecraft:tag_match", "tag": "minecraft:stone_ore_replaceables"}
                }]
            },
            "placement": [
                {"type": "minecraft:count", "count": 4},
                {"type": "minecraft:in_square"},
                {"type": "minecraft:height_range", "height": {"type": "minecraft:uniform", "max_inclusive": {"absolute": 64}, "min_inclusive": {"above_bottom": 0}}},
                {"type": "minecraft:biome"}
            ]
        })
    }

    #[test]
    fn a_pack_feature_joins_a_vanilla_biome() {
        let mut json = WorldgenJson::vanilla();
        json.insert_from_datapack(
            WorldgenRegistry::PlacedFeature,
            "verse:diamond_ore",
            pack_ore(),
        );
        json.insert_from_datapack(
            WorldgenRegistry::Biome,
            "minecraft:plains",
            biome_with("plains", json!("verse:diamond_ore")),
        );
        // The same feature named again, and one written inline.
        json.insert_from_datapack(
            WorldgenRegistry::Biome,
            "minecraft:meadow",
            biome_with("meadow", json!("verse:diamond_ore")),
        );
        json.insert_from_datapack(
            WorldgenRegistry::Biome,
            "minecraft:forest",
            biome_with("forest", pack_ore()),
        );
        // Refused: a tag, an unknown feature, a biome of the pack's own.
        json.insert_from_datapack(
            WorldgenRegistry::Biome,
            "minecraft:desert",
            biome_with("desert", json!("#verse:ores")),
        );
        json.insert_from_datapack(
            WorldgenRegistry::Biome,
            "minecraft:taiga",
            biome_with("taiga", json!("verse:missing")),
        );
        json.insert_from_datapack(
            WorldgenRegistry::Biome,
            "verse:own",
            biome_with("plains", json!("verse:diamond_ore")),
        );

        let (built, errors) = json.build_datapack_biomes();
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert_eq!(
            built.names(),
            ["minecraft:forest", "minecraft:meadow", "minecraft:plains"]
        );
        assert_eq!(built.own_count(), 2);
        let plains = &built.features[&Biome::PLAINS.id];
        // Numbered as met, biomes in id order: forest's inline one, then the named one.
        assert_eq!(
            built.features[&Biome::FOREST.id][6].last(),
            Some(&FeatureKey::Pack(0))
        );
        assert_eq!(
            built.features[&Biome::MEADOW.id][6].last(),
            Some(&FeatureKey::Pack(1))
        );
        assert_eq!(plains[6].last(), Some(&FeatureKey::Pack(1)));
        let mut vanilla = compiled(&Biome::PLAINS);
        vanilla[6].push(FeatureKey::Pack(1));
        assert_eq!(plains, &vanilla);

        // The decoration order: the pack's feature after plains' last ore, every vanilla
        // feature in its vanilla order, those after it one index further.
        let lists = |biome: &Biome| {
            built
                .features
                .get(&biome.id)
                .cloned()
                .unwrap_or_else(|| compiled(biome))
        };
        let with_pack = sort_features_per_step_with(OVERWORLD_BIOMES, lists);
        let without = sort_features_per_step_with(OVERWORLD_BIOMES, compiled);
        let own = |k: &FeatureKey| matches!(k, FeatureKey::Pack(_));
        let step: Vec<FeatureKey> = with_pack[6].iter().copied().filter(|k| !own(k)).collect();
        assert_eq!(step, without[6]);
        assert_eq!(with_pack[6].len(), without[6].len() + 2);
        let at = with_pack[6]
            .iter()
            .position(|k| *k == FeatureKey::Pack(1))
            .unwrap();
        let last_plains_ore = FeatureKey::Vanilla(PlacedFeatureKey::DiskGravel);
        assert!(
            with_pack[6]
                .iter()
                .position(|k| *k == last_plains_ore)
                .unwrap()
                < at
        );
        for other in [0, 1, 2, 3, 4, 5, 7, 8, 9, 10] {
            assert_eq!(with_pack[other], without[other], "step {other}");
        }
    }

    #[test]
    fn biome_filter_follows_the_effective_lists() {
        // Nothing installed in this test binary: the compiled lists answer.
        assert!(has_feature(
            &Biome::PLAINS,
            FeatureKey::Vanilla(PlacedFeatureKey::OreDiamond)
        ));
        assert!(!has_feature(&Biome::PLAINS, FeatureKey::Pack(0)));
        assert!(!has_feature(
            &Biome::NETHER_WASTES,
            FeatureKey::Vanilla(PlacedFeatureKey::OreDiamond)
        ));
        assert_eq!(
            decorating(FeatureKey::Pack(3), || top_feature(PACK_FEATURE_STAND_IN)),
            FeatureKey::Pack(3)
        );
        assert_eq!(
            top_feature(PlacedFeatureKey::OreDiamond),
            FeatureKey::Vanilla(PlacedFeatureKey::OreDiamond)
        );
    }
}
