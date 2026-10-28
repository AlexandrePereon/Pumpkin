//! Registries whose Extractor dump is a plain list of names, extended at startup.
//!
//! Menu types (`screens.json`), sound events (`sounds.json`), particle types
//! (`particles.json`), custom stats (`custom_stats.json`) and point of interest
//! types (`poi_types.json`) are each dumped as a JSON array in raw-id order,
//! vanilla names bare and any other namespace kept. Pumpkin's generated enums
//! cover the vanilla prefix; what follows it belongs to mods, has to be
//! dictated to Fabric clients (all these registries are synced), and is sent by
//! raw id.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use pumpkin_data::particle::Particle;
use pumpkin_data::sound::Sound;
use pumpkin_data::statistic::CustomStatistic;
use tracing::{info, warn};

/// One name-list registry: its dump file, how many entries the generated
/// vanilla set has, and — once installed — every name in raw-id order.
pub struct NameListExtension {
    file: &'static str,
    what: &'static str,
    vanilla_count: usize,
    /// All names, vanilla first; set only when the dump has some outside the
    /// vanilla set.
    names: OnceLock<Vec<&'static str>>,
}

/// Sound events beyond `pumpkin_data::sound::Sound`.
pub static SOUND_EVENTS: NameListExtension =
    NameListExtension::new("sounds.json", "sound event", Sound::NAMES.len());

/// Particle types beyond `pumpkin_data::particle::Particle`.
pub static PARTICLE_TYPES: NameListExtension = NameListExtension::new(
    "particles.json",
    "particle type",
    Particle::SulfurCubeGoo as usize + 1,
);

/// Custom stats beyond `pumpkin_data::statistic::CustomStatistic`.
pub static CUSTOM_STATS: NameListExtension = NameListExtension::new(
    "custom_stats.json",
    "custom stat",
    CustomStatistic::InteractWithSmithingTable as usize + 1,
);

/// Point of interest types. Pumpkin generates none; 26.3 has 21, from
/// `armorer` to `lightning_rod`.
pub static POI_TYPES: NameListExtension =
    NameListExtension::new("poi_types.json", "point of interest type", 21);

impl NameListExtension {
    #[must_use]
    pub const fn new(file: &'static str, what: &'static str, vanilla_count: usize) -> Self {
        Self {
            file,
            what,
            vanilla_count,
            names: OnceLock::new(),
        }
    }

    /// Reads this registry's file from an Extractor dump and installs its
    /// names if any lies outside the vanilla set.
    pub fn install_from_dump(&self, dir: &Path) {
        let path = dir.join(self.file);
        let names: Vec<String> = match fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
        {
            Ok(names) => names,
            Err(error) => {
                warn!("Could not load {}: {error}", path.display());
                return;
            }
        };
        match self.validate(&names) {
            Ok(0) => {}
            Ok(count) => {
                let names = names
                    .into_iter()
                    .map(|n| &*Box::leak(n.into_boxed_str()))
                    .collect();
                if self.names.set(names).is_ok() {
                    info!("Installed {count} extension {}(s)", self.what);
                }
            }
            Err(error) => warn!("Extension {}s not installed: {error}", self.what),
        }
    }

    /// The number of extension entries, if the vanilla ones come first, bare,
    /// and the rest are namespaced and fit a raw id.
    fn validate(&self, names: &[String]) -> Result<usize, String> {
        let (vanilla, extension) = names.split_at(self.vanilla_count.min(names.len()));
        if vanilla.len() != self.vanilla_count || vanilla.iter().any(|name| name.contains(':')) {
            return Err(format!(
                "{} does not start with the vanilla {}s",
                self.file, self.what
            ));
        }
        if let Some(name) = extension.iter().find(|name| !name.contains(':')) {
            return Err(format!(
                "{name} comes after the vanilla {}s without a namespace",
                self.what
            ));
        }
        if names.len() > usize::from(u16::MAX) {
            return Err(format!("too many {}s", self.what));
        }
        Ok(extension.len())
    }

    /// The raw id of an extension entry, by full name.
    #[must_use]
    pub fn id_of(&self, name: &str) -> Option<u16> {
        let index = self
            .names
            .get()?
            .iter()
            .skip(self.vanilla_count)
            .position(|n| *n == name)?;
        u16::try_from(self.vanilla_count + index).ok()
    }

    /// Every entry with its raw id, for registry sync; empty without extension.
    #[must_use]
    pub fn registry_entries(&self) -> Vec<(&'static str, u16)> {
        self.names.get().map_or_else(Vec::new, |names| {
            names
                .iter()
                .zip(0u16..)
                .map(|(name, id)| (*name, id))
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::NameListExtension;

    const VANILLA_COUNT: usize = 3;
    static LIST: NameListExtension = NameListExtension::new("test.json", "entry", VANILLA_COUNT);

    fn vanilla() -> Vec<String> {
        (0..VANILLA_COUNT).map(|i| format!("entry_{i}")).collect()
    }

    #[test]
    fn counts_namespaced_entries_after_vanilla() {
        let mut names = vanilla();
        assert_eq!(LIST.validate(&names), Ok(0));
        names.push("verse:chime".into());
        assert_eq!(LIST.validate(&names), Ok(1));
    }

    #[test]
    fn rejects_bad_dumps() {
        assert!(LIST.validate(&vanilla()[1..]).is_err());
        let mut names = vanilla();
        names.push("chime".into());
        assert!(LIST.validate(&names).is_err());
    }
}
