//! Which world the server serves, and where it keeps it: a forged world in a
//! world store (`VOXEL_WORLD_DB`, by default the `.sqlite` beside the world
//! file), imported from the forge file on first start or forged into the
//! store when there is none — stored as tiles with its world map and the
//! forge file as an archive row, and served from those tiles; or the
//! procedural noise world on request, which keeps nothing.

use std::path::{Path, PathBuf};
use std::time::Instant;

use worldgen::forge::{forge, ForgeParams};
use worldgen::forged::ForgedWorld;
use world_store::{StoreHandle, StoreUrl};

/// The default world, relative to the repo root.
pub const DEFAULT_WORLD_PATH: &str = "worlds/default.world";

/// `VOXEL_WORLD_FILE` value that selects the procedural noise world.
pub const PROCEDURAL_KEYWORD: &str = worldgen::forged::PROCEDURAL_KEYWORD;

/// The world store: a `.sqlite` path or a `postgres://` URL. Unset or empty =
/// [`store_url_for`] the world file.
pub const WORLD_DB_ENV: &str = "VOXEL_WORLD_DB";

/// Why a world with save files from before the world store is not started.
pub const LEGACY_SAVES_REFUSAL: &str =
    "this world has file saves from before the world store; delete them with: just clean-old-worlds";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldSource {
    /// The noise world: no store, nothing persisted.
    Procedural,
    /// A forged world in the store at `url`. `import`: the world file (forge
    /// file) the store takes its world from when it has none yet — it may not
    /// exist — and next to which old file saves would sit.
    Store { url: StoreUrl, import: Option<PathBuf> },
}

/// `VOXEL_WORLD_FILE` + `VOXEL_WORLD_DB` -> the world to serve. A world file
/// unset or empty is the default world (unless a store is named, which then
/// imports nothing), `procedural` the noise world, anything else a world file
/// (relative paths from the repo root). The store is `VOXEL_WORLD_DB`, else
/// the world file's `.sqlite`.
pub fn resolve(world_file: Option<&str>, world_db: Option<&str>) -> Result<WorldSource, String> {
    let world_db = world_db.map(str::trim).filter(|s| !s.is_empty());
    let world_file = match world_file.map(str::trim) {
        Some(PROCEDURAL_KEYWORD) => {
            if world_db.is_some() {
                log::warn!("[server] {WORLD_DB_ENV} is ignored: the procedural world keeps nothing");
            }
            return Ok(WorldSource::Procedural);
        }
        None | Some("") if world_db.is_some() => None,
        None | Some("") => Some(from_repo_root(Path::new(DEFAULT_WORLD_PATH))),
        Some(path) => Some(from_repo_root(Path::new(path))),
    };
    let url = match (world_db, &world_file) {
        (Some(db), _) => match StoreUrl::parse(db).map_err(|e| format!("{WORLD_DB_ENV}={db:?}: {e}"))? {
            StoreUrl::Sqlite(path) => StoreUrl::Sqlite(from_repo_root(&path)),
            other => other,
        },
        (None, Some(file)) => store_url_for(file),
        (None, None) => unreachable!("no store named means the default world file"),
    };
    Ok(WorldSource::Store { url, import: world_file })
}

/// The store of a world file when `VOXEL_WORLD_DB` is unset: the `.sqlite`
/// beside it (`worlds/default.world` -> `worlds/default.sqlite`).
pub fn store_url_for(world_file: &Path) -> StoreUrl {
    StoreUrl::Sqlite(worldgen::world_store::store_path_for(world_file))
}

/// Relative world paths resolve from the repo root, like the block manifest
/// (and like `worldgen::forged::active` resolves `VOXEL_WORLD_FILE`).
pub fn from_repo_root(path: &Path) -> PathBuf {
    worldgen::forged::from_repo_root(path)
}

/// The file saves of the world file `world_file` from before the world store
/// (`<world>.regions/`, `<world>.players/`, `<world>.history`) that exist.
pub fn legacy_saves(world_file: &Path) -> Vec<PathBuf> {
    ["regions", "players", "history"]
        .into_iter()
        .map(|ext| world_file.with_extension(ext))
        .filter(|p| p.exists())
        .collect()
}

/// Refuse a store that holds no region beside old file saves of the same
/// world: starting would silently play its pristine terrain while the saves
/// sit there ([`LEGACY_SAVES_REFUSAL`]).
pub fn check_legacy_saves(store: &StoreHandle, world_file: Option<&Path>) -> Result<(), String> {
    let Some(world_file) = world_file else { return Ok(()) };
    let legacy = legacy_saves(world_file);
    if legacy.is_empty() {
        return Ok(());
    }
    let stored = store.stored_regions().map_err(|e| e.to_string())?;
    if !stored.is_empty() {
        return Ok(());
    }
    let listed: Vec<String> = legacy.iter().map(|p| p.display().to_string()).collect();
    Err(format!("{LEGACY_SAVES_REFUSAL} ({})", listed.join(", ")))
}

/// The world in the store, as tiles, with its stored world map. A store
/// with none takes it from `import` (when that file exists; the file is then
/// deleted) else from a forge with `params` — either way written as tiles
/// ([`world_store::write_tiled`], which keeps the forge file as the archive
/// row). A store holding only the archive (imported before tiling, ruling
/// R-B4) is tiled from it here. A world file that will not load, or whose
/// cell size the tiled store does not support, is never imported (the error
/// says so, and the file is kept); one is never written over an existing
/// world.
///
/// A stored tile format or world map this build does not serve
/// (`FormatKind::Tiles` / `FormatKind::WorldMap`) is RE-TILED here from the
/// archive row when it has one — the same `write_tiled` as `just worldctl
/// retile`, in one transaction — rather than refused: re-tiling is lossless
/// (the archive is the forge file; saved regions, players and history are
/// other rows and untouched), and a refusal would only make the player run
/// that very command before a single-player world starts again. Logged as
/// `[server] … re-tiling` and `[perf] event=world_retiled`. Without an
/// archive it is refused (re-forge).
pub fn ensure_world(
    store: &StoreHandle,
    import: Option<&Path>,
    params: &ForgeParams,
) -> Result<(ForgedWorld, game::WorldMap), String> {
    let started = Instant::now();
    match world_store::load_forged(store) {
        Ok(Some(loaded)) => {
            forget_imported_file(store, import);
            return Ok(loaded);
        }
        Ok(None) => {}
        Err(e @ world_store::StoreError::OtherFormat { kind, .. }) if kind.retileable() => {
            log::warn!("[server] {e}");
            log::info!("[server] re-tiling the world from its archived forge file (saves are kept)");
            tile_from_archive(store)?;
            game::perf_event!("world_retiled", "ms={} kind={kind:?}", game::perf::ms(started.elapsed()));
            forget_imported_file(store, import);
            return load_written(store);
        }
        Err(e) => return Err(e.to_string()),
    }
    let info = store.world_info().map_err(|e| e.to_string())?;
    if info.has_artifact {
        // Imported before tiling (Plan A): the archive is the forge file.
        log::info!("[server] the world store holds its forge file but no tiles: tiling it (once)");
        tile_from_archive(store)?;
        game::perf_event!("world_tiled", "ms={} from=archive", game::perf::ms(started.elapsed()));
        forget_imported_file(store, import);
    } else if let Some(path) = import.filter(|p| p.exists()) {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let size = bytes.len();
        // Every refusal comes before anything is written, and the file is
        // deleted only after the commit: an error here keeps it.
        world_store::write_tiled(store, bytes).map_err(|e| match e {
            // The only `Corrupt` before anything is written: the file does not load.
            world_store::StoreError::Corrupt(why) => {
                format!("{}: {why} — delete it to forge a new world", path.display())
            }
            world_store::StoreError::Unsupported(why) => format!(
                "{}: {why} — the file is kept; forge the world at a supported cell size (8 m or more)",
                path.display()
            ),
            e @ world_store::StoreError::Backend(_) => {
                format!("importing {}: {e} — retry; the store and the file are untouched", path.display())
            }
            e => e.to_string(),
        })?;
        // In the store now: the file is not read again.
        if let Err(e) = std::fs::remove_file(path) {
            log::error!("[server] cannot delete the imported world file {}: {e}", path.display());
        }
        log::info!("[server] imported {} into the world store", path.display());
        game::perf_event!(
            "world_imported",
            "ms={} bytes={size} path={}",
            game::perf::ms(started.elapsed()),
            path.display()
        );
    } else {
        log::info!("[server] the world store has no world: forging a default world (this takes a while)");
        let bytes = forge(params).artifact(params).to_bytes();
        world_store::write_tiled(store, bytes).map_err(|e| format!("the forged world: {e}"))?;
        game::perf_event!("world_forged", "ms={} into=store", game::perf::ms(started.elapsed()));
    }
    load_written(store)
}

/// The world [`ensure_world`] just wrote, read back.
fn load_written(store: &StoreHandle) -> Result<(ForgedWorld, game::WorldMap), String> {
    world_store::load_forged(store)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the world store lost its world right after writing it".to_string())
}

/// Re-writes the store's tiles and world map from its archived forge file
/// (`write_tiled`, one transaction; nothing else in the store is touched).
fn tile_from_archive(store: &StoreHandle) -> Result<(), String> {
    let archive = store.forged_archive().map_err(|e| e.to_string())?.ok_or("the world store lost its world")?;
    world_store::write_tiled(store, archive).map(drop).map_err(archive_tiling_error)
}

/// What to do when the store's own archive cannot be tiled: a backend error
/// is transient (the write is one transaction, so nothing changed); anything
/// else means this build cannot serve the stored world, and re-creating it
/// throws away what the store has saved — said in so many words.
fn archive_tiling_error(e: world_store::StoreError) -> String {
    match e {
        e @ world_store::StoreError::Backend(_) => {
            format!("tiling the world store's forge file: {e} — retry; the store is untouched")
        }
        e => format!(
            "tiling the world store's forge file: {e} — this build cannot serve this world; keep the store and use a \
             build that can (`just worldctl export --db <db> <out.world>` writes its forge file out). Deleting the \
             store re-creates the world but DISCARDS everything saved in it (regions, players, history)"
        ),
    }
}

/// A forge file left at `import` after its world is in the store (a crash
/// between the import's commit and the delete): deleted when its bytes are
/// the stored archive's, else left alone with a warning.
fn forget_imported_file(store: &StoreHandle, import: Option<&Path>) {
    let Some(path) = import.filter(|p| p.exists()) else { return };
    let archive = match store.forged_archive() {
        Ok(archive) => archive,
        Err(e) => {
            log::warn!("[server] {} is ignored: cannot read the stored forge file to compare ({e})", path.display());
            return;
        }
    };
    let same = archive.is_some_and(|archive| std::fs::read(path).ok().is_some_and(|b| b == archive));
    if same {
        match std::fs::remove_file(path) {
            Ok(()) => log::info!("[server] {} was already imported: deleted", path.display()),
            Err(e) => log::error!("[server] cannot delete the imported world file {}: {e}", path.display()),
        }
    } else {
        log::warn!("[server] {} is ignored: the world store already holds another world file", path.display());
    }
}

/// Where [`ensure_history`] keeps a stored history it replaces, beside a
/// `.sqlite` store: `<db>.history.bak`.
pub fn history_backup_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(".history.bak");
    PathBuf::from(name)
}

/// The world's history from the store, else bootstrapped from the world map
/// (it reaches the store with the next save cut). One of another world or an
/// older version (the world id is derived from code-built map summaries, so
/// a code change can orphan it) is replaced by a new one — the next cut
/// writes over it — after its bytes are copied to `backup` (a write failure
/// stops the server: it is never discarded silently; with no backup path, a
/// Postgres store, the log says it is discarded). Any other one that will
/// not load stops the server: it holds the world's people.
pub fn ensure_history(
    store: &StoreHandle,
    map: &game::WorldMap,
    backup: Option<&Path>,
) -> Result<Option<game::history::History>, String> {
    use game::history::persist::{self, HistoryLoadError};
    use game::history::SocietyGrid;
    let grid = SocietyGrid::from_world_map(map);
    if let Some(bytes) = store.load_history().map_err(|e| e.to_string())? {
        match persist::decode(&bytes, grid.clone()) {
            Ok(h) => return Ok(Some(h)),
            Err(e @ (HistoryLoadError::OtherWorld | HistoryLoadError::OldVersion { .. })) => match backup {
                Some(bak) => {
                    std::fs::write(bak, &bytes).map_err(|io| {
                        format!("the stored history: {e}; cannot keep a copy in {}: {io}", bak.display())
                    })?;
                    log::warn!(
                        "[server] the stored history: {e} — kept as {}; bootstrapping a new one (the next save replaces it)",
                        bak.display()
                    );
                }
                None => log::error!(
                    "[server] the stored history: {e} — this store keeps no backup, so it is DISCARDED; bootstrapping a new one"
                ),
            },
            Err(e) => return Err(format!("the world store's history does not load: {e}")),
        }
    }
    Ok(bootstrap_history(map, grid))
}

/// A new history for `map`; `None` for a world nobody can live in (still
/// playable: no history).
fn bootstrap_history(map: &game::WorldMap, grid: game::history::SocietyGrid) -> Option<game::history::History> {
    use game::history::bootstrap::{self, BootstrapParams};
    let started = Instant::now();
    let params = BootstrapParams { seed: u64::from(grid.world_id()), ..BootstrapParams::default() };
    let history = match bootstrap::bootstrap(map, &params) {
        Ok(h) => h,
        Err(e) => {
            log::error!("[server] {e}; running without history");
            return None;
        }
    };
    let stats = history.stats();
    game::perf_event!(
        "history_bootstrapped",
        "ms={} civs={} sites={} alive={}",
        game::perf::ms(started.elapsed()),
        stats.civs_alive,
        stats.sites_alive,
        stats.alive
    );
    Some(history)
}

/// A world opened for serving: its store (none for the procedural world),
/// its forged world (to install) with its stored world map, and the world
/// tick it starts at.
pub struct OpenedWorld {
    pub store: Option<StoreHandle>,
    pub world: Option<ForgedWorld>,
    /// The forged world's map, read from the store (Decision 6: built once
    /// when the world was written, never at start).
    pub map: Option<game::WorldMap>,
    /// The tick of the store's last committed save cut (0 for a new world).
    pub world_tick: game::Tick,
}

/// Open the world `source` names: its store (a refusal — locked, other
/// dials, other format — is the error), refused beside old file saves
/// ([`check_legacy_saves`]), and its world taken from the store, imported or
/// forged ([`ensure_world`]). The store is shut down again on an error.
pub fn open_world(source: &WorldSource, params: &ForgeParams) -> Result<OpenedWorld, String> {
    let (url, import) = match source {
        WorldSource::Procedural => return Ok(OpenedWorld { store: None, world: None, map: None, world_tick: 0 }),
        WorldSource::Store { url, import } => (url, import.as_deref()),
    };
    let store = StoreHandle::open(url, world_store::Dials::BUILT).map_err(|e| format!("world store {url:?}: {e}"))?;
    let opened = check_legacy_saves(&store, import).and_then(|()| {
        let (world, map) = ensure_world(&store, import, params)?;
        let world_tick = store.world_info().map_err(|e| e.to_string())?.world_tick;
        Ok((world, map, world_tick))
    });
    match opened {
        Ok((world, map, world_tick)) => {
            Ok(OpenedWorld { store: Some(store), world: Some(world), map: Some(map), world_tick })
        }
        Err(e) => {
            store.shutdown();
            Err(e)
        }
    }
}

/// Makes the opened world the process's forged world
/// (`worldgen::forged::install`; the procedural world installs `None`) and
/// returns its world map — the stored one (`[perf] event=world_map
/// source=store`). Call once, before anything asks `worldgen::forged::active`.
pub fn install_world(opened: &mut OpenedWorld) -> Option<game::WorldMap> {
    worldgen::forged::install(opened.world.take());
    let map = opened.map.take()?;
    game::perf_event!("world_map", "source=store width={} height={}", map.width, map.height);
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmo-world-file-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn memory_store() -> StoreHandle {
        StoreHandle::open(&StoreUrl::Memory, world_store::Dials::BUILT).unwrap()
    }

    #[test]
    fn resolve_defaults_to_the_default_world_and_its_sqlite() {
        let default = from_repo_root(Path::new(DEFAULT_WORLD_PATH));
        let want = WorldSource::Store { url: store_url_for(&default), import: Some(default.clone()) };
        assert_eq!(resolve(None, None).unwrap(), want);
        assert_eq!(resolve(Some(""), Some(" ")).unwrap(), want);
        assert_eq!(store_url_for(&default), StoreUrl::Sqlite(default.with_extension("sqlite")));
        assert!(default.with_extension("sqlite").ends_with("worlds/default.sqlite"));
    }

    #[test]
    fn resolve_procedural_keyword() {
        assert_eq!(resolve(Some("procedural"), None).unwrap(), WorldSource::Procedural);
        assert_eq!(resolve(Some("procedural"), Some("x.sqlite")).unwrap(), WorldSource::Procedural);
    }

    #[test]
    fn resolve_explicit_world_file_and_store() {
        let file = from_repo_root(Path::new("worlds/w/terrain.world"));
        assert_eq!(
            resolve(Some("worlds/w/terrain.world"), None).unwrap(),
            WorldSource::Store { url: StoreUrl::Sqlite(file.with_extension("sqlite")), import: Some(file.clone()) }
        );
        assert_eq!(
            resolve(Some("worlds/w/terrain.world"), Some("postgres://u@h/db")).unwrap(),
            WorldSource::Store { url: StoreUrl::Postgres("postgres://u@h/db".into()), import: Some(file) }
        );
        assert_eq!(
            resolve(None, Some("/tmp/x.sqlite")).unwrap(),
            WorldSource::Store { url: StoreUrl::Sqlite(PathBuf::from("/tmp/x.sqlite")), import: None }
        );
    }

    #[test]
    fn corrupt_world_file_is_not_imported() {
        let dir = scratch("corrupt");
        let path = dir.join("bad.world");
        std::fs::write(&path, b"not a world").unwrap();
        let store = memory_store();
        // Default params: a forge here would take seconds, so this also
        // proves nothing was forged.
        let err = ensure_world(&store, Some(&path), &ForgeParams::default()).err().expect("refused");
        assert!(err.contains("bad.world") && err.contains("delete"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), b"not a world", "the file is left alone");
        assert!(!store.world_info().unwrap().has_artifact, "nothing imported");
        assert!(world_store::load_forged(&store).unwrap().is_none(), "no tiles either");
        store.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn ensure_world_forges_into_the_store_once() {
        let store = memory_store();
        let params = ForgeParams { size_m: 2048.0, cell_m: 64.0, seed: 4, ..ForgeParams::default() };
        let (first, map) = ensure_world(&store, None, &params).unwrap();
        assert!(first.tile_cache().is_some(), "written as tiles");
        let info = store.world_info().unwrap();
        assert!(info.has_artifact);
        assert_eq!(info.content_crc, Some(first.content_crc()));
        let (again, again_map) = ensure_world(&store, None, &params).unwrap();
        assert_eq!(again.content_crc(), first.content_crc(), "read back, not forged again");
        assert_eq!(again_map, map);
        drop((first, again));
        store.shutdown();
    }

    pub(super) fn land_map(width: u32, height: u32, sea: impl Fn(i32, i32) -> bool) -> game::WorldMap {
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
    fn ensure_history_bootstraps_without_a_stored_one() {
        let store = memory_store();
        let map = land_map(48, 48, |x, z| x < 2 || z < 2 || x >= 46 || z >= 46);
        let first = ensure_history(&store, &map, None).unwrap().unwrap();
        let second = ensure_history(&store, &map, None).unwrap().unwrap();
        assert_eq!(
            bincode::serialize(&first).unwrap(),
            bincode::serialize(&second).unwrap(),
            "a bootstrap is deterministic (until a cut stores it)"
        );
        store.shutdown();
    }

    fn store_history(store: &StoreHandle, history: &game::history::History) {
        let cut = game::WorldCut {
            id: game::CutId(1),
            tick: 1,
            regions: Vec::new(),
            in_flight: Vec::new(),
            players: Vec::new(),
            history: Some(game::history::persist::encode(history)),
        };
        store.commit_cut(cut).recv().unwrap().expect("the cut commits");
    }

    #[test]
    fn ensure_history_loads_the_stored_one() {
        let store = memory_store();
        let map = land_map(48, 48, |x, z| x < 2 || z < 2 || x >= 46 || z >= 46);
        let mut history = ensure_history(&store, &map, None).unwrap().unwrap();
        // Time passes: the stored history is not the bootstrap's.
        let later = game::history::CalendarTick(history.now().0 + 3 * game::history::DAYS_PER_YEAR * game::history::TICKS_PER_DAY);
        history.advance(later);
        store_history(&store, &history);
        let loaded = ensure_history(&store, &map, None).unwrap().unwrap();
        assert_eq!(bincode::serialize(&loaded).unwrap(), bincode::serialize(&history).unwrap());
        // Another world's (the map changed): replaced by a new bootstrap, the
        // stored one kept beside the store.
        let dir = scratch("history-bak");
        let bak = history_backup_path(&dir.join("w.sqlite"));
        assert!(bak.ends_with("w.sqlite.history.bak"));
        let other = land_map(48, 48, |x, z| x < 3 || z < 2 || x >= 46 || z >= 46);
        let fresh = ensure_history(&store, &other, Some(&bak)).unwrap().expect("a new history for the other world");
        assert_ne!(bincode::serialize(&fresh).unwrap(), bincode::serialize(&history).unwrap());
        assert_eq!(std::fs::read(&bak).unwrap(), game::history::persist::encode(&history), "the old one is kept");
        store.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_history_refuses_a_corrupt_one() {
        let store = memory_store();
        let map = land_map(48, 48, |x, z| x < 2 || z < 2 || x >= 46 || z >= 46);
        let history = ensure_history(&store, &map, None).unwrap().unwrap();
        let mut bytes = game::history::persist::encode(&history);
        bytes.truncate(bytes.len() / 2);
        let cut = game::WorldCut {
            id: game::CutId(1),
            tick: 1,
            regions: Vec::new(),
            in_flight: Vec::new(),
            players: Vec::new(),
            history: Some(bytes),
        };
        store.commit_cut(cut).recv().unwrap().expect("the cut commits");
        let err = ensure_history(&store, &map, None).err().expect("refused");
        assert!(err.contains("history"), "{err}");
        store.shutdown();
    }

    #[test]
    fn ensure_history_without_habitable_land_runs_without_history() {
        let store = memory_store();
        let sea = land_map(16, 16, |_, _| true);
        assert!(ensure_history(&store, &sea, None).unwrap().is_none());
        store.shutdown();
    }
}
