//! On-disk worlds for the main menu: `worlds/<slug>/{terrain.world, world.json,
//! preview.png}`, plus the legacy single `worlds/default.world`.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::forge::knobs::Knobs;
use crate::forged::WorldArtifact;

pub const WORLDS_DIR: &str = "worlds";
pub const TERRAIN_FILE: &str = "terrain.world";
pub const META_FILE: &str = "world.json";
pub const PREVIEW_FILE: &str = "preview.png";
pub const LEGACY_WORLD: &str = "worlds/default.world";
pub const META_FORMAT: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WorldMeta {
    pub format_version: u32,
    pub name: String,
    pub slug: String,
    /// RFC 3339, UTC; ordering is lexical.
    pub created_utc: String,
    pub seed: u64,
    pub size_m: f64,
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
    pub preview: Option<PathBuf>,
    pub meta: Option<WorldMeta>,
    pub legacy: bool,
    pub loadable: Result<(), String>,
}

fn check_terrain(terrain: &Path) -> Result<(), String> {
    if !terrain.is_file() {
        return Err(format!("{TERRAIN_FILE} is missing"));
    }
    match crate::forged::read_artifact_version(terrain) {
        Ok(v) if v == crate::forged::ARTIFACT_VERSION => Ok(()),
        Ok(v) if v < crate::forged::ARTIFACT_VERSION => {
            Err(format!("needs re-forge: {}", crate::forged::ArtifactError::OldVersion { found: v }))
        }
        Ok(v) => Err(format!(
            "world file version {v} is newer than the {} this build reads",
            crate::forged::ARTIFACT_VERSION
        )),
        Err(e) => Err(e.to_string()),
    }
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
            let meta = std::fs::read(dir.join(META_FILE))
                .ok()
                .and_then(|b| serde_json::from_slice::<WorldMeta>(&b).ok());
            let preview = Some(dir.join(PREVIEW_FILE)).filter(|p| p.is_file());
            let loadable = check_meta(&meta).and_then(|()| check_terrain(&terrain));
            found.push(WorldEntry { dir, terrain, preview, meta, legacy: false, loadable });
        }
    }
    found.sort_by(|a, b| {
        let key = |e: &WorldEntry| e.meta.as_ref().map(|m| m.created_utc.clone()).unwrap_or_default();
        key(b).cmp(&key(a)).then_with(|| a.dir.cmp(&b.dir))
    });
    let legacy = root.join("default.world");
    if legacy.is_file() {
        found.push(WorldEntry {
            dir: root.to_path_buf(),
            loadable: check_terrain(&legacy),
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

/// Writes the world into `root/.<slug>.tmp/`, then renames it to `root/<slug>/`.
#[cfg(not(target_arch = "wasm32"))]
pub fn create(root: &Path, meta: &WorldMeta, artifact: &WorldArtifact, preview_png: &[u8]) -> io::Result<PathBuf> {
    let tmp = root.join(format!(".{}.tmp", meta.slug));
    let fin = root.join(&meta.slug);
    if fin.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists", fin.display())));
    }
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let written = (|| {
        artifact
            .save(&tmp.join(TERRAIN_FILE))
            .map_err(|e| io::Error::other(e.to_string()))?;
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
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the legacy default world is not deletable"));
    }
    if entry.dir.parent() != Some(root) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "world is not under the worlds directory"));
    }
    std::fs::remove_dir_all(&entry.dir)
}

/// Every tunable by key, as stored in `WorldMeta`.
pub fn knobs_map(knobs: &Knobs) -> BTreeMap<String, f64> {
    crate::forge::knobs::SPECS
        .iter()
        .filter_map(|s| knobs.get(s.key).map(|v| (s.key.to_string(), v)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forged::{QRaster, ARTIFACT_VERSION, GEO_STEP_M, HEIGHT_STEP_M, PRECIP_STEP_MM, TEMPERATURE_STEP_C};

    fn scratch(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/world-store-tests").join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn artifact() -> WorldArtifact {
        WorldArtifact {
            version: ARTIFACT_VERSION,
            seed: 1,
            size_m: 128.0,
            cell_m: 64.0,
            n: 2,
            height: QRaster::fit(&[0.0; 4], HEIGHT_STEP_M),
            uplifted: QRaster::fit(&[0.0; 4], GEO_STEP_M),
            bedding: QRaster::fit(&[0.0; 4], GEO_STEP_M),
            temperature_c: QRaster::fit(&[14.0; 4], TEMPERATURE_STEP_C),
            precip_mm: QRaster::fit(&[900.0; 4], PRECIP_STEP_MM),
            column: vec![(0.0, 0)],
            rock_names: vec!["granite".into()],
            rivers: vec![],
            lakes: vec![],
            spawn: [64.0, 64.0],
            edges: [crate::forged::EdgeKind::Sea; 4],
        }
    }

    fn meta(name: &str, slug: &str, created: &str) -> WorldMeta {
        WorldMeta {
            format_version: META_FORMAT,
            name: name.into(),
            slug: slug.into(),
            created_utc: created.into(),
            seed: 1,
            size_m: 128.0,
            cell_m: 64.0,
            preset: "default".into(),
            knobs: knobs_map(&Knobs::default()),
            forge_ms: 5,
            region_blocks: game::REGION_BLOCKS as u32,
            chunk_blocks: game::CHUNK_BLOCKS as u32,
            artifact_version: ARTIFACT_VERSION,
        }
    }

    #[test]
    fn slug_lowercases_and_dedups() {
        let root = scratch("slug");
        assert_eq!(slug_for(&root, "My Island!"), "my-island");
        create(&root, &meta("My Island!", "my-island", "2026-01-01T00:00:00Z"), &artifact(), b"png").unwrap();
        assert_eq!(slug_for(&root, "my island"), "my-island-2");
        assert_eq!(slug_for(&root, ""), "world");
    }

    #[test]
    fn create_then_list_round_trips() {
        let root = scratch("roundtrip");
        let m = meta("A", "a", "2026-01-01T00:00:00Z");
        create(&root, &m, &artifact(), b"png").unwrap();
        let l = list(&root);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].meta, Some(m));
        assert_eq!(l[0].loadable, Ok(()));
        assert!(l[0].terrain.exists());
        assert!(l[0].preview.is_some());
    }

    #[test]
    fn list_skips_temp_dirs() {
        let root = scratch("temp");
        std::fs::create_dir_all(root.join(".x.tmp")).unwrap();
        assert!(list(&root).is_empty());
    }

    #[test]
    fn unreadable_meta_is_listed_not_loadable() {
        let root = scratch("badmeta");
        let d = root.join("b");
        std::fs::create_dir_all(&d).unwrap();
        artifact().save(&d.join(TERRAIN_FILE)).unwrap();
        std::fs::write(d.join(META_FILE), b"garbage").unwrap();
        let l = list(&root);
        assert_eq!(l.len(), 1);
        assert!(l[0].meta.is_none());
        assert!(l[0].loadable.as_ref().unwrap_err().contains("world.json"));
    }

    #[test]
    fn other_region_size_is_not_loadable() {
        let root = scratch("region");
        let mut m = meta("R", "r", "2026-01-01T00:00:00Z");
        m.region_blocks = game::REGION_BLOCKS as u32 * 2;
        create(&root, &m, &artifact(), b"png").unwrap();
        let e = list(&root).remove(0).loadable.unwrap_err();
        let (a, b) = (game::REGION_BLOCKS * 2, game::REGION_BLOCKS);
        assert!(e.contains(&format!("made with {a} m regions, this build uses {b} m")), "{e}");
    }

    #[test]
    fn old_artifact_version_is_not_loadable() {
        let root = scratch("oldver");
        let mut a = artifact();
        a.version = ARTIFACT_VERSION - 1;
        let mut m = meta("O", "o", "2026-01-01T00:00:00Z");
        m.artifact_version = a.version;
        create(&root, &m, &a, b"png").unwrap();
        let e = list(&root).remove(0).loadable.unwrap_err();
        assert!(e.starts_with("needs re-forge"), "{e}");
        assert!(e.contains(&format!("world file version {} is older than {ARTIFACT_VERSION}", a.version)), "{e}");
    }

    #[test]
    fn legacy_default_world_listed_last_and_undeletable() {
        let root = scratch("legacy");
        create(&root, &meta("A", "a", "2026-01-01T00:00:00Z"), &artifact(), b"png").unwrap();
        artifact().save(&root.join("default.world")).unwrap();
        let l = list(&root);
        assert_eq!(l.len(), 2);
        let last = l.last().unwrap();
        assert!(last.legacy);
        assert_eq!(last.loadable, Ok(()));
        assert_eq!(delete(&root, last).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn delete_removes_world_dir() {
        let root = scratch("delete");
        create(&root, &meta("A", "a", "2026-01-01T00:00:00Z"), &artifact(), b"png").unwrap();
        let e = list(&root).remove(0);
        delete(&root, &e).unwrap();
        assert!(list(&root).is_empty());
    }

    #[test]
    fn list_orders_newest_first() {
        let root = scratch("order");
        create(&root, &meta("Old", "old", "2026-01-01T00:00:00Z"), &artifact(), b"p").unwrap();
        create(&root, &meta("New", "new", "2026-06-01T00:00:00Z"), &artifact(), b"p").unwrap();
        let names: Vec<_> = list(&root).into_iter().map(|e| e.meta.unwrap().slug).collect();
        assert_eq!(names, ["new", "old"]);
    }
}
