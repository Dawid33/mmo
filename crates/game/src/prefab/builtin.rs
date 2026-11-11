//! The game's own prefabs: `assets/prefabs/`, embedded at compile time and installed
//! into a process global so `game` and `worldgen` read them without an engine (the
//! `install_block_props` pattern). Server and client must agree on the set, as they
//! must on `blocks.ron`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use super::{prefab_from_ron, prefab_name_key, resolve, ResolvedPrefab, Trust};

/// Where the game's own prefabs live, relative to the repository root.
pub const BUILTIN_PREFAB_DIR: &str = "assets/prefabs";
/// A prefab file is `<name>.prefab.ron`.
pub const PREFAB_FILE_SUFFIX: &str = ".prefab.ron";

/// A set of resolved prefabs addressed by the crc32 of their name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrefabSet {
    by_key: BTreeMap<u32, Arc<ResolvedPrefab>>,
}

impl PrefabSet {
    /// Builds a set from `(file stem, RON text)` pairs. Fails, naming the file, on a
    /// parse error, an invalid prefab, a stem that is not the prefab's name, or two
    /// names with the same key.
    pub fn from_sources<'a>(sources: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Self, String> {
        let mut by_key: BTreeMap<u32, Arc<ResolvedPrefab>> = BTreeMap::new();
        for (stem, src) in sources {
            let file = prefab_from_ron(src).map_err(|e| format!("prefab {stem}: {e}"))?;
            if file.name != stem {
                return Err(format!("prefab {stem}: the file holds a prefab named {:?}", file.name));
            }
            let resolved = resolve(&file, Trust::Builtin).map_err(|problems| {
                let list: Vec<String> = problems.iter().map(ToString::to_string).collect();
                format!("prefab {stem}: {}", list.join("; "))
            })?;
            if let Some(old) = by_key.insert(prefab_name_key(stem), Arc::new(resolved)) {
                return Err(format!("prefab {stem}: its name key collides with prefab {}", old.name));
            }
        }
        Ok(Self { by_key })
    }

    pub fn get(&self, name_key: u32) -> Option<&Arc<ResolvedPrefab>> {
        self.by_key.get(&name_key)
    }

    pub fn named(&self, name: &str) -> Option<&Arc<ResolvedPrefab>> {
        self.get(prefab_name_key(name)).filter(|p| p.name == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_key.values().map(|p| p.name.as_str())
    }
}

/// The prefabs embedded at build time: `(file stem, RON text)`, sorted by stem.
pub fn embedded_prefab_sources() -> &'static [(&'static str, &'static str)] {
    include!(concat!(env!("OUT_DIR"), "/builtin_prefabs.rs"))
}

/// The built-in set: from `disk_dir` when that directory exists (native: edit and
/// restart, no rebuild), else from the embedded copy.
pub fn load_builtin_prefabs(disk_dir: Option<&Path>) -> Result<PrefabSet, String> {
    let Some(entries) = disk_dir.and_then(|d| std::fs::read_dir(d).ok()) else {
        return PrefabSet::from_sources(embedded_prefab_sources().iter().copied());
    };
    let mut files: Vec<(String, String)> = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_name) = entry.file_name().into_string() else { continue };
        let Some(stem) = file_name.strip_suffix(PREFAB_FILE_SUFFIX) else { continue };
        let src = std::fs::read_to_string(entry.path()).map_err(|e| format!("prefab {stem}: {e}"))?;
        files.push((stem.to_string(), src));
    }
    files.sort();
    PrefabSet::from_sources(files.iter().map(|(stem, src)| (stem.as_str(), src.as_str())))
}

static PREFABS: OnceLock<PrefabSet> = OnceLock::new();

/// Installs the process-global built-in set. Idempotent for an equal set; panics if a
/// different one is installed after.
pub fn install_prefabs(set: PrefabSet) {
    let installed = PREFABS.get_or_init(|| set.clone());
    assert_eq!(*installed, set, "conflicting built-in prefabs installed in one process");
}

/// The installed built-in set, or an empty one when none was installed.
pub fn prefabs() -> &'static PrefabSet {
    static EMPTY: PrefabSet = PrefabSet { by_key: BTreeMap::new() };
    PREFABS.get().unwrap_or(&EMPTY)
}
