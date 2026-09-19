//! Dev-only disk cache for `worldgen::generate_region` output, so the server
//! and the client's offline `LocalServer` don't regenerate identical pristine
//! terrain on every restart or test run.
//!
//! Regions land in `target/worldgen-cache/<key>/`, one bincode file per
//! region. The key is a crc32 over the world seed, the block manifest, and
//! every file under `crates/worldgen/src`, so editing worldgen (or retuning
//! materials) invalidates the whole cache automatically. Changes that alter
//! generated output *without* touching those inputs (e.g. column encoding in
//! `game`) either fail decode — handled as a miss — or need a manual
//! `just worldgen-cache-clear`.
//!
//! Cached bytes are the exact bincode of the generated columns, so a cache
//! hit is bit-identical to regeneration — determinism is unaffected.

use std::fs;
use std::path::{Path, PathBuf};

use game::{Column, ColumnCoords, RegionCoords};
use log::{info, warn};

/// Default: on in debug builds, off in release so profiling measures real
/// generation. `WORLDGEN_CACHE=1|0` overrides in either direction.
const DEFAULT_ENABLED: bool = cfg!(debug_assertions);

#[derive(Clone)]
pub struct WorldgenCache {
    /// `None` = caching disabled; every load misses and every store is a no-op.
    dir: Option<PathBuf>,
}

impl WorldgenCache {
    /// Decide enablement from `WORLDGEN_CACHE`/build profile and derive the
    /// cache directory. `block_manifest` is part of the key because the
    /// material ids resolved from it are baked into generated columns.
    pub fn from_env(seed: u64, block_manifest: &str) -> Self {
        let enabled = !cfg!(target_arch = "wasm32")
            && match std::env::var("WORLDGEN_CACHE").ok().as_deref() {
                Some("0") | Some("off") | Some("false") => false,
                Some(_) => true,
                None => DEFAULT_ENABLED,
            };
        if !enabled {
            return Self { dir: None };
        }
        let Some(key) = cache_key(seed, block_manifest) else {
            warn!("worldgen cache: cannot hash crates/worldgen sources, caching disabled");
            return Self { dir: None };
        };
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/worldgen-cache")
            .join(format!("{key:08x}"));
        if let Err(e) = fs::create_dir_all(&dir) {
            warn!("worldgen cache: cannot create {}: {e}, caching disabled", dir.display());
            return Self { dir: None };
        }
        info!("worldgen cache: {}", dir.display());
        Self::at_dir(dir)
    }

    /// Cache rooted at an explicit, existing directory (tests).
    pub fn at_dir(dir: PathBuf) -> Self {
        Self { dir: Some(dir) }
    }

    pub fn load(&self, rc: RegionCoords) -> Option<Vec<(ColumnCoords, Column)>> {
        let path = self.dir.as_ref()?.join(region_file(rc));
        let bytes = fs::read(&path).ok()?;
        match bincode::deserialize(&bytes) {
            Ok(columns) => {
                info!("worldgen cache: hit {}", region_file(rc));
                Some(columns)
            }
            Err(e) => {
                // Format drift in `game`'s column types lands here: treat it
                // as a miss and regenerate rather than fail the region.
                warn!("worldgen cache: discarding unreadable {}: {e}", path.display());
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    pub fn store(&self, rc: RegionCoords, columns: &[(ColumnCoords, Column)]) {
        let Some(dir) = &self.dir else { return };
        let bytes = match bincode::serialize(columns) {
            Ok(b) => b,
            Err(e) => {
                warn!("worldgen cache: cannot serialize region {rc:?}: {e}");
                return;
            }
        };
        // Write-then-rename so a server killed mid-write never leaves a
        // truncated region file for the next run to trip over.
        let path = dir.join(region_file(rc));
        // Unique per writer: LOD workers and the manager share one cache
        // directory and may generate the same region at the same time.
        let thread = format!("{:?}", std::thread::current().id());
        let thread: String = thread.chars().filter(char::is_ascii_digit).collect();
        let tmp = dir.join(format!("{}.tmp.{}.{thread}", region_file(rc), std::process::id()));
        if let Err(e) = fs::write(&tmp, &bytes).and_then(|()| fs::rename(&tmp, &path)) {
            warn!("worldgen cache: cannot write {}: {e}", path.display());
            let _ = fs::remove_file(&tmp);
        } else {
            info!("worldgen cache: stored {} ({} KiB)", region_file(rc), bytes.len() / 1024);
        }
    }

    /// Load-or-generate-and-store. A hit is bit-identical to regeneration
    /// (cached bytes are the exact bincode of the generated columns).
    pub fn generate_cached(
        &self,
        rc: RegionCoords,
        seed: u64,
        materials: crate::material::Materials,
    ) -> Vec<(ColumnCoords, Column)> {
        if let Some(columns) = self.load(rc) {
            return columns;
        }
        let columns = crate::generate_region(rc, seed, materials);
        self.store(rc, &columns);
        columns
    }
}

fn region_file(rc: RegionCoords) -> String {
    format!("r{}_{}.bin", rc.x, rc.z)
}

/// crc32 over seed + block manifest + all files under `crates/worldgen/src`
/// (sorted by path). `None` if the source tree is unreadable (e.g. a binary
/// deployed away from the repo) — the cache just disables itself then.
fn cache_key(seed: u64, block_manifest: &str) -> Option<u32> {
    let mut h = crc32fast::Hasher::new();
    h.update(&seed.to_le_bytes());
    h.update(block_manifest.as_bytes());
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_files(&src, &mut files).ok()?;
    files.sort();
    for f in files {
        h.update(f.to_string_lossy().as_bytes());
        h.update(&fs::read(&f).ok()?);
    }
    Some(h.finalize())
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::Block;

    fn temp_cache(tag: &str) -> (PathBuf, WorldgenCache) {
        let dir = std::env::temp_dir().join(format!("worldgen-cache-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        (dir.clone(), WorldgenCache::at_dir(dir))
    }

    #[test]
    fn store_then_load_roundtrips_bit_exact() {
        let (dir, cache) = temp_cache("roundtrip");
        let rc = RegionCoords::new(-3, 7);
        assert!(cache.load(rc).is_none());

        let column = Column::new(2, Block::default(), Block::default(), vec![]);
        let columns = vec![(ColumnCoords::new(1, 2), column)];
        cache.store(rc, &columns);

        let loaded = cache.load(rc).expect("stored region should load");
        // Column has no PartialEq by design; compare via bincode bytes.
        assert_eq!(
            bincode::serialize(&loaded).unwrap(),
            bincode::serialize(&columns).unwrap()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_is_a_miss_and_gets_removed() {
        let (dir, cache) = temp_cache("corrupt");
        let rc = RegionCoords::new(0, 0);
        let path = dir.join(region_file(rc));
        fs::write(&path, b"not bincode").unwrap();

        assert!(cache.load(rc).is_none());
        assert!(!path.exists(), "unreadable cache file should be deleted");
        let _ = fs::remove_dir_all(&dir);
    }
}
