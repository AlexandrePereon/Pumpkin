//! Surface rules built at runtime from worldgen JSON.
//!
//! The vanilla surface rules are compiled (`pumpkin_data::material_rule`). A datapack can
//! override any `worldgen/material_rule` or `worldgen/material_condition` file, add its own,
//! or point a noise settings file at another rule (`material_rule`). Such a pack changes the
//! surface only if the server builds the affected rules from the merged JSON at startup; this
//! module does that, against the vanilla JSON (embedded, `pumpkin_data::worldgen_json`) and the
//! datapacks' files, into the same [`MaterialRule`] values the codegen emits, so the surface
//! step runs unchanged.
//!
//! The reading follows `tools/pumpkin-codegen/src/material_rule.rs`: for the vanilla files
//! the result is the compiled rule (see the tests). Only the rules of noise settings that a
//! datapack touches are built; every other dimension keeps its compiled rule.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use pumpkin_data::biome::Biome;
use pumpkin_data::chunk::DoublePerlinNoiseParameters;
use pumpkin_data::material_rule::{
    AboveYMaterialCondition, BadLandsMaterialRule, BiomeMaterialCondition, BlockMaterialRule,
    ConditionMaterialRule, HoleMaterialCondition, MaterialCondition, MaterialRule,
    NoiseThresholdMaterialCondition, NotMaterialCondition, OreVeinMaterialRule,
    SequenceMaterialRule, StoneDepthMaterialCondition, SurfaceMaterialCondition,
    VerticalGradientMaterialCondition, WaterMaterialCondition,
};
use pumpkin_util::math::vertical_surface_type::VerticalSurfaceType;
use pumpkin_util::y_offset::{AboveBottom, Absolute, BelowTop, YOffset};
use serde_json::{Map, Value};

use crate::generation::noise::router::datapack::{
    Result, WorldgenJson, WorldgenRegistry, block_state, full_id, leak,
};

/// Rule and condition references nested deeper than this are taken for a cycle.
const MAX_DEPTH: usize = 256;

impl WorldgenJson {
    /// Builds the surface rule of every noise settings entry that a datapack touches: the
    /// settings file itself, or a rule, condition or noise its rule references. Returns the
    /// built rules by noise settings id, and one message per entry that failed (that entry
    /// keeps its compiled rule).
    #[must_use]
    pub fn build_datapack_surface_rules(
        &self,
    ) -> (Vec<(String, &'static MaterialRule)>, Vec<String>) {
        let mut built = Vec::new();
        let mut errors = Vec::new();
        if !self.has_datapack_entries() {
            return (built, errors);
        }
        let ids: Vec<String> = self
            .ids(WorldgenRegistry::NoiseSettings)
            .map(str::to_string)
            .collect();
        for id in ids {
            match self.build_surface_rule(&id, false) {
                Ok(Some(rule)) => built.push((id, rule)),
                Ok(None) => {}
                Err(error) => errors.push(format!("{id}: {error}")),
            }
        }
        (built, errors)
    }

    /// Builds the surface rule a noise settings entry names. With `force` unset, returns
    /// `None` when no datapack entry takes part in it.
    pub fn build_surface_rule(
        &self,
        settings_id: &str,
        force: bool,
    ) -> Result<Option<&'static MaterialRule>> {
        let (settings, from_datapack) = self
            .get(WorldgenRegistry::NoiseSettings, settings_id)
            .ok_or_else(|| format!("no noise settings {settings_id}"))?;
        // 26.3 names the rule (`material_rule`); older files inline it (`surface_rule`).
        let Some(rule) = settings
            .get("material_rule")
            .or_else(|| settings.get("surface_rule"))
        else {
            return Ok(None);
        };
        let mut parser = RuleParser {
            json: self,
            touched: *from_datapack,
            depth: 0,
            next_noise_id: DoublePerlinNoiseParameters::COUNT,
        };
        let rule = parser.rule(rule)?;
        if !force && !parser.touched {
            return Ok(None);
        }
        Ok(Some(leak(rule)))
    }
}

static INSTALLED: OnceLock<BTreeMap<String, &'static MaterialRule>> = OnceLock::new();

/// Installs the surface rules built from datapacks, by noise settings id; returns false if
/// some were already. Once per process, before the worlds are created.
pub fn install(built: Vec<(String, &'static MaterialRule)>) -> bool {
    INSTALLED
        .set(
            built
                .into_iter()
                .map(|(id, rule)| (full_id(&id), rule))
                .collect(),
        )
        .is_ok()
}

/// The surface rule a datapack built for the noise settings `id`, if any.
#[must_use]
pub fn installed(id: &str) -> Option<&'static MaterialRule> {
    INSTALLED.get()?.get(&full_id(id)).copied()
}

struct RuleParser<'a> {
    json: &'a WorldgenJson,
    /// Whether a datapack entry took part.
    touched: bool,
    depth: usize,
    next_noise_id: usize,
}

fn object<'v>(value: &'v Value, what: &str) -> Result<&'v Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| format!("{what} is not an object: {value}"))
}

fn int(obj: &Map<String, Value>, key: &str) -> Result<i64> {
    obj.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("missing integer {key}"))
}

fn small<T: TryFrom<i64>>(obj: &Map<String, Value>, key: &str) -> Result<T> {
    let value = int(obj, key)?;
    T::try_from(value).map_err(|_| format!("{key} out of range: {value}"))
}

fn boolean(obj: &Map<String, Value>, key: &str) -> Result<bool> {
    obj.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("missing boolean {key}"))
}

fn float(obj: &Map<String, Value>, key: &str) -> Result<f64> {
    obj.get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing number {key}"))
}

fn string<'v>(obj: &'v Map<String, Value>, key: &str) -> Result<&'v str> {
    obj.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string {key}"))
}

fn field<'v>(obj: &'v Map<String, Value>, key: &str) -> Result<&'v Value> {
    obj.get(key).ok_or_else(|| format!("missing {key}"))
}

/// `VerticalAnchor`: `{"absolute": y}`, `{"above_bottom": n}` or `{"below_top": n}`.
fn y_offset(value: &Value) -> Result<YOffset> {
    let obj = object(value, "vertical anchor")?;
    if obj.contains_key("absolute") {
        Ok(YOffset::Absolute(Absolute {
            absolute: small(obj, "absolute")?,
        }))
    } else if obj.contains_key("above_bottom") {
        Ok(YOffset::AboveBottom(AboveBottom {
            above_bottom: small(obj, "above_bottom")?,
        }))
    } else if obj.contains_key("below_top") {
        Ok(YOffset::BelowTop(BelowTop {
            below_top: small(obj, "below_top")?,
        }))
    } else {
        Err(format!("unknown vertical anchor {value}"))
    }
}

fn md5_halves(name: &str) -> (u64, u64) {
    let bytes = md5::compute(name.as_bytes());
    let mut lo = [0u8; 8];
    let mut hi = [0u8; 8];
    lo.copy_from_slice(&bytes[0..8]);
    hi.copy_from_slice(&bytes[8..16]);
    (u64::from_be_bytes(lo), u64::from_be_bytes(hi))
}

impl RuleParser<'_> {
    /// A reference into `registry`, resolved and parsed by `parse`.
    fn reference<T>(
        &mut self,
        registry: WorldgenRegistry,
        id: &str,
        parse: impl FnOnce(&mut Self, &Value) -> Result<T>,
    ) -> Result<T> {
        let json = self.json;
        let (value, from_datapack) = json
            .get(registry, id)
            .ok_or_else(|| format!("unknown {} {id}", registry.folder()))?;
        self.touched |= *from_datapack;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(format!(
                "{} {id} nests too deep (a cycle?)",
                registry.folder()
            ));
        }
        let result = parse(self, value);
        self.depth -= 1;
        result
    }

    fn rule(&mut self, value: &Value) -> Result<MaterialRule> {
        if let Value::String(id) = value {
            return self.reference(WorldgenRegistry::MaterialRule, id, Self::rule);
        }
        let obj = object(value, "material rule")?;
        let kind = string(obj, "type")?;
        Ok(match full_id(kind).as_str() {
            "minecraft:block" => MaterialRule::Block(BlockMaterialRule {
                result_state: block_state(field(obj, "result_state")?)?,
            }),
            "minecraft:sequence" => {
                let sequence = field(obj, "sequence")?
                    .as_array()
                    .ok_or("sequence is not a list")?
                    .iter()
                    .map(|rule| self.rule(rule))
                    .collect::<Result<Vec<_>>>()?;
                MaterialRule::Sequence(SequenceMaterialRule {
                    sequence: Box::leak(sequence.into_boxed_slice()),
                })
            }
            "minecraft:condition" => {
                let if_true = self.condition(field(obj, "if_true")?)?;
                let then_run = self.rule(field(obj, "then_run")?)?;
                MaterialRule::Condition(ConditionMaterialRule {
                    if_true,
                    then_run: leak(then_run),
                })
            }
            // Vanilla's own spelling, and the obvious one.
            "minecraft:bandlands" | "minecraft:badlands" => {
                MaterialRule::Badlands(BadLandsMaterialRule)
            }
            "minecraft:ore_vein" => {
                // As the codegen: the vein's range and kind follow from its density's name;
                // Pumpkin's vein router evaluates the rest.
                let is_iron = string(obj, "density")?.contains("iron");
                let (min_y, max_y) = if is_iron { (-60, -8) } else { (0, 50) };
                MaterialRule::OreVein(OreVeinMaterialRule {
                    ore_block: block_state(field(obj, "ore_block")?)?,
                    raw_ore_block: block_state(field(obj, "raw_ore_block")?)?,
                    filler_block: block_state(field(obj, "filler_block")?)?,
                    raw_ore_chance: float(obj, "raw_ore_chance")? as f32,
                    min_y,
                    max_y,
                    is_iron,
                })
            }
            other => return Err(format!("unsupported material rule type {other}")),
        })
    }

    fn condition(&mut self, value: &Value) -> Result<MaterialCondition> {
        if let Value::String(id) = value {
            return self.reference(WorldgenRegistry::MaterialCondition, id, Self::condition);
        }
        let obj = object(value, "material condition")?;
        let kind = string(obj, "type")?;
        Ok(match full_id(kind).as_str() {
            "minecraft:biome" => {
                let names: Vec<&str> = match field(obj, "biome_is")? {
                    Value::String(name) => vec![name.as_str()],
                    Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
                    other => return Err(format!("bad biome_is {other}")),
                };
                let biomes = names
                    .into_iter()
                    .map(|name| {
                        name.strip_prefix("minecraft:")
                            .and_then(Biome::from_name)
                            .or_else(|| Biome::from_name(name))
                            .ok_or_else(|| format!("unknown biome {name}"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                MaterialCondition::Biome(BiomeMaterialCondition {
                    biome_is: Box::leak(biomes.into_boxed_slice()),
                })
            }
            "minecraft:noise_threshold" => {
                let (noise, from_datapack) = self
                    .json
                    .noise(string(obj, "noise")?, &mut self.next_noise_id)?;
                self.touched |= from_datapack;
                MaterialCondition::NoiseThreshold(NoiseThresholdMaterialCondition {
                    noise,
                    min_threshold: float(obj, "min_threshold")?,
                    max_threshold: float(obj, "max_threshold")?,
                })
            }
            "minecraft:vertical_gradient" => {
                let (random_lo, random_hi) = md5_halves(string(obj, "random_name")?);
                MaterialCondition::VerticalGradient(VerticalGradientMaterialCondition {
                    random_lo,
                    random_hi,
                    true_at_and_below: y_offset(field(obj, "true_at_and_below")?)?,
                    false_at_and_above: y_offset(field(obj, "false_at_and_above")?)?,
                })
            }
            "minecraft:y_above" => MaterialCondition::YAbove(AboveYMaterialCondition {
                anchor: y_offset(field(obj, "anchor")?)?,
                surface_depth_multiplier: small(obj, "surface_depth_multiplier")?,
                add_stone_depth: boolean(obj, "add_stone_depth")?,
            }),
            "minecraft:water" => MaterialCondition::Water(WaterMaterialCondition {
                offset: small(obj, "offset")?,
                surface_depth_multiplier: small(obj, "surface_depth_multiplier")?,
                add_stone_depth: boolean(obj, "add_stone_depth")?,
            }),
            "minecraft:temperature" => MaterialCondition::Temperature,
            "minecraft:steep" => MaterialCondition::Steep,
            "minecraft:not" => {
                let invert = self.condition(field(obj, "invert")?)?;
                MaterialCondition::Not(NotMaterialCondition {
                    invert: leak(invert),
                })
            }
            "minecraft:hole" => MaterialCondition::Hole(HoleMaterialCondition),
            "minecraft:above_preliminary_surface" => {
                MaterialCondition::AbovePreliminarySurface(SurfaceMaterialCondition)
            }
            "minecraft:stone_depth" => MaterialCondition::StoneDepth(StoneDepthMaterialCondition {
                offset: small(obj, "offset")?,
                add_surface_depth: boolean(obj, "add_surface_depth")?,
                secondary_depth_range: small(obj, "secondary_depth_range")?,
                surface_type: match string(obj, "surface_type")? {
                    "ceiling" => VerticalSurfaceType::Ceiling,
                    "floor" => VerticalSurfaceType::Floor,
                    other => return Err(format!("unknown surface type {other}")),
                },
            }),
            other => return Err(format!("unsupported material condition type {other}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use pumpkin_data::Block;
    use pumpkin_data::material_rule::{MaterialCondition, MaterialRule};
    use pumpkin_util::math::vertical_surface_type::VerticalSurfaceType;
    use serde_json::json;

    use crate::generation::noise::router::datapack::{WorldgenJson, WorldgenRegistry};

    fn same_condition(a: &MaterialCondition, b: &MaterialCondition) -> bool {
        use MaterialCondition as C;
        match (a, b) {
            (C::Biome(a), C::Biome(b)) => {
                a.biome_is.len() == b.biome_is.len()
                    && a.biome_is.iter().zip(b.biome_is).all(|(x, y)| x.id == y.id)
            }
            (C::NoiseThreshold(a), C::NoiseThreshold(b)) => {
                a.noise.id == b.noise.id
                    && a.noise.first_octave == b.noise.first_octave
                    && a.noise.amplitudes == b.noise.amplitudes
                    && (a.noise.lo, a.noise.hi) == (b.noise.lo, b.noise.hi)
                    && a.min_threshold.to_bits() == b.min_threshold.to_bits()
                    && a.max_threshold.to_bits() == b.max_threshold.to_bits()
            }
            (C::VerticalGradient(a), C::VerticalGradient(b)) => {
                (a.random_lo, a.random_hi) == (b.random_lo, b.random_hi)
                    && a.true_at_and_below == b.true_at_and_below
                    && a.false_at_and_above == b.false_at_and_above
            }
            (C::YAbove(a), C::YAbove(b)) => {
                a.anchor == b.anchor
                    && a.surface_depth_multiplier == b.surface_depth_multiplier
                    && a.add_stone_depth == b.add_stone_depth
            }
            (C::Water(a), C::Water(b)) => {
                a.offset == b.offset
                    && a.surface_depth_multiplier == b.surface_depth_multiplier
                    && a.add_stone_depth == b.add_stone_depth
            }
            (C::Temperature, C::Temperature)
            | (C::Steep, C::Steep)
            | (C::Hole(_), C::Hole(_))
            | (C::AbovePreliminarySurface(_), C::AbovePreliminarySurface(_)) => true,
            (C::Not(a), C::Not(b)) => same_condition(a.invert, b.invert),
            (C::StoneDepth(a), C::StoneDepth(b)) => {
                a.offset == b.offset
                    && a.add_surface_depth == b.add_surface_depth
                    && a.secondary_depth_range == b.secondary_depth_range
                    && matches!(
                        (&a.surface_type, &b.surface_type),
                        (VerticalSurfaceType::Ceiling, VerticalSurfaceType::Ceiling)
                            | (VerticalSurfaceType::Floor, VerticalSurfaceType::Floor)
                    )
            }
            _ => false,
        }
    }

    fn same_rule(a: &MaterialRule, b: &MaterialRule) -> bool {
        use MaterialRule as R;
        match (a, b) {
            (R::Block(a), R::Block(b)) => a.result_state.id == b.result_state.id,
            (R::Sequence(a), R::Sequence(b)) => {
                a.sequence.len() == b.sequence.len()
                    && a.sequence
                        .iter()
                        .zip(b.sequence)
                        .all(|(x, y)| same_rule(x, y))
            }
            (R::Condition(a), R::Condition(b)) => {
                same_condition(&a.if_true, &b.if_true) && same_rule(a.then_run, b.then_run)
            }
            (R::Badlands(_), R::Badlands(_)) => true,
            (R::OreVein(a), R::OreVein(b)) => {
                a.ore_block.id == b.ore_block.id
                    && a.raw_ore_block.id == b.raw_ore_block.id
                    && a.filler_block.id == b.filler_block.id
                    && a.raw_ore_chance.to_bits() == b.raw_ore_chance.to_bits()
                    && (a.min_y, a.max_y, a.is_iron) == (b.min_y, b.max_y, b.is_iron)
            }
            _ => false,
        }
    }

    #[test]
    fn vanilla_json_rebuilds_the_compiled_rules() {
        let json = WorldgenJson::vanilla();
        for settings in [
            "overworld",
            "amplified",
            "large_biomes",
            "nether",
            "end",
            "caves",
            "floating_islands",
        ] {
            let built = json
                .build_surface_rule(settings, true)
                .unwrap_or_else(|e| panic!("{settings}: {e}"))
                .unwrap_or_else(|| panic!("{settings}: no rule"));
            let rule = json
                .get(WorldgenRegistry::NoiseSettings, settings)
                .and_then(|(v, _)| v.get("material_rule")?.as_str())
                .unwrap();
            let compiled = MaterialRule::from_name(rule).unwrap();
            assert!(same_rule(built, compiled), "{settings} ({rule}) differs");
        }
        // Nothing built without a datapack entry.
        assert!(
            json.build_surface_rule("overworld", false)
                .unwrap()
                .is_none()
        );
        assert!(json.build_datapack_surface_rules().0.is_empty());
    }

    #[test]
    fn an_overridden_rule_rebuilds_only_the_settings_that_use_it() {
        let mut json = WorldgenJson::vanilla();
        json.insert_from_datapack(
            WorldgenRegistry::MaterialRule,
            "minecraft:overworld/surface",
            json!({ "type": "minecraft:block", "result_state": { "Name": "minecraft:sand" } }),
        );
        let (built, errors) = json.build_datapack_surface_rules();
        assert!(errors.is_empty(), "{errors:?}");
        let mut ids: Vec<&str> = built.iter().map(|(id, _)| id.as_str()).collect();
        ids.sort_unstable();
        // Caves and floating islands reuse the overworld's surface; the Nether and End do not.
        assert_eq!(
            ids,
            [
                "minecraft:amplified",
                "minecraft:caves",
                "minecraft:floating_islands",
                "minecraft:large_biomes",
                "minecraft:overworld"
            ]
        );
        let (_, overworld) = built
            .iter()
            .find(|(id, _)| id == "minecraft:overworld")
            .unwrap();
        assert!(!same_rule(
            overworld,
            MaterialRule::from_name("overworld").unwrap()
        ));
    }

    fn find_log(rule: &MaterialRule) -> Option<&'static pumpkin_data::BlockState> {
        match rule {
            MaterialRule::Block(b)
                if Block::from_state_id(b.result_state.id) == &Block::OAK_LOG =>
            {
                Some(b.result_state)
            }
            MaterialRule::Sequence(s) => s.sequence.iter().find_map(find_log),
            MaterialRule::Condition(c) => find_log(c.then_run),
            _ => None,
        }
    }

    #[test]
    fn block_states_keep_their_properties() {
        let mut json = WorldgenJson::vanilla();
        json.insert_from_datapack(
            WorldgenRegistry::MaterialRule,
            "minecraft:overworld/surface",
            json!({ "type": "minecraft:block", "result_state": {
                "id": "minecraft:oak_log", "properties": { "axis": "x" } } }),
        );
        let rule = json
            .build_surface_rule("overworld", false)
            .unwrap()
            .unwrap();
        let state = find_log(rule).expect("the overridden surface");
        assert_eq!(Block::from_state_id(state.id), &Block::OAK_LOG);
        assert_ne!(state.id, Block::OAK_LOG.default_state.id);
        // As vanilla: a value the property cannot take keeps its default; unknown keys go.
        let bad = json!({ "type": "minecraft:block", "result_state": {
            "id": "minecraft:oak_log", "properties": { "axis": "w", "colour": "red" } } });
        json.insert_from_datapack(
            WorldgenRegistry::MaterialRule,
            "minecraft:overworld/surface",
            bad,
        );
        let rule = json
            .build_surface_rule("overworld", false)
            .unwrap()
            .unwrap();
        assert_eq!(find_log(rule).unwrap().id, Block::OAK_LOG.default_state.id);
        // The older form still reads.
        let old = json!({ "type": "minecraft:block", "result_state": {
            "Name": "minecraft:oak_log", "Properties": { "axis": "z" } } });
        json.insert_from_datapack(
            WorldgenRegistry::MaterialRule,
            "minecraft:overworld/surface",
            old,
        );
        let rule = json
            .build_surface_rule("overworld", false)
            .unwrap()
            .unwrap();
        let z = find_log(rule).unwrap().id;
        assert!(z != state.id && z != Block::OAK_LOG.default_state.id);
    }
}
