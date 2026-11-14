//! Prefabs in the running game, for development: the built-in prefab set is always installed, and a
//! dev structure source stamps named prefabs beside one region's landing column when the environment
//! asks (`VOXEL_DEV_STRUCTURES`). Everything here runs once, at startup, before any terrain is generated.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use game::RegionCoords;
use worldgen::structures::{dev_placements, install, parse_dev_spec, DevHomeSource, DevRegionSource, StructureList, StructureSource};

/// Extra prefab directories, `:`-separated, loaded together with `assets/prefabs`.
pub const PREFAB_DIRS_ENV: &str = "VOXEL_PREFAB_DIRS";
/// `<rx>,<rz>=<spec>`: the prefabs of `<spec>` (see `parse_dev_spec`) beside region (rx, rz)'s landing.
pub const DEV_STRUCTURES_ENV: &str = "VOXEL_DEV_STRUCTURES";

/// Where a region's landing column is (region-local block x, z), if the world map says.
pub type LandingOf = Arc<dyn Fn(RegionCoords) -> Option<[u32; 2]> + Send + Sync>;

/// What `install_from_env` did (or `from_env` would do) about `VOXEL_DEV_STRUCTURES`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevStructures {
    /// Not set: generation is untouched.
    Off,
    /// `<rx>,<rz>=<spec>`: this many placements beside that region's landing.
    Region { x: i32, z: i32, placements: usize },
    /// `all=<spec>`: the spec's prefabs beside every region's landing, laid out lazily.
    EveryRegion { spec: String },
    /// `home=<spec>`: the spec's prefabs beside the landing of the first region generated in full (the
    /// embark region in a debug launch), and nowhere else.
    Home { spec: String },
}

/// Whether `VOXEL_DEV_STRUCTURES` asks for anything (the caller then builds a `LandingOf`).
pub fn requested() -> bool {
    !std::env::var(DEV_STRUCTURES_ENV).unwrap_or_default().trim().is_empty()
}

/// What `install_from_env` installs, built by [`from_env`] without installing anything.
pub struct DevSetup {
    /// The prefab set: `assets/prefabs` plus `VOXEL_PREFAB_DIRS`.
    pub prefabs: game::prefab::PrefabSet,
    /// The dev structure source, when `VOXEL_DEV_STRUCTURES` asks for one.
    pub source: Option<Arc<dyn StructureSource>>,
    pub what: DevStructures,
}

/// Installs the prefab set (always) and, when `VOXEL_DEV_STRUCTURES` is set and non-empty, a structure
/// source with the dev placements: `<rx>,<rz>=<spec>` (one region), `all=<spec>` (every region) or `home=<spec>` (the first region generated). See
/// [`from_env`]; on an error nothing is installed.
pub fn install_from_env(
    registry: &game::BlockRegistry,
    landing_of: LandingOf,
    seed: u64,
    mats: worldgen::material::Materials,
) -> Result<DevStructures, String> {
    let setup = from_env(registry, landing_of, seed, mats)?;
    game::prefab::install_prefabs(setup.prefabs);
    if let Some(source) = setup.source {
        install(source);
    }
    Ok(setup.what)
}

/// Reads `VOXEL_PREFAB_DIRS` and `VOXEL_DEV_STRUCTURES` and builds what [`install_from_env`] installs,
/// with no global side effect (the source's ground reads the installed forged world, if any, when it is
/// asked). Every error names the offending text.
pub fn from_env(
    registry: &game::BlockRegistry,
    landing_of: LandingOf,
    seed: u64,
    mats: worldgen::material::Materials,
) -> Result<DevSetup, String> {
    let block_named = |n: &str| registry.id_of(n);
    let builtin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/prefabs");
    let extra = std::env::var(PREFAB_DIRS_ENV).unwrap_or_default();
    let set = if extra.trim().is_empty() {
        game::prefab::load_builtin_prefabs(Some(&builtin).filter(|d| d.is_dir()).map(|d| d.as_path()), &block_named)?
    } else {
        let mut dirs: Vec<PathBuf> = vec![builtin];
        dirs.extend(extra.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
        let refs: Vec<&Path> = dirs.iter().map(|d| d.as_path()).collect();
        game::prefab::load_prefab_dirs(&refs, &block_named).map_err(|e| format!("{PREFAB_DIRS_ENV}=\"{extra}\": {e}"))?
    };

    let var = std::env::var(DEV_STRUCTURES_ENV).unwrap_or_default();
    if var.trim().is_empty() {
        return Ok(DevSetup { prefabs: set, source: None, what: DevStructures::Off });
    }
    let bad = |why: &str| format!("{DEV_STRUCTURES_ENV}=\"{var}\": {why}");
    let (region, spec) = var.split_once('=').ok_or_else(|| bad("expected <rx>,<rz>=<spec>, all=<spec> or home=<spec>"))?;
    let entries = parse_dev_spec(spec).map_err(|e| bad(&e))?;
    let edge = game::REGION_BLOCKS as i32;
    // The column the row is laid out beside: the region's landing, else its centre.
    let origin_of = move |rc: RegionCoords| {
        let [lx, lz] = landing_of(rc).map_or([edge / 2, edge / 2], |l| [l[0] as i32, l[1] as i32]);
        [rc.x * edge + lx, rc.z * edge + lz]
    };
    let ground = move |x: i32, z: i32| worldgen::ground_y(x, z, seed, mats);
    if region.trim() == "all" {
        let source = DevRegionSource::new(entries, &set, Box::new(origin_of), Box::new(ground)).map_err(|e| bad(&e))?;
        let what = DevStructures::EveryRegion { spec: spec.to_string() };
        return Ok(DevSetup { prefabs: set, source: Some(Arc::new(source)), what });
    }
    if region.trim() == "home" {
        let source = DevHomeSource::new(entries, &set, Box::new(origin_of), Box::new(ground)).map_err(|e| bad(&e))?;
        let what = DevStructures::Home { spec: spec.to_string() };
        return Ok(DevSetup { prefabs: set, source: Some(Arc::new(source)), what });
    }
    let (rx, rz) = region.split_once(',').ok_or_else(|| bad("the region must be <rx>,<rz>, all or home"))?;
    let num = |s: &str| s.trim().parse::<i32>().map_err(|_| bad(&format!("\"{}\" is not a number", s.trim())));
    let rc = RegionCoords { x: num(rx)?, z: num(rz)? };
    let placements = dev_placements(&entries, origin_of(rc), &set, &ground).map_err(|e| bad(&e))?;
    let n = placements.len();
    let source = StructureList::new(placements, set.content_hash());
    let what = DevStructures::Region { x: rc.x, z: rc.z, placements: n };
    Ok(DevSetup { prefabs: set, source: Some(Arc::new(source)), what })
}
