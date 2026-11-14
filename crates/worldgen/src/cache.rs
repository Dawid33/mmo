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

    /// A cache that never reads or writes a file (a dev structure source is installed, or tests).
    pub fn disabled() -> Self {
        Self { dir: None }
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
            if fits_this_build(&columns) {
                return columns;
            }
            // Written by a build with another REGION_CHUNKS (older caches
            // were not keyed by it): regenerate instead of serving a corner.
            warn!(
                "worldgen cache: {} holds {} columns, this build needs {}; regenerating",
                region_file(rc),
                columns.len(),
                game::REGION_CHUNKS * game::REGION_CHUNKS
            );
        }
        let columns = crate::generate_region(rc, seed, materials);
        self.store(rc, &columns);
        columns
    }
}

/// Whether cached columns are a whole region of THIS build's size.
fn fits_this_build(columns: &[(ColumnCoords, Column)]) -> bool {
    columns.len() == game::REGION_CHUNKS * game::REGION_CHUNKS
}

fn region_file(rc: RegionCoords) -> String {
    format!("r{}_{}.bin", rc.x, rc.z)
}

/// crc32 over seed + block manifest + all files under `crates/worldgen/src`
/// (sorted by path). `None` if the source tree is unreadable (e.g. a binary
/// deployed away from the repo) — the cache just disables itself then.
fn cache_key(seed: u64, block_manifest: &str) -> Option<u32> {
    cache_key_for(seed, block_manifest, game::REGION_CHUNKS, crate::islands::SPAWN_ISLAND_CELLS)
}

/// `cache_key` for an explicit region edge: a region file holds exactly
/// `region_chunks²` columns, so builds with different `REGION_CHUNKS` must
/// never share a cache directory.
fn cache_key_for(seed: u64, block_manifest: &str, region_chunks: usize, island_cells: u32) -> Option<u32> {
    cache_key_with(seed, block_manifest, region_chunks, island_cells, crate::structures::active().content_hash())
}

/// `cache_key_for` with the structure source's identity given: hashed only when non-zero, after every
/// other input, so the key is unchanged while no structures are installed.
fn cache_key_with(
    seed: u64,
    block_manifest: &str,
    region_chunks: usize,
    island_cells: u32,
    structures_hash: u32,
) -> Option<u32> {
    let mut h = crc32fast::Hasher::new();
    h.update(&seed.to_le_bytes());
    h.update(block_manifest.as_bytes());
    h.update(&(region_chunks as u64).to_le_bytes());
    // The sim chunk edge and region edge in blocks shape every cached chunk
    // (and its serialized grid): builds with other dials never share files.
    h.update(&(game::CHUNK_BLOCKS as u64).to_le_bytes());
    h.update(&(game::REGION_BLOCKS as u64).to_le_bytes());
    // The spawn island's size is a build dial (`VOXEL_ISLAND_CELLS`), not
    // source: it must key the cache itself.
    h.update(&island_cells.to_le_bytes());
    // A forged world replaces the seed as the source of terrain: regions of
    // different world files must never share a cache directory.
    if let Some(world) = crate::forged::active() {
        h.update(&world.content_crc().to_le_bytes());
    }
    if structures_hash != 0 {
        h.update(&structures_hash.to_le_bytes());
    }
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
    use crate::structures::StructureSource;
    use game::Block;

    fn temp_cache(tag: &str) -> (PathBuf, WorldgenCache) {
        let dir = std::env::temp_dir().join(format!("worldgen-cache-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        (dir.clone(), WorldgenCache::at_dir(dir))
    }

    #[test]
    fn disabled_cache_never_touches_a_file() {
        let cache = WorldgenCache::disabled();
        let rc = RegionCoords::new(9, 9);
        let columns = vec![(ColumnCoords::new(0, 0), Column::new(2, Block::default(), Block::default(), vec![]))];
        cache.store(rc, &columns);
        assert!(cache.load(rc).is_none());
        assert!(cache.dir.is_none());
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
    fn builds_with_different_region_sizes_never_share_a_cache() {
        // A 32-chunk region file loaded by a 128-chunk build is a 64 m corner
        // of terrain inside a 256 m region (cell-loading live bug, 2026-09-25).
        let a = cache_key_for(7, "manifest", 32, 16).unwrap();
        let b = cache_key_for(7, "manifest", 128, 16).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn the_cache_key_ignores_no_structures() {
        // The key as it was before structures: the same inputs hashed by hand.
        let mut h = crc32fast::Hasher::new();
        h.update(&7u64.to_le_bytes());
        h.update(b"manifest");
        h.update(&32u64.to_le_bytes());
        h.update(&(game::CHUNK_BLOCKS as u64).to_le_bytes());
        h.update(&(game::REGION_BLOCKS as u64).to_le_bytes());
        h.update(&16u32.to_le_bytes());
        if let Some(world) = crate::forged::active() {
            h.update(&world.content_crc().to_le_bytes());
        }
        let mut files = Vec::new();
        collect_files(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files).unwrap();
        files.sort();
        for f in files {
            h.update(f.to_string_lossy().as_bytes());
            h.update(&fs::read(&f).unwrap());
        }
        let old = h.finalize();
        let none = crate::structures::NoStructures.content_hash();
        assert_eq!(none, 0);
        assert_eq!(cache_key_with(7, "manifest", 32, 16, none).unwrap(), old);
        assert_ne!(cache_key_with(7, "manifest", 32, 16, 0xdead_beef).unwrap(), old);
    }

    #[test]
    fn builds_with_different_spawn_islands_never_share_a_cache() {
        let a = cache_key_for(7, "manifest", 32, 16).unwrap();
        let b = cache_key_for(7, "manifest", 32, 4).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_cached_region_must_have_this_builds_column_count() {
        let column = || Column::new(2, Block::default(), Block::default(), vec![]);
        let one = vec![(ColumnCoords::new(0, 0), column())];
        assert!(!fits_this_build(&one));
        let n = game::REGION_CHUNKS;
        let full: Vec<_> = (0..n * n).map(|i| (ColumnCoords::new(i % n, i / n), column())).collect();
        assert!(fits_this_build(&full));
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
