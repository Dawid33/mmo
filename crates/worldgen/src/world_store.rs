//! On-disk worlds for the main menu: `worlds/<slug>/{terrain.world, world.json,
//! preview.png}`, plus the legacy single `worlds/default.world`.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::forge::knobs::Knobs;
#[cfg(not(target_arch = "wasm32"))]
use crate::forged::WorldArtifact;

pub const WORLDS_DIR: &str = "worlds";
pub const TERRAIN_FILE: &str = "terrain.world";
/// The world store (SQLite, plus `-wal` / `-shm` / `.lock` sidecars): where a
/// single-player world lives once the server has imported `terrain.world`.
pub const STORE_FILE: &str = "terrain.sqlite";
pub const META_FILE: &str = "world.json";

/// The world store beside a forge file: `<terrain>.with_extension("sqlite")` —
/// `worlds/<slug>/terrain.sqlite` (= [`STORE_FILE`]) for a menu world,
/// `worlds/default.sqlite` for the legacy `default.world`. The one rule the
/// menu listing, the client's spawned server and the server's default
/// `VOXEL_WORLD_DB` share.
pub fn store_path_for(terrain: &Path) -> PathBuf {
    terrain.with_extension("sqlite")
}
pub const PREVIEW_FILE: &str = "preview.png";
pub const LEGACY_WORLD: &str = "worlds/default.world";
/// `world.json`'s format. v2: `width_m` and `height_m` replace `size_m`. An
/// older file is listed as needing a re-forge.
pub const META_FORMAT: u32 = 2;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WorldMeta {
    pub format_version: u32,
    pub name: String,
    pub slug: String,
    /// RFC 3339, UTC; ordering is lexical.
    pub created_utc: String,
    pub seed: u64,
    /// West-east extent (x), metres.
    pub width_m: f64,
    /// North-south extent (z), metres.
    pub height_m: f64,
    pub cell_m: f64,
    pub preset: String,
    pub knobs: BTreeMap<String, f64>,
    pub forge_ms: u64,
    pub region_blocks: u32,
    pub chunk_blocks: u32,
    /// `forged::ARTIFACT_VERSION` the terrain file was written with.
    pub artifact_version: u32,
}

#[derive(Clone, Debug)]
pub struct WorldEntry {
    pub dir: PathBuf,
    pub terrain: PathBuf,
    /// `terrain.sqlite` beside the terrain file (may not exist yet).
    pub store: PathBuf,
    pub preview: Option<PathBuf>,
    pub meta: Option<WorldMeta>,
    pub legacy: bool,
    pub loadable: Result<(), String>,
}

/// A world is loadable from `terrain.world` (a pending import, or a legacy
/// file) or, once the server has imported it, from the store. The store is NOT
/// opened here (this crate does not link sqlx): its artifact version is the
/// one `world.json` recorded.
fn check_terrain(terrain: &Path, store: &Path, meta: &Option<WorldMeta>) -> Result<(), String> {
    if terrain.is_file() {
        return check_artifact_file(terrain);
    }
    if store.is_file() {
        let v = match meta {
            Some(m) => m.artifact_version,
            None => return Err(format!("{META_FILE} is missing or unreadable")),
        };
        return check_version(v);
    }
    Err(format!("{TERRAIN_FILE} is missing"))
}

fn check_artifact_file(terrain: &Path) -> Result<(), String> {
    match crate::forged::read_artifact_version(terrain) {
        Ok(v) => check_version(v),
        Err(e) => Err(e.to_string()),
    }
}

fn check_version(v: u32) -> Result<(), String> {
    use crate::forged::{ARTIFACT_VERSION, ArtifactError};
    if v == ARTIFACT_VERSION {
        Ok(())
    } else if v < ARTIFACT_VERSION {
        Err(format!(
            "needs re-forge: {}",
            ArtifactError::OldVersion { found: v }
        ))
    } else {
        Err(format!(
            "world file version {v} is newer than the {ARTIFACT_VERSION} this build reads"
        ))
    }
}

/// A world's `world.json`: its meta, or why it cannot be used. A file of an
/// older [`META_FORMAT`] (which no longer parses as a [`WorldMeta`]) says the
/// world needs a re-forge, like an old world file does.
fn read_meta(path: &Path) -> Result<WorldMeta, String> {
    let unreadable = || format!("{META_FILE} is missing or unreadable");
    let bytes = std::fs::read(path).map_err(|_| unreadable())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| unreadable())?;
    match value
        .get("format_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(v) if v < u64::from(META_FORMAT) => {
            return Err(format!(
                "needs re-forge: {META_FILE} format {v} is older than {META_FORMAT}: re-forge this world"
            ));
        }
        Some(v) if v > u64::from(META_FORMAT) => {
            return Err(format!(
                "{META_FILE} format {v} is newer than the {META_FORMAT} this build reads"
            ));
        }
        _ => {}
    }
    serde_json::from_value(value).map_err(|_| unreadable())
}

fn check_meta(meta: &Option<WorldMeta>) -> Result<(), String> {
    let Some(m) = meta else {
        return Err(format!("{META_FILE} is missing or unreadable"));
    };
    if m.region_blocks != game::REGION_BLOCKS as u32 {
        return Err(format!(
            "made with {} m regions, this build uses {} m",
            m.region_blocks,
            game::REGION_BLOCKS
        ));
    }
    if m.chunk_blocks != game::CHUNK_BLOCKS as u32 {
        return Err(format!(
            "made with {}-block chunks, this build uses {}",
            m.chunk_blocks,
            game::CHUNK_BLOCKS
        ));
    }
    Ok(())
}

/// Worlds under `root` (the `worlds/` directory): newest first, the legacy
/// `default.world` last. Dot-directories (in-progress creates) are skipped.
pub fn list(root: &Path) -> Vec<WorldEntry> {
    let mut found = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let dir = e.path();
            if !dir.is_dir() || e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let terrain = dir.join(TERRAIN_FILE);
            let store = store_path_for(&terrain);
            let read = read_meta(&dir.join(META_FILE));
            let meta = read.as_ref().ok().cloned();
            let preview = Some(dir.join(PREVIEW_FILE)).filter(|p| p.is_file());
            let loadable = read
                .map(drop)
                .and_then(|()| check_meta(&meta))
                .and_then(|()| check_terrain(&terrain, &store, &meta));
            found.push(WorldEntry {
                dir,
                terrain,
                store,
                preview,
                meta,
                legacy: false,
                loadable,
            });
        }
    }
    found.sort_by(|a, b| {
        let key = |e: &WorldEntry| {
            e.meta
                .as_ref()
                .map(|m| m.created_utc.clone())
                .unwrap_or_default()
        };
        key(b).cmp(&key(a)).then_with(|| a.dir.cmp(&b.dir))
    });
    // Listed while its forge file waits for import OR once the server has
    // imported (and deleted) it into `default.sqlite`.
    let legacy = root.join("default.world");
    let legacy_store = store_path_for(&legacy);
    if legacy.is_file() || legacy_store.is_file() {
        found.push(WorldEntry {
            dir: root.to_path_buf(),
            // A store-only legacy world has no `world.json` to check: the
            // server checks the stored artifact when it opens the store.
            loadable: if legacy.is_file() {
                check_artifact_file(&legacy)
            } else {
                Ok(())
            },
            store: legacy_store,
            terrain: legacy,
            preview: None,
            meta: None,
            legacy: true,
        });
    }
    found
}

/// A directory name for `name` that is not taken under `root`.
pub fn slug_for(root: &Path, name: &str) -> String {
    let mut base = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            base.push(c);
        } else if !base.is_empty() && !base.ends_with('-') {
            base.push('-');
        }
    }
    let base = base.trim_end_matches('-');
    let base = if base.is_empty() { "world" } else { base };
    let mut slug = base.to_string();
    let mut n = 2;
    while root.join(&slug).exists() {
        slug = format!("{base}-{n}");
        n += 1;
    }
    slug
}

/// Where a new world is assembled before it is renamed into place:
/// `root/.<slug>.tmp/` (`list` skips dot-directories).
pub fn staging_dir(root: &Path, slug: &str) -> PathBuf {
    root.join(format!(".{slug}.tmp"))
}

/// Writes the world into [`staging_dir`], then renames it to `root/<slug>/`.
#[cfg(not(target_arch = "wasm32"))]
pub fn create(
    root: &Path,
    meta: &WorldMeta,
    artifact: &WorldArtifact,
    preview_png: &[u8],
) -> io::Result<PathBuf> {
    let tmp = staging_dir(root, &meta.slug);
    if root.join(&meta.slug).exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists", root.join(&meta.slug).display()),
        ));
    }
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    if let Err(e) = artifact.save(&tmp.join(TERRAIN_FILE)) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(io::Error::other(e.to_string()));
    }
    finish_staged(root, meta, preview_png)
}

/// Completes a world whose [`staging_dir`] already holds its
/// [`TERRAIN_FILE`] (written by `forge::play::forge_to_file`): writes the
/// preview and the meta next to it and renames the directory to
/// `root/<slug>/`. On any error the staging directory is removed.
#[cfg(not(target_arch = "wasm32"))]
pub fn finish_staged(root: &Path, meta: &WorldMeta, preview_png: &[u8]) -> io::Result<PathBuf> {
    let tmp = staging_dir(root, &meta.slug);
    let fin = root.join(&meta.slug);
    let written = (|| {
        if fin.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists", fin.display()),
            ));
        }
        if !tmp.join(TERRAIN_FILE).is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} has no terrain", tmp.display()),
            ));
        }
        std::fs::write(tmp.join(PREVIEW_FILE), preview_png)?;
        let json = serde_json::to_vec_pretty(meta).map_err(io::Error::other)?;
        std::fs::write(tmp.join(META_FILE), json)?;
        std::fs::rename(&tmp, &fin)
    })();
    if written.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    written.map(|()| fin)
}

/// Removes a world's directory. The legacy `default.world` is never deleted.
pub fn delete(root: &Path, entry: &WorldEntry) -> io::Result<()> {
    if entry.legacy {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the legacy default world is not deletable",
        ));
    }
    if entry.dir.parent() != Some(root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "world is not under the worlds directory",
        ));
    }
    std::fs::remove_dir_all(&entry.dir)
}

/// A world's extent for the menu: "100 × 50 km" (whole km from 10 km up, so
/// 16,384 m reads 16 km), "2.5 × 2.5 km", "800 × 400 m" — the unit of the
/// longer side; in km a side under 1 km keeps one decimal ("16 × 0.4 km").
pub fn size_text(width_m: f64, height_m: f64) -> String {
    let longer = width_m.max(height_m);
    let km = |side: f64, whole: bool| {
        if whole && side >= 1000.0 {
            format!("{}", (side / 1000.0).round() as u64)
        } else {
            format!("{:.1}", side / 1000.0)
        }
    };
    if longer >= 10_000.0 {
        format!("{} × {} km", km(width_m, true), km(height_m, true))
    } else if longer >= 1000.0 {
        format!("{} × {} km", km(width_m, false), km(height_m, false))
    } else {
        format!("{} × {} m", width_m as u64, height_m as u64)
    }
}

/// Every tunable by key, as stored in `WorldMeta` and in the world file.
pub fn knobs_map(knobs: &Knobs) -> BTreeMap<String, f64> {
    crate::forge::knobs::SPECS
        .iter()
        .filter_map(|s| knobs.get(s.key).map(|v| (s.key.to_string(), v)))
        .collect()
}
