//! World-level management: which regions run, who is subscribed to what,
//! where players' home regions are, and the parking lot for cycled-out
//! regions. Threadless: a `RegionSpawner` decides how regions actually run
//! (OS threads on the server, inline for wasm/tests), and time arrives as
//! an explicit `now_ms` so the core stays deterministic and wasm-safe.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crossbeam::channel::{unbounded, Receiver, Sender};

use crate::history::{CalendarTick, EmbarkRefusal, History, SiteId, TICKS_PER_DAY};
use crate::{
    Chunk, ChunkCoords, ClientId, ClientPacket, Column, ColumnCoords, EntityKind, TerrainSnapshot, GameEventKind,
    LodBuildJob, LodBuilder, LodKey, LodPayload, LodResult, Region, RegionCoords, RegionInput,
    RegionOutput, RegionRunner, RegionSeed, SerializedRegion, ServerPacket, PlayRefusal, WorldMap, EmbarkRequest,
};

pub const SPAWN_REGION: RegionCoords = RegionCoords { x: 0, z: 0 };

/// The sites as last broadcast, bucketed (see `WorldManager::sites_key`).
fn sites_key(history: &History) -> Vec<(SiteId, bool, u32, bool)> {
    history.site_markers().into_iter().map(|m| (m.site, m.abandoned, m.population / 10, m.returnable)).collect()
}
pub const UNLOAD_GRACE_MS: u64 = 5000;
/// LRU cap on `lod_cache`; subscribed keys are pinned past this (see
/// `evict_lod_cache`).
pub const LOD_CACHE_MAX_ENTRIES: usize = 8192;
/// LRU cap on *pristine* `terrain_cache` entries — full-resolution snapshots
/// generated only to feed a factor 2/4/16 LOD build for a region that has
/// never run. Only reached with a builder that does not
/// `generates_pristine()` (the synchronous `InlineLodBuilder`: wasm, tests);
/// the native server's thread pool generates in its workers and retains
/// nothing here (each snapshot measured ~43 MB). One production-scale join touches ~280 of them and nothing
/// used to drop them. They are pure worldgen output, so eviction only costs a
/// regeneration; authoritative entries (running/parked regions) are never
/// evicted — they are the only copy of edited terrain outside the region.
/// A snapshot grows with region AREA, so the entry count shrinks with it and
/// the memory ceiling (~2.7 GB) stays where it was: 64 entries at the default
/// `REGION_BLOCKS`, 16 at 128.
pub const PRISTINE_TERRAIN_CACHE_MAX: usize = pristine_cache_max(crate::REGION_BLOCKS);

/// 64 entries at 64 blocks, shrinking with region area; never 0 (it was from
/// 512 chunks up: every insert evicted at once). Written in blocks so it does
/// not change meaning with `CHUNK_BLOCKS`.
pub const fn pristine_cache_max(region_blocks: usize) -> usize {
    let n = 64 * (64 * 64) / (region_blocks * region_blocks);
    if n == 0 { 1 } else { n }
}

/// Internal event handled by the world manager. (Moved from
/// crates/server/src/main.rs; ServerTickTimer is gone — regions self-tick.)
#[derive(Debug)]
pub enum ServerEvent {
    ClientPacket(ClientPacket, ClientId),
    ClientConnected(ClientId),
    ClientDisconnected(ClientId),
}

pub type RegionGenerator = Box<dyn FnMut(RegionCoords) -> Vec<(ColumnCoords, Column)> + Send>;

pub trait RegionSpawner {
    fn spawn(
        &mut self,
        id: RegionCoords,
        seed: RegionSeed,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Sender<RegionInput>;

    /// Reclaim a stopped region's resources (join its thread). Called after
    /// `RegionOutput::Stopped` or when a dead region is detected.
    fn reap(&mut self, _id: RegionCoords) {}

    /// Whether this spawner runs worldgen itself, on the region's own thread.
    /// `true` makes the manager hand out [`RegionSeed::Deferred`] instead of
    /// generating the columns synchronously in `ensure_running`. `false` (the
    /// inline spawner: wasm, tests) keeps the manager generating — there is no
    /// other thread to move the work to.
    fn generates_columns(&self) -> bool {
        false
    }
}

/// Chance (per mille, per region per round) that [`InlineSchedule::jittery`]
/// delivers a region's inputs a tick late.
pub const SCHEDULE_LATE_PER_MILLE: u32 = 300;
/// Chance (per mille, per region per round) that [`InlineSchedule::jittery`]
/// makes a region miss its tick.
pub const SCHEDULE_SKIP_PER_MILLE: u32 = 100;

/// Seeded stand-in for OS thread scheduling, so the inline spawner can replay
/// the timing real region threads produce — reproducibly, from a seed.
///
/// Plain `pump()`/`tick_all()` is one fixed schedule: every region ticks once
/// per round, and everything a region emitted reaches its neighbours before
/// their next tick. On real threads (`server::region_threads`) each region
/// has its own timer. A constant phase offset between two regions is
/// indistinguishable from lockstep, so what this models is the JITTER:
///
/// - tick order is permuted, which permutes the order the manager sees region
///   outputs in (two regions handing off in the same round);
/// - a *late* region does not look at its input queue until after its next
///   tick — a handoff, ghost update or player input that lost the race with
///   the tick it would have preceded in lockstep;
/// - a *skipped* region misses a round entirely, the way
///   `region_thread_loop` drops an overrun deadline instead of replaying it,
///   so its tick count drifts against its neighbours for good.
#[derive(Debug, Clone)]
pub struct InlineSchedule {
    rng: u64,
    /// Chance, per region per round, that its inputs arrive a tick late.
    pub late_per_mille: u32,
    /// Chance, per region per round, that it misses its tick.
    pub skip_per_mille: u32,
}

impl InlineSchedule {
    /// Permuted tick order only — every region still ticks once per round and
    /// nothing arrives late.
    pub fn shuffled(seed: u64) -> Self {
        Self { rng: seed, late_per_mille: 0, skip_per_mille: 0 }
    }

    /// Permuted order plus late inputs; tick counts stay in lockstep (for
    /// harnesses that align client and server by tick).
    pub fn late(seed: u64) -> Self {
        Self { late_per_mille: SCHEDULE_LATE_PER_MILLE, ..Self::shuffled(seed) }
    }

    /// Everything: permuted order, late inputs and missed ticks.
    pub fn jittery(seed: u64) -> Self {
        Self { skip_per_mille: SCHEDULE_SKIP_PER_MILLE, ..Self::late(seed) }
    }

    /// splitmix64 — the schedule must not depend on a platform RNG.
    fn next(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn roll(&mut self, per_mille: u32) -> bool {
        per_mille > 0 && self.next() % 1000 < u64::from(per_mille)
    }

    fn shuffle(&mut self, ids: &mut [RegionCoords]) {
        for i in (1..ids.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            ids.swap(i, j);
        }
    }
}

/// Runs regions inline in the caller's thread: the wasm LocalServer and the
/// headless tests. `pump()`/`tick_all()` stand in for thread scheduling —
/// in lockstep by default, or jittered by an [`InlineSchedule`].
#[derive(Default)]
pub struct InlineSpawner {
    runners: BTreeMap<RegionCoords, (Receiver<RegionInput>, RegionRunner)>,
    schedule: Option<InlineSchedule>,
    /// Regions whose input queue stays untouched until after their next tick
    /// (see [`InlineSchedule`]). Always empty without a schedule.
    late: BTreeSet<RegionCoords>,
}

impl InlineSpawner {
    pub fn with_schedule(schedule: InlineSchedule) -> Self {
        Self { schedule: Some(schedule), ..Self::default() }
    }

    /// Swap the schedule on a live spawner (`None` = back to lockstep), for
    /// harnesses that construct their server before choosing one.
    pub fn set_schedule(&mut self, schedule: Option<InlineSchedule>) {
        self.schedule = schedule;
        self.late.clear();
    }

    pub fn pump(&mut self) {
        let ids: Vec<RegionCoords> =
            self.runners.keys().copied().filter(|id| !self.late.contains(id)).collect();
        for id in ids {
            let mut stopped = false;
            if let Some((recv, runner)) = self.runners.get_mut(&id) {
                while let Ok(input) = recv.try_recv() {
                    if !runner.handle_input(input) {
                        stopped = true;
                        break;
                    }
                }
            }
            if stopped {
                self.runners.remove(&id);
            }
        }
    }

    pub fn tick_all(&mut self) {
        let Some(schedule) = self.schedule.as_mut() else {
            for (_, (_, runner)) in self.runners.iter_mut() {
                runner.tick();
            }
            return;
        };
        let mut ids: Vec<RegionCoords> = self.runners.keys().copied().collect();
        schedule.shuffle(&mut ids);
        // Last round's late regions tick here without having seen their
        // inputs, then catch up at the next `pump()`.
        self.late.clear();
        for id in ids {
            if !schedule.roll(schedule.skip_per_mille) {
                if let Some((_, runner)) = self.runners.get_mut(&id) {
                    runner.tick();
                }
            }
            if schedule.roll(schedule.late_per_mille) {
                self.late.insert(id);
            }
        }
    }

    pub fn running(&self) -> Vec<RegionCoords> {
        self.runners.keys().copied().collect()
    }

    /// Test support: make a region's channel dead without a Shutdown
    /// handshake, simulating a crashed region thread.
    pub fn kill(&mut self, id: RegionCoords) {
        self.runners.remove(&id);
    }

    /// Test support: run a closure against a live region (state assertions
    /// and teleports in headless harnesses).
    pub fn with_region(&mut self, id: RegionCoords, f: impl FnOnce(&mut Region)) {
        if let Some((_, runner)) = self.runners.get_mut(&id) {
            f(runner.region_mut());
        }
    }
}

impl RegionSpawner for InlineSpawner {
    fn spawn(
        &mut self,
        id: RegionCoords,
        seed: RegionSeed,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Sender<RegionInput> {
        let (send, recv) = unbounded();
        let runner = seed.into_runner(
            id,
            out,
            &|rc| unreachable!("InlineSpawner never generates columns ({rc:?})"),
        );
        self.runners.insert(id, (recv, runner));
        send
    }
}

#[derive(Default)]
struct Session {
    subscribed: BTreeSet<RegionCoords>,
    lod_subscribed: BTreeSet<LodKey>,
}

struct RegionLink {
    input: Sender<RegionInput>,
    subscribers: BTreeSet<ClientId>,
    /// Set when the region lost its last keep-alive reason; cleared on
    /// resubscribe. Grace-period timestamp base.
    empty_since_ms: Option<u64>,
    /// Shutdown sent, Stopped not yet received. Subscribes arriving in this
    /// window queue in `resubscribe_pending` and re-run after Stopped.
    stopping: bool,
    resubscribe_pending: Vec<ClientId>,
}

/// Manager-thread work done since the last [`WorldManager::take_perf_counters`]
/// — the inputs to the server's periodic `[server]` line. Wall-clock timings,
/// so this is observability only: nothing in the simulation may read it.
#[derive(Debug, Default, Clone, Copy)]
pub struct ManagerPerfCounters {
    /// Synchronous worldgen calls on the manager thread (region starts and
    /// pristine LOD sources) and their cost — while one runs, no packet routes.
    pub generated: u32,
    pub generate_total: std::time::Duration,
    pub generate_max: std::time::Duration,
    /// `RequestLod`s answered straight from `lod_cache`.
    pub lod_cache_hits: u32,
    /// LOD builds handed to the builder, and results received back.
    pub lod_submitted: u32,
    pub lod_built: u32,
    /// Submit → result latency of those results (queueing + build).
    pub lod_latency_total: std::time::Duration,
    pub lod_latency_max: std::time::Duration,
}

/// Point-in-time queue/cache depths for the same line.
#[derive(Debug, Default, Clone, Copy)]
pub struct ManagerDepths {
    pub sessions: usize,
    pub running: usize,
    pub parked: usize,
    pub lod_in_flight: usize,
    /// LOD keys parked behind a terrain reply (region- plus tile-keyed).
    pub lod_waiting_terrain: usize,
    pub lod_cache: usize,
    pub terrain_cache: usize,
}

pub struct WorldManager<S: RegionSpawner> {
    spawner: S,
    generator: RegionGenerator,
    regions: BTreeMap<RegionCoords, RegionLink>,
    parked: BTreeMap<RegionCoords, SerializedRegion>,
    /// Frozen terrain for parked / never-run regions, so LOD builds never
    /// wake a region. Refreshed on TerrainChanged and on park (Stopped).
    terrain_cache: BTreeMap<RegionCoords, TerrainSnapshot>,
    /// Which `terrain_cache` entries are pristine worldgen output (evictable),
    /// with their last-use stamp from `lod_access_counter`.
    pristine_terrain: BTreeMap<RegionCoords, u64>,
    /// LRU cap on `pristine_terrain`; defaults to `PRISTINE_TERRAIN_CACHE_MAX`,
    /// overridable via `set_pristine_terrain_max` (test hook).
    pristine_terrain_max: usize,
    /// Connected clients and their subscriptions.
    sessions: BTreeMap<ClientId, Session>,
    /// Survives disconnects: which region holds this client's player entity.
    /// (The player parks with its region; reconnects create nothing.)
    homes: BTreeMap<ClientId, RegionCoords>,
    /// The forged world's tile summaries, pushed to every client on connect
    /// and consulted by Play. `None` = no forged world (procedural / tests).
    world_map: Option<WorldMap>,
    /// Server-authored Play events (`ClientLeft` / `CreateClientAt`) for a
    /// region that was mid-stop when Play arrived; replayed in order (waking
    /// the region from its parked blob) once it parks.
    pending_events: BTreeMap<RegionCoords, Vec<RegionInput>>,
    /// The world's history (forged worlds): it puts its people into regions as they
    /// start, takes them back as they park, and sends running regions its decisions.
    /// `None` = no history (procedural worlds, most tests): people are untouched.
    history: Option<History>,
    /// `(now_ms, calendar tick)` the calendar was anchored at by [`Self::set_history`].
    calendar_anchor: (u64, CalendarTick),
    /// The fortress each client plays (embarked or returned to, not retired). Not
    /// persisted: a fortress is played only while its player is connected; on disconnect
    /// the entry stays (like `homes`) so a reconnect resumes it, but it no longer keeps
    /// anyone else from returning there (see [`Self::return_to`]).
    fortress_of: BTreeMap<ClientId, SiteId>,
    /// The calendar day last broadcast (`ServerPacket::Calendar`).
    calendar_day: Option<u64>,
    /// What clients were last told of the sites, bucketed: (site, abandoned, population
    /// / 10, returnable). A change is broadcast at the next day's turn.
    sites_key: Vec<(SiteId, bool, u32, bool)>,
    out: Sender<(Option<ClientId>, ServerPacket)>,
    region_out_send: Sender<(RegionCoords, RegionOutput)>,
    /// LOD subscriptions per key, and the reverse map for disconnects.
    lod_subscribers: BTreeMap<LodKey, BTreeSet<ClientId>>,
    /// key -> RLE payload; LRU-evicted past LOD_CACHE_MAX_ENTRIES,
    /// subscribed keys pinned. `u64` is a monotonically increasing access
    /// stamp (not wall time — determinism).
    lod_cache: BTreeMap<LodKey, (LodPayload, u64)>,
    /// LRU cap on `lod_cache`; defaults to `LOD_CACHE_MAX_ENTRIES`, overridable
    /// via `set_lod_cache_max` (test hook — mirrors `LodRings.retirement_max_frames`).
    lod_cache_max: usize,
    lod_access_counter: u64,
    /// Builds submitted, result not yet received — dedups double-requests.
    lod_in_flight: BTreeSet<LodKey>,
    /// When each in-flight build was submitted (metrics only, see `perf`).
    lod_submitted_at: BTreeMap<LodKey, crate::perf::Stopwatch>,
    perf: ManagerPerfCounters,
    /// Region-keyed builds waiting on a running region's terrain reply.
    lod_pending_terrain: BTreeMap<RegionCoords, BTreeSet<LodKey>>,
    /// Tile-keyed builds waiting on a subset of `key.regions()` whose terrain
    /// reply hasn't landed yet. Settled incrementally by `lod_terrain_ready`
    /// as each awaited region's reply seeds `terrain_cache`.
    lod_tile_waiting: BTreeMap<LodKey, BTreeSet<RegionCoords>>,
    lod_builder: Box<dyn LodBuilder>,
    lod_results: Receiver<LodResult>,
}

impl<S: RegionSpawner> WorldManager<S> {
    pub fn new(
        spawner: S,
        generator: RegionGenerator,
        out: Sender<(Option<ClientId>, ServerPacket)>,
        region_out_send: Sender<(RegionCoords, RegionOutput)>,
        lod_builder: Box<dyn LodBuilder>,
        lod_results: Receiver<LodResult>,
    ) -> Self {
        Self {
            spawner,
            generator,
            regions: BTreeMap::new(),
            parked: BTreeMap::new(),
            terrain_cache: BTreeMap::new(),
            pristine_terrain: BTreeMap::new(),
            pristine_terrain_max: PRISTINE_TERRAIN_CACHE_MAX,
            sessions: BTreeMap::new(),
            homes: BTreeMap::new(),
            world_map: None,
            pending_events: BTreeMap::new(),
            history: None,
            calendar_anchor: (0, CalendarTick(0)),
            fortress_of: BTreeMap::new(),
            calendar_day: None,
            sites_key: Vec::new(),
            out,
            region_out_send,
            lod_subscribers: BTreeMap::new(),
            lod_cache: BTreeMap::new(),
            lod_cache_max: LOD_CACHE_MAX_ENTRIES,
            lod_access_counter: 0,
            lod_in_flight: BTreeSet::new(),
            lod_submitted_at: BTreeMap::new(),
            perf: ManagerPerfCounters::default(),
            lod_pending_terrain: BTreeMap::new(),
            lod_tile_waiting: BTreeMap::new(),
            lod_builder,
            lod_results,
        }
    }

    /// Install the world map; connecting clients receive it, Play checks
    /// targets against it, and every region is bounded by its extent
    /// (`RegionInput::SetMapExtent`): nothing crosses the world's outer seam.
    pub fn set_world_map(&mut self, map: WorldMap) {
        let extent = Some(map.extent());
        self.world_map = Some(map);
        let running: Vec<RegionCoords> = self.regions.keys().copied().collect();
        for rc in running {
            self.send_to_region(rc, RegionInput::SetMapExtent(extent));
        }
    }

    /// Install the world's history. Its calendar runs from `history.now()` at `now_ms`,
    /// one calendar tick per `TICK_RATE` ms; every region already running is caught up
    /// (its people spawned) at once. Installed once: a second history would orphan the
    /// bodies the first one placed.
    pub fn set_history(&mut self, history: History, now_ms: u64) {
        debug_assert!(self.history.is_none(), "a history is already installed");
        self.calendar_anchor = (now_ms, history.now());
        self.calendar_day = Some(history.now().0 / TICKS_PER_DAY);
        self.sites_key = sites_key(&history);
        self.history = Some(history);
        let running: Vec<RegionCoords> =
            self.regions.iter().filter(|(_, l)| !l.stopping).map(|(rc, _)| *rc).collect();
        for rc in running {
            let events = self.history.as_mut().unwrap().region_started(rc);
            for kind in events {
                self.send_to_region(rc, RegionInput::Event(kind));
            }
        }
        self.deliver_directives();
    }

    /// The calendar at `now_ms`: the anchor's tick plus one per `TICK_RATE` ms since.
    /// `None` without a history.
    pub fn calendar_now(&self, now_ms: u64) -> Option<CalendarTick> {
        self.history.as_ref()?;
        let (ms, tick) = self.calendar_anchor;
        Some(CalendarTick(tick.0 + now_ms.saturating_sub(ms) / crate::TICK_RATE))
    }

    pub fn history(&self) -> Option<&History> {
        self.history.as_ref()
    }

    /// The history, to change directly (tests, tools). Whatever it decides for running
    /// regions reaches them at the next [`Self::maintain`]. Never call its region lifecycle
    /// methods (`region_started` / `region_stopping` / `region_parked` / `person_crossed`)
    /// through it: the manager owns them, and a call behind its back breaks the one-body
    /// invariant.
    pub fn history_mut(&mut self) -> Option<&mut History> {
        self.history.as_mut()
    }

    /// Send every directive the history has decided to its region. Only running regions
    /// get directives — the history defers what acts on a stopping one — so one for any
    /// other region is a bug, and dropped.
    fn deliver_directives(&mut self) {
        let Some(history) = self.history.as_mut() else { return };
        // Regions lost (thread died, respawned) during this delivery: the respawn's
        // catch-up already placed their people, so what is left for them is stale.
        let mut lost = BTreeSet::new();
        for d in history.take_directives() {
            if lost.contains(&d.region) {
                log::warn!("history directive for {:?}, lost and respawned meanwhile, dropped: {:?}", d.region, d.event);
                continue;
            }
            let live = self.regions.get(&d.region).is_some_and(|l| !l.stopping);
            if !live {
                log::error!("history directive for {:?}, which is not running, dropped: {:?}", d.region, d.event);
                continue;
            }
            if !self.send_to_region(d.region, RegionInput::Event(d.event)) {
                lost.insert(d.region);
            }
        }
    }

    /// `rc` stopped without parking its people (its thread died): the history takes back
    /// everyone it had there, as if it had parked empty.
    fn history_region_lost(&mut self, rc: RegionCoords) {
        if let Some(history) = self.history.as_mut() {
            history.region_parked(rc, Vec::new());
        }
    }

    fn map_extent(&self) -> Option<crate::MapExtent> {
        self.world_map.as_ref().map(WorldMap::extent)
    }

    /// Play: move the client's player to the landing spot of `target`.
    /// Refusals change no state.
    fn play_region(&mut self, id: ClientId, target: RegionCoords, now_ms: u64) {
        let refuse = |this: &Self, r: PlayRefusal| {
            let _ = this.out.send((Some(id), ServerPacket::PlayRefused(target, r)));
        };
        let Some(map) = &self.world_map else {
            return refuse(self, PlayRefusal::NoWorldMap);
        };
        let Some(tile) = map.tile(target) else {
            return refuse(self, PlayRefusal::OffMap);
        };
        let home = self.homes.get(&id).copied();
        if home == Some(target) {
            return refuse(self, PlayRefusal::AlreadyHere);
        }
        let edge = crate::REGION_BLOCKS as u32;
        let landing = match tile.landing {
            Some(l) if l[0] < edge && l[1] < edge => l,
            _ => return refuse(self, PlayRefusal::NoDryLand),
        };
        self.move_client(id, target, landing, now_ms);
        let from = home.map_or("none".to_string(), |h| format!("{},{}", h.x, h.z));
        crate::perf_event!(
            "play_region",
            "client={id} from={from} to={},{}",
            target.x,
            target.z
        );
    }

    /// Move the client's player from its home (if any) to `landing` in `target`: it
    /// leaves the old home, is created in `target` (woken if parked — after any history
    /// catch-up, so a fortress's people are there first), and `target` becomes its home.
    fn move_client(&mut self, id: ClientId, target: RegionCoords, landing: [u32; 2], now_ms: u64) {
        let home = self.homes.get(&id).copied();
        if home == Some(target) {
            let _ = self.out.send((Some(id), ServerPacket::PlayerRegion(Some(target), id)));
            return;
        }
        if let Some(home) = home {
            self.send_play_event(home, GameEventKind::ClientLeft(id));
        }
        self.send_play_event(target, GameEventKind::CreateClientAt(id, landing));
        self.homes.insert(id, target);
        let _ = self.out.send((Some(id), ServerPacket::PlayerRegion(Some(target), id)));
        if let Some(home) = home {
            self.refresh_keepalive(home, now_ms);
        }
        self.refresh_keepalive(target, now_ms);
    }

    /// Where a player lands in `rc`: the world map tile's dry landing, else the society
    /// grid's, else the region's centre.
    fn landing_in(&self, rc: RegionCoords) -> [u32; 2] {
        let edge = crate::REGION_BLOCKS as u32;
        let on_map = self.world_map.as_ref().and_then(|m| m.tile(rc)).and_then(|t| t.landing);
        let on_grid = self.history.as_ref().and_then(|h| h.grid().landing(rc));
        on_map
            .or(on_grid)
            .filter(|l| l[0] < edge && l[1] < edge)
            .unwrap_or([edge / 2, edge / 2])
    }

    fn send_fortress(&self, id: ClientId) {
        let _ = self.out.send((Some(id), ServerPacket::Fortress(self.fortress_of.get(&id).copied())));
    }

    fn embark_result(&self, id: ClientId, r: Result<SiteId, EmbarkRefusal>) {
        let _ = self.out.send((Some(id), ServerPacket::EmbarkResult(r)));
    }

    /// The player's embark: one fortress per player; the history validates and founds
    /// the site at once, then the player moves in (the region's catch-up, or the
    /// founding itself when it already runs, spawns the party).
    fn embark(&mut self, id: ClientId, req: EmbarkRequest, now_ms: u64) {
        if self.fortress_of.contains_key(&id) && self.history.is_some() {
            return self.embark_result(id, Err(EmbarkRefusal::AlreadyHasFortress));
        }
        let Some(history) = self.history.as_mut() else {
            return self.embark_result(id, Err(EmbarkRefusal::NoHistory));
        };
        let site = match history.player_embark(&req) {
            Ok(site) => site,
            Err(e) => return self.embark_result(id, Err(e)),
        };
        let landing = self.landing_in(req.region);
        self.move_client(id, req.region, landing, now_ms);
        self.fortress_of.insert(id, site);
        self.embark_result(id, Ok(site));
        self.send_fortress(id);
        crate::perf_event!(
            "embark",
            "client={id} site={} people={} to={},{}",
            site.0,
            req.people.len(),
            req.region.x,
            req.region.z
        );
    }

    /// Retire: the player stops playing its fortress and leaves its home region, which
    /// parks by the usual grace rule (its people go back to the abstract history).
    fn retire(&mut self, id: ClientId, now_ms: u64) {
        self.fortress_of.remove(&id);
        if let Some(home) = self.homes.remove(&id) {
            self.send_play_event(home, GameEventKind::ClientLeft(id));
            self.refresh_keepalive(home, now_ms);
        }
        let _ = self.out.send((Some(id), ServerPacket::PlayerRegion(None, id)));
        if let Some(history) = &self.history {
            let _ = self.out.send((Some(id), ServerPacket::Sites(history.site_markers())));
        }
        self.send_fortress(id);
    }

    /// Return to a retired fortress.
    ///
    /// Player identity: there are no accounts and a `ClientId` is one connection, so
    /// "return only to a fortress you founded" is implemented as "only to a
    /// **player-founded** site no other *connected* player is playing" — the same in
    /// single player. A disconnected player's claim is dropped when someone returns.
    fn return_to(&mut self, id: ClientId, site: SiteId, now_ms: u64) {
        let Some(history) = &self.history else {
            return self.embark_result(id, Err(EmbarkRefusal::NoHistory));
        };
        if self.fortress_of.contains_key(&id) {
            return self.embark_result(id, Err(EmbarkRefusal::AlreadyHasFortress));
        }
        if !history.returnable(site) {
            return self.embark_result(id, Err(EmbarkRefusal::NotPlayerFounded));
        }
        let in_use = self
            .fortress_of
            .iter()
            .any(|(c, s)| *s == site && *c != id && self.sessions.contains_key(c));
        if in_use {
            return self.embark_result(id, Err(EmbarkRefusal::InUse));
        }
        let region = history.site(site).expect("returnable").region;
        self.fortress_of.retain(|_, s| *s != site);
        let landing = self.landing_in(region);
        self.move_client(id, region, landing, now_ms);
        self.fortress_of.insert(id, site);
        self.embark_result(id, Ok(site));
        self.send_fortress(id);
    }

    /// Send each notice the history made to the connected clients whose home is its region.
    fn route_notices(&mut self) {
        let Some(history) = self.history.as_mut() else { return };
        for (rc, line) in history.take_notices() {
            for c in self.sessions.keys().filter(|c| self.homes.get(c) == Some(&rc)) {
                let _ = self.out.send((Some(*c), ServerPacket::Notice(line.clone())));
            }
        }
    }

    /// At a calendar day's turn: the calendar to every connected client, and the sites
    /// if they changed (founded, abandoned, a population crossing a multiple of 10).
    fn broadcast_daily(&mut self, now: CalendarTick) {
        let Some(history) = &self.history else { return };
        let day = now.0 / TICKS_PER_DAY;
        if self.calendar_day == Some(day) {
            return;
        }
        self.calendar_day = Some(day);
        let key = sites_key(history);
        let sites = (key != self.sites_key).then(|| history.site_markers());
        self.sites_key = key;
        for c in self.sessions.keys() {
            let _ = self.out.send((Some(*c), ServerPacket::Calendar(now)));
            if let Some(sites) = &sites {
                let _ = self.out.send((Some(*c), ServerPacket::Sites(sites.clone())));
            }
        }
    }

    /// Deliver a server-authored Play event to `rc`, waking it if parked. A
    /// region mid-stop cannot take events (they would be dropped): queue them
    /// for when it parks.
    fn send_play_event(&mut self, rc: RegionCoords, kind: GameEventKind) {
        self.ensure_running(rc);
        let input = RegionInput::Event(kind);
        if self.regions.get(&rc).map_or(false, |l| l.stopping) {
            self.pending_events.entry(rc).or_default().push(input);
        } else {
            self.send_to_region(rc, input);
        }
    }

    /// Returns false when the server should quit (a Quit game event).
    pub fn handle_server_event(&mut self, ev: ServerEvent, now_ms: u64) -> bool {
        match ev {
            ServerEvent::ClientConnected(id) => {
                self.sessions.insert(id, Session::default());
                if !self.homes.contains_key(&id) {
                    // Server-authoritative player creation, once per client.
                    self.ensure_running(SPAWN_REGION);
                    self.homes.insert(id, SPAWN_REGION);
                    self.send_to_region(
                        SPAWN_REGION,
                        RegionInput::Event(GameEventKind::CreateClient(id)),
                    );
                } else {
                    // Reconnect: the player already exists in its home
                    // region (running or parked); nothing to create.
                    log::info!("client {id} reconnected");
                }
                if let Some(map) = &self.world_map {
                    let _ = self.out.send((Some(id), ServerPacket::WorldMap(map.clone())));
                }
                if let (Some(history), Some(now)) = (&self.history, self.calendar_now(now_ms)) {
                    let _ = self.out.send((Some(id), ServerPacket::Calendar(now)));
                    let _ = self.out.send((Some(id), ServerPacket::Sites(history.site_markers())));
                    self.send_fortress(id);
                }
            }
            ServerEvent::ClientDisconnected(id) => self.client_disconnected(id, now_ms),
            ServerEvent::ClientPacket(packet, id) => match packet {
                ClientPacket::RequestPlayerRegion => {
                    let home = self.homes.get(&id).copied();
                    let _ = self.out.send((Some(id), ServerPacket::PlayerRegion(home, id)));
                }
                ClientPacket::RequestRegionConnection(rc) => self.subscribe(id, rc),
                ClientPacket::ReleaseRegionConnection(rc) => self.unsubscribe(id, rc, now_ms),
                ClientPacket::RequestLod(key) => self.lod_subscribe(id, key, now_ms),
                ClientPacket::ReleaseLod(key) => self.lod_unsubscribe(id, key),
                ClientPacket::PlayRegion(rc) => self.play_region(id, rc, now_ms),
                ClientPacket::Leave => self.client_disconnected(id, now_ms),
                ClientPacket::RequestEmbarkOptions => {
                    let options = self.history.as_ref().map(History::embark_options).unwrap_or_default();
                    let _ = self.out.send((Some(id), ServerPacket::EmbarkOptions(options)));
                }
                ClientPacket::Embark(req) => self.embark(id, req, now_ms),
                ClientPacket::Retire => self.retire(id, now_ms),
                ClientPacket::ReturnTo(site) => self.return_to(id, site, now_ms),
                ClientPacket::RequestPersonInfo(person) => {
                    if let Some(info) = self.history.as_ref().and_then(|h| h.person_info(person)) {
                        let _ = self.out.send((Some(id), ServerPacket::PersonInfo(info)));
                    }
                }
                packet @ (ClientPacket::GameEvent(_) | ClientPacket::GameEventAfterTick(..)) => {
                    let (event, after_tick) = match packet {
                        ClientPacket::GameEventAfterTick(event, tick) => (event, Some(tick)),
                        ClientPacket::GameEvent(event) => (event, None),
                        _ => unreachable!(),
                    };
                    match event.kind {
                    GameEventKind::Tick => {}
                    GameEventKind::Quit => return false,
                    kind @ (GameEventKind::PlayerInput(..)
                    | GameEventKind::UiAction(..)
                    | GameEventKind::Order(..)) => {
                        // Manager-authoritative routing: the client's stamp
                        // lags its own predicted handoff; homes is truth.
                        // UiAction and Order ride the same path as
                        // PlayerInput — all must follow the player across a
                        // region handoff.
                        let cid = kind.origin_client().expect("PlayerInput/UiAction/Order carry a client");
                        let home = self.homes.get(&cid).copied();
                        match home {
                            Some(home) if self.regions.contains_key(&home) => {
                                // DEBUG: one line per player input drowned the
                                // server log (1618 of 1648 lines in a 60 s capture).
                                log::debug!(
                                    "[server] recv {kind:?} from cid={cid:?} -> route to home {home:?}"
                                );
                                // The stamp counts ticks of the region the
                                // client predicted into; after a flip the
                                // home it routes to counts its own.
                                let input = match after_tick {
                                    Some(tick) if event.region_id == home => {
                                        RegionInput::EventAfterTick(kind, tick)
                                    }
                                    _ => RegionInput::Event(kind),
                                };
                                self.send_to_region(home, input);
                            }
                            _ => log::info!(
                                "[server] DROP {kind:?} from cid={cid:?}: home={home:?} not running (loaded regions: {:?})",
                                self.regions.keys().collect::<Vec<_>>()
                            ),
                        }
                    }
                    // Manager-internal / server-authored event kinds: a client
                    // must never inject these directly (sessions are created
                    // and moved by the manager). Drop rather than forward.
                    GameEventKind::EntityArrived(_)
                    | GameEventKind::GhostUpdate(_)
                    | GameEventKind::CreateClient(_)
                    | GameEventKind::CreateClientAt(..)
                    | GameEventKind::SpawnPerson(_)
                    | GameEventKind::DespawnPerson(_)
                    | GameEventKind::WalkOut(..)
                    | GameEventKind::ClientLeft(_) => {
                        log::debug!(
                            "dropping internal event kind sent by client {id} for {:?}",
                            event.region_id
                        );
                    }
                    }
                }
            },
        }
        self.deliver_directives();
        self.route_notices();
        true
    }

    pub fn handle_region_output(&mut self, rc: RegionCoords, output: RegionOutput, now_ms: u64) {
        match output {
            RegionOutput::EventProcessed(event) => {
                if let Some(link) = self.regions.get(&rc) {
                    if let GameEventKind::PlayerInput(_, ev) = &event.kind {
                        log::debug!(
                            "[server] region {rc:?} processed PlayerInput id={} input={ev:?} -> echo to {} subscriber(s): {:?}",
                            event.id,
                            link.subscribers.len(),
                            link.subscribers
                        );
                    }
                    for client in &link.subscribers {
                        let _ = self
                            .out
                            .send((Some(*client), ServerPacket::GameEvent(event.clone())));
                    }
                }
            }
            RegionOutput::Snapshot(client, rollback) => {
                let _ = self.out.send((Some(client), ServerPacket::Region(rc, rollback)));
            }
            RegionOutput::SyncClock { tick_rate, tick } => {
                if let Some(link) = self.regions.get(&rc) {
                    for client in &link.subscribers {
                        let _ = self.out.send((
                            Some(*client),
                            ServerPacket::SyncClock(rc, tick_rate, tick, Duration::ZERO),
                        ));
                    }
                }
            }
            RegionOutput::Terrain(client, snap) => {
                self.cache_authoritative_terrain(rc, snap);
                self.lod_terrain_ready(rc, client);
            }
            RegionOutput::PeopleParked(people) => {
                if let Some(history) = self.history.as_mut() {
                    history.region_parked(rc, people);
                }
            }
            RegionOutput::TerrainChanged(snap) => {
                self.cache_authoritative_terrain(rc, snap);
                self.invalidate_region_lod(rc);
            }
            RegionOutput::Stopped(serialized, far) => {
                // `PeopleParked` comes first; a region that sent none gave nobody back.
                if self.history.as_ref().is_some_and(|h| h.region_live(rc)) {
                    self.history_region_lost(rc);
                }
                self.cache_authoritative_terrain(rc, far);
                self.invalidate_region_lod(rc);
                self.parked.insert(rc, serialized);
                let pending = self
                    .regions
                    .remove(&rc)
                    .map(|l| l.resubscribe_pending)
                    .unwrap_or_default();
                self.spawner.reap(rc);
                if let Some(events) = self.pending_events.remove(&rc) {
                    self.ensure_running(rc);
                    for input in events {
                        self.send_to_region(rc, input);
                    }
                    self.refresh_keepalive(rc, now_ms);
                }
                for client in pending {
                    // A client asked for this region while it was stopping:
                    // now that the state is parked, cycle it right back in.
                    self.subscribe(client, rc);
                }
            }
            RegionOutput::Departures(list) => {
                for (mut bundle, target) in list {
                    let client = bundle.client.as_ref().map(|(c, _)| *c);
                    if crate::off_map(self.map_extent(), target) {
                        // Regions hold bodies at the outer seam, so this is a
                        // region that was not bounded: refuse the crossing and
                        // hand the entity back to where it left, on its seam.
                        log::error!("departure from {rc:?} into off-map {target:?} refused: returned");
                        let t = &mut bundle.isometry.translation;
                        t.x = t.x.clamp(0.0, crate::REGION_SIZE);
                        t.z = t.z.clamp(0.0, crate::REGION_SIZE);
                        self.send_to_region(rc, RegionInput::Event(GameEventKind::EntityArrived(bundle)));
                        continue;
                    }
                    if let (EntityKind::Person { person }, Some(history)) = (bundle.kind, self.history.as_mut()) {
                        // A person is delivered only into a running region; the history
                        // absorbs anyone walking into one that is not (no wake).
                        if !history.person_crossed(person, target) {
                            continue;
                        }
                    }
                    bundle.isometry = crate::rebase_isometry(&bundle.isometry, rc, target);
                    // Arrivals ALWAYS wake the target (parked blob or gen).
                    self.ensure_running(target);
                    self.send_to_region(
                        target,
                        RegionInput::Event(GameEventKind::EntityArrived(bundle)),
                    );
                    if let Some(c) = client {
                        self.homes.insert(c, target);
                        let _ = self
                            .out
                            .send((Some(c), ServerPacket::PlayerRegion(Some(target), c)));
                        // The old home may have just lost its keep-alive
                        // reason; the new one just gained it.
                        self.refresh_keepalive(rc, now_ms);
                        self.refresh_keepalive(target, now_ms);
                    }
                }
            }
            RegionOutput::GhostUpdates(list) => {
                for (mut data, target) in list {
                    // Ghost updates NEVER wake a region.
                    let running = self
                        .regions
                        .get(&target)
                        .map_or(false, |link| !link.stopping);
                    if !running {
                        continue;
                    }
                    data.isometry = crate::rebase_isometry(&data.isometry, rc, target);
                    self.send_to_region(target, RegionInput::Event(GameEventKind::GhostUpdate(data)));
                }
            }
        }
        // After the arrivals above: a directive for a delivered person (walk on) must
        // reach the target behind the body.
        self.deliver_directives();
    }

    /// The history's calendar moves to `now_ms` (its decisions go to running regions),
    /// then grace-period unloads: a region with no subscribers and no connected
    /// client's player entity parks after UNLOAD_GRACE_MS.
    pub fn maintain(&mut self, now_ms: u64) {
        if let Some(to) = self.calendar_now(now_ms) {
            self.history.as_mut().unwrap().advance(to);
            self.deliver_directives();
            self.broadcast_daily(to);
            self.route_notices();
        }
        let connected_homes: BTreeSet<RegionCoords> = self
            .sessions
            .keys()
            .filter_map(|c| self.homes.get(c).copied())
            .collect();
        let expired: Vec<RegionCoords> = self
            .regions
            .iter()
            .filter(|(rc, link)| {
                !link.stopping
                    && link.subscribers.is_empty()
                    && !connected_homes.contains(*rc)
                    && link
                        .empty_since_ms
                        .map_or(false, |t| now_ms.saturating_sub(t) >= UNLOAD_GRACE_MS)
            })
            .map(|(rc, _)| *rc)
            .collect();
        for rc in expired {
            let link = self.regions.get_mut(&rc).unwrap();
            link.stopping = true;
            if link.input.send(RegionInput::Shutdown).is_err() {
                // Already dead: nothing to park (state lost), just clean up.
                log::error!("region {:?} died before shutdown", rc);
                self.regions.remove(&rc);
                self.pending_events.remove(&rc);
                self.spawner.reap(rc);
                self.history_region_lost(rc);
            } else if let Some(history) = self.history.as_mut() {
                history.region_stopping(rc);
            }
        }
        // Anything a lost region's people decided on their return.
        self.deliver_directives();
    }

    pub fn shutdown_all(&mut self) {
        let mut dead = Vec::new();
        for (rc, link) in self.regions.iter_mut() {
            if !link.stopping {
                link.stopping = true;
                if link.input.send(RegionInput::Shutdown).is_err() {
                    log::error!("region {:?} died before shutdown", rc);
                    dead.push(*rc);
                } else if let Some(history) = self.history.as_mut() {
                    history.region_stopping(*rc);
                }
            }
        }
        for rc in dead {
            self.history_region_lost(rc);
        }
    }

    pub fn running_regions(&self) -> Vec<RegionCoords> {
        self.regions.keys().copied().collect()
    }

    pub fn parked_regions(&self) -> Vec<RegionCoords> {
        self.parked.keys().copied().collect()
    }

    pub fn spawner_mut(&mut self) -> &mut S {
        &mut self.spawner
    }

    /// Counters accumulated since the previous call; resets them.
    pub fn take_perf_counters(&mut self) -> ManagerPerfCounters {
        std::mem::take(&mut self.perf)
    }

    /// Drop a client's session and subscriptions. Idempotent: a second call
    /// (e.g. `Leave` then the transport closing) finds no session.
    fn client_disconnected(&mut self, id: ClientId, now_ms: u64) {
        if let Some(session) = self.sessions.remove(&id) {
            for rc in session.subscribed {
                self.unsubscribe_link(id, rc, now_ms);
            }
            for key in session.lod_subscribed {
                if let Some(set) = self.lod_subscribers.get_mut(&key) {
                    set.remove(&id);
                    if set.is_empty() {
                        self.lod_subscribers.remove(&key);
                    }
                }
            }
        }
        // The home region may have just lost its keep-alive reason.
        if let Some(home) = self.homes.get(&id).copied() {
            self.refresh_keepalive(home, now_ms);
        }
    }

    pub fn depths(&self) -> ManagerDepths {
        ManagerDepths {
            sessions: self.sessions.len(),
            running: self.regions.len(),
            parked: self.parked.len(),
            lod_in_flight: self.lod_in_flight.len(),
            lod_waiting_terrain: self.lod_pending_terrain.values().map(BTreeSet::len).sum::<usize>()
                + self.lod_tile_waiting.len(),
            lod_cache: self.lod_cache.len(),
            terrain_cache: self.terrain_cache.len(),
        }
    }

    /// The injected generator, timed. It runs synchronously on the manager
    /// thread, so its cost is routing latency for every connected client.
    fn generate_timed(&mut self, rc: RegionCoords) -> Vec<(ColumnCoords, Column)> {
        let watch = crate::perf::Stopwatch::start();
        let columns = (self.generator)(rc);
        let took = watch.elapsed();
        self.perf.generated += 1;
        self.perf.generate_total += took;
        self.perf.generate_max = self.perf.generate_max.max(took);
        columns
    }

    fn mark_lod_in_flight(&mut self, key: LodKey) {
        self.lod_in_flight.insert(key);
        self.lod_submitted_at.insert(key, crate::perf::Stopwatch::start());
        self.perf.lod_submitted += 1;
    }

    fn ensure_running(&mut self, rc: RegionCoords) {
        if self.regions.contains_key(&rc) {
            return;
        }
        let seed = if self.spawner.generates_columns() {
            // Worldgen happens on the region's thread (and not at all for a
            // parked region with a healthy blob).
            RegionSeed::Deferred(self.parked.remove(&rc))
        } else {
            let watch = crate::perf::Stopwatch::start();
            let columns = self.generate_timed(rc); // now Vec<(ColumnCoords, Column)>
            let parked = self.parked.contains_key(&rc);
            crate::perf_event!(
                "region_generate",
                "region={},{} ms={} columns={} parked={parked}",
                rc.x,
                rc.z,
                crate::perf::ms(watch.elapsed()),
                columns.len()
            );
            match self.parked.remove(&rc) {
                Some(serialized) => RegionSeed::Parked(serialized, columns),
                None => RegionSeed::Fresh(columns),
            }
        };
        let input = self.spawner.spawn(rc, seed, self.region_out_send.clone());
        // First in the region's queue, so it is bounded before its first tick.
        if let Some(extent) = self.map_extent() {
            let _ = input.send(RegionInput::SetMapExtent(Some(extent)));
        }
        // Then the history's catch-up: its people here now, before anything else.
        if let Some(history) = self.history.as_mut() {
            for kind in history.region_started(rc) {
                let _ = input.send(RegionInput::Event(kind));
            }
        }
        self.regions.insert(
            rc,
            RegionLink {
                input,
                subscribers: BTreeSet::new(),
                empty_since_ms: None,
                stopping: false,
                resubscribe_pending: Vec::new(),
            },
        );
    }

    fn subscribe(&mut self, client: ClientId, rc: RegionCoords) {
        // Invariant: a client appears in `link.subscribers` (or in
        // `resubscribe_pending`) only while it has a live session whose
        // `subscribed` set contains `rc`. Sessionless subscribes — ghost
        // packets, or a Stopped-replay for a client that disconnected during
        // the stopping window — are dropped here; otherwise they'd pin the
        // region forever with a subscriber no disconnect can remove.
        let Some(session) = self.sessions.get_mut(&client) else {
            log::debug!(
                "dropping subscribe from sessionless client {client} for {:?}",
                rc
            );
            return;
        };
        session.subscribed.insert(rc);
        self.ensure_running(rc);
        let link = self.regions.get_mut(&rc).unwrap();
        if link.stopping {
            // Recorded in session.subscribed above, so a disconnect (or
            // release) before Stopped arrives still reaches
            // unsubscribe_link and clears the pending intent.
            link.resubscribe_pending.push(client);
            return;
        }
        link.subscribers.insert(client);
        link.empty_since_ms = None;
        self.send_to_region(rc, RegionInput::RequestSnapshot(client));
    }

    fn unsubscribe(&mut self, client: ClientId, rc: RegionCoords, now_ms: u64) {
        if let Some(session) = self.sessions.get_mut(&client) {
            session.subscribed.remove(&rc);
        }
        self.unsubscribe_link(client, rc, now_ms);
    }

    fn unsubscribe_link(&mut self, client: ClientId, rc: RegionCoords, now_ms: u64) {
        let Some(link) = self.regions.get_mut(&rc) else { return };
        link.subscribers.remove(&client);
        // The client may only exist as a pending intent (subscribed during
        // the stopping window); clear that too so a Stopped-replay can't
        // resurrect the subscription.
        link.resubscribe_pending.retain(|c| c != &client);
        self.refresh_keepalive(rc, now_ms);
    }

    /// Re-evaluate whether `rc` still has a keep-alive reason; start the
    /// grace timer if not.
    fn refresh_keepalive(&mut self, rc: RegionCoords, now_ms: u64) {
        let is_home = self
            .sessions
            .keys()
            .any(|c| self.homes.get(c) == Some(&rc));
        let Some(link) = self.regions.get_mut(&rc) else { return };
        if link.subscribers.is_empty() && !is_home {
            link.empty_since_ms.get_or_insert(now_ms);
        } else {
            link.empty_since_ms = None;
        }
    }

    /// Route an input to a running region; a failed send means the region
    /// thread died — respawn it (parked state if any, else regenerated) and
    /// resnapshot every subscriber. The failed input itself is dropped; the
    /// snapshot resync covers the gap. Returns whether the input reached the
    /// region (or its stop queue) — `false` when it is not running or was
    /// just lost and respawned.
    ///
    /// A region that is stopping is never sent anything nor respawned: its
    /// thread may already have exited after `Shutdown`, and its
    /// `PeopleParked` / `Stopped` are still on their way — a respawn now
    /// would be parked by them. Its events queue in `pending_events` and
    /// replay once it has parked (`Stopped` wakes it); its other inputs
    /// (snapshot / terrain requests, the map extent, client key input) are
    /// dropped — the restart re-sends what it needs.
    fn send_to_region(&mut self, rc: RegionCoords, input: RegionInput) -> bool {
        let Some(link) = self.regions.get(&rc) else {
            log::debug!("send_to_region: {:?} not running", rc);
            return false;
        };
        if link.stopping {
            match input {
                RegionInput::Event(GameEventKind::PlayerInput(..)) => {
                    log::debug!("send_to_region: {rc:?} is stopping; player input dropped");
                }
                RegionInput::Event(kind) => {
                    self.pending_events.entry(rc).or_default().push(RegionInput::Event(kind));
                }
                other => log::debug!("send_to_region: {rc:?} is stopping; {other:?} dropped"),
            }
            return true;
        }
        if link.input.send(input).is_ok() {
            return true;
        }
        log::error!("region {:?} thread died; respawning", rc);
        let subscribers = self.regions.remove(&rc).unwrap().subscribers;
        self.pending_events.remove(&rc);
        self.spawner.reap(rc);
        // Its people's bodies died with it: back to the history, then caught up again.
        self.history_region_lost(rc);
        self.ensure_running(rc);
        let link = self.regions.get_mut(&rc).unwrap();
        link.subscribers = subscribers.clone();
        for client in subscribers {
            let _ = link.input.send(RegionInput::RequestSnapshot(client));
        }
        false
    }

    fn lod_subscribe(&mut self, client: ClientId, key: LodKey, _now_ms: u64) {
        if !crate::lod::LOD_FACTORS.contains(&key.factor) {
            log::debug!("dropping RequestLod with invalid factor {}", key.factor);
            return;
        }
        let Some(session) = self.sessions.get_mut(&client) else {
            log::debug!("dropping RequestLod from sessionless client {client}");
            return;
        };
        session.lod_subscribed.insert(key);
        self.lod_subscribers.entry(key).or_default().insert(client);
        self.lod_access_counter += 1;
        let stamp = self.lod_access_counter;
        if let Some((payload, access)) = self.lod_cache.get_mut(&key) {
            *access = stamp;
            self.perf.lod_cache_hits += 1;
            let _ = self
                .out
                .send((Some(client), ServerPacket::LodData(key, payload.clone())));
            return;
        }
        if !self.lod_in_flight.contains(&key) {
            self.start_lod_build(key, client);
        }
        // else: result fans out to all subscribers when it lands.
    }

    fn lod_unsubscribe(&mut self, client: ClientId, key: LodKey) {
        if let Some(session) = self.sessions.get_mut(&client) {
            session.lod_subscribed.remove(&key);
        }
        if let Some(set) = self.lod_subscribers.get_mut(&key) {
            set.remove(&client);
            if set.is_empty() {
                self.lod_subscribers.remove(&key);
            }
        }
    }

    /// Kick off a build for `key`. Sub-block factors: authoritative terrain if
    /// the region has state (running-and-seeded or parked-with-cache), request
    /// terrain if running-but-unseeded, generate otherwise. Block-scale and
    /// coarser factors coarse-sample instead (`start_tile_build`). `client` is
    /// the requester used for terrain round-trips.
    fn start_lod_build(&mut self, key: LodKey, client: ClientId) {
        if key.factor < crate::lod::COARSE_MIN_FACTOR {
            debug_assert_eq!(key.region_span(), 1, "sub-block factors key per region");
            let rc = key.origin_region();
            let running = self.regions.get(&rc).map_or(false, |l| !l.stopping);
            if let Some(snap) = self.terrain_cache.get(&rc) {
                if let Some(stamp) = self.pristine_terrain.get_mut(&rc) {
                    self.lod_access_counter += 1;
                    *stamp = self.lod_access_counter;
                }
                let chunks = snap.chunks.clone();
                self.mark_lod_in_flight(key);
                self.lod_builder.submit(LodBuildJob::Region { key, chunks });
            } else if running {
                // Terrain reply (RegionOutput::Terrain) seeds terrain_cache
                // and re-enters via lod_terrain_ready.
                self.lod_pending_terrain.entry(rc).or_default().insert(key);
                self.send_to_region(rc, RegionInput::RequestTerrain(client));
            } else if self.lod_builder.generates_pristine() {
                // Never-run region: the builder's own threads generate it.
                // Nothing full-resolution is kept here — the LOD payload is
                // what gets cached (`lod_cache`), and a region that later
                // runs or parks reports authoritative terrain itself.
                self.mark_lod_in_flight(key);
                self.lod_builder.submit(LodBuildJob::Pristine { key, region: rc });
            } else {
                let columns = self.generate_timed(rc);
                let snap = TerrainSnapshot::from_columns(columns);
                self.terrain_cache.insert(rc, snap.clone());
                self.lod_access_counter += 1;
                self.pristine_terrain.insert(rc, self.lod_access_counter);
                self.evict_pristine_terrain();
                self.mark_lod_in_flight(key);
                self.lod_builder
                    .submit(LodBuildJob::Region { key, chunks: snap.chunks });
            }
        } else {
            self.start_tile_build(key, client);
        }
    }

    /// Coarse keys (`COARSE_MIN_FACTOR` and up, region- or tile-keyed): coarse
    /// base + one overlay per region with
    /// authoritative state. Regions running-but-unseeded are requested and
    /// the build deferred until every such reply has landed; pristine and
    /// parked-with-cache regions need nothing. Pristine regions are NOT
    /// generated here — coarse sampling covers them (the point of tiles).
    ///
    /// Caveat: `terrain_cache` reflects the LAST pushed/parked terrain of a
    /// running region — mid-cadence edits appear on the next
    /// `TerrainChanged` (invalidation + rebuild), matching the spec's
    /// freshness decision.
    fn start_tile_build(&mut self, key: LodKey, client: ClientId) {
        let mut waiting = BTreeSet::new();
        for rc in key.regions() {
            let running = self.regions.get(&rc).map_or(false, |l| !l.stopping);
            if running && !self.terrain_cache.contains_key(&rc) {
                waiting.insert(rc);
                self.lod_pending_terrain.entry(rc).or_default().insert(key);
                self.send_to_region(rc, RegionInput::RequestTerrain(client));
            }
        }
        if !waiting.is_empty() {
            self.lod_tile_waiting.insert(key, waiting);
            return;
        }
        let overlays: Vec<(RegionCoords, Vec<(ChunkCoords, Chunk)>)> = key
            .regions()
            .into_iter()
            .filter_map(|rc| self.terrain_cache.get(&rc).map(|snap| (rc, snap.chunks.clone())))
            .collect();
        self.mark_lod_in_flight(key);
        self.lod_builder.submit(LodBuildJob::Tile { key, overlays });
    }

    /// A region's terrain just landed in terrain_cache: submit any LOD builds
    /// that were waiting on it, region- or tile-keyed.
    fn lod_terrain_ready(&mut self, rc: RegionCoords, client: ClientId) {
        let Some(keys) = self.lod_pending_terrain.remove(&rc) else { return };
        for key in keys {
            if let Some(waiting) = self.lod_tile_waiting.get_mut(&key) {
                waiting.remove(&rc);
                if !waiting.is_empty() {
                    continue;
                }
                self.lod_tile_waiting.remove(&key);
            }
            if !self.lod_in_flight.contains(&key) {
                self.start_lod_build(key, client);
            }
        }
    }

    pub fn handle_lod_result(&mut self, key: LodKey, payload: LodPayload) {
        self.lod_in_flight.remove(&key);
        self.perf.lod_built += 1;
        if let Some(watch) = self.lod_submitted_at.remove(&key) {
            let took = watch.elapsed();
            self.perf.lod_latency_total += took;
            self.perf.lod_latency_max = self.perf.lod_latency_max.max(took);
        }
        self.lod_access_counter += 1;
        self.lod_cache.insert(key, (payload.clone(), self.lod_access_counter));
        self.evict_lod_cache();
        if let Some(subs) = self.lod_subscribers.get(&key) {
            for client in subs {
                let _ = self
                    .out
                    .send((Some(*client), ServerPacket::LodData(key, payload.clone())));
            }
        }
    }

    /// Drain the builder results channel (Inline path; the native server
    /// selects on the receiver instead and calls handle_lod_result directly).
    pub fn pump_lod_results(&mut self) {
        while let Ok((key, payload)) = self.lod_results.try_recv() {
            self.handle_lod_result(key, payload);
        }
    }

    /// A region's terrain changed (cadence push) or it parked: every LOD key
    /// depending on it is stale. Drop them from the cache; rebuild the ones
    /// with live subscribers (the fresh terrain_cache entry is the new source).
    fn invalidate_region_lod(&mut self, rc: RegionCoords) {
        for factor in crate::lod::LOD_FACTORS {
            let key = LodKey::for_region(factor, rc);
            self.lod_cache.remove(&key);
            let has_subs = self.lod_subscribers.get(&key).is_some_and(|s| !s.is_empty());
            if has_subs && !self.lod_in_flight.contains(&key) {
                // Any subscriber works as the terrain-request correlator.
                let client = *self.lod_subscribers[&key].iter().next().unwrap();
                self.start_lod_build(key, client);
            }
        }
    }

    /// Test hook: current cache size (`lod_cache_evicts_unsubscribed_lru_but_pins_subscribed`).
    pub fn lod_cache_len(&self) -> usize {
        self.lod_cache.len()
    }

    /// Test hook: shrink/grow the LRU cap below `LOD_CACHE_MAX_ENTRIES` so
    /// eviction is exercisable without filling thousands of real entries.
    pub fn set_lod_cache_max(&mut self, n: usize) {
        self.lod_cache_max = n;
        self.evict_lod_cache();
    }

    /// A region reported its own terrain: the entry is now authoritative and
    /// must never be evicted as pristine worldgen output.
    fn cache_authoritative_terrain(&mut self, rc: RegionCoords, snap: TerrainSnapshot) {
        self.pristine_terrain.remove(&rc);
        self.terrain_cache.insert(rc, snap);
    }

    /// Test hook: total `terrain_cache` entries (pristine + authoritative).
    pub fn terrain_cache_len(&self) -> usize {
        self.terrain_cache.len()
    }

    /// Test hook: shrink the pristine cap so eviction is exercisable without
    /// generating dozens of regions.
    pub fn set_pristine_terrain_max(&mut self, n: usize) {
        self.pristine_terrain_max = n;
        self.evict_pristine_terrain();
    }

    /// Drop least-recently-used pristine snapshots past the cap. In-flight
    /// builds own a clone of the chunks, so eviction never races a job.
    fn evict_pristine_terrain(&mut self) {
        while self.pristine_terrain.len() > self.pristine_terrain_max {
            let Some(victim) = self
                .pristine_terrain
                .iter()
                .min_by_key(|(_, stamp)| **stamp)
                .map(|(rc, _)| *rc)
            else {
                break;
            };
            self.pristine_terrain.remove(&victim);
            self.terrain_cache.remove(&victim);
        }
    }

    fn evict_lod_cache(&mut self) {
        while self.lod_cache.len() > self.lod_cache_max {
            let victim = self
                .lod_cache
                .iter()
                .filter(|(k, _)| !self.lod_subscribers.contains_key(k))
                .min_by_key(|(_, (_, access))| *access)
                .map(|(k, _)| *k);
            match victim {
                Some(k) => {
                    self.lod_cache.remove(&k);
                }
                None => break, // everything pinned by subscribers
            }
        }
    }
}
