//! The datapacks' terrain-shaping worldgen files, read before the worlds are created.
//!
//! `worldgen/density_function`, `worldgen/noise` and `worldgen/noise_settings` are built
//! into the noise routers the generator uses (see
//! [`pumpkin_world::generation::noise::router::datapack`]); `worldgen/material_rule` and
//! `worldgen/material_condition`, with the rule a noise settings file names, into its
//! surface rules (see [`pumpkin_world::generation::surface_datapack`]); `worldgen/feature`,
//! `worldgen/placed_feature` and `worldgen/block_state_provider` into the features they
//! override (see [`pumpkin_world::generation::feature::datapack`]).
//!
//! As in vanilla, worldgen registries are fixed for the server's lifetime: `/reload` and
//! `/datapack enable` do not change the terrain of a running server.

use std::fs;
use std::path::Path;

use pumpkin_world::generation::feature::datapack as feature_datapack;
use pumpkin_world::generation::noise::router::datapack::{self, WorldgenJson, WorldgenRegistry};
use pumpkin_world::generation::surface_datapack;
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

/// Builds and installs the noise settings and surface rules that the enabled datapacks change.
///
/// Packs apply in the order `level.dat` enables them, a later one overriding an earlier one,
/// as in vanilla. Call once, before the worlds are loaded.
pub fn install(world_path: &Path, enabled_packs: &[String]) {
    let datapacks = world_path.join("datapacks");
    let mut json = WorldgenJson::vanilla();
    let mut files = 0;
    for pack in enabled_packs {
        let name = pack.strip_prefix("file/").unwrap_or(pack);
        let path = datapacks.join(name);
        if path.is_dir() {
            files += load_pack(&path, &mut json);
        }
    }
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
}
