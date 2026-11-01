//! Which world the server serves: a forged world file by default (forged on
//! first start if missing), or the procedural noise world on request.

use std::path::{Path, PathBuf};
use std::time::Instant;

use worldgen::forge::{forge, ForgeParams};

/// The default world, relative to the repo root.
pub const DEFAULT_WORLD_PATH: &str = "worlds/default.world";

/// `VOXEL_WORLD_FILE` value that selects the procedural noise world.
pub const PROCEDURAL_KEYWORD: &str = worldgen::forged::PROCEDURAL_KEYWORD;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldSource {
    Procedural,
    Forged(PathBuf),
}

/// `VOXEL_WORLD_FILE` -> the world to serve: unset or empty is the default
/// forged world, `procedural` the noise world, anything else a world file.
pub fn resolve(env_value: Option<&str>) -> WorldSource {
    match env_value {
        None | Some("") => WorldSource::Forged(PathBuf::from(DEFAULT_WORLD_PATH)),
        Some(PROCEDURAL_KEYWORD) => WorldSource::Procedural,
        Some(path) => WorldSource::Forged(PathBuf::from(path)),
    }
}

/// Relative world paths resolve from the repo root, like the block manifest
/// (and like `worldgen::forged::active` resolves `VOXEL_WORLD_FILE`).
pub fn from_repo_root(path: &Path) -> PathBuf {
    worldgen::forged::from_repo_root(path)
}

/// Forge the world file if it does not exist. An existing file is never read
/// or replaced here (a corrupt one is the caller's to report). The save is
/// atomic (temp file + rename, `WorldArtifact::save`), so a forge interrupted
/// on first start leaves no truncated world to trip the next one.
pub fn ensure_forged(path: &Path, params: &ForgeParams) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    log::info!("[server] {} missing: forging a default world (this takes a while)", path.display());
    let started = Instant::now();
    let forged = forge(params);
    forged
        .artifact(params)
        .save(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    game::perf_event!(
        "world_forged",
        "ms={} path={}",
        game::perf::ms(started.elapsed()),
        path.display()
    );
    Ok(())
}

/// The world's history, next to the world file: loaded if it exists, otherwise
/// bootstrapped from the world map and saved. A file that will not load is
/// never replaced (it holds the world's people) — the error says to delete it.
pub fn ensure_history(world_path: &Path, map: &game::WorldMap) -> Result<game::history::History, String> {
    use game::history::bootstrap::{self, BootstrapParams};
    use game::history::{persist, SocietyGrid};
    let path = persist::history_path_for(world_path);
    let grid = SocietyGrid::from_world_map(map);
    if path.exists() {
        return persist::load(&path, grid).map_err(|e| {
            format!("{}: {e} — delete {} to re-create this world's history", path.display(), path.display())
        });
    }
    let started = Instant::now();
    let params = BootstrapParams { seed: u64::from(grid.world_id()), ..BootstrapParams::default() };
    let history = bootstrap::bootstrap(map, &params)
        .map_err(|e| format!("{}: cannot bootstrap the world's history: {e}", path.display()))?;
    persist::save(&history, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    let stats = history.stats();
    game::perf_event!(
        "history_bootstrapped",
        "ms={} civs={} sites={} alive={}",
        game::perf::ms(started.elapsed()),
        stats.civs_alive,
        stats.sites_alive,
        stats.alive
    );
    Ok(history)
}

/// Save the history atomically; failures are logged, never fatal.
pub fn save_history(world_path: &Path, history: &game::history::History) {
    let path = game::history::persist::history_path_for(world_path);
    let started = Instant::now();
    match game::history::persist::save(history, &path) {
        Ok(()) => {
            let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
            game::perf_event!("history_saved", "ms={} bytes={bytes}", game::perf::ms(started.elapsed()));
        }
        Err(e) => log::error!("[server] cannot save the history to {}: {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmo-world-file-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn resolve_defaults_to_the_forged_world() {
        let default = WorldSource::Forged(PathBuf::from(DEFAULT_WORLD_PATH));
        assert_eq!(resolve(None), default);
        assert_eq!(resolve(Some("")), default);
    }

    #[test]
    fn resolve_procedural_keyword() {
        assert_eq!(resolve(Some("procedural")), WorldSource::Procedural);
    }

    #[test]
    fn resolve_explicit_path() {
        assert_eq!(
            resolve(Some("target/world-forge/best.world")),
            WorldSource::Forged(PathBuf::from("target/world-forge/best.world"))
        );
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn ensure_forged_writes_once() {
        let dir = scratch("once");
        let path = dir.join("nested/w.world");
        let params = ForgeParams { size_m: 2048.0, cell_m: 64.0, seed: 4, ..ForgeParams::default() };
        ensure_forged(&path, &params).unwrap();
        worldgen::forged::ForgedWorld::load(&path).expect("the forged file loads");
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        ensure_forged(&path, &params).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn ensure_forged_leaves_no_temp_file() {
        let dir = scratch("atomic");
        let path = dir.join("w.world");
        let params = ForgeParams { size_m: 2048.0, cell_m: 64.0, seed: 4, ..ForgeParams::default() };
        ensure_forged(&path, &params).unwrap();
        assert!(path.exists());
        assert!(!worldgen::forged::save_temp_path(&path).exists(), "the temp file is renamed away");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("w.world")], "only the world file remains");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_existing_world_is_not_overwritten() {
        let dir = scratch("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.world");
        std::fs::write(&path, b"not a world").unwrap();
        // Default params: a forge here would take seconds and replace the
        // file, so this also proves nothing was forged.
        ensure_forged(&path, &ForgeParams::default()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"not a world");
        assert!(worldgen::forged::ForgedWorld::load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn land_map(width: u32, height: u32, sea: impl Fn(i32, i32) -> bool) -> game::WorldMap {
        let mut tiles = Vec::new();
        for z in 0..height as i32 {
            for x in 0..width as i32 {
                let is_sea = sea(x, z);
                tiles.push(game::TileSummary {
                    ground_min_m: 0.0,
                    ground_mean_m: 10.0,
                    ground_max_m: 50.0,
                    relief_m: 50.0,
                    slope: [0.0, 0.0],
                    sea_fraction: if is_sea { 1.0 } else { 0.0 },
                    lake_fraction: 0.0,
                    lake_level_m: None,
                    rivers: 1,
                    river_max_discharge_m2: 0.0,
                    river_max_width_m: 0.0,
                    surface_rock: None,
                    landing: Some([256, 256]),
                    biome: if is_sea { game::Biome::Ocean } else { game::Biome::Grassland },
                    region: 0,
                    temperature_c: 15.0,
                    precip_mm: 800.0,
                    savagery: 0,
                    alignment: 0,
                    volcano: false,
                });
            }
        }
        game::WorldMap {
            origin: game::RegionCoords::new(0, 0),
            width,
            height,
            tiles,
            rock_names: vec![],
            region_types: vec![],
        }
    }

    #[test]
    fn ensure_history_bootstraps_then_loads() {
        let dir = scratch("history");
        std::fs::create_dir_all(&dir).unwrap();
        let world = dir.join("w.world");
        let map = land_map(48, 48, |x, z| x < 2 || z < 2 || x >= 46 || z >= 46);
        let first = ensure_history(&world, &map).unwrap();
        let file = game::history::persist::history_path_for(&world);
        assert!(file.exists(), "the first call writes the history next to the world");
        let second = ensure_history(&world, &map).unwrap();
        assert_eq!(bincode::serialize(&first).unwrap(), bincode::serialize(&second).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_history_refuses_other_world() {
        let dir = scratch("history-other");
        std::fs::create_dir_all(&dir).unwrap();
        let world = dir.join("w.world");
        let map = land_map(48, 48, |x, z| x < 2 || z < 2 || x >= 46 || z >= 46);
        ensure_history(&world, &map).unwrap();
        let file = game::history::persist::history_path_for(&world);
        let before = std::fs::read(&file).unwrap();
        let other = land_map(48, 48, |x, z| x < 3 || z < 2 || x >= 46 || z >= 46);
        let err = ensure_history(&world, &other).unwrap_err();
        assert!(err.contains("delete") && err.contains("w.history"), "{err}");
        assert_eq!(std::fs::read(&file).unwrap(), before, "the file is left alone");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
