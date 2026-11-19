//! Which world the server serves, and where it keeps it: a forged world in a
//! world store (`VOXEL_WORLD_DB`, by default the `.sqlite` beside the world
//! file), imported from the forge file on first start or forged into the
//! store when there is none — stored as tiles with its world map and the
//! forge file as an archive row, and served from those tiles; or the
//! procedural noise world on request, which keeps nothing.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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

/// What a process does when its world store holds no world, no archive and has no
/// import file: `forge` (default) forges a default world itself; `wait` — a pod whose
/// world is forged by `worldctl forge` after it started — waits for that.
pub const EMPTY_STORE_ENV: &str = "VOXEL_EMPTY_STORE";

/// How often a waiting process looks at its store again.
pub const EMPTY_STORE_POLL: Duration = Duration::from_secs(5);

/// How often a waiting process says why (at most).
const EMPTY_STORE_LOG: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmptyStore {
    #[default]
    Forge,
    Wait,
}

/// [`EMPTY_STORE_ENV`]'s value -> what an empty store does. Unset or empty = `forge`.
pub fn empty_store_from(value: Option<&str>) -> Result<EmptyStore, String> {
    match value.map(str::trim) {
        None | Some("") | Some("forge") => Ok(EmptyStore::Forge),
        Some("wait") => Ok(EmptyStore::Wait),
        Some(other) => Err(format!("{EMPTY_STORE_ENV}={other}: expected `forge` or `wait`")),
    }
}

/// [`empty_store_from`] the process environment; read once by each caller of [`open_world_with`].
pub fn empty_store_from_env() -> Result<EmptyStore, String> {
    empty_store_from(std::env::var(EMPTY_STORE_ENV).ok().as_deref())
}

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
        (Some(db), _) => match StoreUrl::parse(db).map_err(|e| format!("{WORLD_DB_ENV}={}: {e}", world_store::redact_url(db)))? {
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
///
/// An empty store (no archive, no import file) is forged into with `params` under
/// `EmptyStore::Forge`; under `Wait` this polls ([`wait_for_forge`]) until another
/// process wrote a world, and returns `Ok(None)` when `stop` is set first (the caller exits 0).
///
/// Split roles on one Postgres world: the coordinator and every host run this at start.
/// After a build that changes the tile format two of them may re-tile at once; the
/// re-tile is one transaction, so one wins and a concurrent one may fail its start (start
/// it again — it then finds the new tiles). Bump a split world's processes one at a time
/// (coordinator first), or re-tile it once beforehand with `just worldctl retile`.
pub fn ensure_world(
    store: &StoreHandle,
    import: Option<&Path>,
    params: &ForgeParams,
    empty: EmptyStore,
    stop: Option<&AtomicBool>,
) -> Result<Option<(ForgedWorld, game::WorldMap)>, String> {
    let started = Instant::now();
    match world_store::load_forged(store) {
        Ok(Some(loaded)) => {
            forget_imported_file(store, import);
            return Ok(Some(loaded));
        }
        Ok(None) => {}
        Err(e @ world_store::StoreError::OtherFormat { kind, .. }) if kind.retileable() => {
            log::warn!("[server] {e}");
            log::info!("[server] re-tiling the world from its archived forge file (saves are kept)");
            tile_from_archive(store)?;
            game::perf_event!("world_retiled", "ms={} kind={kind:?}", game::perf::ms(started.elapsed()));
            forget_imported_file(store, import);
            return load_written(store).map(Some);
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
            e @ (world_store::StoreError::Backend(_) | world_store::StoreError::LockConflict { .. }) => {
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
    } else if empty == EmptyStore::Wait {
        if !wait_for_forge(store, import, stop) {
            return Ok(None);
        }
        // Something is there now: read it the usual way (tiled or not, imported or not).
        return ensure_world(store, import, params, empty, stop);
    } else {
        log::info!("[server] the world store has no world: forging a default world (this takes a while)");
        let bytes = forge(params).artifact(params).to_bytes();
        world_store::write_tiled(store, bytes).map_err(|e| format!("the forged world: {e}"))?;
        game::perf_event!("world_forged", "ms={} into=store", game::perf::ms(started.elapsed()));
    }
    load_written(store).map(Some)
}

/// Waits until the store holds a world (or an archive) or `import` exists, looking every
/// [`EMPTY_STORE_POLL`]; `false` when `stop` is set first. Forges nothing. A store error while
/// polling is logged (throttled like the wait) and the wait goes on.
fn wait_for_forge(store: &StoreHandle, import: Option<&Path>, stop: Option<&AtomicBool>) -> bool {
    let health = crate::health::Health::global();
    health.set_waiting(Some("forge"));
    let found = wait_for_forge_polling(store, import, stop);
    health.set_waiting(None);
    found
}

/// [`wait_for_forge`]'s polling.
fn wait_for_forge_polling(store: &StoreHandle, import: Option<&Path>, stop: Option<&AtomicBool>) -> bool {
    let mut logged_at: Option<Instant> = None;
    loop {
        let looked = store.world_info().map(|info| info.has_artifact);
        if looked.as_ref().is_ok_and(|has| *has) || import.is_some_and(Path::exists) {
            return true;
        }
        let now = Instant::now();
        if logged_at.is_none_or(|at| now >= at + EMPTY_STORE_LOG) {
            logged_at = Some(now);
            match looked {
                Err(e) => log::warn!("[server] the world store cannot be read ({e}): still waiting for 'worldctl forge'"),
                Ok(_) => log::warn!("[server] the world store has no world: waiting for 'worldctl forge'"),
            }
        }
        let until = now + EMPTY_STORE_POLL;
        while Instant::now() < until {
            if stop.is_some_and(|s| s.load(Ordering::SeqCst)) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
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
        e @ (world_store::StoreError::Backend(_) | world_store::StoreError::LockConflict { .. }) => {
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

/// Where [`ensure_coordinator_state`] keeps an R1 history row it cannot use, beside a
/// `.sqlite` store: `<db>.history.bak`.
pub fn history_backup_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(".history.bak");
    PathBuf::from(name)
}

/// Where [`ensure_coordinator_state`] keeps a coordinator state it replaces, beside a
/// `.sqlite` store: `<db>.coordinator.bak`.
pub fn coordinator_backup_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(".coordinator.bak");
    PathBuf::from(name)
}

/// Where the world coordinator's starting state came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordinatorSource {
    /// Its own `coordinator` row's projection (Elastic 2a: `core` + `history`).
    Stored,
    /// An R1 world's `history` row (no projection yet: a forged world that never ran): the
    /// history with no claims and an empty answer memo. The `history` row is left as it is
    /// and never written again.
    Migrated,
    /// A new history for the world map (none stored, or the stored one was of another
    /// world / an older version and no region was saved yet).
    Bootstrapped,
}

/// The world coordinator to start ([`crate::coordinator_rt::CoordinatorRuntime`]).
pub struct CoordinatorStart {
    /// Its clock at [`Self::store_time`]'s store ms (loaded or built at it: the calendar of a
    /// loaded core continues from its stored anchor, a new one anchors there).
    pub coordinator: game::Coordinator,
    /// The database clock sampled after the row's read (ms since the epoch), and when: the
    /// core's clock is the store's (Elastic 2a); the runtime extrapolates it by the monotonic
    /// clock from that instant until its first `hosts` read re-samples it.
    pub store_time: (u64, Instant),
    pub source: CoordinatorSource,
    /// The row's version the state was read (or bootstrapped) at: what the first write names.
    /// There is no claim (Elastic 2b Task 3): a coordinator still running that commits after
    /// this read moves the version, and this start's first write meets a `Conflict` and rebases.
    pub version: u64,
    /// This replica registered with no other replica live (Elastic 2b Task 4): its first input
    /// ([`game::coordinator::Input::ReplicaJoined`]) drops every replica registered before it.
    pub alone: bool,
    /// The row version whose transition last wrote the history this core holds (0: unknown —
    /// a refresh then reads the history whole).
    pub history_version: u64,
}

/// The refusal of a world whose coordinator state this build cannot take over while regions
/// are saved (Elastic 2a): worlds are re-forged, never migrated — a fresh history beside saved
/// bodies would reissue the PersonIds those bodies carry.
fn refuse_old_world(what: &str) -> String {
    format!(
        "the world store's coordinator state {what}, and the world has saved regions: this build cannot take it \
         over (a new history beside the saved people would give their ids to others) — re-forge the world \
         (the debug world: `just run --reset`; a menu world: make a new one; the dev Postgres worlds: \
         `just pg-reset`, which deletes all dev Postgres data)"
    )
}

/// Loads the world coordinator's state: its row's projection (Elastic 2a: `core` + `history`),
/// else an R1 world's `history` row migrated into one (claims empty — a player's record claim
/// is taken back when the player comes online — and no memo), else a history bootstrapped from
/// the world map. A migrated or bootstrapped core is written at once as the row's first version
/// ([`StoreHandle::bootstrap_coordinator`], Elastic 2b Task 3: first writer wins — a replica
/// that finds the row there loads it instead), its cut ids' high half set once from the row's
/// epoch. There is no claim any more: the row's version is the coordinator's fence.
///
/// A stored state of another world or an older format (the world id is derived from
/// code-built map summaries, so a code change can orphan it), or a row with no projection, is
/// REFUSED while the world has saved regions (re-forge: [`refuse_old_world`]); with none saved
/// it is replaced by a new bootstrap after its history is copied beside a SQLite store
/// (`sqlite_db`: [`coordinator_backup_path`] / [`history_backup_path`]; a copy that cannot be
/// written stops the server — it is never discarded silently; with no path, a Postgres store,
/// the log says it is discarded). Anything else that will not load stops the server: it holds
/// the world's people. `None`: a world nobody can live in (playable, no history).
pub fn ensure_coordinator_state(
    store: &StoreHandle,
    map: &game::WorldMap,
    sqlite_db: Option<&Path>,
) -> Result<Option<CoordinatorStart>, String> {
    ensure_coordinator_state_at(store, map, sqlite_db, "")
}

/// [`ensure_coordinator_state`] for a replica listening for hosts at `addr` (a label in its
/// `coordinator_replicas` row). Elastic 2b Task 4: the replica REGISTERS first — its id issued
/// by the store, never reused (`StoreHandle::register_replica`) — then reads the row.
pub fn ensure_coordinator_state_at(
    store: &StoreHandle,
    map: &game::WorldMap,
    sqlite_db: Option<&Path>,
    addr: &str,
) -> Result<Option<CoordinatorStart>, String> {
    let registered = store.register_replica(addr).map_err(|e| format!("registering the coordinator replica: {e}"))?;
    start_from_row(store, map, sqlite_db, registered)
}

/// [`ensure_coordinator_state_at`] once registered as `registered`.
fn start_from_row(
    store: &StoreHandle,
    map: &game::WorldMap,
    sqlite_db: Option<&Path>,
    registered: world_store::ReplicaRegistered,
) -> Result<Option<CoordinatorStart>, String> {
    use game::history::persist::{self, HistoryLoadError};
    use game::history::SocietyGrid;
    let grid = SocietyGrid::from_world_map(map);
    let replaceable = |e: &HistoryLoadError| matches!(e, HistoryLoadError::OtherWorld | HistoryLoadError::OldVersion { .. });
    let process = registered.replica;
    let saved_regions = || -> Result<bool, String> {
        store.stored_regions().map(|r| !r.is_empty()).map_err(|e| format!("reading the world's saved regions: {e}"))
    };
    // The core's clock is the store's: sampled once the row is read.
    let store_ms = || store.db_now_ms().map_err(|e| format!("reading the world store's clock: {e}"));
    let started = |coordinator: game::Coordinator, store_ms: u64, source: CoordinatorSource, version: u64, history_version: u64| {
        CoordinatorStart { coordinator, store_time: (store_ms, Instant::now()), source, version, alone: registered.alone, history_version }
    };
    // A new core (migrated or bootstrapped): the row's first version, unless one is there (a
    // replaced state: the first transition writes over it at its version), or another replica
    // bootstrapped it meanwhile (`None`: load that instead).
    let first = |mut coordinator: game::Coordinator, replacing: Option<u64>| -> Result<Option<(game::Coordinator, u64)>, String> {
        if let Some(version) = replacing {
            // Final review T3: its cut ids' high half past every one the replaced core made (a
            // reused cut id would be answered from that core's stored decision, unchecked).
            let epoch = store
                .raise_coordinator_epoch()
                .map_err(|e| format!("raising the replaced coordinator row's epoch: {e}"))?
                .ok_or_else(|| "the coordinator row being replaced is gone".to_string())?;
            log::info!("[coordinator] replacing the stored coordinator state: its cut ids' high half is now {epoch}");
            coordinator.set_cut_epoch(epoch);
            return Ok(Some((coordinator, version)));
        }
        coordinator.set_cut_epoch(1);
        let (core, history) = coordinator.encode_projection();
        match store.bootstrap_coordinator(core, history).map_err(|e| format!("bootstrapping the world coordinator's row: {e}"))? {
            world_store::Bootstrapped::Written { version, epoch } => {
                // Its cut ids' high half, once: the row's epoch (1, or past a row an older build
                // claimed and never wrote — then its first transition writes the raised one).
                if epoch == 1 {
                    coordinator.bootstrapped(version);
                } else {
                    coordinator.set_cut_epoch(epoch);
                    coordinator.set_version(version);
                }
                log::info!("[coordinator] bootstrapped the world coordinator's row (version {version}, epoch {epoch})");
                Ok(Some((coordinator, version)))
            }
            world_store::Bootstrapped::Exists => Ok(None),
        }
    };
    let stored = store.load_coordinator().map_err(|e| e.to_string())?;
    let replacing = stored.as_ref().map(|s| s.version);
    if let Some(row) = stored {
        let now = store_ms()?;
        match game::Coordinator::load(&row.core, &row.history, grid.clone(), process, now) {
            Ok(coordinator) => {
                return Ok(Some(started(coordinator, now, CoordinatorSource::Stored, row.version, row.history_version)));
            }
            Err(e) if replaceable(&e) => {
                if saved_regions()? {
                    return Err(refuse_old_world(&format!("does not load ({e})")));
                }
                keep_replaced("coordinator state", &e, &row.history, sqlite_db.map(coordinator_backup_path))?
            }
            Err(e) => return Err(format!("the world store's coordinator state does not load: {e}")),
        }
    } else if saved_regions()? {
        // A world saved before Elastic 2a (its old state dropped by migration 0016), or one
        // whose coordinator never wrote while its hosts saved: nothing to run its people from.
        return Err(refuse_old_world("is missing (a world saved by an older build, or a coordinator that never wrote)"));
    } else if let Some(bytes) = store.load_history().map_err(|e| e.to_string())? {
        match persist::decode(&bytes, grid.clone()) {
            Ok(history) => {
                log::info!("[server] migrating the world's history row into the coordinator's state");
                let now = store_ms()?;
                return match first(game::Coordinator::new_for(history, now, process), replacing)? {
                    Some((coordinator, version)) => {
                        let written = if replacing.is_some() { 0 } else { version };
                        Ok(Some(started(coordinator, now, CoordinatorSource::Migrated, version, written)))
                    }
                    None => start_from_row(store, map, sqlite_db, registered),
                };
            }
            Err(e) if replaceable(&e) => keep_replaced("history", &e, &bytes, sqlite_db.map(history_backup_path))?,
            Err(e) => return Err(format!("the world store's history does not load: {e}")),
        }
    }
    let Some(history) = bootstrap_history(map, grid) else { return Ok(None) };
    let now = store_ms()?;
    match first(game::Coordinator::new_for(history, now, process), replacing)? {
        Some((coordinator, version)) => {
            // A replaced state's history is written by the first transition (0: read it whole).
            let written = if replacing.is_some() { 0 } else { version };
            Ok(Some(started(coordinator, now, CoordinatorSource::Bootstrapped, version, written)))
        }
        // Another replica bootstrapped the row since this one read it: load it.
        None => start_from_row(store, map, sqlite_db, registered),
    }
}

/// A stored `what` that is replaced by a bootstrap: copied to `backup` first (a write
/// failure is the error), or logged as discarded when there is nowhere to keep it.
fn keep_replaced(
    what: &str,
    why: &game::history::persist::HistoryLoadError,
    bytes: &[u8],
    backup: Option<PathBuf>,
) -> Result<(), String> {
    match backup {
        Some(bak) => {
            std::fs::write(&bak, bytes)
                .map_err(|io| format!("the stored {what}: {why}; cannot keep a copy in {}: {io}", bak.display()))?;
            log::warn!(
                "[server] the stored {what}: {why} — kept as {}; bootstrapping a new history (saved at once)",
                bak.display()
            );
        }
        None => log::error!(
            "[server] the stored {what}: {why} — this store keeps no backup, so it is DISCARDED; bootstrapping a new history"
        ),
    }
    Ok(())
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
    open_world_with(source, params, EmptyStore::Forge, None).map(|opened| opened.expect("no stop flag, no stop"))
}

/// [`open_world`] with what an empty store does and the flag that ends a wait
/// (`Ok(None)`: the flag was set while waiting for a world; the store is shut down).
pub fn open_world_with(
    source: &WorldSource,
    params: &ForgeParams,
    empty: EmptyStore,
    stop: Option<&AtomicBool>,
) -> Result<Option<OpenedWorld>, String> {
    let (url, import) = match source {
        WorldSource::Procedural => return Ok(Some(OpenedWorld { store: None, world: None, map: None, world_tick: 0 })),
        WorldSource::Store { url, import } => (url, import.as_deref()),
    };
    let store = StoreHandle::open(url, world_store::Dials::BUILT, world_store::OpenMode::Writer)
        .map_err(|e| format!("world store {url}: {e}"))?;
    let opened = check_legacy_saves(&store, import).and_then(|()| {
        let Some((world, map)) = ensure_world(&store, import, params, empty, stop)? else {
            return Ok(None);
        };
        let world_tick = store.world_info().map_err(|e| e.to_string())?.world_tick;
        Ok(Some((world, map, world_tick)))
    });
    match opened {
        Ok(Some((world, map, world_tick))) => {
            Ok(Some(OpenedWorld { store: Some(store), world: Some(world), map: Some(map), world_tick }))
        }
        Ok(None) => {
            store.shutdown();
            Ok(None)
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

