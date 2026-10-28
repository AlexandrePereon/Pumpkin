//! Block and item tags from datapacks.
//!
//! Vanilla's tags are generated into `pumpkin_data::tag`. A datapack may
//! extend them — adding its blocks to `minecraft:mineable/pickaxe` — or
//! declare its own; both reach the generated lookups through
//! `pumpkin_data::tag_extension`, and the client through `UpdateTags`.
//!
//! Upstream's `tag_loader` (the `DynamicRegistryManager`) also reads every
//! tag file, into its own map, which nothing consumes yet; this loader stays
//! beside it until it can build on that one.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;

use pumpkin_data::Block;
use pumpkin_data::item::Item;
use pumpkin_data::tag::{RegistryKey, get_latest_map, get_tag_includes};
use pumpkin_data::tag_extension::{self, ExtensionTag};
use serde::Deserialize;
use tracing::{info, warn};

/// The registries whose tags are read, by their folder names under `tags/`.
const REGISTRIES: [(&str, RegistryKey); 4] = [
    ("block", RegistryKey::Block),
    ("blocks", RegistryKey::Block),
    ("item", RegistryKey::Item),
    ("items", RegistryKey::Item),
];

#[derive(Deserialize)]
struct TagFile {
    #[serde(default)]
    replace: bool,
    values: Vec<TagValue>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TagValue {
    Id(String),
    Entry {
        id: String,
        #[serde(default = "required_by_default")]
        required: bool,
    },
}

const fn required_by_default() -> bool {
    true
}

/// One entry of a tag file: an id, or `#` and a tag.
struct Entry {
    id: String,
    required: bool,
}

/// Tag files read so far, per registry and tag, in pack order: a file that
/// replaces drops what earlier packs gave.
#[derive(Default)]
pub struct RawTags {
    tags: HashMap<RegistryKey, BTreeMap<String, Vec<Entry>>>,
    /// Tags whose last replacing file drops vanilla's entries too.
    replaced: HashSet<(RegistryKey, String)>,
}

impl RawTags {
    fn is_empty(&self) -> bool {
        self.tags.values().all(BTreeMap::is_empty)
    }
}

/// Reads one namespace's `tags/` folder; returns how many tag files it held.
pub fn load_tags_from_dir(namespace: &str, tags_dir: &Path, raw: &mut RawTags) -> usize {
    let mut count = 0;
    for (folder, key) in REGISTRIES {
        let dir = tags_dir.join(folder);
        if dir.is_dir() {
            count += load_recursive(namespace, &dir, &dir, key, raw);
        }
    }
    count
}

fn load_recursive(
    namespace: &str,
    base: &Path,
    dir: &Path,
    key: RegistryKey,
    raw: &mut RawTags,
) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    let mut count = 0;
    for path in paths {
        if path.is_dir() {
            count += load_recursive(namespace, base, &path, key, raw);
            continue;
        }
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            continue;
        }
        let Ok(rel) = path.strip_prefix(base) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let name = format!("{namespace}:{}", rel.trim_end_matches(".json"));
        let file: TagFile = match fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        {
            Ok(file) => file,
            Err(error) => {
                warn!("Tag {name}: {error}");
                continue;
            }
        };
        let tag = raw
            .tags
            .entry(key)
            .or_default()
            .entry(name.clone())
            .or_default();
        if file.replace {
            tag.clear();
            raw.replaced.insert((key, name));
        }
        tag.extend(file.values.into_iter().map(|value| match value {
            TagValue::Id(id) => Entry { id, required: true },
            TagValue::Entry { id, required } => Entry { id, required },
        }));
        count += 1;
    }
    count
}

/// Resolves the tags read and installs them, once: tags do not change on
/// `/reload`, which also sends the client none.
pub fn install(raw: &RawTags) {
    if raw.is_empty() {
        return;
    }
    let mut tags = Vec::new();
    for (&key, files) in &raw.tags {
        tags.extend(Resolver::new(key, files, &raw.replaced).resolve_all());
    }
    let count = tags.len();
    match tag_extension::install(tags) {
        Ok(()) => info!("Installed {count} datapack tag(s)"),
        Err(tag_extension::InstallError::AlreadyInstalled) => {
            info!("Datapack tags are only read at startup; /reload leaves them as they were");
        }
        Err(error) => warn!("Could not install datapack tags: {error}"),
    }
}

fn namespaced(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// Members of each tag, by raw id, with the names they go by.
type Members = Vec<(String, u16)>;

struct Resolver<'a> {
    key: RegistryKey,
    files: &'a BTreeMap<String, Vec<Entry>>,
    replaced: &'a HashSet<(RegistryKey, String)>,
    done: HashMap<String, Option<Members>>,
}

impl<'a> Resolver<'a> {
    fn new(
        key: RegistryKey,
        files: &'a BTreeMap<String, Vec<Entry>>,
        replaced: &'a HashSet<(RegistryKey, String)>,
    ) -> Self {
        Self {
            key,
            files,
            replaced,
            done: HashMap::new(),
        }
    }

    fn id_of(&self, name: &str) -> Option<u16> {
        match self.key {
            RegistryKey::Block => Block::from_name(name).map(|block| block.id.as_u16()),
            RegistryKey::Item => Item::from_registry_key(name).map(|item| item.id),
            _ => None,
        }
    }

    /// The tags the files define, and every vanilla tag that includes one of
    /// them, directly or not: its members change with them.
    fn affected(&self) -> BTreeSet<String> {
        let vanilla = get_latest_map(self.key);
        let mut affected: BTreeSet<String> = self.files.keys().cloned().collect();
        loop {
            let before = affected.len();
            for name in vanilla.keys() {
                if !affected.contains(*name)
                    && get_tag_includes(self.key, name)
                        .iter()
                        .any(|inc| affected.contains(*inc))
                {
                    affected.insert((*name).to_owned());
                }
            }
            if affected.len() == before {
                return affected;
            }
        }
    }

    fn resolve_all(mut self) -> Vec<ExtensionTag> {
        let mut tags = Vec::new();
        for name in self.affected() {
            let Some(members) = self.resolve(&name, &mut Vec::new()) else {
                continue;
            };
            let (values, ids) = members
                .into_iter()
                .map(|(member, id)| {
                    let bare = member.strip_prefix("minecraft:").unwrap_or(&member);
                    (bare.to_owned(), id)
                })
                .unzip();
            tags.push(ExtensionTag {
                registry: self.key,
                name,
                values,
                ids,
            });
        }
        tags
    }

    /// A tag's members, or `None` when a required entry is missing — vanilla
    /// then drops the tag, and Pumpkin keeps its generated one.
    fn resolve(&mut self, name: &str, stack: &mut Vec<String>) -> Option<Members> {
        if let Some(done) = self.done.get(name) {
            return done.clone();
        }
        if stack.iter().any(|s| s == name) {
            warn!("Tag {name} includes itself");
            return None;
        }
        stack.push(name.to_owned());
        let members = self.members(name, stack);
        stack.pop();
        self.done.insert(name.to_owned(), members.clone());
        members
    }

    fn members(&mut self, name: &str, stack: &mut Vec<String>) -> Option<Members> {
        let mut members: Members = Vec::new();
        let push = |members: &mut Members, entry: (String, u16)| {
            if !members.iter().any(|(_, id)| *id == entry.1) {
                members.push(entry);
            }
        };

        // Vanilla's entries, unless a datapack replaced the tag; and what the
        // tags it includes have gained since.
        if !self.replaced.contains(&(self.key, name.to_owned()))
            && let Some(vanilla) = get_latest_map(self.key).get(name)
        {
            for (value, id) in vanilla.0.iter().zip(vanilla.1) {
                push(&mut members, (namespaced(value), *id));
            }
            for include in get_tag_includes(self.key, name) {
                for entry in self.resolve(include, stack)? {
                    push(&mut members, entry);
                }
            }
        }

        let files = self.files;
        for entry in files.get(name).into_iter().flatten() {
            if let Some(tag) = entry.id.strip_prefix('#') {
                let tag = namespaced(tag);
                let known = files.contains_key(&tag) || get_latest_map(self.key).contains_key(&tag);
                match known.then(|| self.resolve(&tag, stack)).flatten() {
                    Some(entries) => entries.into_iter().for_each(|e| push(&mut members, e)),
                    None if entry.required => {
                        warn!("Tag {name}: tag #{tag} is missing; tag skipped");
                        return None;
                    }
                    None => {}
                }
            } else {
                let id = namespaced(&entry.id);
                match self.id_of(&id) {
                    Some(raw) => push(&mut members, (id, raw)),
                    None if entry.required => {
                        warn!("Tag {name}: {id} is not registered; tag skipped");
                        return None;
                    }
                    None => {}
                }
            }
        }
        Some(members)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use pumpkin_data::Block;
    use pumpkin_data::tag::{RegistryKey, get_latest_map};
    use pumpkin_data::tag_extension::ExtensionTag;

    use super::{Entry, Resolver};

    fn entry(id: &str, required: bool) -> Entry {
        Entry {
            id: id.to_owned(),
            required,
        }
    }

    fn resolve(
        files: &BTreeMap<String, Vec<Entry>>,
        replaced: &HashSet<(RegistryKey, String)>,
    ) -> Vec<ExtensionTag> {
        Resolver::new(RegistryKey::Block, files, replaced).resolve_all()
    }

    fn ids<'a>(tags: &'a [ExtensionTag], name: &str) -> Option<&'a [u16]> {
        tags.iter().find(|t| t.name == name).map(|t| &t.ids[..])
    }

    #[test]
    fn additions_reach_the_vanilla_tags_that_include_them() {
        let stone = Block::STONE.id.as_u16();
        let files = BTreeMap::from([(
            "minecraft:oak_logs".to_owned(),
            vec![entry("stone", true), entry("othermod:missing", false)],
        )]);
        let tags = resolve(&files, &HashSet::new());

        // oak_logs <- logs_that_burn <- logs <- mineable/axe, each by `#`.
        for name in [
            "minecraft:oak_logs",
            "minecraft:logs_that_burn",
            "minecraft:logs",
            "minecraft:mineable/axe",
        ] {
            let tag = ids(&tags, name).unwrap_or_else(|| panic!("{name} not resolved"));
            assert!(tag.contains(&stone), "{name} lacks stone");
            let vanilla = get_latest_map(RegistryKey::Block).get(name).unwrap().1;
            assert_eq!(tag.len(), vanilla.len() + 1, "{name}");
        }
        assert!(ids(&tags, "minecraft:mineable/pickaxe").is_none());
        let oak = tags
            .iter()
            .find(|t| t.name == "minecraft:oak_logs")
            .unwrap();
        assert_eq!(oak.values.last().map(String::as_str), Some("stone"));
    }

    #[test]
    fn replace_and_missing_entries() {
        let stone = Block::STONE.id.as_u16();
        let files = BTreeMap::from([
            ("minecraft:oak_logs".to_owned(), vec![entry("stone", true)]),
            (
                "verse:broken".to_owned(),
                vec![entry("stone", true), entry("othermod:missing", true)],
            ),
            (
                "verse:nested".to_owned(),
                vec![
                    entry("#minecraft:oak_logs", true),
                    entry("#verse:absent", false),
                ],
            ),
        ]);
        let replaced = HashSet::from([(RegistryKey::Block, "minecraft:oak_logs".to_owned())]);
        let tags = resolve(&files, &replaced);

        assert_eq!(ids(&tags, "minecraft:oak_logs"), Some(&[stone][..]));
        assert_eq!(ids(&tags, "verse:nested"), Some(&[stone][..]));
        assert!(ids(&tags, "verse:broken").is_none());
    }
}
