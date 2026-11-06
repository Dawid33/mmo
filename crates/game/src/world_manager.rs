//! World-level management: which regions run, who is subscribed to what,
//! where players' home regions are, and the parking lot for cycled-out
//! regions. Threadless: a `RegionSpawner` decides how regions actually run
//! (OS threads on the server, inline for wasm/tests), and time arrives as
//! an explicit `now_ms` so the core stays deterministic and wasm-safe.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crossbeam::channel::{unbounded, Receiver, Sender};

use crate::history::{AdventureRefusal, CalendarTick, EmbarkRefusal, History, SiteId, TICKS_PER_DAY};
use crate::{
    Chunk, ChunkCoords, ClientId, ClientPacket, Column, ColumnCoords, EntityKind, TerrainSnapshot, GameEventKind,
    LodBuildJob, LodBuilder, LodKey, LodPayload, LodResult, Region, RegionCoords, RegionInput,
    RegionOutput, RegionRunner, RegionSave, RegionSeed, SerializedRegion, ServerPacket, PlayRefusal, WorldMap, EmbarkRequest,
    Order, PlayMode, PlayerId, PlayerRecord,
};

/// Level hint for an adventure landing: above any surface, so it clamps to the
/// column's top and the nearest standable tile is the topmost.
const ADVENTURE_LANDING_HINT_Y: i32 = 1 << 20;
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
    /// A session started: the network edge has resolved the connection's
    /// `Hello` token to `player`, with the record the store keeps for it
    /// (`None` for a new player, or one never parked).
    ClientConnected {
        client: ClientId,
        player: crate::PlayerId,
        record: Option<crate::PlayerRecord>,
    },
    ClientDisconnected(ClientId),
}

impl ServerEvent {
    /// A session for an in-process harness or test: the player id is the
    /// client id and there is no saved record.
    pub fn connected(client: ClientId) -> Self {
        Self::ClientConnected { client, player: crate::PlayerId(client as u64), record: None }
    }
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

    /// The world's current tick: where a region spawned now starts counting.
    fn world_tick(&self) -> crate::Tick;
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
///   `region_thread_loop` falls behind its deadline, and runs a catch-up tick
///   next round so it stays within one tick of the round counter.
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
    /// Region states saved by an earlier process (a restarted server's
    /// store): a region that has not run in this one starts from its save,
    /// through [`crate::restore_saved`], as `ThreadRegionSpawner::with_store`
    /// does on the server.
    saved: BTreeMap<RegionCoords, crate::SavedRegion>,
    /// Rounds advanced by `tick_all`: the world tick of this lockstep world.
    rounds: crate::Tick,
    /// Test support ([`InlineSpawner::hold`]): regions that sit out the next
    /// `n` calls of `tick_all`, neither ticking nor draining their inputs.
    held: BTreeMap<RegionCoords, u32>,
    /// Test support ([`InlineSpawner::record_slot_start_hashes`]).
    record_slot_hashes: bool,
}

impl InlineSpawner {
    /// Test support: every runner, now and spawned later, records its
    /// slot-start hashes ([`RegionRunner::record_slot_start_hashes`]).
    pub fn record_slot_start_hashes(&mut self) {
        self.record_slot_hashes = true;
        for (_, runner) in self.runners.values_mut() {
            runner.record_slot_start_hashes();
        }
    }

    /// Every runner's slot-start hashes since the last call.
    pub fn take_slot_start_hashes(&mut self) -> Vec<(RegionCoords, crate::Tick, u32)> {
        let mut all = Vec::new();
        for (id, (_, runner)) in self.runners.iter_mut() {
            all.extend(runner.take_slot_start_hashes().into_iter().map(|(t, h)| (*id, t, h)));
        }
        all
    }

    pub fn with_schedule(schedule: InlineSchedule) -> Self {
        Self { schedule: Some(schedule), ..Self::default() }
    }

    /// Regions that have not run yet start from `saved` (a restarted server).
    pub fn with_saved(saved: BTreeMap<RegionCoords, crate::Rollback>) -> Self {
        let saved = saved
            .into_iter()
            .map(|(rc, state)| (rc, crate::SavedRegion { state, transfers: Vec::new() }))
            .collect();
        Self::with_saved_regions(saved)
    }

    /// [`Self::with_saved`], with the crossings saved alongside each state.
    pub fn with_saved_regions(saved: BTreeMap<RegionCoords, crate::SavedRegion>) -> Self {
        Self { saved, ..Self::default() }
    }

    /// Swap the schedule on a live spawner (`None` = back to lockstep), for
    /// harnesses that construct their server before choosing one.
    pub fn set_schedule(&mut self, schedule: Option<InlineSchedule>) {
        self.schedule = schedule;
        self.late.clear();
    }

    /// Test support: `rc` sits out the next `rounds` calls of `tick_all` —
    /// no tick, and `pump` leaves its inputs queued — the way a region thread
    /// that fell behind looks to its neighbours. Afterwards it catches up
    /// like any skipped region (at most [`crate::MAX_CATCH_UP_BURST`] ticks a
    /// round while behind the round counter). `rounds == 0` releases it now.
    pub fn hold(&mut self, rc: RegionCoords, rounds: u32) {
        if rounds > 0 {
            self.held.insert(rc, rounds);
        } else {
            self.held.remove(&rc);
        }
    }

    pub fn pump(&mut self) {
        let ids: Vec<RegionCoords> = self
            .runners
            .keys()
            .copied()
            .filter(|id| !self.late.contains(id) && !self.held.contains_key(id))
            .collect();
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

    /// One tick, plus catch-up ticks (at most [`crate::MAX_CATCH_UP_BURST`]
    /// in all) while the runner is behind `world`: a skipped round is made up
    /// next round instead of leaving the region behind for good.
    fn tick_to(runner: &mut RegionRunner, world: crate::Tick) {
        runner.tick();
        let mut burst = 1;
        while runner.current_tick() < world && burst < crate::MAX_CATCH_UP_BURST {
            runner.tick();
            burst += 1;
        }
    }

    pub fn tick_all(&mut self) {
        self.rounds += 1;
        // Held regions sit this round out; the last held round releases them
        // for the next `pump`.
        let held: BTreeSet<RegionCoords> = self.held.keys().copied().collect();
        self.held.retain(|_, n| {
            *n -= 1;
            *n > 0
        });
        let Some(schedule) = self.schedule.as_mut() else {
            let world = self.rounds;
            for (id, (_, runner)) in self.runners.iter_mut() {
                if !held.contains(id) {
                    Self::tick_to(runner, world);
                }
            }
            return;
        };
        let mut ids: Vec<RegionCoords> =
            self.runners.keys().copied().filter(|id| !held.contains(id)).collect();
        schedule.shuffle(&mut ids);
        // Last round's late regions tick here without having seen their
        // inputs, then catch up at the next `pump()`.
        self.late.clear();
        for id in ids {
            if !schedule.roll(schedule.skip_per_mille) {
                if let Some((_, runner)) = self.runners.get_mut(&id) {
                    Self::tick_to(runner, self.rounds);
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
        // A respawned region is a new thread: not held, not late.
        self.held.remove(&id);
        self.late.remove(&id);
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
        let never_ran = matches!(seed, RegionSeed::Fresh(_) | RegionSeed::Deferred(None));
        let runner = match self.saved.remove(&id).filter(|_| never_ran) {
            Some(saved) => {
                let mut runner = RegionRunner::new(id, crate::restore_saved(id, saved.state), out);
                runner.requeue_saved(saved.transfers);
                runner
            }
            None => seed.into_runner(
                id,
                out,
                &|rc| unreachable!("InlineSpawner never generates columns ({rc:?})"),
            ),
        };
        let mut runner = runner;
        if self.record_slot_hashes {
            runner.record_slot_start_hashes();
        }
        self.runners.insert(id, (recv, runner));
        send
    }

    fn world_tick(&self) -> crate::Tick {
        self.rounds
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
    /// Crossings routed but not yet confirmed ([`WorldManager::in_flight_count`]).
    pub transfers_in_flight: usize,
}

/// What a session is told when a new session of its player takes over
/// (`ServerPacket::Refused`); the server edge then closes it.
pub const LOGGED_IN_ELSEWHERE: &str = "logged in elsewhere";

/// A park in flight (`WorldManager::parking`).
#[derive(Clone, Copy, Debug)]
struct Parking {
    player: PlayerId,
    /// Where the body is: its home at the disconnect, or where a seam crossing
    /// in flight took it.
    region: RegionCoords,
    fortress: Option<SiteId>,
    adventure: Option<SiteId>,
}

/// A move in flight (`WorldManager::moving`): the body is being parked out of
/// `from` (`ParkPlayer`), and its `Client` — inventory, cursor, mode — is then
/// created at `landing` in `target` (`CreateClientAt` carrying it), followed by
/// `then`.
#[derive(Clone, Debug)]
struct Moving {
    /// Where the body is: the home it left, or where a seam crossing in flight
    /// took it.
    from: RegionCoords,
    target: RegionCoords,
    landing: [u32; 2],
    /// Server-authored events for `target` that must follow the body's
    /// creation there (an adventure start's `SetMode`).
    then: Vec<GameEventKind>,
}

/// Requests that place or move the player, held while its restore waits for its
/// park (they would act on a session with no body yet).
fn defers_until_restored(packet: &ClientPacket) -> bool {
    matches!(
        packet,
        ClientPacket::RequestPlayerRegion
            | ClientPacket::PlayRegion(_)
            | ClientPacket::Embark(_)
            | ClientPacket::Retire
            | ClientPacket::ReturnTo(_)
            | ClientPacket::StartAdventure(_)
            | ClientPacket::EndAdventure
    )
}

/// A crossing the manager has routed to its target but not yet seen applied
/// there (`WorldManager::in_flight`).
#[derive(Debug, Clone)]
struct InFlight {
    source: RegionCoords,
    target: RegionCoords,
    stamp: crate::Tick,
    /// In the target's frame.
    bundle: crate::EntityBundle,
    /// The autosave round ([`WorldManager::request_save`]) it departed in, and
    /// whether its source had already answered that round's `Save` — then the
    /// source's saved state still holds it.
    save_round: u64,
    in_source_save: bool,
    /// The autosave round it was last saved with, as a transfer.
    saved_in: Option<u64>,
}

impl InFlight {
    /// Whether this round's save of the SOURCE must carry this crossing
    /// (given it is in no save yet): its target's save of the round will not
    /// hold it, because the target is not (or no longer) asked —
    /// - it departed this round and the target answered before that, or the
    ///   target was never asked (stopping);
    /// - it departed in an earlier round and the target was never asked this
    ///   round: a stopping target, woken with the body only after this round
    ///   began. (Had the target answered this round, it was in flight then
    ///   and went with that save.)
    fn source_saves(&self, round: u64, asked: &BTreeSet<RegionCoords>, answered: &BTreeSet<RegionCoords>) -> bool {
        self.source != self.target
            && !asked.contains(&self.target)
            && (self.save_round == round || !answered.contains(&self.target))
    }
}

pub struct WorldManager<S: RegionSpawner> {
    spawner: S,
    generator: RegionGenerator,
    regions: BTreeMap<RegionCoords, RegionLink>,
    parked: BTreeMap<RegionCoords, SerializedRegion>,
    /// Parked since the last [`Self::request_save`]: their blob is not saved yet.
    unsaved_parked: BTreeSet<RegionCoords>,
    /// World autosave states waiting for [`Self::take_saves`].
    saves: Vec<(RegionCoords, RegionSave)>,
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
    /// The fortress each connected client plays (embarked or returned to, not
    /// retired). On disconnect it moves into the player's record (`parking`, then
    /// `parked_players` and the player store), and a reconnect of the same player
    /// restores it unless someone else has returned to that fortress meanwhile.
    fortress_of: BTreeMap<ClientId, SiteId>,
    /// The site each connected client started its adventure at (Adventure play
    /// mode). A client holds a fortress or an adventure, never both. Kept in the
    /// player's record like `fortress_of`.
    adventurer_of: BTreeMap<ClientId, SiteId>,
    /// The stable player behind each connected client, as the network edge
    /// resolved it from the connection's `Hello` token. Dropped on disconnect.
    /// A player is online at most once: a second session takes over.
    player_of: BTreeMap<ClientId, PlayerId>,
    /// Disconnected clients whose `ParkPlayer` is on its way: the player, the
    /// region the body is in (followed across a seam crossing in flight) and the
    /// claims it held. Settled by `RegionOutput::PlayerParked`.
    parking: BTreeMap<ClientId, Parking>,
    /// Players moving between regions (Play, embark, adventure start / end)
    /// whose park out of the old region is on its way; settled by
    /// `RegionOutput::PlayerParked` like a park, then created in the target.
    moving: BTreeMap<ClientId, Moving>,
    /// Records of players parked by this process: authoritative over the record
    /// the network edge read from the store (whose write may lag a fast
    /// reconnect). Taken when the player is restored.
    parked_players: BTreeMap<PlayerId, PlayerRecord>,
    /// A player that reconnected while its park was still in flight: its new
    /// session and the edge's record (the fallback should the park bring none
    /// back). Restored when `PlayerParked` arrives.
    awaiting_park: BTreeMap<PlayerId, (ClientId, Option<PlayerRecord>)>,
    /// Requests that move the player (Play, embark, …) from a session whose
    /// restore waits for its park: replayed once it is restored.
    deferred_packets: BTreeMap<ClientId, Vec<ClientPacket>>,
    /// Player records for the player store, collected since the last
    /// [`Self::take_player_saves`]: parks and autosave extracts, oldest first.
    player_saves: Vec<(PlayerId, PlayerRecord)>,
    /// The `now_ms` of the latest call in (server event, region output,
    /// maintenance), for paths that learn of a lost region without one.
    clock_ms: u64,
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
    /// Regions handed arrivals in the `Departures` being handled, with their
    /// stamp: a history directive for one (a delivered person's walk-on)
    /// goes as a `Transfer` of the same stamp, so it lands behind the body
    /// even when the arrival is held. Emptied by `deliver_directives`.
    directive_stamps: BTreeMap<RegionCoords, crate::Tick>,
    /// Crossings routed to their target (`Departures`) whose arrival the
    /// target has not confirmed yet (its `EventProcessed(EntityArrived)`):
    /// every crossing entity is always in exactly one of its source's state,
    /// here, or its target's state / held queue. A target that dies is sent
    /// these again once respawned; an autosave saves them with the target
    /// whose saved state does not hold them yet.
    in_flight: BTreeMap<crate::EntityId, InFlight>,
    /// Autosave rounds started ([`Self::request_save`]); the regions asked in
    /// the current one that have not answered yet, and those that have.
    save_round: u64,
    save_asked: BTreeSet<RegionCoords>,
    save_answered: BTreeSet<RegionCoords>,
    /// Crossings of the current round that landed in a target whose save had
    /// already been taken, from a source still to answer: they go with the
    /// source's save (see [`Self::owe_source_save`]).
    save_owed: BTreeMap<RegionCoords, Vec<crate::SavedTransfer>>,
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
            unsaved_parked: BTreeSet::new(),
            saves: Vec::new(),
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
            adventurer_of: BTreeMap::new(),
            player_of: BTreeMap::new(),
            parking: BTreeMap::new(),
            moving: BTreeMap::new(),
            parked_players: BTreeMap::new(),
            awaiting_park: BTreeMap::new(),
            deferred_packets: BTreeMap::new(),
            player_saves: Vec::new(),
            clock_ms: 0,
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
            directive_stamps: BTreeMap::new(),
            in_flight: BTreeMap::new(),
            save_round: 0,
            save_asked: BTreeSet::new(),
            save_answered: BTreeSet::new(),
            save_owed: BTreeMap::new(),
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
        let stamps = std::mem::take(&mut self.directive_stamps);
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
            let input = match stamps.get(&d.region) {
                Some(&stamp) => RegionInput::Transfer(d.event, stamp),
                None => RegionInput::Event(d.event),
            };
            if !self.send_to_region(d.region, input) {
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

    /// Move the client's player from its home (if any) to `landing` in `target`,
    /// keeping its `Client` (inventory, cursor, mode): it is parked out of the old
    /// home (`ParkPlayer`) and, once that answers with its record
    /// ([`Self::player_parked`]), created in `target` with it (`CreateClientAt`; woken
    /// at once, so any history catch-up puts a fortress's people there first).
    /// `target` becomes its home now. A player with no home yet is created fresh.
    /// A second move while one is in flight retargets it.
    fn move_client(&mut self, id: ClientId, target: RegionCoords, landing: [u32; 2], now_ms: u64) {
        let home = self.homes.get(&id).copied();
        if home == Some(target) {
            let _ = self.out.send((Some(id), ServerPacket::PlayerRegion(Some(target), id)));
            return;
        }
        if let Some(m) = self.moving.get_mut(&id) {
            m.target = target;
            m.landing = landing;
            self.ensure_running(target);
        } else if let Some(home) = home {
            self.moving.insert(id, Moving { from: home, target, landing, then: Vec::new() });
            self.send_play_event(home, GameEventKind::ParkPlayer(id));
            self.ensure_running(target);
        } else {
            self.send_play_event(target, GameEventKind::CreateClientAt(id, landing, None));
        }
        self.homes.insert(id, target);
        let _ = self.out.send((Some(id), ServerPacket::PlayerRegion(Some(target), id)));
        if let Some(home) = home {
            self.refresh_keepalive(home, now_ms);
        }
        self.refresh_keepalive(target, now_ms);
    }

    /// A server-authored event for `id`'s player in `rc`: after its creation there
    /// when a move into `rc` is in flight, else at once.
    fn send_player_event(&mut self, id: ClientId, rc: RegionCoords, kind: GameEventKind) {
        match self.moving.get_mut(&id) {
            Some(m) if m.target == rc => m.then.push(kind),
            _ => self.send_play_event(rc, kind),
        }
    }

    /// The move of `client` out of `rc` is answered (`record` = its park, `None` =
    /// no body was there, or the region was lost): the player is created in the
    /// target with its `Client` — an empty one if the body was lost, never the
    /// starting kit, which is only for players new to the world.
    fn finish_move(&mut self, client: ClientId, record: Option<Box<PlayerRecord>>, now_ms: u64) {
        let Some(m) = self.moving.remove(&client) else { return };
        let carried = match record {
            Some(record) => record.client,
            None => {
                log::error!("move of client {client} out of {:?}: no body came back; its items are lost", m.from);
                crate::Client::default()
            }
        };
        self.send_play_event(m.target, GameEventKind::CreateClientAt(client, m.landing, Some(Box::new(carried))));
        for kind in m.then {
            self.send_play_event(m.target, kind);
        }
        self.refresh_keepalive(m.from, now_ms);
        self.refresh_keepalive(m.target, now_ms);
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
        let held = self.fortress_of.get(&id).and_then(|&site| {
            let region = self.history.as_ref()?.site(site)?.region;
            Some(crate::HeldFortress { site, region })
        });
        let _ = self.out.send((Some(id), ServerPacket::Fortress(held)));
    }

    fn embark_result(&self, id: ClientId, r: Result<SiteId, EmbarkRefusal>) {
        let _ = self.out.send((Some(id), ServerPacket::EmbarkResult(r)));
    }

    /// The player's embark: one fortress per player; the history validates and founds
    /// the site at once, then the player moves in (the region's catch-up, or the
    /// founding itself when it already runs, spawns the party).
    fn embark(&mut self, id: ClientId, req: EmbarkRequest, now_ms: u64) {
        if (self.fortress_of.contains_key(&id) || self.adventurer_of.contains_key(&id)) && self.history.is_some() {
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

    /// Retire: the player stops playing its fortress and moves back where a fresh
    /// connection starts (`SPAWN_REGION`); the fortress region parks by the usual grace
    /// rule (its people go back to the abstract history). No fortress = no-op.
    fn retire(&mut self, id: ClientId, now_ms: u64) {
        if self.fortress_of.remove(&id).is_none() {
            return;
        }
        let landing = self.landing_in(SPAWN_REGION);
        self.move_client(id, SPAWN_REGION, landing, now_ms);
        if let Some(history) = &self.history {
            let _ = self.out.send((Some(id), ServerPacket::Sites(history.site_markers())));
        }
        self.send_fortress(id);
    }

    /// Return to a retired fortress.
    ///
    /// "Return only to a fortress you founded" is implemented as "only to a
    /// **player-founded** site no other *connected* player is playing" — the same in
    /// single player. A disconnected player's claim lives in its record; it is
    /// dropped when that player comes back to find someone else playing there.
    fn return_to(&mut self, id: ClientId, site: SiteId, now_ms: u64) {
        let Some(history) = &self.history else {
            return self.embark_result(id, Err(EmbarkRefusal::NoHistory));
        };
        if self.fortress_of.contains_key(&id) || self.adventurer_of.contains_key(&id) {
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
        let landing = self.landing_in(region);
        self.move_client(id, region, landing, now_ms);
        self.fortress_of.insert(id, site);
        self.embark_result(id, Ok(site));
        self.send_fortress(id);
    }

    fn adventure_result(&self, id: ClientId, r: Result<SiteId, AdventureRefusal>) {
        let _ = self.out.send((Some(id), ServerPacket::AdventureResult(r)));
    }

    /// Start an adventure at a populated site: the player moves to the site's region
    /// and a server-originated `Order::SetMode` puts it in Adventure play mode
    /// (sequenced after its `CreateClientAt` there, so client and server agree). One
    /// at a time: no adventure while a fortress or another adventure is held.
    fn start_adventure(&mut self, id: ClientId, site: SiteId, now_ms: u64) {
        let Some(history) = &self.history else {
            return self.adventure_result(id, Err(AdventureRefusal::NoHistory));
        };
        let Some(s) = history.site(site) else {
            return self.adventure_result(id, Err(AdventureRefusal::UnknownSite));
        };
        if s.abandoned.is_some() {
            return self.adventure_result(id, Err(AdventureRefusal::SiteAbandoned));
        }
        if !s.members.iter().any(|&p| history.person(p).is_some_and(|p| p.died.is_none())) {
            return self.adventure_result(id, Err(AdventureRefusal::Unpopulated));
        }
        if self.fortress_of.contains_key(&id) || self.adventurer_of.contains_key(&id) {
            return self.adventure_result(id, Err(AdventureRefusal::AlreadyPlaying));
        }
        let region = s.region;
        let landing = self.landing_in(region);
        // Already at home in that region: `move_client` only resends `PlayerRegion`
        // and the body stays put, so the order itself carries the landing tile
        // (a high level clamps to the column's top and `nearest_standable` searches
        // down). Elsewhere `CreateClientAt` has placed the body.
        let at = (self.homes.get(&id) == Some(&region))
            .then(|| [landing[0] as i32, ADVENTURE_LANDING_HINT_Y, landing[1] as i32]);
        self.move_client(id, region, landing, now_ms);
        self.send_player_event(id, region, GameEventKind::Order(id, Order::SetMode { mode: PlayMode::Adventure, at }));
        self.adventurer_of.insert(id, site);
        self.adventure_result(id, Ok(site));
        self.send_fortress(id);
        crate::perf_event!("adventure_start", "client={id} site={} to={},{}", site.0, region.x, region.z);
    }

    /// End the adventure: back to Fortress mode, then to `SPAWN_REGION` like
    /// [`Self::retire`]. No adventure = no-op.
    fn end_adventure(&mut self, id: ClientId, now_ms: u64) {
        if !self.adventurer_of.contains_key(&id) {
            return;
        }
        if let Some(home) = self.homes.get(&id).copied() {
            self.send_player_event(
                id,
                home,
                GameEventKind::Order(id, Order::SetMode { mode: PlayMode::Fortress, at: None }),
            );
        }
        self.adventurer_of.remove(&id);
        let landing = self.landing_in(SPAWN_REGION);
        self.move_client(id, SPAWN_REGION, landing, now_ms);
        if let Some(history) = &self.history {
            let _ = self.out.send((Some(id), ServerPacket::Sites(history.site_markers())));
        }
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
        self.clock_ms = self.clock_ms.max(now_ms);
        match ev {
            ServerEvent::ClientConnected { client: id, player, record } => {
                if !self.client_connected(id, player, record, now_ms) {
                    return true;
                }
            }
            ServerEvent::ClientDisconnected(id) => self.client_disconnected(id, now_ms),
            ServerEvent::ClientPacket(packet, id) if self.restore_pending(id) && defers_until_restored(&packet) => {
                self.deferred_packets.entry(id).or_default().push(packet);
            }
            ServerEvent::ClientPacket(packet, id) => match packet {
                // The network edge consumes the handshake; in-process
                // harnesses may forward one. Nothing to do here.
                ClientPacket::Hello { .. } => {}
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
                ClientPacket::StartAdventure(site) => self.start_adventure(id, site, now_ms),
                ClientPacket::EndAdventure => self.end_adventure(id, now_ms),
                ClientPacket::RequestPersonInfo(person) => {
                    if let Some(info) = self.history.as_ref().and_then(|h| h.person_info(person)) {
                        let _ = self.out.send((Some(id), ServerPacket::PersonInfo(info)));
                    }
                }
                ClientPacket::RequestLegends(query) => {
                    if let Some(history) = &self.history {
                        let _ = self.out.send((Some(id), ServerPacket::Legends(history.legends(&query))));
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
                                // Regions share one tick numbering, so the
                                // stamp holds in whichever region the input
                                // reaches; that region makes it follow the
                                // body if the body is crossing (`Forward`).
                                let input = match after_tick {
                                    Some(tick) => RegionInput::EventAfterTick(kind, tick),
                                    None => RegionInput::Event(kind),
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
                    | GameEventKind::HaloUpdate(_)
                    | GameEventKind::CreateClient(_)
                    | GameEventKind::CreateClientAt(..)
                    | GameEventKind::SpawnPerson(_)
                    | GameEventKind::DespawnPerson(_)
                    | GameEventKind::WalkOut(..)
                    | GameEventKind::ClientLeft(_)
                    | GameEventKind::ParkPlayer(_)
                    | GameEventKind::RestorePlayer(..) => {
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
        self.clock_ms = self.clock_ms.max(now_ms);
        match output {
            RegionOutput::EventProcessed(event) => {
                if let GameEventKind::EntityArrived(bundle) = &event.kind {
                    if self.in_flight.get(&bundle.id).is_some_and(|f| f.target == rc) {
                        let f = self.in_flight.remove(&bundle.id).expect("checked");
                        self.owe_source_save(f);
                    }
                }
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
            RegionOutput::EventProcessedHashed(event, tick, hash) => {
                if let Some(link) = self.regions.get(&rc) {
                    for client in &link.subscribers {
                        let _ = self
                            .out
                            .send((Some(*client), ServerPacket::HashedGameEvent(event.clone(), tick, hash)));
                    }
                }
            }
            RegionOutput::Snapshot(client, rollback) => {
                let _ = self.out.send((Some(client), ServerPacket::Region(rc, rollback)));
            }
            RegionOutput::Saved(rollback) => {
                self.extract_player_saves(rc, &rollback);
                if let Some(history) = self.history.as_mut() {
                    history.note_positions(rollback.people_here());
                }
                let transfers = self.saved_transfers(rc);
                self.saves.push((rc, RegionSave::Live(rollback, transfers)));
            }
            RegionOutput::PlayerParked(client, record) => self.player_parked(rc, client, record, now_ms),
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
                self.unsaved_parked.insert(rc);
                let pending = self
                    .regions
                    .remove(&rc)
                    .map(|l| l.resubscribe_pending)
                    .unwrap_or_default();
                self.spawner.reap(rc);
                let events = self.pending_events.remove(&rc).unwrap_or_default();
                // A park answered before `Stopped`; one still open here, with no
                // `ParkPlayer` queued for the region's next run, was lost.
                let queued: BTreeSet<ClientId> = events
                    .iter()
                    .filter_map(|e| match e {
                        RegionInput::Event(GameEventKind::ParkPlayer(c))
                        | RegionInput::Transfer(GameEventKind::ParkPlayer(c), _) => Some(*c),
                        _ => None,
                    })
                    .collect();
                if !events.is_empty() {
                    self.ensure_running(rc);
                    for input in events {
                        self.send_to_region(rc, input);
                    }
                    self.refresh_keepalive(rc, now_ms);
                }
                self.settle_lost_parks(rc, &queued);
                for client in pending {
                    // A client asked for this region while it was stopping:
                    // now that the state is parked, cycle it right back in.
                    self.subscribe(client, rc);
                }
            }
            RegionOutput::Departures(list, stamp) => {
                for (mut bundle, target) in list {
                    let client = bundle.client.as_ref().map(|(c, _)| *c);
                    if crate::off_map(self.map_extent(), target) {
                        // Regions hold bodies at the outer seam, so this is a
                        // region that was not bounded: refuse the crossing and
                        // hand the entity back to where it left, on its seam.
                        log::error!("departure from {rc:?} into off-map {target:?} refused: returned");
                        let t = &mut bundle.body.pose.translation;
                        t.x = t.x.clamp(0.0, crate::REGION_SIZE);
                        t.z = t.z.clamp(0.0, crate::REGION_SIZE);
                        self.enter_in_flight(rc, rc, stamp, &bundle);
                        self.send_to_region(rc, RegionInput::Transfer(GameEventKind::EntityArrived(bundle), stamp));
                        continue;
                    }
                    if let (EntityKind::Person { person }, Some(history)) = (bundle.kind, self.history.as_mut()) {
                        // A person is delivered only into a running region; the history
                        // absorbs anyone walking into one that is not (no wake).
                        if !history.person_crossed(person, target) {
                            continue;
                        }
                    }
                    bundle.rebase(rc, target);
                    // Arrivals ALWAYS wake the target (parked blob or gen).
                    self.ensure_running(target);
                    self.enter_in_flight(rc, target, stamp, &bundle);
                    self.send_to_region(
                        target,
                        RegionInput::Transfer(GameEventKind::EntityArrived(bundle), stamp),
                    );
                    self.directive_stamps.insert(target, stamp);
                    if let Some(m) = client.and_then(|c| self.moving.get_mut(&c)) {
                        // The body crossed a seam while its move's park was on the
                        // way: park it where it went (its home is the move target).
                        m.from = target;
                        let c = client.expect("moving is keyed by client");
                        // Behind the arrival, in its slot.
                        self.send_to_region(target, RegionInput::Transfer(GameEventKind::ParkPlayer(c), stamp));
                        self.refresh_keepalive(rc, now_ms);
                        self.refresh_keepalive(target, now_ms);
                    } else if let Some(p) = client.and_then(|c| self.parking.get_mut(&c)) {
                        // The body crossed a seam while its park was on the way:
                        // the old region found nobody; park it where it went.
                        p.region = target;
                        let c = client.expect("parking is keyed by client");
                        // Behind the arrival, in its slot.
                        self.send_to_region(target, RegionInput::Transfer(GameEventKind::ParkPlayer(c), stamp));
                        self.refresh_keepalive(rc, now_ms);
                        self.refresh_keepalive(target, now_ms);
                    } else if let Some(c) = client {
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
            RegionOutput::Forward(kind, stamp) => {
                // An input that reached `rc` after its client's body left:
                // on to the client's home, where the arrival (sent when the
                // `Departures` came, before this) is already queued.
                let client = kind.input_client();
                match client.and_then(|c| self.homes.get(&c).copied()) {
                    Some(home) if home != rc && self.regions.contains_key(&home) => {
                        self.send_to_region(home, RegionInput::EventAfterTick(kind, stamp));
                    }
                    home => log::debug!("[server] forward of {kind:?} from {rc:?} dropped: home {home:?}"),
                }
            }
            RegionOutput::HaloDeltas(list, stamp) => {
                for (update, target) in list {
                    // Like ghosts: halo updates never wake a region (a
                    // starting one is seeded afresh).
                    let running = self.regions.get(&target).is_some_and(|link| !link.stopping);
                    if running {
                        self.send_to_region(target, RegionInput::Transfer(GameEventKind::HaloUpdate(update), stamp));
                    }
                }
            }
            RegionOutput::GhostUpdates(list, stamp) => {
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
                    self.send_to_region(target, RegionInput::Transfer(GameEventKind::GhostUpdate(data), stamp));
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
        self.clock_ms = self.clock_ms.max(now_ms);
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
                let queued = self.restart_for_in_flight(rc);
                self.settle_lost_parks(rc, &queued);
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
            self.regions.remove(&rc);
            self.pending_events.remove(&rc);
            self.spawner.reap(rc);
            self.history_region_lost(rc);
            // Woken for what was in flight to it; the exit loop stops it again.
            let queued = self.restart_for_in_flight(rc);
            self.settle_lost_parks(rc, &queued);
        }
    }

    /// World autosave: every running region (not stopping) is asked for its
    /// state — it arrives as `RegionOutput::Saved` — and every region parked
    /// since the last call is queued at once. A stopping region is saved once
    /// it has parked, by the next call. Collect them with [`Self::take_saves`].
    /// A live save carries the crossings in flight that no saved state of the
    /// round holds (`saved_transfers`), so each is saved exactly once.
    pub fn request_save(&mut self) {
        self.save_round += 1;
        self.save_asked.clear();
        self.save_answered.clear();
        self.save_owed.clear();
        for (rc, link) in &self.regions {
            if link.stopping {
                continue;
            }
            if link.input.send(RegionInput::Save).is_err() {
                log::error!("region {:?} died before it could be saved", rc);
            } else {
                self.save_asked.insert(*rc);
            }
        }
        for rc in std::mem::take(&mut self.unsaved_parked) {
            if let Some(blob) = self.parked.get(&rc) {
                self.saves.push((rc, RegionSave::Parked(blob.clone())));
            }
        }
    }

    /// Region states for the world autosave collected since the last call.
    pub fn take_saves(&mut self) -> Vec<(RegionCoords, RegionSave)> {
        std::mem::take(&mut self.saves)
    }

    /// Crossings routed but not yet confirmed by their target (the `[server]`
    /// counters line).
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Test support: regions asked for this round's save that have not
    /// answered yet (a stopping region is never asked).
    pub fn awaiting_save(&self) -> Vec<RegionCoords> {
        self.save_asked.iter().copied().collect()
    }

    /// Test support: every crossing in flight, with its target.
    pub fn in_flight_ids(&self) -> Vec<(crate::EntityId, RegionCoords)> {
        self.in_flight.iter().map(|(id, f)| (*id, f.target)).collect()
    }

    /// `bundle` (in `target`'s frame) is on its way from `source` to `target`.
    fn enter_in_flight(
        &mut self,
        source: RegionCoords,
        target: RegionCoords,
        stamp: crate::Tick,
        bundle: &crate::EntityBundle,
    ) {
        let entry = InFlight {
            source,
            target,
            stamp,
            bundle: bundle.clone(),
            save_round: self.save_round,
            in_source_save: self.save_answered.contains(&source),
            saved_in: None,
        };
        if let Some(old) = self.in_flight.insert(bundle.id, entry) {
            log::error!("{:?} departed {source:?} while still in flight to {:?}", bundle.id, old.target);
        }
    }

    /// `rc` answered this round's `Save`: the crossings in flight that its
    /// saved state lacks and no other save of the round holds — so each one
    /// is saved exactly once per round:
    /// - in flight to `rc` (unless the source's save, answered before the
    ///   departure, still holds the body);
    /// - departed from `rc` to a target whose save of this round will not
    ///   hold it ([`InFlight::source_saves`]): saved here, rebased back into
    ///   `rc`'s frame — restored, it arrives just past the seam and crosses
    ///   again.
    fn saved_transfers(&mut self, rc: RegionCoords) -> Vec<crate::SavedTransfer> {
        self.save_asked.remove(&rc);
        self.save_answered.insert(rc);
        let round = self.save_round;
        let (asked, answered) = (&self.save_asked, &self.save_answered);
        let mut out = self.save_owed.remove(&rc).unwrap_or_default();
        for f in self.in_flight.values_mut() {
            if f.saved_in == Some(round) || (f.save_round == round && f.in_source_save) {
                continue;
            }
            if f.target == rc {
                out.push((f.bundle.clone(), f.stamp));
            } else if f.source == rc && f.source_saves(round, asked, answered) {
                let mut bundle = f.bundle.clone();
                bundle.rebase(f.target, f.source);
                out.push((bundle, f.stamp));
            } else {
                continue;
            }
            f.saved_in = Some(round);
        }
        out
    }

    /// `f` just landed in its target. If the target's save of this round does
    /// not hold it ([`InFlight::source_saves`]) and its source has yet to
    /// answer, no saved state of the round holds it — the source's save takes
    /// it, rebased back into the source's frame, as for one still in flight.
    fn owe_source_save(&mut self, f: InFlight) {
        let round = self.save_round;
        let in_a_save = f.saved_in == Some(round) || (f.save_round == round && f.in_source_save);
        if !in_a_save
            && f.source_saves(round, &self.save_asked, &self.save_answered)
            && self.save_asked.contains(&f.source)
        {
            let mut bundle = f.bundle;
            bundle.rebase(f.target, f.source);
            self.save_owed.entry(f.source).or_default().push((bundle, f.stamp));
        }
    }

    /// `rc` died: what was in flight to it is sent again to its respawn (or
    /// respawns it), with its stamps. A person's is dropped — the history
    /// took its people back with the region ([`Self::history_region_lost`])
    /// and its catch-up places them again. A player whose park or move was
    /// waiting on `rc` gets its `ParkPlayer` again behind the body; returns
    /// those clients (their parks are still in flight).
    fn restart_for_in_flight(&mut self, rc: RegionCoords) -> BTreeSet<ClientId> {
        let ids: Vec<crate::EntityId> =
            self.in_flight.iter().filter(|(_, f)| f.target == rc).map(|(id, _)| *id).collect();
        let mut parks = BTreeSet::new();
        if ids.is_empty() {
            return parks;
        }
        self.ensure_running(rc);
        for id in ids {
            let f = self.in_flight[&id].clone();
            if matches!(f.bundle.kind, EntityKind::Person { .. }) && self.history.is_some() {
                log::warn!("{id:?} was in flight to lost region {rc:?}: the history places that person again");
                self.in_flight.remove(&id);
                continue;
            }
            log::warn!("{id:?} was in flight to lost region {rc:?}: sent again");
            let client = f.bundle.client.as_ref().map(|(c, _)| *c);
            self.send_to_region(rc, RegionInput::Transfer(GameEventKind::EntityArrived(f.bundle), f.stamp));
            let Some(c) = client else { continue };
            let parked_here = self.parking.get(&c).is_some_and(|p| p.region == rc)
                || self.moving.get(&c).is_some_and(|m| m.from == rc);
            if parked_here {
                self.send_to_region(rc, RegionInput::Transfer(GameEventKind::ParkPlayer(c), f.stamp));
                parks.insert(c);
            }
        }
        parks
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
        let player = self.player_of.remove(&id);
        let fortress = self.fortress_of.remove(&id);
        let adventure = self.adventurer_of.remove(&id);
        self.deferred_packets.remove(&id);
        self.awaiting_park.retain(|_, (c, _)| *c != id);
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
        // Park the player: its body leaves the region and comes back as its
        // record (`PlayerParked`), so no orphan body stays behind.
        if let Some(m) = self.moving.remove(&id) {
            // A move in flight: its `ParkPlayer` is already on the way to where
            // the body is; it becomes this park (the record keeps the old region).
            let target = self.homes.remove(&id);
            if let Some(player) = player {
                self.parking.insert(id, Parking { player, region: m.from, fortress, adventure });
            }
            self.refresh_keepalive(m.from, now_ms);
            if let Some(target) = target {
                self.refresh_keepalive(target, now_ms);
            }
        } else if let Some(home) = self.homes.remove(&id) {
            match player {
                Some(player) => {
                    self.parking.insert(id, Parking { player, region: home, fortress, adventure });
                    self.send_play_event(home, GameEventKind::ParkPlayer(id));
                }
                None => self.send_play_event(home, GameEventKind::ClientLeft(id)),
            }
            // The home region may have just lost its keep-alive reason.
            self.refresh_keepalive(home, now_ms);
        }
    }

    /// The orderly exit's first step: every connected player is parked (as on a
    /// disconnect), so its record reaches the player store before the process
    /// ends. Then [`Self::shutdown_all`]; regions answer `ParkPlayer` first.
    pub fn park_all_players(&mut self, now_ms: u64) {
        let online: Vec<ClientId> = self.sessions.keys().copied().collect();
        for id in online {
            self.client_disconnected(id, now_ms);
        }
    }

    /// Player records for the player store collected since the last call
    /// (parks and autosave extracts, oldest first: a later one for the same
    /// player supersedes an earlier).
    pub fn take_player_saves(&mut self) -> Vec<(PlayerId, PlayerRecord)> {
        std::mem::take(&mut self.player_saves)
    }

    /// A session started. A session of the same player already online is
    /// ended in its favour. Always `true` (kept for a future refusal).
    fn client_connected(&mut self, id: ClientId, player: PlayerId, record: Option<PlayerRecord>, now_ms: u64) -> bool {
        let elsewhere: Vec<ClientId> =
            self.player_of.iter().filter(|(c, p)| **p == player && **c != id).map(|(c, _)| *c).collect();
        for old in elsewhere {
            // The new session takes over (a crashed client's session lingers
            // until the transport times out): the old one is told why and
            // ended — the server edge closes it — and its player parks, so the
            // new session is restored once the park is back.
            log::info!("client {id}: player {} takes over from client {old}", player.0);
            let _ = self.out.send((Some(old), ServerPacket::Refused(LOGGED_IN_ELSEWHERE.to_string())));
            self.client_disconnected(old, now_ms);
        }
        self.sessions.entry(id).or_default();
        self.player_of.insert(id, player);
        let in_flight = self.parking.values().any(|p| p.player == player);
        if self.homes.contains_key(&id) {
            // The same session announced again: its body is already placed.
            log::info!("client {id} re-announced");
        } else if in_flight {
            // Reconnected faster than the park: restore once it is back.
            log::info!("client {id}: player {} is still parking; restore deferred", player.0);
            self.awaiting_park.insert(player, (id, record));
            self.deferred_packets.entry(id).or_default();
        } else {
            let record = self.parked_players.remove(&player).or(record);
            self.place_player(id, record, now_ms);
        }
        if let Some(map) = &self.world_map {
            let _ = self.out.send((Some(id), ServerPacket::WorldMap(map.clone())));
        }
        if let (Some(history), Some(now)) = (&self.history, self.calendar_now(now_ms)) {
            let _ = self.out.send((Some(id), ServerPacket::Calendar(now)));
            let _ = self.out.send((Some(id), ServerPacket::Sites(history.site_markers())));
            let _ = self.out.send((Some(id), ServerPacket::Civs(history.civ_markers())));
            if !self.restore_pending(id) {
                self.send_claims(id);
            }
        }
        true
    }

    /// Whether `id` waits for its player's park before it is restored.
    fn restore_pending(&self, id: ClientId) -> bool {
        self.awaiting_park.values().any(|(c, _)| *c == id)
    }

    /// Give a session its body: restored from `record`; a record whose region is
    /// off the map (the world changed) lands at the spawn region with its
    /// `Client` (no claims); no record = a fresh player at the spawn region.
    fn place_player(&mut self, id: ClientId, record: Option<PlayerRecord>, now_ms: u64) {
        match record {
            Some(record) if !crate::off_map(self.map_extent(), record.region) => self.restore_player(id, record, now_ms),
            Some(record) => {
                log::warn!("client {id}: its record's region {:?} is off the map; placed at spawn", record.region);
                let landing = self.landing_in(SPAWN_REGION);
                self.homes.insert(id, SPAWN_REGION);
                self.send_play_event(
                    SPAWN_REGION,
                    GameEventKind::CreateClientAt(id, landing, Some(Box::new(record.client))),
                );
                let _ = self.out.send((Some(id), ServerPacket::PlayerRegion(Some(SPAWN_REGION), id)));
                self.refresh_keepalive(SPAWN_REGION, now_ms);
            }
            None => {
                // Server-authoritative player creation, once per session.
                self.ensure_running(SPAWN_REGION);
                self.homes.insert(id, SPAWN_REGION);
                self.send_to_region(SPAWN_REGION, RegionInput::Event(GameEventKind::CreateClient(id)));
            }
        }
    }

    /// Put a returning player back: its body and `Client` in the record's region
    /// (`RestorePlayer`, waking it if parked), and its claims — a fortress only if
    /// no connected player has returned to it meanwhile, a site only if the
    /// history still knows it.
    fn restore_player(&mut self, id: ClientId, record: PlayerRecord, now_ms: u64) {
        let region = record.region;
        let known = |site: SiteId| self.history.as_ref().map_or(true, |h| h.site(site).is_some());
        let fortress = record
            .fortress
            .filter(|&site| known(site) && !self.fortress_of.values().any(|&s| s == site));
        let adventure = record.adventure.filter(|&site| fortress.is_none() && known(site));
        self.homes.insert(id, region);
        self.send_play_event(region, GameEventKind::RestorePlayer(id, Box::new(record)));
        if let Some(site) = fortress {
            self.fortress_of.insert(id, site);
        }
        if let Some(site) = adventure {
            self.adventurer_of.insert(id, site);
        }
        let _ = self.out.send((Some(id), ServerPacket::PlayerRegion(Some(region), id)));
        self.refresh_keepalive(region, now_ms);
        crate::perf_event!("player_restored", "client={id} to={},{}", region.x, region.z);
    }

    /// What a session holds, told on connect: its fortress (`Fortress`), and a
    /// restored claim as the answer an embark / adventure start would have had,
    /// so the client leaves the arrival map without embarking again.
    fn send_claims(&self, id: ClientId) {
        self.send_fortress(id);
        if let Some(&site) = self.fortress_of.get(&id) {
            self.embark_result(id, Ok(site));
        }
        if let Some(&site) = self.adventurer_of.get(&id) {
            self.adventure_result(id, Ok(site));
        }
    }

    /// A region answered `ParkPlayer`. The record (with the claims the manager
    /// held) becomes the player's parked record and goes to the store; a session
    /// of the same player waiting for it is restored now.
    fn player_parked(&mut self, rc: RegionCoords, client: ClientId, record: Option<Box<PlayerRecord>>, now_ms: u64) {
        if let Some(m) = self.moving.get(&client) {
            // A move's park; one from a region the body had already left is
            // superseded (see `Departures`).
            if m.from == rc {
                self.finish_move(client, record, now_ms);
            }
            return;
        }
        let Some(p) = self.parking.get(&client).copied() else {
            log::warn!("PlayerParked({client}) from {rc:?}: no park in flight");
            return;
        };
        if p.region != rc {
            // The body had left `rc` before the park reached it; it is parked
            // where it went (see `Departures`).
            return;
        }
        self.parking.remove(&client);
        match record {
            Some(record) => {
                let record = PlayerRecord { fortress: p.fortress, adventure: p.adventure, ..*record };
                self.parked_players.insert(p.player, record.clone());
                self.player_saves.push((p.player, record));
            }
            None => log::warn!("PlayerParked({client}) from {rc:?}: no body to park"),
        }
        self.park_settled(p.player, now_ms);
    }

    /// `player`'s park is over (answered or lost): a session of it waiting for
    /// the park is placed now — from its parked record, else the record the
    /// edge read from the store, else as a fresh player — and its held
    /// requests are replayed.
    fn park_settled(&mut self, player: PlayerId, now_ms: u64) {
        let Some((waiting, edge_record)) = self.awaiting_park.remove(&player) else { return };
        if !self.sessions.contains_key(&waiting) {
            return;
        }
        let record = self.parked_players.remove(&player).or(edge_record);
        self.place_player(waiting, record, now_ms);
        if self.history.is_some() {
            self.send_claims(waiting);
        }
        for packet in self.deferred_packets.remove(&waiting).unwrap_or_default() {
            self.handle_server_event(ServerEvent::ClientPacket(packet, waiting), now_ms);
        }
    }

    /// Region `rc` was lost (its thread died, or it stopped without answering)
    /// with parks in flight: those bodies are gone, so the parks end with no new
    /// record (the player keeps whatever the store last had), and a session of
    /// each player that waits for its park is let in. `queued`: clients whose
    /// `ParkPlayer` is queued for the region's next run (still in flight).
    fn settle_lost_parks(&mut self, rc: RegionCoords, queued: &BTreeSet<ClientId>) {
        let lost: Vec<(ClientId, PlayerId)> = self
            .parking
            .iter()
            .filter(|(c, p)| p.region == rc && !queued.contains(c))
            .map(|(c, p)| (*c, p.player))
            .collect();
        for (client, player) in lost {
            log::error!("region {rc:?} was lost with client {client}'s park in flight (player {})", player.0);
            self.parking.remove(&client);
            self.park_settled(player, self.clock_ms);
        }
        let lost_moves: Vec<ClientId> = self
            .moving
            .iter()
            .filter(|(c, m)| m.from == rc && !queued.contains(c))
            .map(|(c, _)| *c)
            .collect();
        for client in lost_moves {
            self.finish_move(client, None, self.clock_ms);
        }
    }

    /// The world autosave keeps online players too: each body in a region's
    /// saved state becomes its player's record, so a crash loses at most the
    /// autosave interval ([`crate::restore_saved`] strips the bodies on load).
    fn extract_player_saves(&mut self, rc: RegionCoords, rollback: &crate::Rollback) {
        for (client, record) in crate::players_in(rollback, rc) {
            let Some(&player) = self.player_of.get(&client) else { continue };
            let record = PlayerRecord {
                fortress: self.fortress_of.get(&client).copied(),
                adventure: self.adventurer_of.get(&client).copied(),
                ..record
            };
            self.player_saves.push((player, record));
        }
    }

    /// The stable player behind a connected client.
    pub fn player_of(&self, client: ClientId) -> Option<PlayerId> {
        self.player_of.get(&client).copied()
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
            transfers_in_flight: self.in_flight_count(),
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
        // Very first: join the world's tick numbering before anything else.
        let _ = input.send(RegionInput::StartAt(self.spawner.world_tick()));
        // Next, so it is bounded before its first tick.
        if let Some(extent) = self.map_extent() {
            let _ = input.send(RegionInput::SetMapExtent(Some(extent)));
        }
        // Its terrain halo, before its first tick too.
        self.seed_halo(rc, &input);
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

    /// Seed the halo of `rc`, which is starting (`input` is its fresh
    /// channel; nothing has ticked it yet), from each on-map neighbour:
    /// - running: the neighbour answers a `HaloRequest` with its live chunks
    ///   (a stamped Transfer); its cached terrain, if any, goes in first;
    /// - parked or cached (its last pushed / parked terrain): from the cache;
    /// - never run: worldgen — on the region's own thread when the spawner
    ///   generates there (only the band columns), else here (the whole
    ///   region, not cached: LOD keeps its own choice of pristine sources).
    ///
    /// Known limit (as LOD's): a neighbour saved by an earlier process that
    /// has not run in this one is seeded from pristine worldgen.
    fn seed_halo(&mut self, rc: RegionCoords, input: &Sender<RegionInput>) {
        let mut generate_there = Vec::new();
        for n in crate::halo::neighbours(rc) {
            if crate::off_map(self.map_extent(), n) {
                continue;
            }
            let running = self.regions.get(&n).is_some_and(|l| !l.stopping);
            if let Some(snap) = self.terrain_cache.get(&n) {
                if let Some(seed) = crate::halo::halo_seed_from_chunks(n, rc, snap.chunks.iter().map(|(c, k)| (*c, k))) {
                    let _ = input.send(RegionInput::Event(GameEventKind::HaloUpdate(seed)));
                }
            } else if running {
                // The request below is the seed.
            } else if self.spawner.generates_columns() {
                generate_there.push(n);
            } else {
                // Not cached: LOD keeps its own choice of pristine sources.
                let snap = TerrainSnapshot::from_columns(self.generate_timed(n));
                if let Some(seed) = crate::halo::halo_seed_from_chunks(n, rc, snap.chunks.iter().map(|(c, k)| (*c, k))) {
                    let _ = input.send(RegionInput::Event(GameEventKind::HaloUpdate(seed)));
                }
            }
            if running {
                self.send_to_region(n, RegionInput::HaloRequest(rc));
                // And the other way: `n` mirrors this region from whatever
                // it was seeded with (a saved region may have changed since).
                let _ = input.send(RegionInput::HaloRequest(n));
            }
        }
        if !generate_there.is_empty() {
            let _ = input.send(RegionInput::SeedHaloFromWorldgen(generate_there));
        }
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
            .any(|c| self.homes.get(c) == Some(&rc))
            || self.moving.values().any(|m| m.from == rc);
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
    /// snapshot resync covers the gap, and every crossing in flight to the
    /// region (`in_flight`, which a failed arrival is part of) is sent again.
    /// Returns whether the input reached the region (or its stop queue) —
    /// `false` when it is not running or was just lost and respawned.
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
                RegionInput::Event(GameEventKind::PlayerInput(..))
                | RegionInput::EventAfterTick(GameEventKind::PlayerInput(..), _) => {
                    log::debug!("send_to_region: {rc:?} is stopping; player input dropped");
                }
                input @ (RegionInput::Event(_) | RegionInput::EventAfterTick(..) | RegionInput::Transfer(..)) => {
                    // A transfer keeps its stamp for the region's next run.
                    self.pending_events.entry(rc).or_default().push(input);
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
        // What was on its way there was not in its state: sent again.
        let queued = self.restart_for_in_flight(rc);
        // Bodies being parked there died with it (unless their arrival and
        // park were just sent again).
        self.settle_lost_parks(rc, &queued);
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
