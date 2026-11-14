//! Prefabs in the running game, for development: the built-in prefab set is always installed, and a
//! dev structure source stamps named prefabs beside one region's landing column when the environment
//! asks (`VOXEL_DEV_STRUCTURES`). Everything here runs once, at startup, before any terrain is generated.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use game::RegionCoords;
use worldgen::structures::{dev_placements, install, parse_dev_spec, StructureList};

/// Extra prefab directories, `:`-separated, loaded together with `assets/prefabs`.
pub const PREFAB_DIRS_ENV: &str = "VOXEL_PREFAB_DIRS";
/// `<rx>,<rz>=<spec>`: the prefabs of `<spec>` (see `parse_dev_spec`) beside region (rx, rz)'s landing.
pub const DEV_STRUCTURES_ENV: &str = "VOXEL_DEV_STRUCTURES";

/// Installs the prefab set (always) and, when `VOXEL_DEV_STRUCTURES` is set and non-empty, a structure
/// source with the dev placements. Returns the number of placements, `None` when the feature is off.
pub fn install_from_env(
    registry: &game::BlockRegistry,
    landing_of: &dyn Fn(RegionCoords) -> Option<[u32; 2]>,
    seed: u64,
    mats: worldgen::material::Materials,
) -> Result<Option<usize>, String> {
    let block_named = |n: &str| registry.id_of(n);
    let builtin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/prefabs");
    let extra = std::env::var(PREFAB_DIRS_ENV).unwrap_or_default();
    let set = if extra.trim().is_empty() {
        game::prefab::load_builtin_prefabs(Some(&builtin).filter(|d| d.is_dir()).map(|d| d.as_path()), &block_named)?
    } else {
        let mut dirs: Vec<PathBuf> = vec![builtin];
        dirs.extend(extra.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
        let refs: Vec<&Path> = dirs.iter().map(|d| d.as_path()).collect();
        game::prefab::load_prefab_dirs(&refs, &block_named)?
    };
    game::prefab::install_prefabs(set);
    let set = game::prefab::prefabs();

    let var = std::env::var(DEV_STRUCTURES_ENV).unwrap_or_default();
    if var.trim().is_empty() {
        return Ok(None);
    }
    let bad = |why: &str| format!("{DEV_STRUCTURES_ENV}=\"{var}\": {why}");
    let (region, spec) = var.split_once('=').ok_or_else(|| bad("expected <rx>,<rz>=<spec>"))?;
    let (rx, rz) = region.split_once(',').ok_or_else(|| bad("the region must be <rx>,<rz>"))?;
    let num = |s: &str| s.trim().parse::<i32>().map_err(|_| bad(&format!("\"{}\" is not a number", s.trim())));
    let rc = RegionCoords { x: num(rx)?, z: num(rz)? };
    let entries = parse_dev_spec(spec)?;

    let edge = game::REGION_BLOCKS as i32;
    let [lx, lz] = landing_of(rc).map_or([edge / 2, edge / 2], |l| [l[0] as i32, l[1] as i32]);
    let origin = [rc.x * edge + lx, rc.z * edge + lz];
    let ground = |x: i32, z: i32| worldgen::ground_y(x, z, seed, mats);
    let placements = dev_placements(&entries, origin, set, &ground)?;
    let n = placements.len();
    install(Arc::new(StructureList::new(placements, set.content_hash())));
    Ok(Some(n))
}
