//! Which world the server serves: a forged world file by default (forged on
//! first start if missing), or the procedural noise world on request.

use std::path::{Path, PathBuf};
use std::time::Instant;

use worldgen::forge::{forge, ForgeParams};

/// The default world, relative to the repo root.
pub const DEFAULT_WORLD_PATH: &str = "worlds/default.world";

/// `VOXEL_WORLD_FILE` value that selects the procedural noise world.
pub const PROCEDURAL_KEYWORD: &str = "procedural";

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

/// Relative world paths resolve from the repo root, like the block manifest.
pub fn from_repo_root(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
    }
}

/// Forge the world file if it does not exist. An existing file is never read
/// or replaced here (a corrupt one is the caller's to report).
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
    fn corrupt_existing_world_is_not_overwritten() {
        let dir = scratch("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.world");
        std::fs::write(&path, b"not a world").unwrap();
        // Default params: a forge here would be minutes, so this also proves
        // nothing was forged.
        ensure_forged(&path, &ForgeParams::default()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"not a world");
        assert!(worldgen::forged::ForgedWorld::load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
