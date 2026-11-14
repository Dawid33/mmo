//! The game's own prefab set: embedded at build time, or read from disk in development.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use super::codec::{prefab_from_named_bytes, BIN_SUFFIX, RON_SUFFIX};
use super::resolve::{prefab_name_key, resolve, ResolvedPrefab, Trust};
use crate::registry::BlockId;

pub const BUILTIN_PREFAB_DIR: &str = "assets/prefabs";

#[derive(Debug, Clone, Default)]
pub struct PrefabSet {
    by_key: BTreeMap<u32, Arc<ResolvedPrefab>>,
    content_hash: u32,
}

/// Equal when the resolved prefabs are; the source bytes (and so `content_hash`) do not count.
impl PartialEq for PrefabSet {
    fn eq(&self, other: &Self) -> bool {
        self.by_key == other.by_key
    }
}

impl PrefabSet {
    /// `(file name, bytes)` pairs. Errors name the file.
    pub fn from_sources<'a>(
        sources: impl IntoIterator<Item = (&'a str, &'a [u8])>,
        block_named: &dyn Fn(&str) -> Option<BlockId>,
    ) -> Result<Self, String> {
        let mut by_key: BTreeMap<u32, Arc<ResolvedPrefab>> = BTreeMap::new();
        let sources: BTreeMap<&str, &[u8]> = sources.into_iter().collect();
        let mut hasher = crc32fast::Hasher::new();
        for (file_name, bytes) in &sources {
            hasher.update(file_name.as_bytes());
            hasher.update(&[0]);
            hasher.update(bytes);
        }
        for (file_name, bytes) in sources {
            let Some(decoded) = prefab_from_named_bytes(file_name, bytes, None) else { continue };
            let file = decoded.map_err(|e| format!("{file_name}: {e}"))?;
            let resolved = resolve(&file, block_named, Trust::Builtin).map_err(|problems| {
                let all: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
                format!("{file_name}: {} problem(s): {}", all.len(), all.join("; "))
            })?;
            let stem = file_name
                .strip_suffix(RON_SUFFIX)
                .or_else(|| file_name.strip_suffix(BIN_SUFFIX))
                .unwrap_or(file_name);
            if stem != resolved.name {
                return Err(format!("{file_name}: the file stem must be the prefab's name \"{}\"", resolved.name));
            }
            if !resolved.meta.source.is_empty() && resolved.meta.licence.is_empty() {
                return Err(format!("{file_name}: meta.source is set but meta.licence is empty"));
            }
            let key = resolved.name_key;
            if let Some(other) = by_key.get(&key) {
                return Err(format!(
                    "{file_name}: prefab name \"{}\" has the same key as \"{}\"",
                    resolved.name, other.name
                ));
            }
            by_key.insert(key, Arc::new(resolved));
        }
        Ok(Self { by_key, content_hash: hasher.finalize() })
    }

    /// crc32 over the sources in sorted file-name order (name bytes, a 0 byte, content bytes, per file):
    /// equal sources, equal hash; any changed byte, another hash.
    pub fn content_hash(&self) -> u32 {
        self.content_hash
    }

    pub fn get(&self, name_key: u32) -> Option<&Arc<ResolvedPrefab>> {
        self.by_key.get(&name_key)
    }

    pub fn named(&self, name: &str) -> Option<&Arc<ResolvedPrefab>> {
        self.get(prefab_name_key(name)).filter(|p| p.name == name)
    }

    /// Ascending by key.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_key.values().map(|p| p.name.as_str())
    }

    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }
}

/// `(file name, bytes)`, sorted by file name.
pub fn embedded_prefab_sources() -> &'static [(&'static str, &'static [u8])] {
    include!(concat!(env!("OUT_DIR"), "/builtin_prefabs.rs"))
}

/// The same few lines as `takes_file` / `walk` in `crates/game/build.rs`: keep the two filters identical
/// (a file named `*.prefab.ron` or `*.prefab.bin`, not starting with `.`; directories starting with `.`
/// and symlinked directories are not entered; a name that is not UTF-8 is skipped).
fn takes_file(name: &str) -> bool {
    !name.starts_with('.') && (name.ends_with(RON_SUFFIX) || name.ends_with(BIN_SUFFIX))
}

fn read_dir_sorted(dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(name) = e.file_name().into_string() else { continue };
        let kind = e.file_type().map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = e.path();
        if kind.is_dir() {
            if !name.starts_with('.') {
                read_dir_sorted(&path, out)?;
            }
        } else if takes_file(&name) && (kind.is_file() || path.is_file()) {
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            out.push((name, bytes));
        }
    }
    Ok(())
}

/// From `disk_dir` when it exists (recursive, the same file filter as the build script), else the embedded copy.
pub fn load_builtin_prefabs(
    disk_dir: Option<&Path>,
    block_named: &dyn Fn(&str) -> Option<BlockId>,
) -> Result<PrefabSet, String> {
    match disk_dir.filter(|d| d.is_dir()) {
        Some(dir) => {
            let mut files = Vec::new();
            read_dir_sorted(dir, &mut files)?;
            files.sort_by(|a, b| a.0.cmp(&b.0));
            PrefabSet::from_sources(files.iter().map(|(n, b)| (n.as_str(), b.as_slice())), block_named)
        }
        None => PrefabSet::from_sources(embedded_prefab_sources().iter().copied(), block_named),
    }
}

static PREFABS: OnceLock<PrefabSet> = OnceLock::new();

/// Installs the process-global set. Idempotent for an equal set; panics on a different one.
pub fn install_prefabs(set: PrefabSet) {
    let installed = PREFABS.get_or_init(|| set.clone());
    assert_eq!(*installed, set, "conflicting prefab sets installed in one process");
}

/// The installed set, or an empty one when none was.
pub fn prefabs() -> &'static PrefabSet {
    static EMPTY: PrefabSet = PrefabSet { by_key: BTreeMap::new(), content_hash: 0 };
    PREFABS.get().unwrap_or(&EMPTY)
}
