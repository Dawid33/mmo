//! World-level management: which regions run, who is subscribed to what,
//! where players' home regions are, and the parking lot for cycled-out
//! regions. Threadless: a `RegionSpawner` decides how regions actually run
//! (OS threads on the server, inline for wasm/tests), and time arrives as
//! an explicit `now_ms` so the core stays deterministic and wasm-safe.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use crossbeam::channel::{unbounded, Receiver, Sender};

use crate::coordinator::{
    Claim, CoordMsg, HostMsg, OnlinePlayer, RequestId, RunningRegion, WorldLink, WorldReply, WorldRequest,
};
use crate::history::{AdventureRefusal, CalendarTick, EmbarkRefusal, PersonId, SiteId, SocietyGrid};
use crate::{
    ClientId, ClientPacket, EntityKind, TerrainSnapshot, GameEventKind,
    LodBuilder, LodKey, PortableRegion, Region, RegionCoords, RegionInput,
    RegionOutput, RegionRunner, RegionSeed, SerializedRegion, ServerPacket, PlayRefusal, WorldMap, EmbarkRequest,
    Order, PlayMode, PlayerId, PlayerRecord,
};
use crate::save_cut::{
    CutId, InFlightRow, PendingCut, Uncommitted, WorldCut, CUT_DEADLINE_LEADS, SAVE_CUT_LEAD_MAX_TICKS, SAVE_CUT_LEAD_TICKS,
};

mod peers;
mod world_link;
pub use world_link::{WorldLinkCounters, ATTACH_WAIT_MS, WORLD_REQUEST_TIMEOUT_SECS};
use world_link::{PendingRequest, Step, WorldAttach, WorldViewCache};

/// Level hint for an adventure landing: above any surface, so it clamps to the
/// column's top and the nearest standable tile is the topmost.
const ADVENTURE_LANDING_HINT_Y: i32 = 1 << 20;
pub const SPAWN_REGION: RegionCoords = RegionCoords { x: 0, z: 0 };

pub const UNLOAD_GRACE_MS: u64 = 5000;
/// Most arrivals that wait in the manager's mailbox for one region that is
/// not running ([`WorldManager::pending_arrival_count`]); past it the oldest
/// is despawned — one carrying drops leaves them as item piles at the seam
/// (the overflow policy is on `WorldManager::pending_arrivals`). Twice
/// [`crate::slime::SLIME_CAP_PER_REGION`]: room for a neighbour's whole
/// slime population and its refill, while a region next to an unloaded one
/// cannot pump wildlife into the mailbox (and every round's save of it)
/// forever, nor hand the target an unbounded backlog in one burst when it
/// starts.
pub const PENDING_ARRIVALS_PER_REGION_MAX: usize = 2 * crate::slime::SLIME_CAP_PER_REGION;
/// How many of the latest despawned arrivals' ids the manager remembers
/// ([`WorldManager::recently_despawned_arrivals`], test support: the
/// conservation check reads them as an allowed exit).
pub const DESPAWNED_ARRIVAL_IDS_KEPT: usize = 256;
/// LRU cap on `lod_cache`; subscribed keys are pinned past this (see
/// `evict_lod_cache`).
pub const LOD_CACHE_MAX_ENTRIES: usize = 8192;
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
    /// The login service gave `player` a newer login (`SpawnPlayer`, reason
    /// [`LOGGED_IN_ELSEWHERE`]) or revoked it (`Kick`, its reason): every
    /// session of the player is refused with `reason` and parked, as a
    /// takeover does. The new login's session, when it connects, is restored
    /// from that park. No session: nothing happens.
    Supersede { player: crate::PlayerId, reason: String },
}

impl ServerEvent {
    /// A session for an in-process harness or test: the player id is the
    /// client id and there is no saved record.
    pub fn connected(client: ClientId) -> Self {
        Self::ClientConnected { client, player: crate::PlayerId(client as u64), record: None }
    }
}

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

    /// Whether this spawner's regions cut the halo seeds of neighbours that
    /// are not running themselves, on their own thread
    /// (`RegionInput::SeedHaloFromWorldgen` / `SeedHaloFromStore`): worldgen
    /// and store reads then cost the manager thread nothing. `false` (the
    /// inline spawner: wasm, tests — no other thread to move the work to)
    /// keeps the manager cutting them from [`Self::stud_genesis`].
    fn seeds_halos_on_region_threads(&self) -> bool {
        false
    }

    /// What never-run region `rc` would start from, for a spawner that does
    /// not seed halos on its region threads: the manager cuts the halo seeds
    /// of `rc`'s starting neighbours from it (`seed_halo`). `None`: no
    /// terrain there (the neighbours' seeds of it are empty).
    fn stud_genesis(&self, _rc: RegionCoords) -> Option<crate::StudRegionSeed> {
        None
    }

    /// The world's current tick: where a region spawned now starts counting.
    fn world_tick(&self) -> crate::Tick;
}

/// What a fresh stud region starts from (`InlineSpawner::with_stud_genesis`).
pub type InlineStudGenesis = std::sync::Arc<dyn Fn(RegionCoords) -> crate::StudRegionSeed + Send + Sync>;

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
///   tick — a handoff, halo update or player input that lost the race with
///   the tick it would have preceded in lockstep;
/// - a *skipped* region misses a round entirely, the way
///   `region_thread_loop` falls behind its deadline, and runs a catch-up tick
///   next round so it stays within one tick of the round counter.
#[derive(Debug, Clone)]
pub struct InlineSchedule {
    rng: u64,
    /// The seed the schedule started from (the coordinator link's delays roll from it).
    seed: u64,
    /// Most pumps an inline coordinator link holds each output back
    /// ([`Self::coordinator_delay`]); 0 = none.
    pub coordinator_delay_steps: u32,
    /// Chance, per region per round, that its inputs arrive a tick late.
    pub late_per_mille: u32,
    /// Chance, per region per round, that it misses its tick.
    pub skip_per_mille: u32,
}

impl InlineSchedule {
    /// Permuted tick order only — every region still ticks once per round and
    /// nothing arrives late.
    pub fn shuffled(seed: u64) -> Self {
        Self { rng: seed, seed, coordinator_delay_steps: 0, late_per_mille: 0, skip_per_mille: 0 }
    }

    /// Also delay each output of an inline coordinator link
    /// ([`crate::coordinator::InlineWorldLink::with_schedule`]) by 0..=`max_steps` pumps.
    pub fn coordinator_delay(mut self, max_steps: u32) -> Self {
        self.coordinator_delay_steps = max_steps;
        self
    }

    /// `(seed, max_steps)` of the coordinator link's delay, if it has one.
    pub fn coordinator_link_delay(&self) -> Option<(u64, u32)> {
        (self.coordinator_delay_steps > 0).then_some((self.seed, self.coordinator_delay_steps))
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
    /// Regions in the world store's portable format (a restarted server's
    /// store, [`Self::with_portable`]): a region that has not run in this
    /// process starts from it, through [`crate::restore_portable`].
    portable: BTreeMap<RegionCoords, crate::StoredRegion>,
    /// Test support ([`InlineSpawner::hold_inputs`]): regions that keep
    /// ticking but leave their inputs unread for the next `n` rounds.
    inputs_held: BTreeMap<RegionCoords, u32>,
    /// Rounds advanced by `tick_all`: the world tick of this lockstep world.
    rounds: crate::Tick,
    /// Test support ([`InlineSpawner::hold`]): regions that sit out the next
    /// `n` calls of `tick_all`, neither ticking nor draining their inputs.
    held: BTreeMap<RegionCoords, u32>,
    /// Test support ([`InlineSpawner::record_slot_start_hashes`]).
    record_slot_hashes: bool,
    /// Stud terrain seeded into every region built fresh
    /// ([`InlineSpawner::with_stud_genesis`]).
    stud_genesis: Option<InlineStudGenesis>,
    /// Starts that failed (`RegionOutput::LoadFailed`): their input channels,
    /// kept open until the manager reaps them — as a region thread that does
    /// not run keeps its own — so what was sent there is recovered by the
    /// manager, not mistaken for a crashed region.
    failed: BTreeMap<RegionCoords, Receiver<RegionInput>>,
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

    /// A stud world: every region built fresh (never run, not stored) is seeded
    /// with `genesis(rc)` (`Region::seed_stud_world`), as the server's region
    /// threads do; the manager seeds the halos of never-run neighbours from the
    /// same function.
    pub fn with_stud_genesis(mut self, genesis: InlineStudGenesis) -> Self {
        self.stud_genesis = Some(genesis);
        self
    }

    /// Regions that have not run yet start from the world store's `regions`
    /// (a restarted server): restored by [`crate::restore_portable`], their
    /// runner knowing the stored chunk terms. The world tick starts at the
    /// latest stored region's tick, as the server's restored world clock.
    pub fn with_portable(regions: BTreeMap<RegionCoords, crate::StoredRegion>) -> Self {
        let rounds = regions.values().map(|r| r.meta.tick).max().unwrap_or(0);
        Self { portable: regions, rounds, ..Self::default() }
    }

    /// Test support: `rc` keeps ticking but leaves its inputs unread for the
    /// next `rounds` calls of `tick_all` — a region thread whose input
    /// channel lags far behind its clock. `rounds == 0` releases it now.
    pub fn hold_inputs(&mut self, rc: RegionCoords, rounds: u32) {
        if rounds > 0 {
            self.inputs_held.insert(rc, rounds);
        } else {
            self.inputs_held.remove(&rc);
        }
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
            .filter(|id| !self.late.contains(id) && !self.held.contains_key(id) && !self.inputs_held.contains_key(id))
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
        self.inputs_held.retain(|_, n| {
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
        self.inputs_held.remove(&id);
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
        let stored = if matches!(seed, RegionSeed::Fresh) { self.portable.remove(&id) } else { None };
        let runner = match stored {
            Some(stored) => {
                let (region, terms) = crate::restore_portable(id, stored);
                let mut runner = RegionRunner::new(id, region, out);
                runner.set_saved_terms(terms);
                runner
            }
            None => match seed.into_region(id, || self.stud_genesis.as_ref().map(|g| g(id))) {
                Ok(region) => RegionRunner::new(id, region, out),
                // It does not run, and is never regenerated.
                Err(why) => {
                    log::error!("region {id:?}: {why}");
                    let _ = out.send((id, RegionOutput::LoadFailed(why)));
                    self.failed.insert(id, recv);
                    return send;
                }
            },
        };
        let mut runner = runner;
        if self.record_slot_hashes {
            runner.record_slot_start_hashes();
        }
        self.runners.insert(id, (recv, runner));
        send
    }

    fn reap(&mut self, id: RegionCoords) {
        self.failed.remove(&id);
    }

    fn stud_genesis(&self, rc: RegionCoords) -> Option<crate::StudRegionSeed> {
        self.stud_genesis.as_ref().map(|g| g(rc))
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
    pub lod_cache: usize,
    pub terrain_cache: usize,
    /// Crossings routed but not yet confirmed ([`WorldManager::in_flight_count`]).
    pub transfers_in_flight: usize,
    /// Non-player arrivals waiting for a region that is not running
    /// ([`WorldManager::pending_arrival_total`]).
    pub pending_arrivals: usize,
    /// Reliable messages to other hosts not acknowledged yet
    /// ([`WorldManager::peer_unreceived`]).
    pub peer_unreceived: usize,
    /// Waiting arrivals despawned past [`PENDING_ARRIVALS_PER_REGION_MAX`]
    /// since the manager started ([`WorldManager::arrivals_despawned`]).
    pub arrivals_despawned: u64,
    /// Items in replies that had nowhere to go, since the manager started
    /// ([`WorldManager::items_destroyed`]).
    pub items_destroyed: u64,
    /// Intent replies given up, since the manager started
    /// ([`WorldManager::intent_replies_lost`]).
    pub intent_replies_lost: u64,
}

/// What a session is told when a new session of its player takes over
/// (`ServerPacket::Refused`); the server edge then closes it.
pub const LOGGED_IN_ELSEWHERE: &str = "logged in elsewhere";

/// The refusal of a connection whose session id is already live (a replayed login ticket).
/// The edge gives it before a sink exists; the manager's check behind it never writes to
/// the id (the live session owns it) and only logs.
pub const DUPLICATE_SESSION_REFUSAL: &str = "this login is already connected; log in again";

/// How long a region whose stored state could not be read
/// ([`RegionOutput::LoadFailed`]) stays stopped before a start is tried
/// again, in seconds of world time.
pub const STORE_LOAD_RETRY_SECS: u64 = 10;

/// [`STORE_LOAD_RETRY_SECS`] in world ticks (`TICK_RATE` is the tick period in ms).
pub const STORE_LOAD_RETRY_TICKS: crate::Tick = STORE_LOAD_RETRY_SECS * 1000 / crate::TICK_RATE;

/// Why a session whose body was to be in a region that could not be loaded
/// is ended ([`ServerPacket::Refused`]).
pub const REGION_UNAVAILABLE_REFUSAL: &str = "your region could not be loaded from the world store; try again shortly";

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

/// An intent or reply routed to a running region as a `Transfer` that the
/// region has not confirmed yet (its `EventProcessed(Intent)`): it lives only
/// in the target's held queue, so a save cut may have to carry it.
#[derive(Debug, Clone)]
struct InFlightIntent {
    msg: crate::IntentMsg,
    target: RegionCoords,
    /// Routed after its source's answer to the pending save cut: the
    /// source's snapshot predates sending it, so the cut must not carry it
    /// (`save_cut.rs`). Cleared by every `begin_cut`.
    in_source_view: bool,
}

/// A crossing the manager has routed to its target but not yet seen applied
/// there (`WorldManager::in_flight`).
#[derive(Debug, Clone)]
struct InFlight {
    target: RegionCoords,
    stamp: crate::Tick,
    /// In the target's frame.
    bundle: crate::EntityBundle,
    /// Departed after its source's answer to the pending save cut: the
    /// source's snapshot still holds the body (`save_cut.rs`). Cleared by
    /// every `begin_cut`.
    in_source_view: bool,
}

/// A non-player crossing into a region that was not running (plan Decision
/// 3: only players wake their target; history people are absorbed): it
/// waits in [`WorldManager::pending_arrivals`] until the region next runs.
#[derive(Debug, Clone)]
struct PendingArrival {
    /// The region it left (where a despawned one's drops are put down).
    source: RegionCoords,
    stamp: crate::Tick,
    /// In the target's frame.
    bundle: crate::EntityBundle,
    /// As [`InFlight::in_source_view`].
    in_source_view: bool,
}

/// A reply (or a restored intent) waiting for a region that is not running
/// (`WorldManager::intent_mailbox`).
#[derive(Debug, Clone)]
struct Mail {
    msg: crate::IntentMsg,
    /// As [`InFlightIntent::in_source_view`].
    in_source_view: bool,
}

fn bundle_at(bundle: &crate::EntityBundle) -> [f32; 3] {
    let t = bundle.body.pose.translation;
    [t.x, t.y, t.z]
}

pub struct WorldManager<S: RegionSpawner> {
    spawner: S,
    regions: BTreeMap<RegionCoords, RegionLink>,
    parked: BTreeMap<RegionCoords, SerializedRegion>,
    /// The terrain each region parked with
    /// (`Stopped`): what a starting neighbour's halo is seeded from without
    /// waking it.
    terrain_cache: BTreeMap<RegionCoords, TerrainSnapshot>,
    /// Connected clients and their subscriptions.
    sessions: BTreeMap<ClientId, Session>,
    /// Survives disconnects: which region holds this client's player entity.
    /// (The player parks with its region; reconnects create nothing.)
    homes: BTreeMap<ClientId, RegionCoords>,
    /// The forged world's tile summaries, pushed to every client on connect
    /// and consulted by Play. `None` = no forged world (procedural / tests).
    world_map: Option<WorldMap>,
    /// The coarse-route map built from `world_map`, shared with every region.
    route_map: Option<std::sync::Arc<crate::RouteMap>>,
    /// Server-authored Play events (`ClientLeft` / `CreateClientAt`) for a
    /// region that was mid-stop when Play arrived; replayed in order (waking
    /// the region from its parked blob) once it parks.
    pending_events: BTreeMap<RegionCoords, Vec<RegionInput>>,
    /// The link to the world coordinator (forged worlds): its history puts its people
    /// into regions as they start ([`HostMsg::RegionStarted`] → [`CoordMsg::SpawnPeople`]),
    /// takes them back as they park, and sends running regions its decisions.
    /// `None` = no world services (procedural worlds, most tests): people are untouched.
    world: Option<Box<dyn WorldLink + Send>>,
    /// What the coordinator last told of the world (civs, sites, calendar).
    world_view: Option<WorldViewCache>,
    /// Regions started whose [`CoordMsg::SpawnPeople`] has not arrived yet, with the
    /// incarnation it is for: a directive for that incarnation waits on it (bounced).
    spawn_waiting: BTreeMap<RegionCoords, u64>,
    /// Regions announced to the coordinator ([`HostMsg::RegionStarted`]) whose
    /// [`HostMsg::RegionParked`] has not been sent yet: exactly one park per start, and
    /// never a new start before it (the coordinator relies on it).
    world_live: BTreeSet<RegionCoords>,
    /// Where the host stands with the coordinator (link down, gathering its `Attach`, …).
    world_attach: WorldAttach,
    /// `Attach`es sent so far (the epoch the coordinator echoes).
    attach_epoch: u64,
    /// The link's [`WorldLink::generation`] when the host last looked: a change while up
    /// is a reconnect.
    world_generation: u64,
    /// Random per host process start ([`Self::set_world_link`]): requests' ids stay unique
    /// across restarts.
    world_boot: u64,
    next_request_seq: u64,
    /// Player world requests not answered yet.
    pending_requests: BTreeMap<RequestId, PendingRequest>,
    /// State-changing requests (embark, return, adventure start) that had been sent when
    /// they timed out toward the client: the coordinator may have applied them. Kept
    /// until their answer comes — sent again after every `Attach` (the memo answers
    /// without re-executing) — and a claim that answer grants is given back
    /// (`ReleaseClaim`; the site stays). They never age: a claim the player does not
    /// know it holds would refuse its every embark.
    expired_requests: BTreeMap<RequestId, (PlayerId, WorldRequest)>,
    /// Requests and claim messages made while the link is down (or the `Attach` is being
    /// gathered), in order: sent right after the `Attach`.
    outbox: VecDeque<HostMsg>,
    /// [`WORLD_REQUEST_TIMEOUT_SECS`] in ms (a test hook may shorten it).
    world_request_timeout_ms: u64,
    world_counters: WorldLinkCounters,
    /// Each site's region, as the coordinator told it (view, sites, replies).
    site_regions: BTreeMap<SiteId, RegionCoords>,
    /// The society grid of the world map (landings where a tile has none).
    society_grid: Option<SocietyGrid>,
    /// The `now_ms` of the latest `maintain`: the link is pumped at it between upkeeps,
    /// so only `maintain` moves the coordinator's calendar.
    world_ms: u64,
    /// Notices the coordinator sent, routed at the end of the event or upkeep
    /// ([`Self::route_notices`]).
    world_notices: Vec<(RegionCoords, String)>,
    /// The fortress each connected client plays (embarked or returned to, not
    /// retired). On disconnect it moves into the player's record (`parking`, then
    /// `parked_players` and the next save cut), and a reconnect of the same player
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
    /// The `now_ms` of the latest call in (server event, region output,
    /// maintenance), for paths that learn of a lost region without one.
    clock_ms: u64,
    /// Regions whose stored state could not be read
    /// ([`RegionOutput::LoadFailed`]), with the world tick from which a start
    /// is tried again: until then nothing starts them (Play, embark and
    /// return there are refused `RegionUnavailable`, arrivals wait in the
    /// mailbox). Never regenerated over stored data.
    unavailable: BTreeMap<RegionCoords, crate::Tick>,
    /// Which regions this process simulates, and the super-region side the
    /// policy groups them by (`set_ownership`). A region it does not own is
    /// never started here and counts as unavailable (no retry timer: the
    /// owning process serves it).
    ownership: (crate::Ownership, u32),
    /// The record a body's creation carries (a move's `CreateClientAt`, a
    /// restore's `RestorePlayer`), by client, with its target region: the only
    /// copy until the target applies the creation (its `EventProcessed`).
    /// A target whose load fails puts it back into `parked_players`.
    creating: BTreeMap<ClientId, (RegionCoords, PlayerId, PlayerRecord)>,
    out: Sender<(Option<ClientId>, ServerPacket)>,
    region_out_send: Sender<(RegionCoords, RegionOutput)>,
    /// LOD subscriptions per key, and the reverse map for disconnects.
    lod_subscribers: BTreeMap<LodKey, BTreeSet<ClientId>>,
    /// key -> far-terrain tile (`stud_lod.rs`); LRU-evicted past
    /// LOD_CACHE_MAX_ENTRIES, subscribed keys pinned. `u64` is a
    /// monotonically increasing access stamp (not wall time — determinism).
    lod_cache: BTreeMap<LodKey, (crate::StudLodTile, u64)>,
    /// LRU cap on `lod_cache`; defaults to `LOD_CACHE_MAX_ENTRIES`, overridable
    /// via `set_lod_cache_max` (test hook — mirrors `LodRings.retirement_max_frames`).
    lod_cache_max: usize,
    lod_access_counter: u64,
    /// Builds submitted, result not yet received — dedups double-requests.
    lod_in_flight: BTreeSet<LodKey>,
    /// When each in-flight build was submitted (metrics only, see `perf`).
    lod_submitted_at: BTreeMap<LodKey, crate::perf::Stopwatch>,
    perf: ManagerPerfCounters,
    lod_builder: Box<dyn LodBuilder>,
    /// Running regions handed arrivals (`Departures`), with the latest
    /// arrival's stamp: a history directive for one (a delivered person's
    /// walk-on) goes as a `Transfer` of that stamp, so it lands behind the
    /// body even when the arrival is held — however late the directive
    /// comes over the world link (a past stamp applies at once). Dropped
    /// when the region stops.
    directive_stamps: BTreeMap<RegionCoords, crate::Tick>,
    /// Crossings routed to their target (`Departures`) whose arrival the
    /// target has not confirmed yet (its `EventProcessed(EntityArrived)`):
    /// every crossing entity is always in exactly one of its source's state,
    /// here, or its target's state / held queue. A target that dies is sent
    /// these again once respawned; a save cut carries those in neither its
    /// source's nor its target's snapshot (`save_cut.rs`).
    in_flight: BTreeMap<crate::EntityId, InFlight>,
    /// The save cut asked for and not complete yet (`save_cut.rs`).
    pending_cut: Option<PendingCut>,
    /// A complete cut waiting for [`Self::take_cut`], with the `seq` of each
    /// uncommitted entry it carries.
    ready_cut: Option<(WorldCut, BTreeMap<RegionCoords, u64>)>,
    /// Cuts taken and not committed or failed yet: the uncommitted entries
    /// (`seq`) each carries.
    outstanding_cuts: BTreeMap<CutId, BTreeMap<RegionCoords, u64>>,
    /// Every region snapshot not committed yet, merged per region.
    uncommitted: BTreeMap<RegionCoords, Uncommitted>,
    uncommitted_seq: u64,
    /// The lead of the next cut: [`SAVE_CUT_LEAD_TICKS`], doubled after each
    /// missed one (up to [`SAVE_CUT_LEAD_MAX_TICKS`]).
    cut_lead: crate::Tick,
    next_cut_id: u64,
    /// Orders the player records a pending cut collects (latest wins).
    cut_order: u64,
    /// Stopping regions a cut's deadline passed waiting for (likely dead):
    /// left out of cuts until their `Stopped` comes.
    silent_stopping: BTreeSet<RegionCoords>,
    /// Test support: the players the last complete cut caught crossing a seam.
    last_cut_caught: Vec<PlayerId>,
    /// The regions the world store holds, with their terrain version
    /// ([`Self::set_stored_regions`], [`Self::cut_committed`]).
    stored_regions: BTreeMap<RegionCoords, u64>,
    /// Where each entity is owned (spec P2's directory): from the regions'
    /// `Spawned` / `Despawned` reports and the `Departures` routed here (an
    /// entity in flight is at its target). Intents and replies addressed by
    /// `EntityId` are routed by it.
    locations: BTreeMap<crate::EntityId, RegionCoords>,
    /// Intents / replies sent to a running region and not yet confirmed by
    /// it, in routing order. A save cut carries those in neither their
    /// source's nor their target's snapshot; sent again to a target that dies.
    in_flight_intents: Vec<InFlightIntent>,
    /// Replies for an entity in a parked or stopping region (and intents a
    /// restore put back on their way): delivered when it next runs, carried
    /// by save cuts meanwhile.
    intent_mailbox: BTreeMap<RegionCoords, Vec<Mail>>,
    /// Non-player arrivals (slimes, dynamic bodies — anything but players
    /// and people) into a region that is not running (absent, parked or
    /// stopping), by target, in arrival order: they do not wake it, and are
    /// delivered — before its first tick — when it next starts. Meanwhile
    /// a save cut carries it (unless its source's snapshot still holds it),
    /// and it is at its target in `locations` (kept when the target parks).
    ///
    /// Overflow ([`PENDING_ARRIVALS_PER_REGION_MAX`], `hold_arrival`): the
    /// oldest waiting arrival is despawned and counted
    /// (`arrivals_despawned`). An arrival with nothing to leave behind is
    /// wildlife — a slime, a plain body — and simply goes. One that carries
    /// drops (`Carried::drops`, a felled tree's logs) is never lost
    /// silently: its drops are put down as item piles at the seam cell on
    /// the side of the region it left (the cell under its body, clamped into
    /// that region), by region-sourced `DepositItems` intents — the Task 6.3
    /// deposit path, so a parked source gets them when it next runs and what
    /// its piles cannot hold is counted as destroyed there.
    pending_arrivals: BTreeMap<RegionCoords, Vec<PendingArrival>>,
    /// Waiting arrivals despawned past [`PENDING_ARRIVALS_PER_REGION_MAX`]
    /// (the `[server]` counters line), the latest of their ids
    /// ([`DESPAWNED_ARRIVAL_IDS_KEPT`]), and the targets warned about since
    /// they last started (one warning per region per burst).
    arrivals_despawned: u64,
    recently_despawned: std::collections::VecDeque<crate::EntityId>,
    despawn_warned: BTreeSet<RegionCoords>,
    /// Replies given up (no running, parked or stopping destination, or past
    /// `MAX_INTENT_REPLY_HOPS`): each logged at warn.
    intent_replies_lost: u64,
    /// The bodies of parked players, by `EntityId`: a reply carrying items
    /// to one of them goes into the player's parked record. Dropped when
    /// the player is restored (its new body has a new id).
    parked_bodies: BTreeMap<crate::EntityId, PlayerId>,
    /// Items in replies that had nowhere to go (logged at warn).
    items_destroyed: u64,
    /// Each running region's incarnation: a fresh number per
    /// `ensure_running`, announced on the output channel by
    /// `RegionOutput::Incarnation` just before the runner is spawned.
    incarnation: BTreeMap<RegionCoords, u64>,
    /// The last incarnation marker seen on the output channel, per region:
    /// while it is not the current one, the outputs read are a dead runner's
    /// late ones and are dropped.
    incarnation_seen: BTreeMap<RegionCoords, u64>,
    next_incarnation: u64,
    /// The session with each other host of the world (`peers.rs`, [`Self::set_peer`]).
    /// Empty for a single host.
    peers: BTreeMap<u32, peers::Peer>,
    /// Reliable messages for a host that has no session yet, in order: queued into its
    /// session by [`Self::set_peer`]. Carried by cuts like a session's unreceived ones.
    peer_waiting: BTreeMap<u32, Vec<crate::Reliable>>,
    /// Transfers to other hosts that departed after their source answered the pending
    /// cut (its snapshot still holds them): not rows of that cut. Cleared by every
    /// `begin_cut`, like [`InFlight::in_source_view`].
    peer_in_source_view: BTreeSet<crate::EntityId>,
    /// Intents and replies to other hosts sent after their source answered the pending
    /// cut (its snapshot predates them): not rows of that cut. One entry per message
    /// (equal messages counted). Cleared by every `begin_cut`.
    peer_intents_in_view: Vec<crate::IntentMsg>,
    /// The regions of other hosts that border one of this host's and run, by their host
    /// (`peers.rs`): what halo traffic to another host is sent to (halo never wakes a
    /// region). Replaced by each [`crate::Volatile::Running`] of that host, kept by its
    /// `RegionUp` / `RegionDown`, emptied when its link goes down.
    foreign_running: BTreeMap<RegionCoords, u32>,
    /// A peer runs a newer world generation ([`Self::take_higher_generation`]).
    higher_generation: Option<u64>,
}

impl<S: RegionSpawner> WorldManager<S> {
    pub fn new(
        spawner: S,
        out: Sender<(Option<ClientId>, ServerPacket)>,
        region_out_send: Sender<(RegionCoords, RegionOutput)>,
        lod_builder: Box<dyn LodBuilder>,
    ) -> Self {
        Self {
            spawner,
            regions: BTreeMap::new(),
            parked: BTreeMap::new(),
            terrain_cache: BTreeMap::new(),
            sessions: BTreeMap::new(),
            homes: BTreeMap::new(),
            world_map: None,
            route_map: None,
            pending_events: BTreeMap::new(),
            world: None,
            world_view: None,
            spawn_waiting: BTreeMap::new(),
            world_live: BTreeSet::new(),
            world_ms: 0,
            world_notices: Vec::new(),
            world_attach: WorldAttach::Down,
            attach_epoch: 0,
            world_generation: 0,
            world_boot: 0,
            next_request_seq: 0,
            pending_requests: BTreeMap::new(),
            expired_requests: BTreeMap::new(),
            outbox: VecDeque::new(),
            world_request_timeout_ms: WORLD_REQUEST_TIMEOUT_SECS * 1000,
            world_counters: WorldLinkCounters::default(),
            site_regions: BTreeMap::new(),
            society_grid: None,
            fortress_of: BTreeMap::new(),
            adventurer_of: BTreeMap::new(),
            player_of: BTreeMap::new(),
            parking: BTreeMap::new(),
            moving: BTreeMap::new(),
            parked_players: BTreeMap::new(),
            awaiting_park: BTreeMap::new(),
            deferred_packets: BTreeMap::new(),
            parked_bodies: BTreeMap::new(),
            items_destroyed: 0,
            clock_ms: 0,
            unavailable: BTreeMap::new(),
            ownership: (crate::Ownership::AllInOne, crate::DEFAULT_SUPER_REGION_REGIONS),
            creating: BTreeMap::new(),
            out,
            region_out_send,
            lod_subscribers: BTreeMap::new(),
            lod_cache: BTreeMap::new(),
            lod_cache_max: LOD_CACHE_MAX_ENTRIES,
            lod_access_counter: 0,
            lod_in_flight: BTreeSet::new(),
            lod_submitted_at: BTreeMap::new(),
            perf: ManagerPerfCounters::default(),
            lod_builder,
            directive_stamps: BTreeMap::new(),
            in_flight: BTreeMap::new(),
            pending_cut: None,
            ready_cut: None,
            outstanding_cuts: BTreeMap::new(),
            uncommitted: BTreeMap::new(),
            uncommitted_seq: 0,
            cut_lead: SAVE_CUT_LEAD_TICKS,
            next_cut_id: 0,
            cut_order: 0,
            silent_stopping: BTreeSet::new(),
            last_cut_caught: Vec::new(),
            stored_regions: BTreeMap::new(),
            locations: BTreeMap::new(),
            in_flight_intents: Vec::new(),
            intent_mailbox: BTreeMap::new(),
            pending_arrivals: BTreeMap::new(),
            arrivals_despawned: 0,
            recently_despawned: std::collections::VecDeque::new(),
            despawn_warned: BTreeSet::new(),
            intent_replies_lost: 0,
            incarnation: BTreeMap::new(),
            incarnation_seen: BTreeMap::new(),
            next_incarnation: 0,
            peers: BTreeMap::new(),
            peer_waiting: BTreeMap::new(),
            peer_in_source_view: BTreeSet::new(),
            peer_intents_in_view: Vec::new(),
            foreign_running: BTreeMap::new(),
            higher_generation: None,
        }
    }

    /// Install the world map; connecting clients receive it, Play checks
    /// targets against it, and every region is bounded by its extent and
    /// routes walks over it (`RegionInput::SetRouteMap`): nothing crosses the
    /// world's outer seam.
    pub fn set_world_map(&mut self, map: WorldMap) {
        let route = std::sync::Arc::new(crate::RouteMap::from_world_map(&map));
        self.society_grid = Some(SocietyGrid::from_world_map(&map));
        self.world_map = Some(map);
        self.route_map = Some(route.clone());
        let running: Vec<RegionCoords> = self.regions.keys().copied().collect();
        for rc in running {
            self.send_to_region(rc, RegionInput::SetRouteMap(Some(route.clone())));
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
        if self.refuses_play(target) {
            return refuse(self, PlayRefusal::RegionUnavailable);
        }
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
        if let (Some(record), Some((player, fortress, adventure))) = (&record, self.player_for(client)) {
            // In no region while its creation is on its way to the target.
            self.cut_transit(player, &PlayerRecord { fortress, adventure, ..(**record).clone() });
        }
        let full = match (&record, self.player_for(client)) {
            (Some(record), Some((player, fortress, adventure))) => {
                Some((player, PlayerRecord { fortress, adventure, ..(**record).clone() }))
            }
            _ => None,
        };
        if self.refuses_play(m.target) {
            // Its target could not be loaded meanwhile: the record stays parked.
            if let Some((player, record)) = full {
                log::warn!("move of client {client}: {:?} is unavailable; player {} parked", m.target, player.0);
                self.parked_players.insert(player, record);
            }
            return;
        }
        let carried = match record {
            Some(record) => record.client,
            None => {
                log::error!("move of client {client} out of {:?}: no body came back; its items are lost", m.from);
                crate::Client::default()
            }
        };
        if let Some((player, record)) = full {
            self.creating.insert(client, (m.target, player, record));
        }
        self.send_play_event(m.target, GameEventKind::CreateClientAt(client, m.landing, Some(Box::new(carried))));
        // Behind the creation, held with it if it is.
        for kind in m.then {
            self.send_play_event_held(m.target, kind, true);
        }
        self.refresh_keepalive(m.from, now_ms);
        self.refresh_keepalive(m.target, now_ms);
    }

    /// Where a player lands in `rc`: the world map tile's dry landing, else the society
    /// grid's, else the region's centre.
    fn landing_in(&self, rc: RegionCoords) -> [u32; 2] {
        let edge = crate::REGION_BLOCKS as u32;
        let on_map = self.world_map.as_ref().and_then(|m| m.tile(rc)).and_then(|t| t.landing);
        let on_grid = self.society_grid.as_ref().and_then(|g| g.landing(rc));
        on_map
            .or(on_grid)
            .filter(|l| l[0] < edge && l[1] < edge)
            .unwrap_or([edge / 2, edge / 2])
    }

    /// Deliver a server-authored Play event to `rc`, waking it if parked. A
    /// region mid-stop cannot take events (they would be dropped): queue them
    /// for when it parks.
    ///
    /// An event that puts a player's body in `rc` (create, restore) is held
    /// to the slot after a pending save cut's tick while `rc` has not
    /// answered it: the body came out of a park, which may have landed after
    /// its old region's snapshot, so it must not be in `rc`'s too.
    fn send_play_event(&mut self, rc: RegionCoords, kind: GameEventKind) {
        let creates = matches!(
            kind,
            GameEventKind::CreateClient(_) | GameEventKind::CreateClientAt(..) | GameEventKind::RestorePlayer(..)
        );
        self.send_play_event_held(rc, kind, creates);
    }

    /// [`Self::send_play_event`]; `hold_for_cut`: held as a body's creation is
    /// (events that must follow a held creation).
    fn send_play_event_held(&mut self, rc: RegionCoords, kind: GameEventKind, hold_for_cut: bool) {
        self.ensure_running(rc);
        let input = match self.cut_hold_stamp(rc).filter(|_| hold_for_cut) {
            Some(stamp) => RegionInput::Transfer(kind, stamp),
            None => RegionInput::Event(kind),
        };
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
            ServerEvent::Supersede { player, reason } => self.supersede(player, &reason, now_ms),
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
                ClientPacket::RequestLod(key) => self.lod_subscribe(id, key),
                ClientPacket::ReleaseLod(key) => self.lod_unsubscribe(id, key),
                ClientPacket::PlayRegion(rc) => self.play_region(id, rc, now_ms),
                ClientPacket::Leave => self.client_disconnected(id, now_ms),
                ClientPacket::RequestEmbarkOptions if self.world.is_some() => {
                    self.world_request(id, WorldRequest::EmbarkOptions, now_ms);
                }
                ClientPacket::RequestEmbarkOptions => {
                    let _ = self.out.send((Some(id), ServerPacket::EmbarkOptions(Default::default())));
                }
                ClientPacket::Embark(req) => self.embark(id, req, now_ms),
                ClientPacket::Retire => self.retire(id, now_ms),
                ClientPacket::ReturnTo(site) => self.return_to(id, site, now_ms),
                ClientPacket::StartAdventure(site) => self.start_adventure(id, site, now_ms),
                ClientPacket::EndAdventure => self.end_adventure(id, now_ms),
                ClientPacket::RequestPersonInfo(person) => {
                    if self.world.is_some() {
                        self.world_request(id, WorldRequest::PersonInfo(person), now_ms);
                    }
                }
                ClientPacket::RequestLegends(query) => {
                    if self.world.is_some() {
                        self.world_request(id, WorldRequest::Legends(query), now_ms);
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
                    | GameEventKind::Order(..)
                    | GameEventKind::Prefab(..)) => {
                        // Manager-authoritative routing: the client's stamp
                        // lags its own predicted handoff; homes is truth.
                        // UiAction and Order ride the same path as
                        // PlayerInput — all must follow the player across a
                        // region handoff.
                        let cid = kind.origin_client().expect("PlayerInput/UiAction/Order/Prefab carry a client");
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
                    | GameEventKind::HaloEntity(_)
                    | GameEventKind::HaloEntityGone(_)
                    | GameEventKind::HaloSourceGone(_)
                    | GameEventKind::HaloUpdate(_)
                    | GameEventKind::Intent(_)
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
        self.flush_world();
        self.route_notices();
        true
    }

    pub fn handle_region_output(&mut self, rc: RegionCoords, output: RegionOutput, now_ms: u64) {
        self.region_output(rc, output, now_ms);
        // A park or loss may have ended the last stop an attach being gathered waits for.
        if matches!(self.world_attach, WorldAttach::Gathering { .. }) {
            self.try_finish_attach();
        }
    }

    fn region_output(&mut self, rc: RegionCoords, output: RegionOutput, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        if let RegionOutput::Incarnation(n) = output {
            self.incarnation_seen.insert(rc, n);
            return;
        }
        // A runner that died and was respawned at once can leave outputs in
        // the channel: they precede the new runner's marker. (No marker seen
        // yet = the region's first runner, or an output injected by a test.)
        if let Some(seen) = self.incarnation_seen.get(&rc) {
            if self.incarnation.get(&rc) != Some(seen) {
                log::info!("[server] late output of a dead incarnation of {rc:?} dropped: {}", output.name());
                return;
            }
        }
        match output {
            RegionOutput::Incarnation(_) => unreachable!("handled above"),
            RegionOutput::EventProcessed(event) => {
                if let GameEventKind::Intent(msg) = &event.kind {
                    if let Some(i) = self.in_flight_intents.iter().position(|f| f.target == rc && f.msg == *msg) {
                        let f = self.in_flight_intents.remove(i);
                        self.cut_confirmed_intent(f);
                    }
                }
                if let GameEventKind::CreateClientAt(c, ..) | GameEventKind::RestorePlayer(c, _) = &event.kind {
                    // The body exists: its record is no longer only in transit.
                    if self.creating.get(c).is_some_and(|(target, ..)| *target == rc) {
                        self.creating.remove(c);
                    }
                }
                if let GameEventKind::EntityArrived(bundle) = &event.kind {
                    if let EntityKind::Person { person } = bundle.kind {
                        self.note_moved(person, rc, Step::Into { to: rc, at: bundle_at(bundle) });
                    }
                    if self.in_flight.get(&bundle.id).is_some_and(|f| f.target == rc) {
                        let f = self.in_flight.remove(&bundle.id).expect("checked");
                        self.cut_confirmed_arrival(f);
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
            RegionOutput::CutSnapshot(id, tick, portable) => self.cut_answer(rc, Some((id, tick)), *portable),
            RegionOutput::CutMissed(id) => self.cut_missed(rc, id),
            RegionOutput::LoadFailed(why) => self.region_load_failed(rc, why, now_ms),
            RegionOutput::PeopleListed(epoch, people) => self.note_listed(rc, Some(epoch), &people),
            RegionOutput::PeoplePositions(people) => {
                if self.world_live.contains(&rc) {
                    self.world_send(HostMsg::Positions(people));
                }
            }
            RegionOutput::PlayerParked(client, record, body) => self.player_parked(rc, client, record, body, now_ms),
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
            RegionOutput::PeopleParked(people) => self.world_region_parked(rc, people),
            RegionOutput::Stopped(serialized, far, stamp, portable) => {
                self.silent_stopping.remove(&rc);
                // Its final snapshot: uncommitted until a cut commits it, and
                // its answer to a pending cut (it is parked at the cut tick).
                self.cut_answer(rc, None, *portable);
                // `PeopleParked` comes first; a region that sent none gave nobody back.
                if self.world_live.contains(&rc) {
                    self.history_region_lost(rc);
                }
                // Its final terrain reseeds the running regions that mirror
                // it: what it changed since its last halo deltas (edits of
                // its last slot, transfers applied at `Shutdown`) never went
                // out as deltas. Stream-borne, so their clients replay it.
                // Stamped with its final tick, like those deltas: a lagging
                // neighbour still holding one applies the reseed after it.
                for n in crate::halo::neighbours(rc) {
                    if self.is_foreign(n) {
                        // Another host's: only when it runs it (`foreign_running`).
                        if let Some(seed) = crate::halo::halo_seed_from_snapshot(rc, n, &far) {
                            self.halo_to_peer(n, crate::Volatile::HaloUpdate { to: n, update: seed, stamp });
                        }
                        continue;
                    }
                    let running = self.regions.get(&n).is_some_and(|l| !l.stopping);
                    if !running {
                        continue;
                    }
                    if let Some(seed) = crate::halo::halo_seed_from_snapshot(rc, n, &far) {
                        self.send_to_region(n, RegionInput::Transfer(GameEventKind::HaloUpdate(seed), stamp));
                    }
                }
                // After its final halo: the other hosts' mirrors of it end.
                self.announce_region(rc, false);
                self.terrain_cache.insert(rc, far);
                // What it held parks with it (its next run reports it again).
                self.forget_locations(rc);
                self.parked.insert(rc, serialized);
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
                // Departed after `rc` answered the pending cut: its snapshot
                // still holds them (`save_cut.rs`).
                let view = self.in_cut_view(rc);
                for (mut bundle, target) in list {
                    let client = bundle.client.as_ref().map(|(c, _)| *c);
                    let off_map = crate::off_map(self.map_extent(), target);
                    // Bodies that would wake their target: a player, a person no
                    // history absorbs. Neither crosses to another host yet (Task 7).
                    let waking = client.is_some()
                        || (matches!(bundle.kind, EntityKind::Person { .. }) && self.world.is_none());
                    if off_map || (waking && (self.is_unavailable(target) || self.is_foreign(target))) {
                        // Off the map: regions hold bodies at the outer seam, so
                        // this is a region that was not bounded. Unavailable: its
                        // stored state could not be read, so it cannot take the
                        // body. Foreign: another host owns it. Either way the
                        // crossing is refused and the entity handed back to where
                        // it left, on its seam.
                        if off_map {
                            log::error!("departure from {rc:?} into off-map {target:?} refused: returned");
                        } else {
                            log::warn!("departure from {rc:?} into unavailable {target:?} refused: returned");
                        }
                        let t = &mut bundle.body.pose.translation;
                        t.x = t.x.clamp(0.0, crate::REGION_SIZE);
                        t.z = t.z.clamp(0.0, crate::REGION_SIZE);
                        self.enter_in_flight(rc, rc, stamp, &bundle, view);
                        self.locations.insert(bundle.id, rc);
                        self.send_to_region(rc, RegionInput::Transfer(GameEventKind::EntityArrived(bundle), stamp));
                        continue;
                    }
                    // Where it left, in `rc`'s frame (a person absorbed at the seam is
                    // listed there, bit-exact: a rebase there and back is not).
                    let left = bundle_at(&bundle);
                    bundle.rebase(rc, target);
                    if self.is_foreign(target) {
                        // Another host's region: to its owner, reachable or not (it
                        // waits in the session); that host decides the arrival.
                        self.depart_to_peer(rc, target, stamp, bundle, view);
                        continue;
                    }
                    self.accept_arrival(Some((rc, left)), target, stamp, bundle, now_ms);
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
                    home => log::info!("[server] forward of {kind:?} from {rc:?} dropped: home {home:?}"),
                }
            }
            RegionOutput::HaloDeltas(list, stamp) => {
                for (update, target) in list {
                    if self.is_foreign(target) {
                        // Only to a region its owner runs (`foreign_running`).
                        self.halo_to_peer(target, crate::Volatile::HaloUpdate { to: target, update, stamp });
                        continue;
                    }
                    // Halo updates never wake a region (a starting one is
                    // seeded afresh).
                    let running = self.regions.get(&target).is_some_and(|link| !link.stopping);
                    if running {
                        self.send_to_region(target, RegionInput::Transfer(GameEventKind::HaloUpdate(update), stamp));
                    }
                }
            }
            RegionOutput::Spawned(ids) => {
                if self.regions.contains_key(&rc) {
                    for id in ids {
                        self.locations.insert(id, rc);
                    }
                }
            }
            RegionOutput::Despawned(ids) => {
                for id in ids {
                    if self.locations.get(&id) == Some(&rc) {
                        self.locations.remove(&id);
                    }
                }
            }
            RegionOutput::Intents(list) => {
                // Sent after `rc` answered the pending cut: its snapshot
                // predates them (`save_cut.rs`).
                let view = self.in_cut_view(rc);
                for msg in list {
                    self.route_intent(msg, rc, view);
                }
            }
            RegionOutput::HaloEntities(list, stamp) => {
                // Late output of a region already lost is dropped: its
                // mirrors were ended (`halo_source_lost`); none comes back.
                let list = if self.regions.contains_key(&rc) { list } else { Vec::new() };
                for (mut msg, target) in list {
                    if self.is_foreign(target) {
                        msg.rebase(rc, target);
                        self.halo_to_peer(target, crate::Volatile::HaloEntity { to: target, msg, stamp });
                        continue;
                    }
                    // Entity-halo messages NEVER wake a region (a starting one
                    // gets the band entities from its neighbours' next ticks).
                    let running = self.regions.get(&target).is_some_and(|link| !link.stopping);
                    if !running {
                        continue;
                    }
                    msg.rebase(rc, target);
                    self.send_to_region(target, RegionInput::Transfer(msg.into_event(), stamp));
                }
            }
        }
        // After the arrivals above: a directive for a delivered person (walk on) must
        // reach the target behind the body (`directive_stamps`).
        self.flush_world();
    }

    /// The world coordinator's calendar moves to `now_ms` (its decisions go to running
    /// regions), then grace-period unloads: a region with no subscribers and no
    /// connected client's player entity parks after UNLOAD_GRACE_MS.
    pub fn maintain(&mut self, now_ms: u64) {
        self.pump_peers(now_ms);
        self.clock_ms = self.clock_ms.max(now_ms);
        self.world_ms = self.world_ms.max(now_ms);
        self.expire_overdue_cut();
        self.retry_unavailable();
        self.expire_world_requests(now_ms);
        self.warn_attach_stragglers(now_ms);
        // A stop ended by a loss found by the upkeep (a dead region thread).
        self.try_finish_attach();
        self.pump_world(self.world_ms);
        self.route_notices();
        let connected_homes: BTreeSet<RegionCoords> = self
            .sessions
            .keys()
            .filter_map(|c| self.homes.get(c).copied())
            .collect();
        // No keep-alive expiry while an `Attach` is gathered: it waits for every stopping
        // region to park, so new stops could postpone it indefinitely. The grace resumes
        // once attached (a dead region thread still ends its region, above and below).
        let gathering = matches!(self.world_attach, WorldAttach::Gathering { .. });
        let expired: Vec<RegionCoords> = self
            .regions
            .iter()
            .filter(|(rc, link)| {
                !gathering
                    && !link.stopping
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
                self.cut_region_lost(rc);
                self.regions.remove(&rc);
                self.pending_events.remove(&rc);
                self.spawner.reap(rc);
                self.halo_source_lost(rc);
                self.announce_region(rc, false);
                self.forget_locations(rc);
                self.history_region_lost(rc);
                let queued = self.restart_for_in_flight(rc);
                self.settle_lost_parks(rc, &queued);
            } else {
                self.world_region_stopping(rc);
            }
        }
        // Anything a lost region's people decided on their return.
        self.flush_world();
    }

    pub fn shutdown_all(&mut self) {
        let mut dead = Vec::new();
        let mut stopping = Vec::new();
        for (rc, link) in self.regions.iter_mut() {
            if !link.stopping {
                link.stopping = true;
                if link.input.send(RegionInput::Shutdown).is_err() {
                    log::error!("region {:?} died before shutdown", rc);
                    dead.push(*rc);
                } else {
                    stopping.push(*rc);
                }
            }
        }
        for rc in stopping {
            self.world_region_stopping(rc);
        }
        for rc in dead {
            self.cut_region_lost(rc);
            self.regions.remove(&rc);
            self.pending_events.remove(&rc);
            self.spawner.reap(rc);
            // Every other region is stopping: a no-op unless one is not.
            self.halo_source_lost(rc);
            self.announce_region(rc, false);
            self.forget_locations(rc);
            self.history_region_lost(rc);
            // Woken for what was in flight to it; the exit loop stops it again.
            let queued = self.restart_for_in_flight(rc);
            self.settle_lost_parks(rc, &queued);
        }
    }

    /// Start a save cut at world tick `now + lead` (`save_cut.rs`): every
    /// running region is asked for its snapshot at the opening of that slot
    /// (`SaveAt`), a stopping one answers with its `Stopped`, and the
    /// uncommitted snapshots of the regions not running are carried as they
    /// are. `None` while a cut is pending or a complete one waits for
    /// [`Self::take_cut`].
    pub fn begin_cut(&mut self, now: crate::Tick) -> Option<CutId> {
        self.expire_overdue_cut();
        if self.pending_cut.is_some() || self.ready_cut.is_some() {
            return None;
        }
        self.next_cut_id += 1;
        let id = CutId(self.next_cut_id);
        let tick = now + self.cut_lead;
        let mut cut = PendingCut::new(id, tick, tick + CUT_DEADLINE_LEADS * self.cut_lead);
        // What was routed before now left its source before the cut tick.
        for f in self.in_flight.values_mut() {
            f.in_source_view = false;
        }
        for f in &mut self.in_flight_intents {
            f.in_source_view = false;
        }
        for p in self.pending_arrivals.values_mut().flatten() {
            p.in_source_view = false;
        }
        for m in self.intent_mailbox.values_mut().flatten() {
            m.in_source_view = false;
        }
        // Transfers to other hosts, too.
        self.peer_in_source_view.clear();
        self.peer_intents_in_view.clear();
        let mut dead = Vec::new();
        for (rc, link) in &self.regions {
            if link.stopping && self.silent_stopping.contains(rc) {
                // Missed an earlier cut's deadline while stopping (its thread
                // is likely dead): at the cut tick as it last snapshotted.
                if let Some(u) = self.uncommitted.get(rc) {
                    cut.regions.insert(*rc, (u.region.clone(), u.seq));
                }
            } else if link.stopping {
                // It parks before the cut tick: its `Stopped` answers (what it
                // applies at `Shutdown` was all sent before this cut began).
                cut.waiting.insert(*rc);
            } else if link.input.send(RegionInput::SaveAt(id, tick)).is_ok() {
                cut.waiting.insert(*rc);
            } else {
                dead.push(*rc);
            }
        }
        // The regions not running are at the cut tick as they parked.
        for (rc, u) in &self.uncommitted {
            if !self.regions.contains_key(rc) {
                cut.regions.insert(*rc, (u.region.clone(), u.seq));
            }
        }
        log::debug!("[server] save cut {} at tick {tick}: {} regions asked", id.0, cut.waiting.len());
        self.pending_cut = Some(cut);
        if !dead.is_empty() {
            // A region thread that died: its state is lost, so this cut cannot
            // hold it. Aborted; the respawn comes with the next send to it.
            log::error!("save cut {}: regions {dead:?} died before they could be asked; aborted", id.0);
            self.pending_cut = None;
            return None;
        }
        self.try_complete_cut();
        Some(id)
    }

    /// The complete cut, once every region asked has answered. Its
    /// uncommitted entries stay until [`Self::cut_committed`].
    pub fn take_cut(&mut self) -> Option<WorldCut> {
        let (cut, carried) = self.ready_cut.take()?;
        self.outstanding_cuts.insert(cut.id, carried);
        Some(cut)
    }

    /// The store committed cut `id`: the uncommitted entries it carried are
    /// cleared (unless a newer snapshot was merged since), and `versions`
    /// (each written region's terrain version) are what the store holds.
    pub fn cut_committed(&mut self, id: CutId, versions: Vec<(RegionCoords, u64)>) {
        if let Some(carried) = self.outstanding_cuts.remove(&id) {
            for (rc, seq) in carried {
                if self.uncommitted.get(&rc).is_some_and(|u| u.seq == seq) {
                    self.uncommitted.remove(&rc);
                }
            }
        } else {
            log::warn!("cut {} committed but not outstanding", id.0);
        }
        self.stored_regions.extend(versions);
    }

    /// The store failed to commit cut `id`: its entries stay uncommitted, so
    /// the next cut carries them again.
    pub fn cut_failed(&mut self, id: CutId) {
        self.outstanding_cuts.remove(&id);
    }

    /// The regions the world store holds, with their terrain versions (set
    /// at startup from the store; kept current by [`Self::cut_committed`]).
    pub fn set_stored_regions(&mut self, regions: BTreeMap<RegionCoords, u64>) {
        self.stored_regions = regions;
    }

    /// See [`Self::set_stored_regions`].
    pub fn stored_regions(&self) -> &BTreeMap<RegionCoords, u64> {
        &self.stored_regions
    }

    /// What a cut caught in flight, put back on its way (a restarted
    /// server, from the store): a crossing waits in its target's arrival
    /// mailbox and an intent or reply in its intent mailbox, both delivered
    /// when the target runs — at once, if it already does. A crossing, intent
    /// or reply for a region another host owns is queued to that host again.
    pub fn restore_in_flight(&mut self, rows: Vec<InFlightRow>) {
        for row in rows {
            match row {
                InFlightRow::Transfer { target, bundle, stamp } if self.is_foreign(target) => {
                    // Another host's region: to its owner again.
                    self.queue_to_peer(
                        self.owner_of(target),
                        crate::Reliable::Transfer { target, stamp, bundle: Box::new(bundle), player: None },
                    );
                }
                InFlightRow::Transfer { target, bundle, stamp } => {
                    self.locations.insert(bundle.id, target);
                    if self.regions.get(&target).is_some_and(|l| !l.stopping) {
                        self.enter_in_flight(target, target, stamp, &bundle, false);
                        self.send_to_region(target, RegionInput::Transfer(GameEventKind::EntityArrived(bundle), stamp));
                    } else {
                        // The same cap as any waiting arrival (cut rows never
                        // hold a player or a person).
                        self.hold_arrival(target, target, stamp, bundle, false);
                    }
                }
                InFlightRow::Intent { target, msg } if self.is_foreign(target) => {
                    // Another host's region: to its owner again.
                    self.queue_to_peer(self.owner_of(target), crate::Reliable::Intent(msg));
                }
                InFlightRow::Intent { target, msg } => {
                    if self.regions.get(&target).is_some_and(|l| !l.stopping) {
                        self.send_intent_to(target, msg, false);
                    } else {
                        self.intent_mailbox.entry(target).or_default().push(Mail { msg, in_source_view: false });
                    }
                }
            }
        }
    }

    /// The lead the next cut is placed at ([`SAVE_CUT_LEAD_TICKS`], doubled
    /// after each missed cut, reset by a complete one).
    pub fn cut_lead(&self) -> crate::Tick {
        self.cut_lead
    }

    /// Test support: the players the last complete cut caught crossing a
    /// seam (in no region snapshot; their records are from the crossing).
    pub fn last_cut_caught_players(&self) -> &[PlayerId] {
        &self.last_cut_caught
    }

    /// Test support: the pending cut's id and tick.
    pub fn pending_cut(&self) -> Option<(CutId, crate::Tick)> {
        self.pending_cut.as_ref().map(|c| (c.id, c.tick))
    }

    /// Test support: the regions the pending cut still waits for.
    pub fn awaiting_cut(&self) -> Vec<RegionCoords> {
        self.pending_cut.as_ref().map(|c| c.waiting.iter().copied().collect()).unwrap_or_default()
    }

    /// Test support: the regions with snapshots not committed yet.
    pub fn uncommitted_regions(&self) -> Vec<RegionCoords> {
        self.uncommitted.keys().copied().collect()
    }

    /// Whether `rc`'s view of the pending cut is taken (answered, or not
    /// running when the cut began): what it sends now is not in the cut.
    /// `false` with no cut pending.
    fn in_cut_view(&self, rc: RegionCoords) -> bool {
        self.pending_cut.as_ref().is_some_and(|c| !c.waiting.contains(&rc))
    }

    /// The stamp a body's creation in `rc` is held to: the slot after the
    /// pending cut's tick, while `rc` has not answered it.
    fn cut_hold_stamp(&self, rc: RegionCoords) -> Option<crate::Tick> {
        let cut = self.pending_cut.as_ref()?;
        cut.waiting.contains(&rc).then_some(cut.tick + 1)
    }

    /// Merge `portable`, `rc`'s newest snapshot, into its uncommitted entry;
    /// returns the entry's new `seq`.
    fn merge_uncommitted(&mut self, rc: RegionCoords, portable: PortableRegion) -> u64 {
        self.uncommitted_seq += 1;
        let seq = self.uncommitted_seq;
        match self.uncommitted.get_mut(&rc) {
            Some(u) => {
                u.region.merge(portable);
                u.seq = seq;
            }
            None => {
                self.uncommitted.insert(rc, Uncommitted { region: portable, seq });
            }
        }
        seq
    }

    /// `rc` sent a snapshot: a `CutSnapshot` (`cut` = its id and tick) or its
    /// `Stopped` one (`None`). Always merged into the uncommitted entry (a
    /// stale cut's too: the region's terms moved on); the answer of the
    /// pending cut when it waits for `rc`.
    fn cut_answer(&mut self, rc: RegionCoords, cut: Option<(CutId, crate::Tick)>, portable: PortableRegion) {
        let players = portable.players.clone();
        let at = portable.meta.tick;
        let seq = self.merge_uncommitted(rc, portable);
        let Some(pending) = self.pending_cut.as_ref() else { return };
        let answers = pending.waiting.contains(&rc) && cut.is_none_or(|(id, _)| id == pending.id);
        if !answers {
            return;
        }
        debug_assert!(cut.is_none_or(|(_, t)| t == pending.tick), "a cut snapshot at another tick");
        if cut.is_none() && at > pending.tick {
            // A `Stopped` answer past the cut tick is not the region at it.
            let id = self.pending_cut.take().expect("checked").id;
            log::error!("save cut {}: {rc:?} parked at tick {at}, past the cut tick; aborted", id.0);
            return;
        }
        let mut records = Vec::new();
        for (client, record) in players {
            let Some((player, fortress, adventure)) = self.player_for(client) else {
                log::warn!("save cut: {rc:?} holds client {client}'s body, but no player is known for it; not saved");
                continue;
            };
            self.cut_order += 1;
            records.push((self.cut_order, player, PlayerRecord { fortress, adventure, ..record }));
        }
        let entry = self.uncommitted[&rc].region.clone();
        let pending = self.pending_cut.as_mut().expect("checked");
        pending.waiting.remove(&rc);
        pending.regions.insert(rc, (entry, seq));
        pending.region_players.insert(rc, records);
        self.try_complete_cut();
    }

    /// A pending cut past its deadline ([`CUT_DEADLINE_LEADS`]) is aborted;
    /// the stopping regions it still waited for are left out of later cuts
    /// until they park. Called by [`Self::maintain`] and [`Self::begin_cut`].
    fn expire_overdue_cut(&mut self) {
        let now = self.spawner.world_tick();
        if self.pending_cut.as_ref().is_none_or(|c| now <= c.deadline) {
            return;
        }
        let cut = self.pending_cut.take().expect("checked");
        log::error!(
            "save cut {} at tick {}: still waiting for {:?} at world tick {now}; aborted",
            cut.id.0,
            cut.tick,
            cut.waiting
        );
        for rc in cut.waiting {
            if self.regions.get(&rc).is_some_and(|l| l.stopping) {
                self.silent_stopping.insert(rc);
            }
        }
    }

    /// The orderly exit stops waiting for the regions still stopping (a
    /// wall-time deadline: the world tick may not move at exit): they are left
    /// out of cuts as after a missed deadline (`silent_stopping` — a later cut
    /// carries their last uncommitted snapshot, if any), and a pending cut
    /// still waiting for one of them is aborted, so the exit's own cut can
    /// save the rest. Returns those regions.
    pub fn give_up_unparked(&mut self) -> Vec<RegionCoords> {
        let stuck: Vec<RegionCoords> =
            self.regions.iter().filter(|(_, l)| l.stopping).map(|(rc, _)| *rc).collect();
        self.silent_stopping.extend(stuck.iter().copied());
        if self.pending_cut.as_ref().is_some_and(|c| stuck.iter().any(|rc| c.waiting.contains(rc))) {
            let cut = self.pending_cut.take().expect("checked");
            log::error!(
                "save cut {} at tick {}: still waiting for {:?} at exit; aborted",
                cut.id.0,
                cut.tick,
                cut.waiting
            );
        }
        stuck
    }

    /// `rc` was past the pending cut's tick when asked: the cut is aborted
    /// and the next one doubles its lead.
    fn cut_missed(&mut self, rc: RegionCoords, id: CutId) {
        if self.pending_cut.as_ref().is_none_or(|c| c.id != id) {
            return;
        }
        let tick = self.pending_cut.as_ref().map_or(0, |c| c.tick);
        self.pending_cut = None;
        self.cut_lead = (self.cut_lead * 2).min(SAVE_CUT_LEAD_MAX_TICKS);
        log::warn!("save cut {} at tick {tick}: {rc:?} was already past it; aborted (next lead {})", id.0, self.cut_lead);
    }

    /// `rc` was lost (its thread died): a pending cut is aborted — what the
    /// region held since it answered (or would have) died with it, and its
    /// respawn is not the state the cut saw.
    fn cut_region_lost(&mut self, rc: RegionCoords) {
        if let Some(cut) = self.pending_cut.take() {
            log::error!("save cut {}: {rc:?} was lost while it was pending; aborted", cut.id.0);
        }
    }

    /// A region started (`ensure_running`) at world tick `start`: a pending
    /// cut whose tick it has not passed asks it too (its answer replaces a
    /// `Stopped` one).
    fn cut_region_started(&mut self, rc: RegionCoords, start: crate::Tick, input: &Sender<RegionInput>) {
        let Some(cut) = self.pending_cut.as_mut() else { return };
        if start <= cut.tick && input.send(RegionInput::SaveAt(cut.id, cut.tick)).is_ok() {
            cut.waiting.insert(rc);
        }
    }

    /// A record passing through the manager (park, restore, move) while a
    /// cut is pending: the cut's fallback for a player in no snapshot.
    fn cut_transit(&mut self, player: PlayerId, record: &PlayerRecord) {
        if let Some(cut) = self.pending_cut.as_mut() {
            cut.transit_players.insert(player, record.clone());
        }
    }

    /// The player behind `client` and its claims, for a cut's record: an
    /// online session's, or a park's in flight.
    fn player_for(&self, client: ClientId) -> Option<(PlayerId, Option<SiteId>, Option<SiteId>)> {
        if let Some(&player) = self.player_of.get(&client) {
            return Some((player, self.fortress_of.get(&client).copied(), self.adventurer_of.get(&client).copied()));
        }
        self.parking.get(&client).map(|p| (p.player, p.fortress, p.adventure))
    }

    /// A player crossing in `bundle` (in `target`'s frame) as its record.
    fn crossing_record(&self, target: RegionCoords, bundle: &crate::EntityBundle) -> Option<(PlayerId, PlayerRecord)> {
        let (client, c) = bundle.client.as_ref()?;
        let (player, fortress, adventure) = self.player_for(*client)?;
        let record = PlayerRecord {
            client: crate::parked_client(c),
            pose: bundle.body.pose,
            region: target,
            fortress,
            adventure,
        };
        Some((player, record))
    }

    /// The cut's place for crossing `f`, in neither its source's nor its
    /// target's view: a row, a player's record, or nothing (a person).
    fn cut_crossing(&mut self, f: &InFlight) {
        if matches!(f.bundle.kind, EntityKind::Person { .. }) {
            return;
        }
        if f.bundle.client.is_some() {
            let record = self.crossing_record(f.target, &f.bundle);
            self.cut_order += 1;
            let order = self.cut_order;
            if let (Some((player, record)), Some(cut)) = (record, self.pending_cut.as_mut()) {
                cut.crossing_players.push((order, player, record));
            }
            return;
        }
        if let Some(cut) = self.pending_cut.as_mut() {
            cut.rows.push(InFlightRow::Transfer { target: f.target, bundle: f.bundle.clone(), stamp: f.stamp });
        }
    }

    /// Crossing `f` was just applied by its target: if the target's view of
    /// the pending cut is taken and the source's snapshot did not hold it, the
    /// cut carries it.
    fn cut_confirmed_arrival(&mut self, f: InFlight) {
        if self.pending_cut.is_some() && !f.in_source_view && self.in_cut_view(f.target) {
            self.cut_crossing(&f);
        }
    }

    /// The same for an intent or reply.
    fn cut_confirmed_intent(&mut self, f: InFlightIntent) {
        if !f.in_source_view && self.in_cut_view(f.target) {
            if let Some(cut) = self.pending_cut.as_mut() {
                cut.rows.push(InFlightRow::Intent { target: f.target, msg: f.msg });
            }
        }
    }

    /// Every region asked has answered: the cut is complete — its regions,
    /// what is still on its way and in no snapshot, and the players.
    fn try_complete_cut(&mut self) {
        if self.pending_cut.as_ref().is_none_or(|c| !c.waiting.is_empty()) {
            return;
        }
        // Still in flight and in neither view.
        let crossings: Vec<InFlight> = self.in_flight.values().filter(|f| !f.in_source_view).cloned().collect();
        for f in &crossings {
            self.cut_crossing(f);
        }
        let mut cut = self.pending_cut.take().expect("checked");
        for (target, list) in &self.pending_arrivals {
            for p in list.iter().filter(|p| !p.in_source_view) {
                cut.rows.push(InFlightRow::Transfer { target: *target, bundle: p.bundle.clone(), stamp: p.stamp });
            }
        }
        for f in self.in_flight_intents.iter().filter(|f| !f.in_source_view) {
            cut.rows.push(InFlightRow::Intent { target: f.target, msg: f.msg });
        }
        // Transfers to other hosts not acknowledged yet (`cut_peer_rows`).
        cut.rows.extend(self.cut_peer_rows());
        for (target, list) in &self.intent_mailbox {
            for m in list.iter().filter(|m| !m.in_source_view) {
                cut.rows.push(InFlightRow::Intent { target: *target, msg: m.msg });
            }
        }
        // Players: the latest record of each in the snapshots and crossings,
        // then the parked ones, then those that passed through meanwhile.
        // A crossing the cut caught is in no snapshot (the in-flight rule).
        debug_assert!(
            cut.crossing_players.iter().all(|(_, p, _)| !cut.region_players.values().flatten().any(|(_, q, _)| q == p)),
            "a player crossing in the cut is also in a region snapshot"
        );
        // A body is in one region at the cut tick.
        debug_assert!(
            {
                let mut seen = BTreeSet::new();
                cut.region_players.values().flatten().all(|(_, p, _)| seen.insert(*p))
            },
            "a player's body is in two snapshots of one cut"
        );
        self.last_cut_caught = cut.crossing_players.iter().map(|(_, p, _)| *p).collect();
        let mut ordered: Vec<(u64, PlayerId, PlayerRecord)> =
            cut.region_players.into_values().flatten().chain(cut.crossing_players).collect();
        ordered.sort_by_key(|(order, _, _)| *order);
        let mut players: BTreeMap<PlayerId, PlayerRecord> = BTreeMap::new();
        for (_, player, record) in ordered {
            players.insert(player, record);
        }
        for (player, record) in cut.parked_at_t.iter().chain(self.parked_players.iter()).chain(cut.transit_players.iter()) {
            players.entry(*player).or_insert_with(|| record.clone());
        }
        let carried: BTreeMap<RegionCoords, u64> = cut.regions.iter().map(|(rc, (_, seq))| (*rc, *seq)).collect();
        let world_cut = WorldCut {
            host: self.ownership.0.ordinal(),
            id: cut.id,
            tick: cut.tick,
            regions: cut.regions.into_iter().map(|(rc, (p, _))| (rc, p)).collect(),
            in_flight: cut.rows,
            players: players.into_iter().collect(),
            // The coordinator saves the history itself (durable before visible).
            history: None,
        };
        log::debug!(
            "[server] save cut {} at tick {} complete: {} regions, {} in flight, {} players",
            world_cut.id.0,
            world_cut.tick,
            world_cut.regions.len(),
            world_cut.in_flight.len(),
            world_cut.players.len()
        );
        self.ready_cut = Some((world_cut, carried));
        self.cut_lead = SAVE_CUT_LEAD_TICKS;
    }

    /// Crossings routed but not yet confirmed by their target (the `[server]`
    /// counters line).
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Non-player arrivals waiting in the mailbox for `rc` to run.
    pub fn pending_arrival_count(&self, rc: RegionCoords) -> usize {
        self.pending_arrivals.get(&rc).map_or(0, Vec::len)
    }

    /// Every arrival waiting in the mailbox (the `[server]` counters line).
    pub fn pending_arrival_total(&self) -> usize {
        self.pending_arrivals.values().map(Vec::len).sum()
    }

    /// Waiting arrivals despawned past [`PENDING_ARRIVALS_PER_REGION_MAX`].
    pub fn arrivals_despawned(&self) -> u64 {
        self.arrivals_despawned
    }

    /// Test support: the latest despawned arrivals' ids, oldest first.
    pub fn recently_despawned_arrivals(&self) -> Vec<crate::EntityId> {
        self.recently_despawned.iter().copied().collect()
    }

    /// Test support: every arrival waiting in the mailbox, with its target.
    pub fn pending_arrival_ids(&self) -> Vec<(crate::EntityId, RegionCoords)> {
        self.pending_arrivals.iter().flat_map(|(rc, list)| list.iter().map(|p| (p.bundle.id, *rc))).collect()
    }

    /// `bundle` (in `target`'s frame), departed from `source` at `stamp`,
    /// waits for `target`, which is not running. `in_source_view`: departed
    /// after `source` answered the pending cut.
    fn hold_arrival(
        &mut self,
        source: RegionCoords,
        target: RegionCoords,
        stamp: crate::Tick,
        bundle: crate::EntityBundle,
        in_source_view: bool,
    ) {
        debug_assert!(
            bundle.client.is_none() && !matches!(bundle.kind, EntityKind::Person { .. }),
            "players and people never wait in the mailbox"
        );
        self.locations.insert(bundle.id, target);
        let list = self.pending_arrivals.entry(target).or_default();
        list.push(PendingArrival { source, stamp, bundle, in_source_view });
        if list.len() <= PENDING_ARRIVALS_PER_REGION_MAX {
            return;
        }
        // Full: the oldest waiting arrival makes room.
        let gone = list.remove(0);
        self.arrivals_despawned += 1;
        self.locations.remove(&gone.bundle.id);
        if self.recently_despawned.len() == DESPAWNED_ARRIVAL_IDS_KEPT {
            self.recently_despawned.pop_front();
        }
        self.recently_despawned.push_back(gone.bundle.id);
        if self.despawn_warned.insert(target) {
            log::warn!(
                "[server] arrivals waiting for {target:?} past {PENDING_ARRIVALS_PER_REGION_MAX}: the oldest are despawned ({:?} first)",
                gone.bundle.kind
            );
        }
        self.put_down_drops(gone.source, target, gone.bundle, gone.in_source_view);
    }

    /// A stud body departing `source` into `target`, which is not running:
    /// settled at once instead of waiting in the mailbox — its drops (a
    /// `DropItems` body's, in `Carried::drops`) put down at the seam cell on
    /// the source's side, like a mailbox overflow's ([`Self::put_down_drops`]);
    /// a `Despawn` body leaves nothing.
    fn settle_at_seam(&mut self, source: RegionCoords, target: RegionCoords, bundle: crate::EntityBundle, view: bool) {
        log::debug!("[server] stud body {:?} into {target:?}, not running: settled at the seam", bundle.id);
        self.locations.remove(&bundle.id);
        self.put_down_drops(source, target, bundle, view);
    }

    /// `bundle`, a body from `source` gone on its way to `target` (a waiting
    /// arrival despawned past the cap, or a stud body settled at the seam),
    /// leaves its drops (if any) as piles at the seam cell on its source's side:
    /// the cell under its body, clamped into the source (see
    /// `pending_arrivals`). Region-sourced deposits, so a refusal (the
    /// source not running) comes back to the source, which puts them down
    /// itself when it next runs. `view`: the body was in its source's view of
    /// the pending cut (its deposits are then not in the cut either).
    fn put_down_drops(&mut self, source: RegionCoords, target: RegionCoords, mut bundle: crate::EntityBundle, view: bool) {
        if bundle.carried.drops.is_empty() {
            return;
        }
        bundle.rebase(target, source);
        let t = bundle.body.pose.translation;
        let b = crate::BLOCK_UNITS as f32;
        let n = crate::REGION_BLOCKS as i32;
        // Pile cells are blocks, or on stud terrain tiles (as wide, 1.2 m tall): a
        // stud body's drops go to the tile under it.
        let cell_height = match bundle.carried.stud_body {
            Some(_) => crate::tile_height_units(),
            None => b,
        };
        let local = [(t.x / b).floor() as i32, (t.y / cell_height).floor() as i32, (t.z / b).floor() as i32];
        let cell = crate::WorldBlockPos::from_local(
            source,
            crate::BlockPos::new(local[0].clamp(0, n - 1), local[1], local[2].clamp(0, n - 1)),
        );
        let due = self.spawner.world_tick();
        log::info!("[server] {:?} gone on its way to {target:?}: its drops {:?} put down at {cell:?}", bundle.id, bundle.carried.drops);
        for stack in bundle.carried.drops {
            let sent = crate::SentIntent {
                target: crate::Target::Pile(cell),
                intent: crate::Intent::DepositItems {
                    cell,
                    stack,
                    expire: due + crate::PILE_DESPAWN_TICKS,
                    pickup: due + crate::PICKUP_DELAY_TICKS,
                    by: crate::EntityId::of_region(source),
                },
                from: source,
                due,
            };
            self.route_intent(crate::IntentMsg::Intent(sent), source, view);
        }
    }

    /// `rc` just started: its waiting arrivals go to it as ordinary
    /// crossings in flight, their cut view carried over.
    fn deliver_pending_arrivals(&mut self, rc: RegionCoords) {
        self.despawn_warned.remove(&rc);
        for p in self.pending_arrivals.remove(&rc).unwrap_or_default() {
            let entry = InFlight {
                target: rc,
                stamp: p.stamp,
                bundle: p.bundle.clone(),
                in_source_view: p.in_source_view,
            };
            if let Some(old) = self.in_flight.insert(p.bundle.id, entry) {
                log::error!("{:?} waited for {rc:?} while in flight to {:?}", p.bundle.id, old.target);
            }
            self.locations.insert(p.bundle.id, rc);
            self.send_to_region(rc, RegionInput::Transfer(GameEventKind::EntityArrived(p.bundle), p.stamp));
        }
    }

    /// `bundle` (in `target`'s frame), departed at `stamp` from `source` — a region of
    /// this host with the body's position there (its own frame, taken before the
    /// rebase), or `None`: another host's (an inbound [`crate::Reliable::Transfer`]) —
    /// arrives at `target`, a region of this host. A person with a history is delivered
    /// only into a running region (else absorbed), and the coordinator told so. A player's
    /// or a person's arrival wakes its target; anything else into a region that is not
    /// running waits for it ([`Self::hold_arrival`]; a stud body settles at the seam
    /// instead). Otherwise it is in flight to the target. An inbound transfer is never in
    /// a source's view of the pending cut.
    fn accept_arrival(
        &mut self,
        source: Option<(RegionCoords, [f32; 3])>,
        target: RegionCoords,
        stamp: crate::Tick,
        bundle: crate::EntityBundle,
        now_ms: u64,
    ) {
        let (source, left) = (source.map(|(rc, _)| rc), source.map(|(_, at)| at));
        let view = source.is_some_and(|s| self.in_cut_view(s));
        // Where drops of a body that never arrives are put down: its source's side of the
        // seam, or (inbound) the target's own cell.
        let from = source.unwrap_or(target);
        let client = bundle.client.as_ref().map(|(c, _)| *c);
        if let (EntityKind::Person { person }, true) = (bundle.kind, self.world.is_some()) {
            // A person is delivered only into a running region; the history
            // absorbs anyone walking into one that is not (no wake).
            let delivered = self.regions.get(&target).is_some_and(|l| !l.stopping);
            if delivered {
                self.note_moved(person, from, Step::Into { to: target, at: bundle_at(&bundle) });
            } else {
                // Where it left its source; an inbound one where it stands (no region of
                // this host is its source).
                let at = left.unwrap_or_else(|| bundle_at(&bundle));
                self.note_moved(person, from, Step::Absorbed { at });
            }
            self.world_send(HostMsg::PersonCrossed { person, to: target, delivered });
            if !delivered {
                self.locations.remove(&bundle.id);
                return;
            }
        }
        // A player's arrival wakes its target (parked blob or gen), as does a
        // person's with no history to absorb them; anything else into a region
        // that is not running — absent, parked or stopping — waits for it to run
        // (a stud body settles at the seam instead).
        let wakes = client.is_some() || matches!(bundle.kind, EntityKind::Person { .. });
        let running = self.regions.get(&target).is_some_and(|l| !l.stopping);
        if !wakes && !running {
            if bundle.carried.stud_body.is_some() {
                // A falling stud body does not wait out a park (with its logs) in
                // the mailbox: it settles now, on the seam on its source's side —
                // its policy's drops put down there, or nothing.
                self.settle_at_seam(from, target, bundle, view);
            } else {
                self.hold_arrival(from, target, stamp, bundle, view);
            }
            return;
        }
        self.ensure_running(target);
        self.enter_in_flight(from, target, stamp, &bundle, view);
        self.locations.insert(bundle.id, target);
        self.send_to_region(target, RegionInput::Transfer(GameEventKind::EntityArrived(bundle), stamp));
        // Never backwards: an inbound stamp may be older than a local one recorded.
        let latest = self.directive_stamps.get(&target).map_or(stamp, |&s| s.max(stamp));
        self.directive_stamps.insert(target, latest);
        let refresh = |m: &mut Self| {
            if let Some(s) = source {
                m.refresh_keepalive(s, now_ms);
            }
            m.refresh_keepalive(target, now_ms);
        };
        if let Some(m) = client.and_then(|c| self.moving.get_mut(&c)) {
            // The body crossed a seam while its move's park was on the way: park
            // it where it went (its home is the move target).
            m.from = target;
            let c = client.expect("moving is keyed by client");
            // Behind the arrival, in its slot.
            self.send_to_region(target, RegionInput::Transfer(GameEventKind::ParkPlayer(c), stamp));
            refresh(self);
        } else if let Some(p) = client.and_then(|c| self.parking.get_mut(&c)) {
            // The body crossed a seam while its park was on the way: the old
            // region found nobody; park it where it went.
            p.region = target;
            let c = client.expect("parking is keyed by client");
            // Behind the arrival, in its slot.
            self.send_to_region(target, RegionInput::Transfer(GameEventKind::ParkPlayer(c), stamp));
            refresh(self);
        } else if let Some(c) = client {
            self.homes.insert(c, target);
            let _ = self.out.send((Some(c), ServerPacket::PlayerRegion(Some(target), c)));
            // The old home may have just lost its keep-alive reason; the new one
            // just gained it.
            refresh(self);
        }
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
        in_source_view: bool,
    ) {
        let entry = InFlight { target, stamp, bundle: bundle.clone(), in_source_view };
        if let Some(old) = self.in_flight.insert(bundle.id, entry) {
            log::error!("{:?} departed {source:?} while still in flight to {:?}", bundle.id, old.target);
        }
    }

    /// `rc` died: what was in flight to it is sent again to its respawn (or
    /// respawns it), with its stamps. A person's is dropped — the history
    /// took its people back with the region ([`Self::history_region_lost`])
    /// and its catch-up places them again. A player whose park or move was
    /// waiting on `rc` gets its `ParkPlayer` again behind the body; returns
    /// those clients (their parks are still in flight).
    fn restart_for_in_flight(&mut self, rc: RegionCoords) -> BTreeSet<ClientId> {
        let intents: Vec<InFlightIntent> = self.in_flight_intents.iter().filter(|f| f.target == rc).cloned().collect();
        self.in_flight_intents.retain(|f| f.target != rc);
        if !intents.is_empty() {
            self.ensure_running(rc);
            log::warn!("{} intent(s) were in flight to lost region {rc:?}: sent again", intents.len());
            for f in intents {
                self.send_intent_to(rc, f.msg, f.in_source_view);
            }
        }
        let ids: Vec<crate::EntityId> =
            self.in_flight.iter().filter(|(_, f)| f.target == rc).map(|(id, _)| *id).collect();
        let mut parks = BTreeSet::new();
        if ids.is_empty() {
            return parks;
        }
        self.ensure_running(rc);
        for id in ids {
            let f = self.in_flight[&id].clone();
            if matches!(f.bundle.kind, EntityKind::Person { .. }) && self.world.is_some() {
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

    /// The incarnation `rc` runs (a fresh number per start), if it runs.
    pub fn region_incarnation(&self, rc: RegionCoords) -> Option<u64> {
        self.regions.contains_key(&rc).then(|| self.incarnation.get(&rc).copied()).flatten()
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
        if let Some(player) = player {
            // Its claim stays (in its record too); another player may take an offline
            // claimant's fortress up.
            self.world_send(HostMsg::PlayerOffline { player });
        }
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
        } else if let Some(home) = self.homes.remove(&id).filter(|h| {
            // A home whose start failed (`region_load_failed`) holds no body:
            // nothing to park; the player keeps its stored record.
            let lost = !self.regions.contains_key(h) && self.unavailable.contains_key(h);
            if lost {
                log::warn!("client {id}: its home {h:?} could not be loaded; nothing to park");
            }
            !lost
        }) {
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
    /// disconnect), so its record is in the exit's save cut before the
    /// process ends. Then [`Self::shutdown_all`]; regions answer `ParkPlayer` first.
    pub fn park_all_players(&mut self, now_ms: u64) {
        let online: Vec<ClientId> = self.sessions.keys().copied().collect();
        for id in online {
            self.client_disconnected(id, now_ms);
        }
    }

    /// Every session of `player` is told `reason` and ended (the server edge
    /// closes it); its player parks.
    fn supersede(&mut self, player: PlayerId, reason: &str, now_ms: u64) {
        let sessions: Vec<ClientId> =
            self.player_of.iter().filter(|(_, p)| **p == player).map(|(c, _)| *c).collect();
        for id in sessions {
            log::info!("client {id}: player {} superseded by the login service ({reason})", player.0);
            let _ = self.out.send((Some(id), ServerPacket::Refused(reason.to_string())));
            self.client_disconnected(id, now_ms);
        }
    }

    /// A session started. A session of the same player already online (under another
    /// id) is ended in its favour. `false` = refused ([`DUPLICATE_SESSION_REFUSAL`]): the
    /// id still has state here (a session, a park, a move, a creation), which stays as it
    /// was — nothing is sent to the id, the live session owns it.
    fn client_connected(&mut self, id: ClientId, player: PlayerId, record: Option<PlayerRecord>, now_ms: u64) -> bool {
        if self.sessions.contains_key(&id)
            || self.player_of.contains_key(&id)
            || self.homes.contains_key(&id)
            || self.parking.contains_key(&id)
            || self.moving.contains_key(&id)
            || self.creating.contains_key(&id)
            || self.fortress_of.contains_key(&id)
            || self.adventurer_of.contains_key(&id)
            || self.awaiting_park.values().any(|(c, _)| *c == id)
        {
            log::warn!("client {id}: {DUPLICATE_SESSION_REFUSAL} (a duplicate ClientConnected is ignored)");
            return false;
        }
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
        if in_flight {
            // Reconnected faster than the park: restore once it is back.
            log::info!("client {id}: player {} is still parking; restore deferred", player.0);
            self.awaiting_park.insert(player, (id, record));
            self.deferred_packets.entry(id).or_default();
        } else {
            let record = self.take_parked(player).or(record);
            self.place_player(id, record, now_ms);
        }
        if let Some(map) = &self.world_map {
            let _ = self.out.send((Some(id), ServerPacket::WorldMap(map.clone())));
        }
        if self.world.is_some() {
            // Before the coordinator's first view arrives there is no calendar to tell;
            // the view, when it comes, goes to every connected client.
            if let Some(now) = self.calendar_now(now_ms) {
                let _ = self.out.send((Some(id), ServerPacket::Calendar(now)));
            }
            let _ = self.out.send((Some(id), ServerPacket::Sites(self.world_sites())));
            let _ = self.out.send((Some(id), ServerPacket::Civs(self.world_civs())));
            if !self.restore_pending(id) {
                self.send_claims(id);
                self.world_player_online(id);
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
        let target = match &record {
            Some(r) if !crate::off_map(self.map_extent(), r.region) => r.region,
            _ => SPAWN_REGION,
        };
        if self.refuses_play(target) {
            // Its region's stored state could not be read: the session is
            // ended, the record kept for the next try.
            log::warn!("client {id}: its region {target:?} is unavailable; session refused");
            // A region another host owns: the store holds the record already,
            // unchanged (our cut must not write it back). Nor is an off-map record
            // parked again (only the store can hold one): a cut writing it would touch a
            // super-region nobody claims, and be refused as fenced for good.
            if self.owns(target) {
                if let (Some(record), Some(player)) = (record, self.player_of.get(&id).copied()) {
                    if crate::off_map(self.map_extent(), record.region) {
                        log::warn!("client {id}: its off-map record is left as stored");
                    } else {
                        self.parked_players.insert(player, record);
                    }
                }
            }
            let _ = self.out.send((Some(id), ServerPacket::Refused(REGION_UNAVAILABLE_REFUSAL.to_string())));
            self.client_disconnected(id, now_ms);
            return;
        }
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
        // Before the coordinator's first view the host cannot tell: the claim stands.
        let known = |site: SiteId| self.world_view.is_none() || self.site_regions.contains_key(&site);
        let fortress = record
            .fortress
            .filter(|&site| known(site) && !self.fortress_of.values().any(|&s| s == site));
        let adventure = record.adventure.filter(|&site| fortress.is_none() && known(site));
        self.homes.insert(id, region);
        if let Some(&player) = self.player_of.get(&id) {
            self.creating.insert(id, (region, player, record.clone()));
        }
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

    /// A region answered `ParkPlayer`. The record (with the claims the manager
    /// held) becomes the player's parked record and goes to the store; a session
    /// of the same player waiting for it is restored now.
    fn player_parked(
        &mut self,
        rc: RegionCoords,
        client: ClientId,
        record: Option<Box<PlayerRecord>>,
        body: Option<crate::EntityId>,
        now_ms: u64,
    ) {
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
                self.cut_transit(p.player, &record);
                if let Some(body) = body {
                    self.parked_bodies.insert(body, p.player);
                }
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
        let record = self.take_parked(player).or(edge_record);
        self.place_player(waiting, record, now_ms);
        if self.world.is_some() && self.sessions.contains_key(&waiting) {
            self.send_claims(waiting);
            self.world_player_online(waiting);
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
            lod_cache: self.lod_cache.len(),
            terrain_cache: self.terrain_cache.len(),
            transfers_in_flight: self.in_flight_count(),
            pending_arrivals: self.pending_arrival_total(),
            peer_unreceived: self.peer_unreceived(),
            arrivals_despawned: self.arrivals_despawned(),
            items_destroyed: self.items_destroyed(),
            intent_replies_lost: self.intent_replies_lost(),
        }
    }

    /// Ends every session with `Refused(why)` (the server edge closes the
    /// connections) and changes nothing else: for a process that stops
    /// serving at once (fenced) and saves nothing.
    pub fn refuse_all_sessions(&mut self, why: &str) {
        for client in self.sessions.keys().copied() {
            let _ = self.out.send((Some(client), ServerPacket::Refused(why.to_string())));
        }
    }

    /// Which regions this process simulates (default: all), with `s` regions
    /// per super-region side. Set before the first session.
    pub fn set_ownership(&mut self, ownership: crate::Ownership, s: u32) {
        self.ownership = (ownership, s);
    }

    /// Whether this process owns `rc` (another process simulates it if not).
    pub fn owns(&self, rc: RegionCoords) -> bool {
        self.ownership.0.owns(rc, self.ownership.1)
    }

    /// Whether nobody can be put into `rc` now: another host owns it and its session is
    /// not reachable ([`Self::reaches`]), or its stored state could not be read and its
    /// retry tick has not come ([`STORE_LOAD_RETRY_SECS`]).
    pub fn is_unavailable(&self, rc: RegionCoords) -> bool {
        !self.reaches(rc) || self.unavailable.get(&rc).is_some_and(|&retry| self.spawner.world_tick() < retry)
    }

    /// Whether Play, embark, return, an adventure start or a placement into `rc` is
    /// refused: it is unavailable, or another host owns it — players do not move between
    /// hosts yet (Task 8 lifts the foreign half).
    fn refuses_play(&self, rc: RegionCoords) -> bool {
        self.is_unavailable(rc) || self.is_foreign(rc)
    }

    /// The regions that are unavailable now (tests, the `[server]` line).
    pub fn unavailable_regions(&self) -> Vec<RegionCoords> {
        self.unavailable.keys().copied().filter(|rc| self.is_unavailable(*rc)).collect()
    }

    /// Regions whose retry tick has come are started again if a session
    /// still subscribes to them; the others start on their next demand.
    fn retry_unavailable(&mut self) {
        if self.unavailable.is_empty() {
            return;
        }
        let now = self.spawner.world_tick();
        let due: Vec<RegionCoords> = self.unavailable.iter().filter(|(_, &t)| now >= t).map(|(rc, _)| *rc).collect();
        for rc in due {
            self.unavailable.remove(&rc);
            let subscribers: Vec<ClientId> =
                self.sessions.iter().filter(|(_, s)| s.subscribed.contains(&rc)).map(|(c, _)| *c).collect();
            if !subscribers.is_empty() {
                log::info!("[server] retrying {rc:?} for {} subscriber(s)", subscribers.len());
            }
            for client in subscribers {
                self.subscribe(client, rc);
            }
        }
    }

    /// `rc`'s spawner could not read its stored state (`LoadFailed`): its
    /// thread has ended without running. It stays stopped until world tick
    /// now + [`STORE_LOAD_RETRY_TICKS`] — never regenerated over stored data
    /// — and what was sent to it waits for its next run: crossings back in
    /// the arrival mailbox, intents in the intent mailbox, a crossing
    /// player's record parked; the people the history placed there go back
    /// to the history. Sessions whose body was to be there are ended
    /// (`Refused`): they have no body, and keep their stored record.
    fn region_load_failed(&mut self, rc: RegionCoords, why: String, now_ms: u64) {
        let retry = self.spawner.world_tick() + STORE_LOAD_RETRY_TICKS;
        log::error!(
            "[server] region {rc:?} could not be loaded from the world store: {why} — it stays stopped \
             (never regenerated over stored data); retried from world tick {retry}"
        );
        if self.regions.remove(&rc).is_none() {
            return;
        }
        self.announce_region(rc, false);
        self.unavailable.insert(rc, retry);
        self.spawner.reap(rc);
        // It never ran in this process, so it has nothing uncommitted: the
        // pending cut stops waiting for it (the store keeps what it holds).
        if let Some(cut) = self.pending_cut.as_mut() {
            cut.waiting.remove(&rc);
        }
        self.history_region_lost(rc);
        let intents: Vec<InFlightIntent> = self.in_flight_intents.iter().filter(|f| f.target == rc).cloned().collect();
        self.in_flight_intents.retain(|f| f.target != rc);
        for f in intents {
            self.intent_mailbox.entry(rc).or_default().push(Mail { msg: f.msg, in_source_view: f.in_source_view });
        }
        let ids: Vec<crate::EntityId> =
            self.in_flight.iter().filter(|(_, f)| f.target == rc).map(|(id, _)| *id).collect();
        for id in ids {
            let f = self.in_flight.remove(&id).expect("listed");
            if matches!(f.bundle.kind, EntityKind::Person { .. }) && self.world.is_some() {
                continue; // The history took its people back.
            }
            if f.bundle.client.is_some() {
                match self.crossing_record(rc, &f.bundle) {
                    Some((player, record)) => {
                        log::error!("player {} was crossing into unavailable {rc:?}: parked", player.0);
                        self.parked_players.insert(player, record);
                    }
                    None => log::error!("{id:?}, a player crossing into unavailable {rc:?}, has no player: lost"),
                }
                continue;
            }
            let p = PendingArrival { source: rc, stamp: f.stamp, bundle: f.bundle, in_source_view: f.in_source_view };
            self.pending_arrivals.entry(rc).or_default().push(p);
        }
        self.forget_locations(rc);
        // Bodies created there never existed: the records they carried are
        // parked again (the next cut carries them, a reconnect restores them).
        let lost: Vec<ClientId> = self.creating.iter().filter(|(_, (t, ..))| *t == rc).map(|(c, _)| *c).collect();
        for client in lost {
            let (_, player, record) = self.creating.remove(&client).expect("listed");
            log::warn!("player {}'s body was to be created in unavailable {rc:?}: its record is parked", player.0);
            self.parked_players.insert(player, record);
        }
        let homed: Vec<ClientId> = self.sessions.keys().copied().filter(|c| self.homes.get(c) == Some(&rc)).collect();
        for client in homed {
            let _ = self.out.send((Some(client), ServerPacket::Refused(REGION_UNAVAILABLE_REFUSAL.to_string())));
            self.client_disconnected(client, now_ms);
        }
        self.settle_lost_parks(rc, &BTreeSet::new());
        self.try_complete_cut();
    }

    fn ensure_running(&mut self, rc: RegionCoords) {
        if self.regions.contains_key(&rc) {
            return;
        }
        if !self.owns(rc) {
            log::debug!("[server] {rc:?} is owned by another host: not started");
            return;
        }
        if let Some(&retry) = self.unavailable.get(&rc) {
            if self.spawner.world_tick() < retry {
                log::debug!("[server] {rc:?} is unavailable until world tick {retry}: not started");
                return;
            }
            self.unavailable.remove(&rc);
        }
        // Built on the spawner's side (a region thread, for the server): the
        // manager thread pays for no worldgen and no deserialisation.
        let seed = match self.parked.remove(&rc) {
            Some(serialized) => RegionSeed::Parked(serialized),
            None => RegionSeed::Fresh,
        };
        // Its incarnation marker goes on the output channel BEFORE the runner
        // exists: everything a previous (dead) runner left there comes
        // before it, everything this one sends after.
        self.next_incarnation += 1;
        self.incarnation.insert(rc, self.next_incarnation);
        let _ = self.region_out_send.send((rc, RegionOutput::Incarnation(self.next_incarnation)));
        let input = self.spawner.spawn(rc, seed, self.region_out_send.clone());
        // Very first: join the world's tick numbering before anything else.
        let start = self.spawner.world_tick();
        let _ = input.send(RegionInput::StartAt(start));
        // A pending save cut it has not passed asks it too, before anything
        // reaches it.
        self.cut_region_started(rc, start, &input);
        // Next, so it is bounded before its first tick.
        if let Some(route) = &self.route_map {
            let _ = input.send(RegionInput::SetRouteMap(Some(route.clone())));
        }
        // Hosts owning a neighbour learn it runs before its halo requests reach them
        // (their answers to it are sent only to a region they know runs).
        self.announce_region(rc, true);
        // Its terrain halo, before its first tick too.
        self.seed_halo(rc, &input);
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
        // A new incarnation: no arrival stamp of an earlier one applies.
        self.directive_stamps.remove(&rc);
        // Then the history's catch-up: its people here now (at once through an inline
        // coordinator), before anything else.
        self.world_region_started(rc);
        // Arrivals that waited for it, in arrival order, before its first
        // tick (their stamps are past, so it applies them on receipt).
        self.deliver_pending_arrivals(rc);
        // Replies that waited for it.
        for m in self.intent_mailbox.remove(&rc).unwrap_or_default() {
            self.send_intent_to(rc, m.msg, m.in_source_view);
        }
    }

    /// Seed the halo of `rc`, which is starting (`input` is its fresh
    /// channel; nothing has ticked it yet), from each on-map neighbour:
    /// - running: the neighbour answers a `HaloRequest` with its live chunks
    ///   (a stamped Transfer); its cached terrain, if any, goes in first;
    /// - running on another host (`foreign_running`): a `Volatile::HaloRequest`
    ///   to that host, answered the same way over the peer link;
    /// - parked or cached (its last pushed / parked terrain): from the cache
    ///   (a region that parks also reseeds its running neighbours from its
    ///   final terrain, on `Stopped`);
    /// - never run: worldgen — on the region's own thread when the spawner
    ///   seeds halos there (only the band columns), else here, from the
    ///   spawner's stud genesis (the whole region, not cached);
    /// - saved by an earlier process, not run in this one (`stored_regions`):
    ///   from the world store on the region's own thread
    ///   (`RegionInput::SeedHaloFromStore`, worldgen if the store cannot
    ///   serve it) when the spawner seeds halos there; a spawner that does
    ///   not (inline: tests, wasm — no store) gets worldgen here, as above.
    fn seed_halo(&mut self, rc: RegionCoords, input: &Sender<RegionInput>) {
        let mut generate_there = Vec::new();
        let mut load_there = Vec::new();
        for n in crate::halo::neighbours(rc) {
            if crate::off_map(self.map_extent(), n) {
                continue;
            }
            // Another host runs it: asked of that host (`peers.rs`); one it does not run
            // is seeded like a parked region of this host.
            if self.is_foreign(n) && self.foreign_halo_request(rc, n) {
                continue;
            }
            let running = self.regions.get(&n).is_some_and(|l| !l.stopping);
            if let Some(snap) = self.terrain_cache.get(&n) {
                if let Some(seed) = crate::halo::halo_seed_from_snapshot(n, rc, &snap) {
                    let _ = input.send(RegionInput::Event(GameEventKind::HaloUpdate(seed)));
                }
            } else if running {
                // The request below is the seed.
            } else if self.spawner.seeds_halos_on_region_threads() && self.stored_regions.contains_key(&n) {
                load_there.push(n);
            } else if self.spawner.seeds_halos_on_region_threads() {
                generate_there.push(n);
            } else {
                // No genesis: the neighbour has no terrain, and the seed says so.
                let studs = self.spawner.stud_genesis(n).unwrap_or_default();
                let snap = TerrainSnapshot::of_stud_edge(crate::StudEdge::from_seed(&studs));
                if let Some(seed) = crate::halo::halo_seed_from_snapshot(n, rc, &snap) {
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
        if !load_there.is_empty() {
            let _ = input.send(RegionInput::SeedHaloFromStore(load_there));
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
        // Unavailable (its store read failed): subscribed again at its retry.
        let Some(link) = self.regions.get_mut(&rc) else { return };
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
    /// Region `rc` was lost without `Stopped` (its thread died), so it sent
    /// no `HaloEntityGone` for its mirrors: each running, non-stopping
    /// neighbour ends them all (`HaloSourceGone`), stamped with the world
    /// tick so a lagging neighbour applies what `rc` sent before first. Its
    /// terrain halo stays as last mirrored (a respawn reseeds it; the
    /// terrain cache may be older than the deltas the neighbour applied).
    fn halo_source_lost(&mut self, rc: RegionCoords) {
        let stamp = self.spawner.world_tick();
        for n in crate::halo::neighbours(rc) {
            if self.regions.get(&n).is_some_and(|l| !l.stopping) {
                self.send_to_region(n, RegionInput::Transfer(GameEventKind::HaloSourceGone(rc), stamp));
            }
        }
    }

    /// `rc` no longer holds anything (parked or lost): its directory
    /// entries go.
    fn forget_locations(&mut self, rc: RegionCoords) {
        self.locations.retain(|_, at| *at != rc);
        // Arrivals waiting for it, and crossings in flight to it (sent again
        // to its next run), are still on their way there.
        for p in self.pending_arrivals.get(&rc).into_iter().flatten() {
            self.locations.insert(p.bundle.id, rc);
        }
        for (id, f) in &self.in_flight {
            if f.target == rc {
                self.locations.insert(*id, rc);
            }
        }
    }

    /// Where `id` is owned, per the directory.
    pub fn location_of(&self, id: crate::EntityId) -> Option<RegionCoords> {
        self.locations.get(&id).copied()
    }

    /// An intent or reply from region `emitter`'s `Intents` output, to the
    /// region owning its target (by position, or by `EntityId` through the
    /// directory) as a `Transfer` stamped with its due tick, booked in flight
    /// for the autosave.
    /// - An intent whose owner is not running — or is stopping — is answered
    ///   `Refused(.., OwnerNotRunning)`: an intent never wakes a region.
    /// - Owned by another host: a reply, and an intent whose owner is reachable, go to
    ///   that host reliably (`peers.rs`); an unreachable owner refuses as above.
    /// - A reply goes where its entity is, else to its `hint`; to a parked or
    ///   stopping region it waits in the mailbox for its next run. With no
    ///   such destination, or past `MAX_INTENT_REPLY_HOPS`, it is given up:
    ///   logged at warn and counted (`intent_replies_lost`).
    /// `view`: sent after the emitter answered the pending cut (`save_cut.rs`);
    /// what the manager makes of it (a refusal, a salvage) inherits it.
    fn route_intent(&mut self, msg: crate::IntentMsg, emitter: RegionCoords, view: bool) {
        use crate::{IntentMsg, IntentRefusal, IntentReply, SentReply, Target};
        if let IntentMsg::Reply(s) = msg {
            if self.salvage_reply(s, emitter, view) {
                return;
            }
        }
        let owner = match msg {
            IntentMsg::Intent(s) => match s.target {
                Target::Entity(id) => self.locations.get(&id).copied(),
                _ => s.target.region(),
            },
            // A region-sourced intent's reply goes to that region itself.
            IntentMsg::Reply(s) if s.reply.to().is_region() => Some(s.reply.to().birth_region),
            IntentMsg::Reply(s) => self.locations.get(&s.reply.to()).copied().or(Some(s.hint)),
        };
        // Another host's region: a reply always goes (reliably: it may carry items), an
        // intent when that host is reachable; else it is refused below as for a region
        // that is not running.
        if let Some(to) = owner.filter(|rc| self.is_foreign(*rc)) {
            let give_up = matches!(msg, IntentMsg::Reply(s) if s.hops > crate::intents::MAX_INTENT_REPLY_HOPS);
            if !give_up && (matches!(msg, IntentMsg::Reply(_)) || self.reaches(to)) {
                self.intent_to_peer(to, msg, view);
                return;
            }
        }
        let running = owner.filter(|rc| self.regions.get(rc).is_some_and(|l| !l.stopping));
        match (running, msg) {
            (_, IntentMsg::Reply(s)) if s.hops > crate::intents::MAX_INTENT_REPLY_HOPS => {
                self.intent_replies_lost += 1;
                log::warn!("[server] intent reply {:?} for {:?} given up after {} hops", s.reply, s.reply.to(), s.hops);
                self.items_lost(&s.reply);
            }
            (Some(rc), msg) => self.send_intent_to(rc, msg, view),
            (None, IntentMsg::Intent(s)) => {
                log::debug!("[server] intent {:?} at {:?}: owner {owner:?} not running; refused", s.intent, s.target);
                let reply = IntentReply::Refused(s.intent, IntentRefusal::OwnerNotRunning);
                self.route_intent(IntentMsg::Reply(SentReply { reply, hint: s.from, due: s.due + 1, hops: 0 }), emitter, view);
            }
            (None, IntentMsg::Reply(s)) => {
                let waiting = owner.filter(|rc| self.parked.contains_key(rc) || self.regions.contains_key(rc));
                match waiting {
                    Some(rc) => self.intent_mailbox.entry(rc).or_default().push(Mail { msg, in_source_view: view }),
                    None => {
                        self.intent_replies_lost += 1;
                        log::warn!("[server] intent reply {:?} for {:?} lost: {owner:?} never ran", s.reply, s.reply.to());
                        self.items_lost(&s.reply);
                    }
                }
            }
        }
    }

    /// A reply carrying items (`IntentReply::carried_items`) whose entity is
    /// gone is never given up silently: to a parked player's old body, the
    /// items go into its parked record (inventory, then an empty cursor) and
    /// to the store; otherwise — once a delivery found the entity gone
    /// (`hops` > 0) — or for what the record cannot hold, they are put down
    /// as a region-sourced deposit at the reply's pile cell (its owner's
    /// refusal comes back to that region, which puts them down itself) — or,
    /// with no cell (a refused place's block), left to be given up, counted.
    /// Returns whether the reply was dealt with here.
    fn salvage_reply(&mut self, s: crate::SentReply, emitter: RegionCoords, view: bool) -> bool {
        let to = s.reply.to();
        let Some((cell, mut stack)) = s.reply.carried_items() else { return false };
        if to.is_region() || self.locations.contains_key(&to) || self.in_flight.contains_key(&to) {
            return false;
        }
        let parked = self.parked_bodies.get(&to).copied().filter(|p| self.parked_players.contains_key(p));
        if let Some(player) = parked {
            // Sent after its region answered the pending cut, the items are
            // still in that region's snapshot: the cut keeps the record as
            // it was (they reach the store with the next cut).
            if view {
                let before = self.parked_players[&player].clone();
                if let Some(cut) = self.pending_cut.as_mut() {
                    cut.parked_at_t.entry(player).or_insert(before);
                }
            }
            let record = self.parked_players.get_mut(&player).expect("checked");
            stack.count = record.client.inventory.add(stack);
            if stack.count > 0 && record.client.cursor.is_none() {
                record.client.cursor = Some(stack);
                stack.count = 0;
            }
            log::info!("[server] reply {:?} for parked {to:?}: folded into player {}'s record", s.reply, player.0);
            let record = record.clone();
            if !view {
                self.cut_transit(player, &record);
            }
            if stack.count == 0 {
                return true;
            }
        } else if s.hops == 0 {
            // First delivery to its hint: the entity may still be there.
            return false;
        }
        // A refused place's block has no cell to go to: given up, counted.
        let Some(cell) = cell else { return false };
        let owner = cell.to_local().0;
        log::info!("[server] reply {:?} for gone {to:?}: {stack:?} put down at {cell:?}", s.reply);
        let sent = crate::SentIntent {
            target: crate::Target::Pile(cell),
            intent: crate::Intent::DepositItems {
                cell,
                stack,
                expire: s.due + crate::PILE_DESPAWN_TICKS,
                pickup: s.due + crate::PICKUP_DELAY_TICKS,
                by: crate::EntityId::of_region(owner),
            },
            from: owner,
            due: s.due,
        };
        self.route_intent(crate::IntentMsg::Intent(sent), emitter, view);
        true
    }

    /// A reply given up: the items it carried are destroyed — counted
    /// ([`Self::items_destroyed`]) and logged at warn.
    fn items_lost(&mut self, reply: &crate::IntentReply) {
        if let Some((cell, stack)) = reply.carried_items() {
            self.items_destroyed += u64::from(stack.count);
            log::warn!("[server] {stack:?} of pile {cell:?} destroyed: their reply {reply:?} had nowhere to go");
        }
    }

    /// Items destroyed with replies that had nowhere to go (see
    /// [`Self::route_intent`]).
    pub fn items_destroyed(&self) -> u64 {
        self.items_destroyed
    }

    /// `player`'s parked record, taken for a restore; its old body's id is
    /// forgotten (a late reply to it is put down where it came from).
    fn take_parked(&mut self, player: PlayerId) -> Option<PlayerRecord> {
        self.parked_bodies.retain(|_, p| *p != player);
        let record = self.parked_players.remove(&player);
        if let Some(record) = &record {
            self.cut_transit(player, record);
        }
        record
    }

    /// Test support: `player`'s parked record.
    pub fn parked_record(&self, player: PlayerId) -> Option<&PlayerRecord> {
        self.parked_players.get(&player)
    }

    /// Replies given up so far (see [`Self::route_intent`]).
    pub fn intent_replies_lost(&self) -> u64 {
        self.intent_replies_lost
    }

    /// Test support: the replies waiting for a parked or stopping region.
    pub fn intent_mailbox(&self, rc: RegionCoords) -> Vec<crate::IntentMsg> {
        self.intent_mailbox.get(&rc).into_iter().flatten().map(|m| m.msg).collect()
    }

    /// Test support: every intent and reply this manager holds on its way to one of its
    /// regions — in flight to a running one, or waiting in a mailbox.
    pub fn intents_held(&self) -> Vec<crate::IntentMsg> {
        let in_flight = self.in_flight_intents.iter().map(|f| f.msg);
        in_flight.chain(self.intent_mailbox.values().flatten().map(|m| m.msg)).collect()
    }

    /// `msg` to running `rc`, held there to its due tick, and in flight until
    /// `rc` confirms it (`in_source_view`: see [`InFlightIntent`]).
    fn send_intent_to(&mut self, rc: RegionCoords, msg: crate::IntentMsg, in_source_view: bool) {
        self.in_flight_intents.push(InFlightIntent { msg, target: rc, in_source_view });
        self.send_to_region(rc, RegionInput::Transfer(GameEventKind::Intent(msg), msg.due()));
    }

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
        self.cut_region_lost(rc);
        let subscribers = self.regions.remove(&rc).unwrap().subscribers;
        self.pending_events.remove(&rc);
        self.spawner.reap(rc);
        self.halo_source_lost(rc);
        self.announce_region(rc, false);
        self.forget_locations(rc);
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

    fn lod_subscribe(&mut self, client: ClientId, key: LodKey) {
        if !crate::lod::LOD_FACTORS.contains(&key.factor) {
            log::debug!("dropping RequestLod with invalid factor {}", key.factor);
            return;
        }
        let Some(session) = self.sessions.get_mut(&client) else {
            log::debug!("dropping RequestLod from sessionless client {client}");
            return;
        };
        if !crate::stud_lod_supported(&key) {
            // No tile at that factor (finer than a stud, or too wide to ship):
            // the client's stud ring table never asks for one.
            log::debug!("dropping RequestLod {key:?}: no stud far terrain at that factor");
            return;
        }
        session.lod_subscribed.insert(key);
        self.lod_subscribers.entry(key).or_default().insert(client);
        self.lod_access_counter += 1;
        let stamp = self.lod_access_counter;
        // Far terrain is the recipe's: a function of the key, so no region is
        // asked for anything and nothing ever invalidates a cached tile.
        if let Some((tile, access)) = self.lod_cache.get_mut(&key) {
            *access = stamp;
            self.perf.lod_cache_hits += 1;
            let _ = self.out.send((Some(client), ServerPacket::StudLodData(key, tile.clone())));
        } else if !self.lod_in_flight.contains(&key) {
            self.lod_in_flight.insert(key);
            self.lod_submitted_at.insert(key, crate::perf::Stopwatch::start());
            self.perf.lod_submitted += 1;
            self.lod_builder.submit(key);
        }
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

    /// Take every tile the builder has finished (never blocks).
    pub fn pump_lod_results(&mut self) {
        while let Some((key, tile)) = self.lod_builder.poll() {
            self.handle_lod_result(key, tile);
        }
    }

    /// A finished tile: sent to the key's subscribers and cached.
    pub fn handle_lod_result(&mut self, key: LodKey, tile: crate::StudLodTile) {
        self.lod_in_flight.remove(&key);
        self.perf.lod_built += 1;
        if let Some(watch) = self.lod_submitted_at.remove(&key) {
            let took = watch.elapsed();
            self.perf.lod_latency_total += took;
            self.perf.lod_latency_max = self.perf.lod_latency_max.max(took);
        }
        self.lod_access_counter += 1;
        if let Some(subs) = self.lod_subscribers.get(&key) {
            for client in subs {
                let _ = self.out.send((Some(*client), ServerPacket::StudLodData(key, tile.clone())));
            }
        }
        self.lod_cache.insert(key, (tile, self.lod_access_counter));
        self.evict_lod_cache();
    }

    /// Test hook: current cache size.
    pub fn lod_cache_len(&self) -> usize {
        self.lod_cache.len()
    }

    /// Test hook: shrink the cap so eviction is exercisable without building
    /// thousands of keys. Evicts immediately if already over the new cap.
    pub fn set_lod_cache_max(&mut self, n: usize) {
        self.lod_cache_max = n;
        self.evict_lod_cache();
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

