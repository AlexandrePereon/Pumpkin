//! The datapacks' terrain-shaping worldgen files, read before the worlds are created.
//!
//! `worldgen/density_function`, `worldgen/noise` and `worldgen/noise_settings` are built
//! into the noise routers the generator uses (see
//! [`pumpkin_world::generation::noise::router::datapack`]); `worldgen/material_rule` and
//! `worldgen/material_condition`, with the rule a noise settings file names, into its
//! surface rules (see [`pumpkin_world::generation::surface_datapack`]); `worldgen/feature`,
//! `worldgen/placed_feature` and `worldgen/block_state_provider` into the features they
//! override (see [`pumpkin_world::generation::feature::datapack`]); `worldgen/biome` into the
//! feature lists and carvers of the vanilla biomes it overrides (see
//! [`pumpkin_world::generation::feature::biome_datapack`]); `worldgen/structure` and
//! `worldgen/structure_set` files of a pack's own namespace into structures of registered
//! types and their sets (see [`pumpkin_world::generation::structure::datapack`]).
//!
//! A `worldgen/biome` file of a pack's own namespace becomes a biome (installed into
//! `pumpkin_data::biome_extension`, ids after vanilla's in the synced registry's order), and a
//! `dimension/overworld.json` (or `the_nether.json`) with an inline `multi_noise` list
//! replaces that dimension's biome source (see [`pumpkin_world::biome::datapack`]). Both are
//! installed first, before the tags (a biome tag can name a pack's biome) and the rest.
//!
//! As in vanilla, worldgen registries are fixed for the server's lifetime: `/reload` and
//! `/datapack enable` do not change the terrain of a running server.

use std::fs;
use std::path::Path;

use pumpkin_world::biome::{self, DatapackDimension};
use pumpkin_world::generation::feature::biome_datapack;
use pumpkin_world::generation::feature::datapack as feature_datapack;
use pumpkin_world::generation::noise::router::datapack::{self, WorldgenJson, WorldgenRegistry};
use pumpkin_world::generation::structure::datapack as structure_datapack;
use pumpkin_world::generation::surface_datapack;
use serde_json::Value;
use tracing::{info, warn};

/// Reads every `.json` under `dir` into `json`, with ids `namespace:relative/path`.
fn load_dir(
    namespace: &str,
    registry: WorldgenRegistry,
    base: &Path,
    dir: &Path,
    json: &mut WorldgenJson,
) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut count = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            count += load_dir(namespace, registry, base, &path, json);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(relative) = path
            .with_extension("")
            .strip_prefix(base)
            .map(Path::to_path_buf)
        else {
            continue;
        };
        let relative = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let id = format!("{namespace}:{relative}");
        match fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
        {
            Ok(value) => {
                json.insert_from_datapack(registry, &id, value);
                count += 1;
            }
            Err(error) => warn!("Skipping worldgen file {}: {error}", path.display()),
        }
    }
    count
}

fn load_pack(pack: &Path, json: &mut WorldgenJson) -> usize {
    let Ok(namespaces) = fs::read_dir(pack.join("data")) else {
        return 0;
    };
    let mut count = 0;
    for namespace in namespaces.flatten() {
        let worldgen = namespace.path().join("worldgen");
        if !worldgen.is_dir() {
            continue;
        }
        let namespace = namespace.file_name().to_string_lossy().into_owned();
        for registry in WorldgenRegistry::ALL {
            let dir = worldgen.join(registry.folder());
            count += load_dir(&namespace, registry, &dir, &dir, json);
        }
    }
    count
}

/// The enabled datapacks' worldgen files and dimension files, read once at startup.
pub struct LoadedWorldgen {
    json: WorldgenJson,
    files: usize,
    /// The packs' `dimension/overworld.json` and `the_nether.json`, the last pack's winning.
    dimensions: Vec<(DatapackDimension, Value)>,
}

/// Reads the enabled datapacks' worldgen and dimension files.
///
/// Packs apply in the order `level.dat` enables them, a later one overriding an earlier one,
/// as in vanilla.
#[must_use]
pub fn load(world_path: &Path, enabled_packs: &[String]) -> LoadedWorldgen {
    let datapacks = world_path.join("datapacks");
    let mut json = WorldgenJson::vanilla();
    let mut files = 0;
    let mut dimensions: Vec<(DatapackDimension, Value)> = Vec::new();
    for pack in enabled_packs {
        let name = pack.strip_prefix("file/").unwrap_or(pack);
        let path = datapacks.join(name);
        if !path.is_dir() {
            continue;
        }
        files += load_pack(&path, &mut json);
        for (dimension, file) in [
            (DatapackDimension::Overworld, "overworld.json"),
            (DatapackDimension::Nether, "the_nether.json"),
        ] {
            let file = path.join("data/minecraft/dimension").join(file);
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            match serde_json::from_str(&text) {
                Ok(value) => {
                    dimensions.retain(|(d, _)| *d != dimension);
                    dimensions.push((dimension, value));
                }
                Err(error) => warn!("Skipping dimension file {}: {error}", file.display()),
            }
        }
    }
    LoadedWorldgen {
        json,
        files,
        dimensions,
    }
}

/// Builds and installs the packs' own biomes, then the dimensions' biome sources (which may
/// name them). Call once, before the tags and [`install`].
pub fn install_biomes(loaded: &LoadedWorldgen) {
    let (biomes, warnings) = biome::datapack::build_own_biomes(&loaded.json);
    for warning in &warnings {
        warn!("Datapack biome: {warning}");
    }
    if !biomes.is_empty() {
        let names: Vec<&str> = biomes.iter().map(|b| b.registry_id).collect();
        match pumpkin_data::biome_extension::install(biomes) {
            Ok(()) => info!(
                "Biomes of the datapacks' own installed: {} (ids from {})",
                names.join(", "),
                pumpkin_data::biome::Biome::ALL.len()
            ),
            Err(error) => warn!("Datapack biomes not installed: {error}"),
        }
    }
    for (dimension, value) in &loaded.dimensions {
        match biome::datapack::dimension_biome_list(value) {
            Ok(Some(list)) => {
                let entries = list.len();
                if biome::install_datapack_source(*dimension, list) {
                    info!(
                        "Biome source of the {dimension:?} built from a datapack ({entries} entries)"
                    );
                } else {
                    warn!(
                        "A datapack biome source was installed already; this change needs a restart"
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                warn!("Datapack biome source of the {dimension:?} kept compiled: {error}");
            }
        }
    }
}

/// Builds and installs the noise settings and surface rules that the enabled datapacks change.
///
/// Call once, before the worlds are loaded.
pub fn install(loaded: LoadedWorldgen) {
    let LoadedWorldgen { json, files, .. } = loaded;
    if files == 0 {
        return;
    }
    let (built, errors) = json.build_datapack_noise_settings();
    for error in &errors {
        warn!("Datapack noise settings kept compiled: {error}");
    }
    let ids: Vec<&str> = built.iter().map(|(id, _)| id.as_str()).collect();
    info!(
        "Loaded {files} datapack worldgen file(s); noise settings built from them: {}",
        if ids.is_empty() {
            "none".to_string()
        } else {
            ids.join(", ")
        }
    );
    if !datapack::install(built) {
        warn!("Datapack noise settings were installed already; this change needs a restart");
    }
    let (rules, errors) = json.build_datapack_surface_rules();
    for error in &errors {
        warn!("Datapack surface rule kept compiled: {error}");
    }
    if !rules.is_empty() {
        let ids: Vec<&str> = rules.iter().map(|(id, _)| id.as_str()).collect();
        info!("Surface rules built from datapacks for: {}", ids.join(", "));
    }
    if !surface_datapack::install(rules) {
        warn!("Datapack surface rules were installed already; this change needs a restart");
    }
    let (features, errors) = json.build_datapack_features();
    for error in &errors {
        warn!("Datapack feature kept compiled: {error}");
    }
    if !features.is_empty() {
        info!(
            "Features built from datapacks: placed {}; configured {}",
            features.placed_names().join(", "),
            features.configured_names().join(", ")
        );
    }
    if !feature_datapack::install(features) {
        warn!("Datapack features were installed already; this change needs a restart");
    }
    let (biomes, errors) = json.build_datapack_biomes();
    for error in &errors {
        if error.starts_with("placed feature kept") {
            warn!("Datapack biome: {error}");
        } else {
            warn!("Datapack biome kept compiled: {error}");
        }
    }
    if !biomes.is_empty() {
        info!(
            "Biome feature lists and carvers built from datapacks for: {} ({} placed feature(s) of their own)",
            biomes.names().join(", "),
            biomes.own_count()
        );
    }
    if !biome_datapack::install(biomes) {
        warn!("Datapack biomes were installed already; this change needs a restart");
    }
    let (structures, errors) = json.build_datapack_structures();
    for error in &errors {
        warn!("Datapack structure skipped: {error}");
    }
    if !structures.is_empty() {
        info!(
            "Structures built from datapacks: {}; structure sets: {}",
            structures.structure_names().join(", "),
            structures.set_names().join(", ")
        );
    }
    if let Err(error) = structure_datapack::install(structures) {
        warn!("Datapack structures not installed: {error}");
    }
}
