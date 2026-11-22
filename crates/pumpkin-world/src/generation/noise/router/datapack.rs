//! Noise routers and noise settings built at runtime from worldgen JSON.
//!
//! The vanilla routers are compiled (`pumpkin_data::noise_router`). A datapack can override
//! any `worldgen/density_function`, `worldgen/noise` or `worldgen/noise_settings` file, or add
//! its own; such a pack changes the terrain only if the server builds the affected routers
//! from the merged JSON at startup. This module does that: it resolves a noise settings file
//! against the vanilla JSON (embedded, `pumpkin_data::worldgen_json`) and the datapacks' files,
//! and lowers it to the same [`BaseNoiseRouters`] the codegen emits, so the rest of the
//! generator runs unchanged.
//!
//! The parsing, the constant folding, the axis slicing and the flattening into component
//! stacks follow `tools/pumpkin-codegen/src/noise_router.rs` step for step: for the vanilla
//! files the result is the compiled router, component for component (see the tests).
//!
//! Only noise settings that a datapack actually touches are built; every other dimension keeps
//! its compiled router.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::OnceLock;

use pumpkin_data::biome::{Parameter, ParameterPoint};
use pumpkin_data::chunk::DoublePerlinNoiseParameters;
use pumpkin_data::noise_router::{
    Axis, BaseMultiNoiseRouter, BaseNoiseFunctionComponent, BaseNoiseRouter, BaseNoiseRouters,
    BaseSurfaceEstimator, BaseVeinRouter, BinaryData, BinaryOperation, ClampData,
    DistanceMetric, DistanceToPointData, FindTopSurfaceData, GradientData,
    InterpolatedNoiseSamplerData, LinearData, LinearOperation, NoiseData, RangeChoiceData,
    RoundingData, RoundingOperation, ShiftedNoiseData, SplinePoint, SplineRepr, Tiling,
    UnaryData, UnaryOperation, WrapperType,
};
use pumpkin_data::noise_settings::{GenerationShapeConfig, NoiseSettings};
use pumpkin_data::{Block, BlockState, Property};
use serde_json::Value;

pub(crate) type Result<T> = std::result::Result<T, String>;

/// Density function references nested deeper than this are taken for a cycle.
const MAX_DEPTH: usize = 512;

/// The worldgen registries the noise routers, surface rules and features are built from.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum WorldgenRegistry {
    DensityFunction,
    Noise,
    NoiseSettings,
    MaterialRule,
    MaterialCondition,
    ConfiguredFeature,
    PlacedFeature,
    BlockStateProvider,
}

impl WorldgenRegistry {
    /// The registry's folder under `data/<namespace>/worldgen/`.
    #[must_use]
    pub const fn folder(self) -> &'static str {
        match self {
            Self::DensityFunction => "density_function",
            Self::Noise => "noise",
            Self::NoiseSettings => "noise_settings",
            Self::MaterialRule => "material_rule",
            Self::MaterialCondition => "material_condition",
            Self::ConfiguredFeature => "feature",
            Self::PlacedFeature => "placed_feature",
            Self::BlockStateProvider => "block_state_provider",
        }
    }

    pub const ALL: [Self; 8] = [
        Self::DensityFunction,
        Self::Noise,
        Self::NoiseSettings,
        Self::MaterialRule,
        Self::MaterialCondition,
        Self::ConfiguredFeature,
        Self::PlacedFeature,
        Self::BlockStateProvider,
    ];
}

/// `minecraft:` when the id has no namespace.
#[must_use]
pub fn full_id(id: &str) -> String {
    if id.contains(':') {
        id.to_string()
    } else {
        format!("minecraft:{id}")
    }
}

/// The worldgen JSON a router is resolved against: vanilla's, then the datapacks' on top.
#[derive(Default)]
pub struct WorldgenJson {
    entries: BTreeMap<(WorldgenRegistry, String), (Value, bool)>,
}

impl WorldgenJson {
    /// The vanilla files, embedded at build time.
    #[must_use]
    pub fn vanilla() -> Self {
        let mut json = Self::default();
        let tables = [
            (
                WorldgenRegistry::DensityFunction,
                pumpkin_data::worldgen_json::DENSITY_FUNCTIONS,
            ),
            (WorldgenRegistry::Noise, pumpkin_data::worldgen_json::NOISES),
            (
                WorldgenRegistry::NoiseSettings,
                pumpkin_data::worldgen_json::NOISE_SETTINGS,
            ),
            (
                WorldgenRegistry::MaterialRule,
                pumpkin_data::worldgen_json::MATERIAL_RULES,
            ),
            (
                WorldgenRegistry::MaterialCondition,
                pumpkin_data::worldgen_json::MATERIAL_CONDITIONS,
            ),
            (
                WorldgenRegistry::BlockStateProvider,
                pumpkin_data::worldgen_json::BLOCK_STATE_PROVIDERS,
            ),
        ];
        for (registry, table) in tables {
            for (id, text) in table {
                if let Ok(value) = serde_json::from_str(text) {
                    json.entries
                        .insert((registry, (*id).to_string()), (value, false));
                }
            }
        }
        json
    }

    /// Adds or replaces an entry from a datapack. Later packs win, as in vanilla.
    pub fn insert_from_datapack(&mut self, registry: WorldgenRegistry, id: &str, value: Value) {
        self.entries.insert((registry, full_id(id)), (value, true));
    }

    /// Whether any datapack entry was added.
    #[must_use]
    pub fn has_datapack_entries(&self) -> bool {
        self.entries.values().any(|(_, from_datapack)| *from_datapack)
    }

    /// An entry and whether a datapack gave it.
    pub(crate) fn get(&self, registry: WorldgenRegistry, id: &str) -> Option<&(Value, bool)> {
        self.entries.get(&(registry, full_id(id)))
    }

    pub(crate) fn ids(&self, registry: WorldgenRegistry) -> impl Iterator<Item = &str> {
        self.entries
            .keys()
            .filter(move |(r, _)| *r == registry)
            .map(|(_, id)| id.as_str())
    }

    /// Builds every noise settings entry that a datapack touches, directly or through a
    /// density function or noise it references. Returns the built settings by id, and one
    /// message per entry that failed (that entry keeps its compiled router).
    #[must_use]
    pub fn build_datapack_noise_settings(&self) -> (Vec<(String, DatapackNoiseSettings)>, Vec<String>) {
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
            match self.build_noise_settings(&id, false) {
                Ok(Some(settings)) => built.push((id, settings)),
                Ok(None) => {}
                Err(error) => errors.push(format!("{id}: {error}")),
            }
        }
        (built, errors)
    }

    /// Builds one noise settings entry. With `force` unset, returns `None` when no datapack
    /// entry takes part in it.
    pub fn build_noise_settings(
        &self,
        id: &str,
        force: bool,
    ) -> Result<Option<DatapackNoiseSettings>> {
        let (settings_json, from_datapack) = self
            .get(WorldgenRegistry::NoiseSettings, id)
            .ok_or_else(|| format!("no noise settings {id}"))?;
        let mut parser = Parser {
            json: self,
            touched: *from_datapack,
            depth: 0,
        };
        let path = full_id(id)
            .split_once(':')
            .map(|(_, path)| path.to_string())
            .unwrap_or_default();
        let mut router = parser.noise_router(settings_json, &path)?;
        // Noise names resolved here so that a datapack noise counts as touching.
        let noises = parser.resolve_noises(&mut router)?;
        if !force && !parser.touched {
            return Ok(None);
        }
        let settings = parse_noise_settings(settings_json, &path, &mut router)?;
        let routers = router.lower(&noises);
        Ok(Some(DatapackNoiseSettings {
            settings: Box::leak(Box::new(settings)),
            router: Box::leak(Box::new(routers)),
        }))
    }
}

/// A noise settings entry built from JSON, leaked once at startup like the compiled ones.
#[derive(Clone, Copy)]
pub struct DatapackNoiseSettings {
    pub settings: &'static NoiseSettings,
    pub router: &'static BaseNoiseRouters,
}

static INSTALLED: OnceLock<BTreeMap<String, DatapackNoiseSettings>> = OnceLock::new();

/// Installs the noise settings built from datapacks; returns false if some were already.
///
/// Once per process, before the worlds are created: worldgen registries are fixed for a
/// server's lifetime, as in vanilla.
pub fn install(built: Vec<(String, DatapackNoiseSettings)>) -> bool {
    INSTALLED
        .set(
            built
                .into_iter()
                .map(|(id, settings)| (full_id(&id), settings))
                .collect(),
        )
        .is_ok()
}

/// The noise settings a datapack built for `id`, if any.
#[must_use]
pub fn installed(id: &str) -> Option<&'static DatapackNoiseSettings> {
    INSTALLED.get()?.get(&full_id(id))
}

// ------------------------------------------------------------------- representation

#[derive(Clone, Copy)]
struct F32(f32);

impl Hash for F32 {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_le_bytes().hash(state);
    }
}

#[derive(Clone, Copy)]
struct F64(f64);

impl Hash for F64 {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_le_bytes().hash(state);
    }
}

#[derive(Hash, Clone, Copy)]
enum Ax {
    X,
    Y,
    Z,
}

impl Ax {
    const fn bit(self) -> u8 {
        match self {
            Self::X => AXIS_X,
            Self::Y => AXIS_Y,
            Self::Z => AXIS_Z,
        }
    }

    const fn lower(self) -> Axis {
        match self {
            Self::X => Axis::X,
            Self::Y => Axis::Y,
            Self::Z => Axis::Z,
        }
    }
}

#[derive(Hash, Clone, Copy)]
enum Tile {
    ClampToEdge,
    Repeat,
    MirroredRepeat,
}

#[derive(Hash, Clone, Copy)]
enum Metric {
    Euclidean,
    EuclideanSquared,
    Manhattan,
    Chebyshev,
}

#[derive(Hash, Clone, Copy)]
enum Rounding {
    Floor,
    Nearest,
    Ceil,
    Truncate,
}

#[derive(Hash, Clone, Copy)]
enum BinOp {
    Add,
    Mul,
    Min,
    Max,
    Sub,
    Div,
    Pow,
}

#[derive(Hash, Clone, Copy)]
enum LinOp {
    Add,
    Mul,
}

#[derive(Hash, Clone, Copy)]
enum UnOp {
    Invert,
    Abs,
    Square,
    Cube,
    HalfNegative,
    QuarterNegative,
    Squeeze,
    Negate,
    Sqrt,
    Log,
    Sign,
}

#[derive(Hash, Clone, Copy)]
enum Wrap {
    Interpolated { cell_size_xz: i32, cell_size_y: i32 },
    Cache,
}

#[derive(Hash, Clone)]
enum Spline {
    Standard {
        location_function: Box<Df>,
        locations: Box<[F32]>,
        values: Box<[Self]>,
        derivatives: Box<[F32]>,
    },
    Fixed {
        value: F32,
    },
}

/// A density function tree, as the codegen's `DensityFunctionRepr`. Hashed for
/// deduplication, so variant and field order matter only within this file.
#[derive(Hash, Clone)]
enum Df {
    Beardifier,
    BlendAlpha,
    BlendOffset,
    BlendDensity {
        input: Box<Self>,
    },
    FindTopSurface {
        density: Box<Self>,
        upper_bound: Box<Self>,
        lower_bound: i32,
        cell_height: i32,
    },
    EndIslands,
    Noise {
        noise: String,
        xz_scale: F64,
        y_scale: F64,
    },
    ShiftA {
        noise: String,
    },
    ShiftB {
        noise: String,
    },
    ShiftedNoise {
        shift_x: Box<Self>,
        shift_y: Box<Self>,
        shift_z: Box<Self>,
        xz_scale: F64,
        y_scale: F64,
        noise: String,
    },
    InterpolatedNoiseSampler {
        xz_scale: F64,
        y_scale: F64,
        xz_factor: F64,
        y_factor: F64,
        smear_scale_multiplier: F64,
    },
    IntervalSelect {
        input: Box<Self>,
        thresholds: Box<[F32]>,
        functions: Box<[Self]>,
    },
    Wrapper {
        input: Box<Self>,
        wrapper: Wrap,
    },
    Constant {
        value: F32,
    },
    Gradient {
        axis: Ax,
        tiling: Tile,
        from_coordinate: i32,
        to_coordinate: i32,
        from_value: F32,
        to_value: F32,
    },
    DistanceToPoint {
        point: [i32; 3],
        metric: Metric,
    },
    Lerp {
        alpha: Box<Self>,
        first: Box<Self>,
        second: Box<Self>,
    },
    Rounding {
        input: Box<Self>,
        multiple: Box<Self>,
        operation: Rounding,
    },
    Slice {
        axis: Ax,
        coordinate: i32,
        input: Box<Self>,
    },
    Binary {
        argument1: Box<Self>,
        argument2: Box<Self>,
        operation: BinOp,
        min_value: F32,
        max_value: F32,
    },
    Linear {
        input: Box<Self>,
        operation: LinOp,
        argument: F32,
        min_value: F32,
        max_value: F32,
    },
    Unary {
        input: Box<Self>,
        operation: UnOp,
        min_value: F32,
        max_value: F32,
    },
    Clamp {
        input: Box<Self>,
        min_value: F32,
        max_value: F32,
    },
    RangeChoice {
        input: Box<Self>,
        when_in_range: Box<Self>,
        when_out_range: Box<Self>,
        min_inclusive: F32,
        max_exclusive: F32,
    },
    Spline {
        spline: Spline,
        min_value: F32,
        max_value: F32,
    },
}

const fn constant(value: f32) -> Df {
    Df::Constant { value: F32(value) }
}

// ------------------------------------------------------------------- parsing

struct Parser<'a> {
    json: &'a WorldgenJson,
    /// Whether a datapack entry took part.
    touched: bool,
    depth: usize,
}

fn f64_of(obj: &serde_json::Map<String, Value>, key: &str) -> Option<f64> {
    obj.get(key).and_then(Value::as_f64)
}

fn i32_of(obj: &serde_json::Map<String, Value>, key: &str) -> Option<i32> {
    obj.get(key).and_then(Value::as_i64).map(|v| v as i32)
}

fn field<'v>(obj: &'v serde_json::Map<String, Value>, keys: &[&str]) -> Result<&'v Value> {
    keys.iter()
        .find_map(|key| obj.get(*key))
        .ok_or_else(|| format!("missing {}", keys.join("/")))
}

fn clean_noise_name(name: &str) -> String {
    name.strip_prefix("minecraft:").unwrap_or(name).to_string()
}

fn parse_axis(s: &str) -> Result<Ax> {
    match s {
        "x" | "X" => Ok(Ax::X),
        "y" | "Y" => Ok(Ax::Y),
        "z" | "Z" => Ok(Ax::Z),
        other => Err(format!("unknown axis {other}")),
    }
}

fn parse_tiling(s: &str) -> Tile {
    match s {
        "repeat" => Tile::Repeat,
        "mirrored_repeat" => Tile::MirroredRepeat,
        _ => Tile::ClampToEdge,
    }
}

fn parse_metric(s: &str) -> Result<Metric> {
    match s {
        "euclidean" => Ok(Metric::Euclidean),
        "euclidean_squared" => Ok(Metric::EuclideanSquared),
        "manhattan" => Ok(Metric::Manhattan),
        "chebyshev" => Ok(Metric::Chebyshev),
        other => Err(format!("unknown distance metric {other}")),
    }
}

impl Parser<'_> {
    fn reference(&mut self, id: &str) -> Result<Df> {
        let (value, from_datapack) = self
            .json
            .get(WorldgenRegistry::DensityFunction, id)
            .ok_or_else(|| format!("unknown density function {id}"))?;
        self.touched |= *from_datapack;
        if self.depth >= MAX_DEPTH {
            return Err(format!("density function {id} nests too deep (a cycle?)"));
        }
        self.depth += 1;
        let parsed = self.df(value);
        self.depth -= 1;
        parsed.map_err(|error| format!("{id}: {error}"))
    }

    fn sub(&mut self, obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Result<Box<Df>> {
        let value = field(obj, keys)?;
        Ok(Box::new(self.df(value)?))
    }

    #[expect(clippy::too_many_lines)]
    fn df(&mut self, value: &Value) -> Result<Df> {
        let obj = match value {
            Value::Number(n) => return Ok(constant(n.as_f64().unwrap_or(0.0) as f32)),
            Value::String(s) if s == "minecraft:y" => {
                return Ok(Df::Gradient {
                    axis: Ax::Y,
                    tiling: Tile::ClampToEdge,
                    from_coordinate: -4064,
                    to_coordinate: 4062,
                    from_value: F32(-4064.0),
                    to_value: F32(4062.0),
                });
            }
            Value::String(s) if s == "minecraft:zero" => return Ok(constant(0.0)),
            Value::String(s) => return self.reference(s),
            Value::Object(obj) => obj,
            other => return Err(format!("unsupported density function {other}")),
        };
        let kind = obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or("density function without a type")?;
        let kind = kind.strip_prefix("minecraft:").unwrap_or(kind);
        let min_value = f64_of(obj, "min_value").unwrap_or(f64::NEG_INFINITY);
        let max_value = f64_of(obj, "max_value").unwrap_or(f64::INFINITY);
        Ok(match kind {
            "constant" => constant(
                obj.get("value")
                    .or_else(|| obj.get("argument"))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0) as f32,
            ),
            "y_clamped_gradient" => Df::Gradient {
                axis: Ax::Y,
                tiling: Tile::ClampToEdge,
                from_coordinate: i32_of(obj, "from_y").unwrap_or(-64),
                to_coordinate: i32_of(obj, "to_y").unwrap_or(320),
                from_value: F32(f64_of(obj, "from_value").unwrap_or(0.0) as f32),
                to_value: F32(f64_of(obj, "to_value").unwrap_or(0.0) as f32),
            },
            "gradient" => Df::Gradient {
                axis: parse_axis(obj.get("axis").and_then(Value::as_str).unwrap_or("y"))?,
                tiling: parse_tiling(
                    obj.get("tiling")
                        .and_then(Value::as_str)
                        .unwrap_or("clamp_to_edge"),
                ),
                from_coordinate: field(obj, &["from_coordinate", "from_y"])?
                    .as_i64()
                    .ok_or("from_coordinate is not an integer")?
                    as i32,
                to_coordinate: field(obj, &["to_coordinate", "to_y"])?
                    .as_i64()
                    .ok_or("to_coordinate is not an integer")? as i32,
                from_value: F32(f64_of(obj, "from_value").ok_or("missing from_value")? as f32),
                to_value: F32(f64_of(obj, "to_value").ok_or("missing to_value")? as f32),
            },
            "distance_to_point" => {
                let point = field(obj, &["point"])?
                    .as_array()
                    .ok_or("point is not an array")?;
                let coordinate = |i: usize| {
                    point
                        .get(i)
                        .and_then(Value::as_i64)
                        .map_or(0, |v| v as i32)
                };
                Df::DistanceToPoint {
                    point: [coordinate(0), coordinate(1), coordinate(2)],
                    metric: parse_metric(
                        obj.get("metric")
                            .and_then(Value::as_str)
                            .unwrap_or("euclidean"),
                    )?,
                }
            }
            "lerp" => Df::Lerp {
                alpha: self.sub(obj, &["alpha"])?,
                first: self.sub(obj, &["first"])?,
                second: self.sub(obj, &["second"])?,
            },
            "floor" | "round" | "ceil" | "truncate" => Df::Rounding {
                input: self.sub(obj, &["input", "argument"])?,
                multiple: match obj.get("multiple") {
                    Some(multiple) => Box::new(self.df(multiple)?),
                    None => Box::new(constant(1.0)),
                },
                operation: match kind {
                    "floor" => Rounding::Floor,
                    "round" => Rounding::Nearest,
                    "ceil" => Rounding::Ceil,
                    _ => Rounding::Truncate,
                },
            },
            "slice" => Df::Slice {
                axis: parse_axis(obj.get("axis").and_then(Value::as_str).unwrap_or("y"))?,
                coordinate: i32_of(obj, "coordinate").ok_or("missing coordinate")?,
                input: self.sub(obj, &["input"])?,
            },
            "old_blended_noise" => Df::InterpolatedNoiseSampler {
                xz_scale: F64(f64_of(obj, "xz_scale").unwrap_or(0.25)),
                y_scale: F64(f64_of(obj, "y_scale").unwrap_or(0.125)),
                xz_factor: F64(f64_of(obj, "xz_factor").unwrap_or(80.0)),
                y_factor: F64(f64_of(obj, "y_factor").unwrap_or(160.0)),
                smear_scale_multiplier: F64(f64_of(obj, "smear_scale_multiplier").unwrap_or(8.0)),
            },
            "add" | "mul" | "min" | "max" | "sub" | "div" | "pow" => {
                let argument1 = self.df(field(obj, &["left", "argument1", "base"])?)?;
                let argument2 = self.df(field(obj, &["right", "argument2", "exponent"])?)?;
                let min_value = F32(min_value as f32);
                let max_value = F32(max_value as f32);
                if kind == "add" || kind == "mul" {
                    let operation = if kind == "add" { LinOp::Add } else { LinOp::Mul };
                    if let Df::Constant { value } = argument1 {
                        return Ok(Df::Linear {
                            input: Box::new(argument2),
                            operation,
                            argument: value,
                            min_value,
                            max_value,
                        });
                    } else if let Df::Constant { value } = argument2 {
                        return Ok(Df::Linear {
                            input: Box::new(argument1),
                            operation,
                            argument: value,
                            min_value,
                            max_value,
                        });
                    }
                }
                Df::Binary {
                    argument1: Box::new(argument1),
                    argument2: Box::new(argument2),
                    operation: match kind {
                        "add" => BinOp::Add,
                        "mul" => BinOp::Mul,
                        "min" => BinOp::Min,
                        "max" => BinOp::Max,
                        "sub" => BinOp::Sub,
                        "div" => BinOp::Div,
                        _ => BinOp::Pow,
                    },
                    min_value,
                    max_value,
                }
            }
            "abs" | "square" | "cube" | "half_negative" | "quarter_negative" | "squeeze"
            | "invert" | "reciprocal" | "negate" | "sqrt" | "log" | "sign" => Df::Unary {
                input: self.sub(obj, &["argument", "input"])?,
                operation: match kind {
                    "abs" => UnOp::Abs,
                    "square" => UnOp::Square,
                    "cube" => UnOp::Cube,
                    "half_negative" => UnOp::HalfNegative,
                    "quarter_negative" => UnOp::QuarterNegative,
                    "squeeze" => UnOp::Squeeze,
                    "invert" | "reciprocal" => UnOp::Invert,
                    "negate" => UnOp::Negate,
                    "sqrt" => UnOp::Sqrt,
                    "log" => UnOp::Log,
                    _ => UnOp::Sign,
                },
                min_value: F32(min_value as f32),
                max_value: F32(max_value as f32),
            },
            "clamp" => Df::Clamp {
                input: self.sub(obj, &["input", "argument"])?,
                min_value: F32(
                    obj.get("min")
                        .or_else(|| obj.get("min_value"))
                        .and_then(Value::as_f64)
                        .unwrap_or(f64::NEG_INFINITY) as f32,
                ),
                max_value: F32(
                    obj.get("max")
                        .or_else(|| obj.get("max_value"))
                        .and_then(Value::as_f64)
                        .unwrap_or(f64::INFINITY) as f32,
                ),
            },
            "range_choice" => Df::RangeChoice {
                input: self.sub(obj, &["input"])?,
                min_inclusive: F32(f64_of(obj, "min_inclusive").unwrap_or(f64::NEG_INFINITY) as f32),
                max_exclusive: F32(f64_of(obj, "max_exclusive").unwrap_or(f64::INFINITY) as f32),
                when_in_range: self.sub(obj, &["when_in_range"])?,
                when_out_range: self.sub(obj, &["when_out_of_range"])?,
            },
            "interval_select" => {
                let input = self.sub(obj, &["input"])?;
                let thresholds = field(obj, &["thresholds"])?
                    .as_array()
                    .ok_or("thresholds is not an array")?
                    .iter()
                    .map(|v| F32(v.as_f64().unwrap_or(0.0) as f32))
                    .collect();
                let functions = field(obj, &["functions"])?
                    .as_array()
                    .ok_or("functions is not an array")?
                    .iter()
                    .map(|v| self.df(v))
                    .collect::<Result<Vec<_>>>()?;
                Df::IntervalSelect {
                    input,
                    thresholds,
                    functions: functions.into_boxed_slice(),
                }
            }
            "spline" => Df::Spline {
                spline: self.spline(field(obj, &["spline"])?)?,
                min_value: F32(min_value as f32),
                max_value: F32(max_value as f32),
            },
            "noise" => {
                let noise = clean_noise_name(
                    obj.get("noise")
                        .and_then(Value::as_str)
                        .ok_or("missing noise")?,
                );
                let xz_scale = F64(f64_of(obj, "xz_scale").unwrap_or(1.0));
                let y_scale = F64(f64_of(obj, "y_scale").unwrap_or(1.0));
                if ["shift_x", "shift_y", "shift_z"]
                    .iter()
                    .any(|key| obj.contains_key(*key))
                {
                    let mut shift = |key: &str| -> Result<Box<Df>> {
                        Ok(Box::new(match obj.get(key) {
                            Some(value) => self.df(value)?,
                            None => constant(0.0),
                        }))
                    };
                    Df::ShiftedNoise {
                        shift_x: shift("shift_x")?,
                        shift_y: shift("shift_y")?,
                        shift_z: shift("shift_z")?,
                        xz_scale,
                        y_scale,
                        noise,
                    }
                } else {
                    Df::Noise {
                        noise,
                        xz_scale,
                        y_scale,
                    }
                }
            }
            "shifted_noise" => Df::ShiftedNoise {
                shift_x: self.sub(obj, &["shift_x"])?,
                shift_y: self.sub(obj, &["shift_y"])?,
                shift_z: self.sub(obj, &["shift_z"])?,
                xz_scale: F64(f64_of(obj, "xz_scale").unwrap_or(1.0)),
                y_scale: F64(f64_of(obj, "y_scale").unwrap_or(1.0)),
                noise: clean_noise_name(
                    obj.get("noise")
                        .and_then(Value::as_str)
                        .ok_or("missing noise")?,
                ),
            },
            "shift_a" | "shift_b" | "shift" => {
                let noise = clean_noise_name(
                    field(obj, &["noise", "argument"])?
                        .as_str()
                        .ok_or("noise is not a name")?,
                );
                if kind == "shift_b" {
                    Df::ShiftB { noise }
                } else {
                    Df::ShiftA { noise }
                }
            }
            "blend_alpha" => Df::BlendAlpha,
            "blend_offset" => Df::BlendOffset,
            "blend_density" => Df::BlendDensity {
                input: self.sub(obj, &["input", "argument"])?,
            },
            "end_islands" | "end_outer_islands" => Df::EndIslands,
            "beardifier" => Df::Beardifier,
            "cache" | "interpolated" | "flat_cache" | "cache_flat" | "cache_2d" | "cache_once"
            | "cache_all_in_cell" => Df::Wrapper {
                input: self.sub(obj, &["input", "argument"])?,
                wrapper: if kind == "interpolated" {
                    Wrap::Interpolated {
                        cell_size_xz: i32_of(obj, "cell_size_xz").ok_or("missing cell_size_xz")?,
                        cell_size_y: i32_of(obj, "cell_size_y").ok_or("missing cell_size_y")?,
                    }
                } else {
                    Wrap::Cache
                },
            },
            "find_top_surface" | "weird_utility_density" => Df::FindTopSurface {
                density: self.sub(obj, &["density"])?,
                upper_bound: self.sub(obj, &["upper_bound"])?,
                lower_bound: i32_of(obj, "lower_bound").unwrap_or(-64),
                cell_height: i32_of(obj, "cell_height").unwrap_or(8),
            },
            other => return Err(format!("unknown density function type {other}")),
        })
    }

    fn spline(&mut self, value: &Value) -> Result<Spline> {
        if let Some(n) = value.as_f64() {
            return Ok(Spline::Fixed {
                value: F32(n as f32),
            });
        }
        let obj = value.as_object().ok_or("unsupported spline")?;
        if obj.contains_key("value") && !obj.contains_key("points") {
            return self.spline(&obj["value"]);
        }
        let location_function = Box::new(self.df(field(obj, &["coordinate"])?)?);
        let points = field(obj, &["points"])?
            .as_array()
            .ok_or("points is not an array")?;
        let mut locations = Vec::with_capacity(points.len());
        let mut values = Vec::with_capacity(points.len());
        let mut derivatives = Vec::with_capacity(points.len());
        for point in points {
            locations.push(F32(
                point["location"].as_f64().ok_or("spline point without location")? as f32,
            ));
            values.push(self.spline(&point["value"])?);
            derivatives.push(F32(
                point["derivative"]
                    .as_f64()
                    .ok_or("spline point without derivative")? as f32,
            ));
        }
        Ok(Spline::Standard {
            location_function,
            locations: locations.into_boxed_slice(),
            values: values.into_boxed_slice(),
            derivatives: derivatives.into_boxed_slice(),
        })
    }

    fn noise_router(&mut self, settings: &Value, path: &str) -> Result<RouterRepr> {
        let router = settings
            .get("noise_router")
            .ok_or("noise settings without noise_router")?;
        let aquifers = settings.get("aquifers");
        let zero = Value::from(0.0);
        let pick = |keys: &[&str], aquifer_key: Option<&str>| -> Value {
            keys.iter()
                .find_map(|key| router.get(*key))
                .or_else(|| aquifer_key.and_then(|key| aquifers.and_then(|a| a.get(key))))
                .cloned()
                .unwrap_or_else(|| zero.clone())
        };
        // As the codegen: 26.3 noise settings name no ore veins; the overworld-like ones use
        // vanilla's.
        let overworld_like =
            path.starts_with("overworld") || path == "amplified" || path == "large_biomes";
        let vein = |key: &str, default: &str| -> Value {
            router.get(key).cloned().unwrap_or_else(|| {
                if overworld_like {
                    Value::from(default)
                } else {
                    zero.clone()
                }
            })
        };
        Ok(RouterRepr {
            barrier_noise: self.df(&pick(&["barrier"], Some("barrier")))?,
            fluid_level_floodedness_noise: self.df(&pick(
                &["fluid_level_floodedness"],
                Some("fluid_level_floodedness"),
            ))?,
            fluid_level_spread_noise: self.df(&pick(
                &["fluid_level_spread"],
                Some("fluid_level_spread"),
            ))?,
            lava_noise: self.df(&pick(&["lava"], Some("lava")))?,
            temperature: self.df(&pick(&["temperature"], None))?,
            vegetation: self.df(&pick(&["vegetation"], None))?,
            continents: self.df(&pick(&["continents"], None))?,
            erosion: self.df(&pick(&["erosion"], None))?,
            depth: self.df(&pick(&["depth"], None))?,
            ridges: self.df(&pick(&["ridges"], None))?,
            preliminary_surface_level: self.df(&pick(
                &["preliminary_surface_level", "chunk_surface_level"],
                Some("surface_level"),
            ))?,
            final_density: self.df(&pick(&["final_density"], None))?,
            vein_toggle: self.df(&vein("vein_toggle", "minecraft:overworld/ore_vein/toggle"))?,
            vein_ridged: self.df(&vein("vein_ridged", "minecraft:overworld/ore_vein/mask"))?,
            vein_gap: self.df(&vein("vein_gap", "minecraft:overworld/ore_vein/gap"))?,
        })
    }

    /// The noise parameters of every noise the router names, by cleaned name.
    fn resolve_noises(
        &mut self,
        router: &mut RouterRepr,
    ) -> Result<HashMap<String, DoublePerlinNoiseParameters>> {
        let mut names = BTreeSet::new();
        router.for_each_root(&mut |df| df.collect_noises(&mut names));
        let mut noises = HashMap::new();
        // Ids past the compiled ones, for noises a datapack adds; ids only tell the two
        // legacy Nether noises apart.
        let mut next_id = DoublePerlinNoiseParameters::COUNT;
        for name in names {
            let (parameters, from_datapack) = self.json.noise(&name, &mut next_id)?;
            self.touched |= from_datapack;
            noises.insert(name, parameters);
        }
        Ok(noises)
    }
}

impl WorldgenJson {
    /// The noise `name` resolves to, and whether a datapack gave it. A noise the datapacks
    /// add takes the next id past `next_id`: ids only tell noises apart.
    pub(crate) fn noise(
        &self,
        name: &str,
        next_id: &mut usize,
    ) -> Result<(DoublePerlinNoiseParameters, bool)> {
        let id = full_id(name);
        let (value, from_datapack) = self
            .get(WorldgenRegistry::Noise, &id)
            .ok_or_else(|| format!("unknown noise {id}"))?;
        let compiled = DoublePerlinNoiseParameters::id_to_parameters(&id);
        let parameters = match compiled {
            Some(c) if !*from_datapack => DoublePerlinNoiseParameters::new(
                c.id,
                c.first_octave,
                c.amplitudes,
                c.lo,
                c.hi,
                c.string_hash,
            ),
            _ => {
                let numeric_id = compiled.map_or_else(
                    || {
                        *next_id += 1;
                        *next_id
                    },
                    |c| c.id,
                );
                parse_noise(value, &id, numeric_id)?
            }
        };
        Ok((parameters, *from_datapack))
    }
}

/// A `worldgen/noise` entry, read as the codegen reads vanilla's.
fn parse_noise(value: &Value, id: &str, numeric_id: usize) -> Result<DoublePerlinNoiseParameters> {
    let obj = value.as_object().ok_or_else(|| format!("noise {id} is not an object"))?;
    let first_octave = i32_of(obj, "firstOctave")
        .or_else(|| i32_of(obj, "base_octave"))
        .unwrap_or(-7);
    let list = |key: &str| {
        obj.get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect::<Vec<_>>())
    };
    let amplitudes = list("amplitudes")
        .or_else(|| list("amplitude_modifiers"))
        .or_else(|| {
            obj.get("octave_count")
                .and_then(Value::as_u64)
                .map(|n| vec![1.0; (n as usize).max(1)])
        })
        .unwrap_or_else(|| vec![1.0]);
    let hash = md5::compute(id.as_bytes());
    let mut lo = [0u8; 8];
    let mut hi = [0u8; 8];
    lo.copy_from_slice(&hash[0..8]);
    hi.copy_from_slice(&hash[8..16]);
    Ok(DoublePerlinNoiseParameters::new(
        numeric_id,
        first_octave,
        Box::leak(amplitudes.into_boxed_slice()),
        u64::from_be_bytes(lo),
        u64::from_be_bytes(hi),
        pumpkin_util::math::java_string_hash(id),
    ))
}

// ------------------------------------------------------------------- optimisation

const AXIS_X: u8 = 1;
const AXIS_Y: u8 = 2;
const AXIS_Z: u8 = 4;
const AXES_XZ: u8 = AXIS_X | AXIS_Z;
const AXES_ALL: u8 = AXIS_X | AXIS_Y | AXIS_Z;

fn noise_domain_axes(xz_scale: f64, y_scale: f64) -> u8 {
    let mut axes = AXES_ALL;
    if y_scale == 0.0 {
        axes &= !AXIS_Y;
    }
    if xz_scale == 0.0 {
        axes &= !AXES_XZ;
    }
    axes
}

impl Spline {
    fn for_each_function(&mut self, f: &mut dyn FnMut(&mut Df)) {
        if let Self::Standard {
            location_function,
            values,
            ..
        } = self
        {
            f(location_function);
            for value in values.iter_mut() {
                value.for_each_function(f);
            }
        }
    }

    fn domain_axes(&self) -> u8 {
        match self {
            Self::Fixed { .. } => 0,
            Self::Standard {
                location_function,
                values,
                ..
            } => values
                .iter()
                .fold(location_function.domain_axes(), |axes, value| {
                    axes | value.domain_axes()
                }),
        }
    }
}

impl Df {
    fn domain_axes(&self) -> u8 {
        match self {
            Self::Constant { .. } => 0,
            Self::BlendAlpha
            | Self::BlendOffset
            | Self::EndIslands
            | Self::ShiftA { .. }
            | Self::ShiftB { .. } => AXES_XZ,
            Self::Beardifier | Self::InterpolatedNoiseSampler { .. } | Self::DistanceToPoint { .. } => {
                AXES_ALL
            }
            Self::Gradient { axis, .. } => axis.bit(),
            Self::Noise {
                xz_scale, y_scale, ..
            } => noise_domain_axes(xz_scale.0, y_scale.0),
            Self::ShiftedNoise {
                shift_x,
                shift_y,
                shift_z,
                xz_scale,
                y_scale,
                ..
            } => {
                noise_domain_axes(xz_scale.0, y_scale.0)
                    | shift_x.domain_axes()
                    | shift_y.domain_axes()
                    | shift_z.domain_axes()
            }
            Self::BlendDensity { input }
            | Self::Wrapper { input, .. }
            | Self::Linear { input, .. }
            | Self::Unary { input, .. }
            | Self::Clamp { input, .. } => input.domain_axes(),
            Self::Slice { axis, input, .. } => input.domain_axes() & !axis.bit(),
            Self::FindTopSurface {
                density,
                upper_bound,
                ..
            } => (density.domain_axes() | upper_bound.domain_axes()) & !AXIS_Y,
            Self::IntervalSelect {
                input, functions, ..
            } => functions
                .iter()
                .fold(input.domain_axes(), |axes, f| axes | f.domain_axes()),
            Self::Lerp {
                alpha,
                first,
                second,
            } => alpha.domain_axes() | first.domain_axes() | second.domain_axes(),
            Self::Rounding {
                input, multiple, ..
            } => input.domain_axes() | multiple.domain_axes(),
            Self::Binary {
                argument1,
                argument2,
                ..
            } => argument1.domain_axes() | argument2.domain_axes(),
            Self::RangeChoice {
                input,
                when_in_range,
                when_out_range,
                ..
            } => input.domain_axes() | when_in_range.domain_axes() | when_out_range.domain_axes(),
            Self::Spline { spline, .. } => spline.domain_axes(),
        }
    }

    fn for_each_child(&mut self, f: &mut dyn FnMut(&mut Self)) {
        match self {
            Self::Beardifier
            | Self::BlendAlpha
            | Self::BlendOffset
            | Self::EndIslands
            | Self::Noise { .. }
            | Self::ShiftA { .. }
            | Self::ShiftB { .. }
            | Self::InterpolatedNoiseSampler { .. }
            | Self::Constant { .. }
            | Self::Gradient { .. }
            | Self::DistanceToPoint { .. } => {}
            Self::BlendDensity { input }
            | Self::Wrapper { input, .. }
            | Self::Linear { input, .. }
            | Self::Unary { input, .. }
            | Self::Clamp { input, .. }
            | Self::Slice { input, .. } => f(input),
            Self::FindTopSurface {
                density,
                upper_bound,
                ..
            } => {
                f(density);
                f(upper_bound);
            }
            Self::ShiftedNoise {
                shift_x,
                shift_y,
                shift_z,
                ..
            } => {
                f(shift_x);
                f(shift_y);
                f(shift_z);
            }
            Self::IntervalSelect {
                input, functions, ..
            } => {
                f(input);
                for function in functions.iter_mut() {
                    f(function);
                }
            }
            Self::Lerp {
                alpha,
                first,
                second,
            } => {
                f(alpha);
                f(first);
                f(second);
            }
            Self::Rounding {
                input, multiple, ..
            } => {
                f(input);
                f(multiple);
            }
            Self::Binary {
                argument1,
                argument2,
                ..
            } => {
                f(argument1);
                f(argument2);
            }
            Self::RangeChoice {
                input,
                when_in_range,
                when_out_range,
                ..
            } => {
                f(input);
                f(when_in_range);
                f(when_out_range);
            }
            Self::Spline { spline, .. } => spline.for_each_function(f),
        }
    }

    fn collect_noises(&mut self, names: &mut BTreeSet<String>) {
        match self {
            Self::Noise { noise, .. }
            | Self::ShiftA { noise }
            | Self::ShiftB { noise }
            | Self::ShiftedNoise { noise, .. } => {
                names.insert(noise.clone());
            }
            _ => {}
        }
        self.for_each_child(&mut |child| child.collect_noises(names));
    }

    fn existing_removed_axes(&self) -> u8 {
        let mut axes = 0;
        let mut function = self;
        while let Self::Slice { axis, input, .. } = function {
            axes |= axis.bit();
            function = input;
        }
        axes
    }

    fn remove_axes(&mut self, axes: u8) {
        let filtered = axes & !self.existing_removed_axes();
        for (bit, axis) in [(AXIS_X, Ax::X), (AXIS_Z, Ax::Z), (AXIS_Y, Ax::Y)] {
            if filtered & bit != 0 {
                let input = std::mem::replace(self, constant(0.0));
                *self = Self::Slice {
                    axis,
                    coordinate: 0,
                    input: Box::new(input),
                };
            }
        }
    }

    fn slice_uniform_axes(&mut self, parent_axes: u8) {
        if matches!(self, Self::Constant { .. } | Self::Gradient { .. }) {
            return;
        }
        let axes = self.domain_axes();
        if let Self::FindTopSurface {
            density,
            upper_bound,
            ..
        } = self
        {
            density.slice_uniform_axes(axes | AXIS_Y);
            upper_bound.slice_uniform_axes(axes);
        } else {
            self.for_each_child(&mut |child| child.slice_uniform_axes(axes));
        }
        if parent_axes != axes {
            self.remove_axes(parent_axes & !axes);
        }
    }

    #[expect(clippy::too_many_lines)]
    fn optimize(&mut self) {
        match self {
            Self::BlendDensity { input } | Self::Slice { input, .. } | Self::Wrapper { input, .. } => {
                input.optimize();
            }
            Self::FindTopSurface {
                density,
                upper_bound,
                ..
            } => {
                density.optimize();
                upper_bound.optimize();
            }
            Self::ShiftedNoise {
                shift_x,
                shift_y,
                shift_z,
                ..
            } => {
                shift_x.optimize();
                shift_y.optimize();
                shift_z.optimize();
            }
            Self::IntervalSelect {
                input, functions, ..
            } => {
                input.optimize();
                for f in functions.iter_mut() {
                    f.optimize();
                }
            }
            Self::Lerp {
                alpha,
                first,
                second,
            } => {
                alpha.optimize();
                first.optimize();
                second.optimize();
            }
            Self::Rounding {
                input, multiple, ..
            } => {
                input.optimize();
                multiple.optimize();
            }
            Self::RangeChoice {
                input,
                when_in_range,
                when_out_range,
                ..
            } => {
                input.optimize();
                when_in_range.optimize();
                when_out_range.optimize();
            }
            Self::Linear {
                input,
                operation,
                argument,
                ..
            } => {
                input.optimize();
                if let Self::Constant { value } = **input {
                    *self = constant(match operation {
                        LinOp::Add => value.0 + argument.0,
                        LinOp::Mul => value.0 * argument.0,
                    });
                }
            }
            Self::Binary {
                argument1,
                argument2,
                operation,
                ..
            } => {
                argument1.optimize();
                argument2.optimize();
                if let (Self::Constant { value: v1 }, Self::Constant { value: v2 }) =
                    (&**argument1, &**argument2)
                {
                    let (a, b) = (v1.0, v2.0);
                    *self = constant(match operation {
                        BinOp::Add => a + b,
                        BinOp::Mul => a * b,
                        BinOp::Min => a.min(b),
                        BinOp::Max => a.max(b),
                        BinOp::Sub => a - b,
                        BinOp::Div => {
                            if b == 0.0 {
                                0.0
                            } else {
                                a / b
                            }
                        }
                        BinOp::Pow => a.powf(b),
                    });
                }
            }
            Self::Unary {
                input, operation, ..
            } => {
                input.optimize();
                if let Self::Constant { value } = **input {
                    let v = value.0;
                    *self = constant(match operation {
                        UnOp::Abs => v.abs(),
                        UnOp::Square => v * v,
                        UnOp::Cube => v * v * v,
                        UnOp::HalfNegative => {
                            if v > 0.0 {
                                v
                            } else {
                                v * 0.5
                            }
                        }
                        UnOp::QuarterNegative => {
                            if v > 0.0 {
                                v
                            } else {
                                v * 0.25
                            }
                        }
                        UnOp::Squeeze => {
                            let c = v.clamp(-1.0, 1.0);
                            c / 2.0 - c * c * c / 24.0
                        }
                        UnOp::Invert => {
                            if v == 0.0 {
                                f32::INFINITY
                            } else {
                                1.0 / v
                            }
                        }
                        UnOp::Negate => -v,
                        UnOp::Sqrt => v.sqrt(),
                        UnOp::Log => v.ln(),
                        UnOp::Sign => {
                            if v > 0.0 {
                                1.0
                            } else if v < 0.0 {
                                -1.0
                            } else {
                                0.0
                            }
                        }
                    });
                }
            }
            Self::Clamp {
                input,
                min_value,
                max_value,
            } => {
                input.optimize();
                if let Self::Constant { value } = **input {
                    *self = constant(value.0.clamp(min_value.0, max_value.0));
                }
            }
            _ => {}
        }
    }

    fn unique_id(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }

    /// The first `interpolated` wrapper's cell size, in blocks.
    fn interpolation_cell(&mut self) -> Option<(i32, i32)> {
        if let Self::Wrapper {
            wrapper:
                Wrap::Interpolated {
                    cell_size_xz,
                    cell_size_y,
                },
            ..
        } = self
        {
            return Some((*cell_size_xz, *cell_size_y));
        }
        let mut found = None;
        self.for_each_child(&mut |child| {
            if found.is_none() {
                found = child.interpolation_cell();
            }
        });
        found
    }
}

// ------------------------------------------------------------------- lowering

pub(crate) fn leak<T>(value: T) -> &'static T {
    Box::leak(Box::new(value))
}

fn noise_parameters(
    noises: &HashMap<String, DoublePerlinNoiseParameters>,
    name: &str,
) -> DoublePerlinNoiseParameters {
    // Every name was resolved by `resolve_noises` before lowering.
    noises.get(name).map_or_else(
        || DoublePerlinNoiseParameters::new(usize::MAX, 0, &[], 0, 0, 0),
        |p| DoublePerlinNoiseParameters::new(p.id, p.first_octave, p.amplitudes, p.lo, p.hi, p.string_hash),
    )
}

/// Flattens trees into one component stack, sharing equal subtrees, in the codegen's order.
struct Stack<'n> {
    components: Vec<BaseNoiseFunctionComponent>,
    indices: BTreeMap<u64, usize>,
    noises: &'n HashMap<String, DoublePerlinNoiseParameters>,
}

impl<'n> Stack<'n> {
    const fn new(noises: &'n HashMap<String, DoublePerlinNoiseParameters>) -> Self {
        Self {
            components: Vec::new(),
            indices: BTreeMap::new(),
            noises,
        }
    }

    fn index(&mut self, df: &Df) -> usize {
        let id = df.unique_id();
        if let Some(index) = self.indices.get(&id) {
            return *index;
        }
        let component = self.component(df);
        self.components.push(component);
        let index = self.components.len() - 1;
        self.indices.insert(id, index);
        index
    }

    fn spline(&mut self, spline: &Spline) -> SplineRepr {
        match spline {
            Spline::Fixed { value } => SplineRepr::Fixed { value: value.0 },
            Spline::Standard {
                location_function,
                locations,
                values,
                derivatives,
            } => {
                let location_function_index = self.index(location_function);
                let points: Vec<SplinePoint> = locations
                    .iter()
                    .zip(values.iter())
                    .zip(derivatives.iter())
                    .map(|((location, value), derivative)| SplinePoint {
                        location: location.0,
                        value: leak(self.spline(value)),
                        derivative: derivative.0,
                    })
                    .collect();
                SplineRepr::Standard {
                    location_function_index,
                    points: Box::leak(points.into_boxed_slice()),
                }
            }
        }
    }

    #[expect(clippy::too_many_lines)]
    fn component(&mut self, df: &Df) -> BaseNoiseFunctionComponent {
        match df {
            Df::Spline { spline, .. } => BaseNoiseFunctionComponent::Spline {
                spline: leak(self.spline(spline)),
            },
            Df::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => BaseNoiseFunctionComponent::FindTopSurface {
                density_index: self.index(density),
                upper_bound_index: self.index(upper_bound),
                data: leak(FindTopSurfaceData {
                    lower_bound: *lower_bound,
                    cell_height: *cell_height,
                }),
            },
            Df::EndIslands => BaseNoiseFunctionComponent::EndIslands,
            Df::Noise {
                noise,
                xz_scale,
                y_scale,
            } => BaseNoiseFunctionComponent::Noise {
                data: leak(NoiseData {
                    noise_id: noise_parameters(self.noises, noise),
                    xz_scale: xz_scale.0,
                    y_scale: y_scale.0,
                }),
            },
            Df::ShiftA { noise } => BaseNoiseFunctionComponent::ShiftA {
                noise_id: noise_parameters(self.noises, noise),
            },
            Df::ShiftB { noise } => BaseNoiseFunctionComponent::ShiftB {
                noise_id: noise_parameters(self.noises, noise),
            },
            Df::BlendDensity { input } => BaseNoiseFunctionComponent::BlendDensity {
                input_index: self.index(input),
            },
            Df::BlendAlpha => BaseNoiseFunctionComponent::BlendAlpha,
            Df::BlendOffset => BaseNoiseFunctionComponent::BlendOffset,
            Df::Beardifier => BaseNoiseFunctionComponent::Beardifier,
            Df::ShiftedNoise {
                shift_x,
                shift_y,
                shift_z,
                xz_scale,
                y_scale,
                noise,
            } => BaseNoiseFunctionComponent::ShiftedNoise {
                shift_x_index: self.index(shift_x),
                shift_y_index: self.index(shift_y),
                shift_z_index: self.index(shift_z),
                data: leak(ShiftedNoiseData {
                    xz_scale: xz_scale.0,
                    y_scale: y_scale.0,
                    noise_id: noise_parameters(self.noises, noise),
                }),
            },
            Df::RangeChoice {
                input,
                when_in_range,
                when_out_range,
                min_inclusive,
                max_exclusive,
            } => BaseNoiseFunctionComponent::RangeChoice {
                input_index: self.index(input),
                when_in_range_index: self.index(when_in_range),
                when_out_range_index: self.index(when_out_range),
                data: leak(RangeChoiceData {
                    min_inclusive: min_inclusive.0,
                    max_exclusive: max_exclusive.0,
                }),
            },
            Df::Binary {
                argument1,
                argument2,
                operation,
                ..
            } => BaseNoiseFunctionComponent::Binary {
                argument1_index: self.index(argument1),
                argument2_index: self.index(argument2),
                data: leak(BinaryData {
                    operation: match operation {
                        BinOp::Add => BinaryOperation::Add,
                        BinOp::Mul => BinaryOperation::Mul,
                        BinOp::Min => BinaryOperation::Min,
                        BinOp::Max => BinaryOperation::Max,
                        BinOp::Sub => BinaryOperation::Sub,
                        BinOp::Div => BinaryOperation::Div,
                        BinOp::Pow => BinaryOperation::Pow,
                    },
                }),
            },
            Df::Gradient {
                axis,
                tiling,
                from_coordinate,
                to_coordinate,
                from_value,
                to_value,
            } => BaseNoiseFunctionComponent::Gradient {
                data: leak(GradientData {
                    axis: axis.lower(),
                    tiling: match tiling {
                        Tile::ClampToEdge => Tiling::ClampToEdge,
                        Tile::Repeat => Tiling::Repeat,
                        Tile::MirroredRepeat => Tiling::MirroredRepeat,
                    },
                    from_coordinate: *from_coordinate,
                    to_coordinate: *to_coordinate,
                    from_value: from_value.0,
                    to_value: to_value.0,
                }),
            },
            Df::DistanceToPoint { point, metric } => BaseNoiseFunctionComponent::DistanceToPoint {
                data: leak(DistanceToPointData {
                    point: *point,
                    metric: match metric {
                        Metric::Euclidean => DistanceMetric::Euclidean,
                        Metric::EuclideanSquared => DistanceMetric::EuclideanSquared,
                        Metric::Manhattan => DistanceMetric::Manhattan,
                        Metric::Chebyshev => DistanceMetric::Chebyshev,
                    },
                }),
            },
            Df::Lerp {
                alpha,
                first,
                second,
            } => BaseNoiseFunctionComponent::Lerp {
                alpha_index: self.index(alpha),
                first_index: self.index(first),
                second_index: self.index(second),
            },
            Df::Rounding {
                input,
                multiple,
                operation,
            } => BaseNoiseFunctionComponent::Rounding {
                input_index: self.index(input),
                multiple_index: self.index(multiple),
                data: leak(RoundingData {
                    operation: match operation {
                        Rounding::Floor => RoundingOperation::Floor,
                        Rounding::Nearest => RoundingOperation::Round,
                        Rounding::Ceil => RoundingOperation::Ceil,
                        Rounding::Truncate => RoundingOperation::Truncate,
                    },
                }),
            },
            Df::Slice {
                axis,
                coordinate,
                input,
            } => BaseNoiseFunctionComponent::Slice {
                input_index: self.index(input),
                axis: axis.lower(),
                coordinate: *coordinate,
            },
            Df::Constant { value } => BaseNoiseFunctionComponent::Constant { value: value.0 },
            Df::Wrapper { input, wrapper } => BaseNoiseFunctionComponent::Wrapper {
                input_index: self.index(input),
                wrapper: match wrapper {
                    Wrap::Interpolated {
                        cell_size_xz,
                        cell_size_y,
                    } => WrapperType::Interpolated {
                        cell_size_xz: *cell_size_xz,
                        cell_size_y: *cell_size_y,
                    },
                    Wrap::Cache => WrapperType::Cache,
                },
            },
            Df::Linear {
                input,
                operation,
                argument,
                ..
            } => BaseNoiseFunctionComponent::Linear {
                input_index: self.index(input),
                data: leak(LinearData {
                    operation: match operation {
                        LinOp::Add => LinearOperation::Add,
                        LinOp::Mul => LinearOperation::Mul,
                    },
                    argument: argument.0,
                }),
            },
            Df::Clamp {
                input,
                min_value,
                max_value,
            } => BaseNoiseFunctionComponent::Clamp {
                input_index: self.index(input),
                data: leak(ClampData {
                    min_value: min_value.0,
                    max_value: max_value.0,
                }),
            },
            Df::Unary {
                input, operation, ..
            } => BaseNoiseFunctionComponent::Unary {
                input_index: self.index(input),
                data: leak(UnaryData {
                    operation: match operation {
                        UnOp::Abs => UnaryOperation::Abs,
                        UnOp::Square => UnaryOperation::Square,
                        UnOp::Cube => UnaryOperation::Cube,
                        UnOp::HalfNegative => UnaryOperation::HalfNegative,
                        UnOp::QuarterNegative => UnaryOperation::QuarterNegative,
                        UnOp::Squeeze => UnaryOperation::Squeeze,
                        UnOp::Invert => UnaryOperation::Invert,
                        UnOp::Negate => UnaryOperation::Negate,
                        UnOp::Sqrt => UnaryOperation::Sqrt,
                        UnOp::Log => UnaryOperation::Log,
                        UnOp::Sign => UnaryOperation::Sign,
                    },
                }),
            },
            Df::IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                let input_index = self.index(input);
                let functions_indices: Vec<usize> =
                    functions.iter().map(|f| self.index(f)).collect();
                let thresholds: Vec<f32> = thresholds.iter().map(|t| t.0).collect();
                BaseNoiseFunctionComponent::IntervalSelect {
                    input_index,
                    thresholds: Box::leak(thresholds.into_boxed_slice()),
                    functions_indices: Box::leak(functions_indices.into_boxed_slice()),
                }
            }
            Df::InterpolatedNoiseSampler {
                xz_scale,
                y_scale,
                xz_factor,
                y_factor,
                smear_scale_multiplier,
            } => BaseNoiseFunctionComponent::InterpolatedNoiseSampler {
                data: leak(InterpolatedNoiseSamplerData {
                    xz_scale: xz_scale.0,
                    y_scale: y_scale.0,
                    xz_factor: xz_factor.0,
                    y_factor: y_factor.0,
                    smear_scale_multiplier: smear_scale_multiplier.0,
                }),
            },
        }
    }

    fn finish(self) -> &'static [BaseNoiseFunctionComponent] {
        Box::leak(self.components.into_boxed_slice())
    }
}

/// The roots of one noise router, as the codegen's `NoiseRouterRepr`.
struct RouterRepr {
    barrier_noise: Df,
    fluid_level_floodedness_noise: Df,
    fluid_level_spread_noise: Df,
    lava_noise: Df,
    temperature: Df,
    vegetation: Df,
    continents: Df,
    erosion: Df,
    depth: Df,
    ridges: Df,
    preliminary_surface_level: Df,
    final_density: Df,
    vein_toggle: Df,
    vein_ridged: Df,
    vein_gap: Df,
}

impl RouterRepr {
    fn for_each_root(&mut self, f: &mut dyn FnMut(&mut Df)) {
        for df in [
            &mut self.barrier_noise,
            &mut self.fluid_level_floodedness_noise,
            &mut self.fluid_level_spread_noise,
            &mut self.lava_noise,
            &mut self.temperature,
            &mut self.vegetation,
            &mut self.continents,
            &mut self.erosion,
            &mut self.depth,
            &mut self.ridges,
            &mut self.preliminary_surface_level,
            &mut self.final_density,
            &mut self.vein_toggle,
            &mut self.vein_ridged,
            &mut self.vein_gap,
        ] {
            f(df);
        }
    }

    fn lower(mut self, noises: &HashMap<String, DoublePerlinNoiseParameters>) -> BaseNoiseRouters {
        self.for_each_root(&mut Df::optimize);
        self.for_each_root(&mut |df| df.slice_uniform_axes(AXES_ALL));

        let mut noise = Stack::new(noises);
        let noise_erosion = noise.index(&self.erosion);
        let noise_depth = noise.index(&self.depth);
        let barrier_noise = noise.index(&self.barrier_noise);
        let fluid_level_floodedness_noise = noise.index(&self.fluid_level_floodedness_noise);
        let fluid_level_spread_noise = noise.index(&self.fluid_level_spread_noise);
        let lava_noise = noise.index(&self.lava_noise);
        let final_density = noise.index(&self.final_density);
        let vein_toggle = noise.index(&self.vein_toggle);
        let vein_ridged = noise.index(&self.vein_ridged);
        let vein_gap = noise.index(&self.vein_gap);

        let mut surface = Stack::new(noises);
        surface.index(&self.preliminary_surface_level);

        let mut multi = Stack::new(noises);
        let ridges = multi.index(&self.ridges);
        let temperature = multi.index(&self.temperature);
        let vegetation = multi.index(&self.vegetation);
        let continents = multi.index(&self.continents);
        let multi_erosion = multi.index(&self.erosion);
        let multi_depth = multi.index(&self.depth);

        let mut vein = Stack::new(noises);
        let vein_router_toggle = vein.index(&self.vein_toggle);
        let vein_router_ridged = vein.index(&self.vein_ridged);
        let vein_router_gap = vein.index(&self.vein_gap);

        BaseNoiseRouters {
            noise: BaseNoiseRouter {
                full_component_stack: noise.finish(),
                barrier_noise,
                fluid_level_floodedness_noise,
                fluid_level_spread_noise,
                lava_noise,
                erosion: noise_erosion,
                depth: noise_depth,
                final_density,
                vein_toggle,
                vein_ridged,
                vein_gap,
            },
            surface_estimator: BaseSurfaceEstimator {
                full_component_stack: surface.finish(),
            },
            multi_noise: BaseMultiNoiseRouter {
                full_component_stack: multi.finish(),
                temperature,
                vegetation,
                continents,
                erosion: multi_erosion,
                depth: multi_depth,
                ridges,
            },
            vein: BaseVeinRouter {
                full_component_stack: vein.finish(),
                vein_toggle: vein_router_toggle,
                vein_ridged: vein_router_ridged,
                vein_gap: vein_router_gap,
            },
        }
    }
}

// ------------------------------------------------------------------- noise settings

/// A block state as 26.3's `BlockState.CODEC` reads it: `"name"` (the default state), or
/// `{"id": …, "properties": {…}}` (`StateHolder.codec`); also the older `{"Name": …,
/// "Properties": {…}}`. As vanilla's `StateDefinition` codec, a property not given — or given
/// a value it cannot take — keeps the default state's value, and unknown keys are ignored.
/// Mod blocks resolve too, once the block extension is installed.
pub(crate) fn block_state(value: &Value) -> Result<&'static BlockState> {
    let (name, properties) = match value {
        Value::String(name) => (name.as_str(), None),
        Value::Object(obj) => {
            let name = obj
                .get("id")
                .or_else(|| obj.get("Name"))
                .and_then(Value::as_str)
                .ok_or("block state without id")?;
            let properties = obj
                .get("properties")
                .or_else(|| obj.get("Properties"))
                .and_then(Value::as_object);
            (name, properties)
        }
        other => return Err(format!("unsupported block state {other}")),
    };
    let block = Block::from_name(name).ok_or_else(|| format!("unknown block {name}"))?;
    let Some(properties) = properties.filter(|p| !p.is_empty()) else {
        return Ok(block.default_state);
    };
    let Some(mut props) = block.properties(block.default_state.id).map(|p| p.to_props()) else {
        return Ok(block.default_state);
    };
    for slot in &mut props {
        let key = slot.key_str();
        if let Some(parsed) = properties
            .get(key)
            .and_then(Value::as_str)
            .and_then(|value| {
                Property::from_str_pair(key, value)
                    .filter(|p| p.key_str() == key && p.value_str() == value)
            })
        {
            *slot = parsed;
        }
    }
    Ok(BlockState::from_id(
        block.from_properties(&props).to_state_id(block),
    ))
}

fn parameter(value: &Value) -> Result<Parameter> {
    let quantize = |v: &Value| -> Result<i64> {
        v.as_f64()
            .map(|f| (f as f32 * 10000.0) as i64)
            .ok_or_else(|| format!("bad climate parameter {v}"))
    };
    match value {
        Value::Array(span) if span.len() == 2 => {
            Ok(Parameter::new(quantize(&span[0])?, quantize(&span[1])?))
        }
        other => {
            let point = quantize(other)?;
            Ok(Parameter::new(point, point))
        }
    }
}

/// A `spawn_target` entry, read as the codegen reads vanilla's.
fn parameter_point(value: &Value) -> Result<ParameterPoint> {
    let obj = value.as_object().ok_or("spawn target is not an object")?;
    let span = Parameter::new(-10000, 10000);
    let mut point = ParameterPoint {
        temperature: span,
        humidity: span,
        continentalness: span,
        erosion: span,
        depth: Parameter::new(0, 0),
        weirdness: span,
        offset: 0,
    };
    for (key, value) in obj {
        let p = parameter(value)?;
        if key.contains("temperature") {
            point.temperature = p;
        } else if key.contains("vegetation") || key.contains("humidity") {
            point.humidity = p;
        } else if key.contains("continents") || key.contains("continentalness") {
            point.continentalness = p;
        } else if key.contains("erosion") {
            point.erosion = p;
        } else if key.contains("depth") {
            point.depth = p;
        } else if key.contains("ridges") || key.contains("weirdness") {
            point.weirdness = p;
        } else if key.contains("offset") {
            point.offset = p.min;
        }
    }
    Ok(point)
}

fn parse_noise_settings(
    settings: &Value,
    path: &str,
    router: &mut RouterRepr,
) -> Result<NoiseSettings> {
    let obj = settings
        .as_object()
        .ok_or("noise settings is not an object")?;
    let shape = obj.get("noise").ok_or("noise settings without noise")?;
    let min_y = shape
        .get("min_y")
        .and_then(Value::as_i64)
        .ok_or("noise without min_y")?;
    let height = shape
        .get("height")
        .and_then(Value::as_u64)
        .ok_or("noise without height")?;
    let explicit = |key: &str| shape.get(key).and_then(Value::as_u64).map(|v| v as u8);
    // 26.3 files give no cell size: the codegen takes it from the file name. Here the final
    // density's interpolation cell gives it, the same for vanilla's files and right for a
    // datapack's own.
    let (default_h, default_v) = router.final_density.interpolation_cell().map_or_else(
        || match path {
            "end" | "floating_islands" => (2, 1),
            _ => (1, 2),
        },
        |(xz, y)| ((xz / 4) as u8, (y / 4) as u8),
    );
    let aquifers_enabled = obj.get("aquifers").is_some()
        || obj
            .get("aquifers_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let spawn_target = obj
        .get("spawn_target")
        .and_then(Value::as_array)
        .map(|targets| targets.iter().map(parameter_point).collect::<Result<Vec<_>>>())
        .transpose()?
        .unwrap_or_default();
    Ok(NoiseSettings {
        aquifers_enabled,
        ore_veins_enabled: obj
            .get("ore_veins_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(aquifers_enabled),
        legacy_random_source: obj
            .get("legacy_random_source")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        sea_level: obj
            .get("sea_level")
            .and_then(Value::as_i64)
            .ok_or("noise settings without sea_level")? as i32,
        default_fluid: block_state(obj.get("default_fluid").ok_or("missing default_fluid")?)?,
        shape: GenerationShapeConfig {
            min_y: i8::try_from(min_y).map_err(|_| format!("min_y {min_y} out of range"))?,
            height: u16::try_from(height).map_err(|_| format!("height {height} out of range"))?,
            size_horizontal: explicit("size_horizontal")
                .filter(|v| *v != 0)
                .unwrap_or(default_h),
            size_vertical: explicit("size_vertical")
                .filter(|v| *v != 0)
                .unwrap_or(default_v),
        },
        default_block: block_state(obj.get("default_block").ok_or("missing default_block")?)?,
        spawn_target: Box::leak(spawn_target.into_boxed_slice()),
    })
}

#[cfg(test)]
mod tests {
    use pumpkin_data::noise_router::{
        AMPLIFIED_BASE_NOISE_ROUTER, END_BASE_NOISE_ROUTER, LARGE_BIOMES_BASE_NOISE_ROUTER,
        NETHER_BASE_NOISE_ROUTER, OVERWORLD_BASE_NOISE_ROUTER,
    };
    use pumpkin_util::math::vector3::Vector3;
    use serde_json::json;

    use super::*;
    use crate::generation::GlobalRandomConfig;
    use crate::generation::noise::router::chunk_density_function::ChunkNoiseFunctionBuilderOptions;
    use crate::generation::noise::router::chunk_noise_router::ChunkNoiseRouter;
    use crate::generation::noise::router::multi_noise_sampler::MultiNoiseSampler;
    use crate::generation::noise::router::proto_noise_router::ProtoNoiseRouters;

    /// Every density value a router gives on a grid of positions, for two seeds.
    fn samples(base: &BaseNoiseRouters, legacy: bool) -> Vec<u32> {
        let mut out = Vec::new();
        for seed in [0u64, 1_790_205_903_367_433_061] {
            let random_config = GlobalRandomConfig::new(seed, legacy);
            let proto = ProtoNoiseRouters::generate(base, &random_config);
            let options = ChunkNoiseFunctionBuilderOptions::new(Vec::new(), Vec::new(), None);
            let mut router = ChunkNoiseRouter::generate(&proto.noise, &options);
            let mut multi = MultiNoiseSampler::generate(&proto.multi_noise);
            for x in (-200..200).step_by(37) {
                for z in (-200..200).step_by(41) {
                    for y in (-64..320).step_by(23) {
                        let pos = Vector3::new(x, y, z);
                        for value in [
                            router.final_density(&pos),
                            router.vein_toggle(&pos),
                            router.vein_ridged(&pos),
                            router.vein_gap(&pos),
                        ] {
                            out.push(value.to_bits());
                        }
                    }
                    let point = multi.sample(x >> 2, 16, z >> 2);
                    for value in [
                        point.temperature,
                        point.humidity,
                        point.continentalness,
                        point.erosion,
                        point.depth,
                        point.weirdness,
                    ] {
                        out.push(value as u32);
                    }
                }
            }
        }
        out
    }

    fn build(json: &WorldgenJson, id: &str) -> std::result::Result<DatapackNoiseSettings, String> {
        json.build_noise_settings(id, true)?
            .ok_or_else(|| format!("{id} not built"))
    }

    /// Built from the vanilla JSON, each router is the compiled one: the same stack sizes,
    /// the same settings, the same density everywhere.
    #[test]
    fn vanilla_json_rebuilds_the_compiled_routers() -> std::result::Result<(), String> {
        let json = WorldgenJson::vanilla();
        for (id, compiled, settings) in [
            ("overworld", &OVERWORLD_BASE_NOISE_ROUTER, &NoiseSettings::OVERWORLD),
            ("amplified", &AMPLIFIED_BASE_NOISE_ROUTER, &NoiseSettings::AMPLIFIED),
            ("large_biomes", &LARGE_BIOMES_BASE_NOISE_ROUTER, &NoiseSettings::LARGE_BIOMES),
            ("nether", &NETHER_BASE_NOISE_ROUTER, &NoiseSettings::NETHER),
            ("end", &END_BASE_NOISE_ROUTER, &NoiseSettings::END),
        ] {
            let built = build(&json, id)?;
            assert_eq!(built.settings, settings, "{id}: settings differ");
            let sizes = |r: &BaseNoiseRouters| {
                [
                    r.noise.full_component_stack.len(),
                    r.surface_estimator.full_component_stack.len(),
                    r.multi_noise.full_component_stack.len(),
                    r.vein.full_component_stack.len(),
                    r.noise.final_density,
                    r.multi_noise.continents,
                ]
            };
            assert_eq!(sizes(built.router), sizes(compiled), "{id}: stacks differ");
            assert_eq!(
                samples(built.router, settings.legacy_random_source),
                samples(compiled, settings.legacy_random_source),
                "{id}: densities differ"
            );
        }
        Ok(())
    }

    /// Without a datapack entry nothing is built: every dimension keeps its compiled router.
    #[test]
    fn untouched_settings_are_not_built() {
        let json = WorldgenJson::vanilla();
        let (built, errors) = json.build_datapack_noise_settings();
        assert!(built.is_empty() && errors.is_empty());
    }

    /// A datapack override of one density function rebuilds exactly the settings that use it
    /// and moves their terrain; the others stay compiled.
    #[test]
    fn a_density_override_rebuilds_only_its_users() -> std::result::Result<(), String> {
        let mut json = WorldgenJson::vanilla();
        // The vanilla body under another name, so that the override can build on it.
        let offset = json
            .get(WorldgenRegistry::DensityFunction, "minecraft:overworld/offset")
            .map(|(v, _)| v.clone())
            .ok_or("no vanilla offset")?;
        json.insert_from_datapack(
            WorldgenRegistry::DensityFunction,
            "minecraft:overworld/offset_vanilla",
            offset,
        );
        json.insert_from_datapack(
            WorldgenRegistry::DensityFunction,
            "minecraft:overworld/offset",
            json!({"type": "minecraft:add", "argument1": "minecraft:overworld/offset_vanilla", "argument2": 0.25}),
        );
        let (built, errors) = json.build_datapack_noise_settings();
        assert!(errors.is_empty(), "{errors:?}");
        let ids: Vec<&str> = built.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"minecraft:overworld"), "{ids:?}");
        assert!(!ids.contains(&"minecraft:nether"), "{ids:?}");
        assert!(!ids.contains(&"minecraft:end"), "{ids:?}");
        let overworld = built
            .iter()
            .find(|(id, _)| id == "minecraft:overworld")
            .map(|(_, s)| *s)
            .ok_or("no overworld")?;
        assert_ne!(
            samples(overworld.router, false),
            samples(&OVERWORLD_BASE_NOISE_ROUTER, false)
        );
        Ok(())
    }

    /// A noise a datapack adds gets its own parameters, seeded from its id as vanilla does.
    #[test]
    fn a_datapack_noise_is_seeded_from_its_id() -> std::result::Result<(), String> {
        let parameters = parse_noise(
            &json!({"base_octave": -9, "amplitude_modifiers": [1.0, 1.0, 2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0]}),
            "minecraft:continentalness",
            7,
        )?;
        let compiled = DoublePerlinNoiseParameters::CONTINENTALNESS;
        assert_eq!(
            (parameters.first_octave, parameters.amplitudes, parameters.lo, parameters.hi, parameters.string_hash),
            (compiled.first_octave, compiled.amplitudes, compiled.lo, compiled.hi, compiled.string_hash)
        );
        Ok(())
    }
}
