//! The per-region actor core: one `RegionRunner` per running region, fed
//! `RegionInput`s and emitting `RegionOutput`s over channels. This message
//! pair is the future network seam — the runner never sees subscriber
//! lists, client sessions, or network types. On the server each runner
//! lives on its own thread with its own tick timer (crates/server); in the
//! wasm build LocalServer pumps runners inline (crates/client).

use crate::{
    ClientId, EntityBundle, TerrainSnapshot, GameEvent, GameEventKind, HaloEntityMsg, Region,
    RegionCoords, RegionId, Rollback, Tick,
    TICK_RATE,
};
use crossbeam::channel::Sender;

/// Bincode-serialized region state — the parking-lot format. Same payload
/// the wire's `ServerPacket::Region` carries, kept opaque so parking never
/// aliases live state.
#[derive(Debug, Clone)]
pub struct SerializedRegion(pub Vec<u8>);

impl SerializedRegion {
    pub fn from_rollback(r: &Rollback) -> Self {
        Self(bincode::serialize(r).expect("region state must serialize"))
    }

    pub fn to_rollback(&self) -> Result<Rollback, Box<bincode::ErrorKind>> {
        bincode::deserialize(&self.0)
    }
}

#[derive(Debug)]
pub enum RegionInput {
    /// A routed client event or a manager-authoritative event
    /// (CreateClient). The region assigns the authoritative event id.
    /// Applied on receipt, so it overtakes the buffered [`RegionInput::Transfer`]s
    /// (except an event for a client whose body is buffered, which queues behind
    /// it): an event that must follow region-to-region messages travels as a
    /// `Transfer` instead — the manager's directives do, stamped with its
    /// `directive_stamps`.
    Event(GameEventKind),
    /// A client event the client predicted right after its own tick number
    /// `.1` of this region. Held until this region has run that tick, so it
    /// lands in the slot the client predicted instead of wherever the two
    /// clocks happen to stand on arrival; a client level with or behind the
    /// server gets it applied on arrival, like [`RegionInput::Event`].
    EventAfterTick(GameEventKind, u64),
    /// Something another region sent during its tick T (an `EntityArrived`,
    /// an entity- or terrain-halo update, a park that must follow an
    /// arrival), stamped T+1: buffered in the region's [`crate::SlotInbox`]
    /// and applied at the END of slot T+1 — right before the `Tick` that
    /// closes it, with every other message of that slot, in canonical order
    /// (never on receipt, so thread timing cannot order them). One that comes
    /// after its slot closed lands at the end of the open slot. Held for as
    /// long as it takes (no `MAX_INPUT_LEAD_TICKS` cap: the stamp is a
    /// region's, never a client's).
    Transfer(GameEventKind, Tick),
    /// A new subscriber needs the full state; replied with
    /// `RegionOutput::Snapshot(client, ...)`.
    RequestSnapshot(ClientId),
    /// The finite world's extent and coarse-route map
    /// (`Region::set_route_map`): the manager sends it right after spawning,
    /// before any tick, when it has a map.
    SetRouteMap(Option<std::sync::Arc<crate::RouteMap>>),
    /// Start the region's tick count at this world tick, and its entity ids
    /// past every earlier incarnation's (`Region::start_at`).
    /// The manager sends it FIRST after spawning, before `SetRouteMap`.
    StartAt(crate::Tick),
    /// World save cut `.0` at world tick `.1` (`save_cut.rs`): this region's
    /// portable snapshot at the opening of slot `.1` — after slot `.1 − 1`
    /// closed, before any slot-`.1` input — comes back as
    /// `RegionOutput::CutSnapshot`, or `RegionOutput::CutMissed` when the
    /// region is already past that point. Does not affect sim state.
    SaveAt(crate::CutId, Tick),
    /// Neighbour `.0` just started: send it this region's chunks it mirrors
    /// as a halo seed (`RegionOutput::HaloDeltas`, stamped with this
    /// region's tick). Does not affect this region's state.
    HaloRequest(RegionCoords),
    /// Seed this (just started, not yet ticked) region's halo from worldgen
    /// for these never-run neighbours, with the runner's halo generator
    /// ([`RegionRunner::set_stud_halo_generator`]) — on the region's own thread.
    SeedHaloFromWorldgen(Vec<RegionCoords>),
    /// Seed this (just started, not yet ticked) region's halo for these
    /// neighbours that the world store holds and that have not run in this
    /// process, from their stored terrain
    /// ([`RegionRunner::set_stored_terrain`]) — on the region's own thread.
    /// A neighbour the store cannot serve is seeded from worldgen, as
    /// [`RegionInput::SeedHaloFromWorldgen`].
    SeedHaloFromStore(Vec<RegionCoords>),
    /// The history people whose bodies stand here, for the host's `Attach` (of
    /// this epoch) to a (re)connected world coordinator: answered with
    /// [`RegionOutput::PeopleListed`] (empty included) once every transfer held
    /// when it arrived has been applied (a spawn or an arrival waiting for its
    /// slot is a body too). Does not affect sim state.
    ListPeople(u64),
    /// Graceful stop; replied with `RegionOutput::Stopped(state)`.
    Shutdown,
}

#[derive(Debug)]
pub enum RegionOutput {
    EventProcessed(GameEvent),
    /// An `EventProcessed` that also carries the region's
    /// [`crate::state_hash`] right after the event: a `Tick` landing on a
    /// multiple of [`crate::state_hash_interval`]. Fanned out as
    /// `ServerPacket::HashedGameEvent`, so the hash can never be split from
    /// the event it describes.
    /// Fields: the event, the tick it ran, the hash.
    EventProcessedHashed(GameEvent, Tick, u32),
    Snapshot(ClientId, Rollback),
    SyncClock { tick_rate: u64, tick: Tick },
    /// The parked state, its final terrain, and its final stamp (its
    /// `current_tick()` at `Shutdown`: what the manager stamps the final
    /// terrain's halo reseed with, so it lands after every delta this region
    /// sent before), and its final portable snapshot (against the terms of
    /// its previous one): a pending save cut takes it as this region's
    /// answer, and the manager keeps it uncommitted until a cut commits it.
    Stopped(SerializedRegion, TerrainSnapshot, Tick, Box<crate::PortableRegion>),
    /// Boundary crossings this tick: bundles + ABSOLUTE target regions.
    /// Isometries are still source-local; the manager rebases. The stamp is
    /// the source's `current_tick()` right after the tick that produced
    /// them (T+1 for its tick T): the slot each target applies them in, as a
    /// [`RegionInput::Transfer`].
    Departures(Vec<(EntityBundle, RegionCoords)>, Tick),
    /// Entity-halo messages (`halo.rs`) for the neighbours, same framing
    /// and stamp: an update per owned entity in each neighbour's band, and
    /// the explicit ends (`HaloEntityGone`) — on `Shutdown` too, just before
    /// `Stopped`, for every mirror this region keeps. Poses are source-local;
    /// the manager rebases.
    HaloEntities(Vec<(HaloEntityMsg, RegionCoords)>, Tick),
    /// Terrain halo updates (`halo.rs`) for the neighbours, same framing and
    /// stamp: the band chunks this tick and its slot's events changed, or a
    /// seed answering a [`RegionInput::HaloRequest`] (stamped with the tick
    /// it was answered at).
    HaloDeltas(Vec<(crate::HaloUpdate, RegionCoords)>, Tick),
    /// Sent on `Shutdown` just before `Stopped`: every person that had a
    /// body here, with its region-local translation. They were despawned
    /// first, so the parked blob holds no people; the manager owns them.
    PeopleParked(Vec<(crate::history::PersonId, [f32; 3])>),
    /// Reply to a [`GameEventKind::ParkPlayer`], right after it ran: what the
    /// body and `Client` were (claims empty — the manager adds them), `None`
    /// when the client had no body here (it had just crossed a seam). The
    /// parked body's `EntityId`: a reply still owed to it (items a pile
    /// grant carries) is folded into the record by the manager.
    PlayerParked(ClientId, Option<Box<crate::PlayerRecord>>, Option<crate::EntityId>),
    /// A client input that reached this region after the client's body left
    /// it (within [`MAX_INPUT_LEAD_TICKS`]): the manager sends it on to the
    /// client's home as `EventAfterTick(kind, stamp)`. The stamp is the
    /// input's own, raised to the departure's, so it can never land ahead of
    /// the body. A region sends its `Departures` before any `Forward` for
    /// them, so the arrival is always queued at the target first.
    Forward(GameEventKind, Tick),
    /// Intents and replies this region sent to other regions
    /// (`intents.rs`), each with its due tick (its transfer stamp): the
    /// manager routes them to the owner — by position, or by `EntityId`
    /// through its location directory — or refuses an intent whose owner is
    /// not running.
    Intents(Vec<crate::IntentMsg>),
    /// Entities that came to be owned here since the previous report
    /// (spawned, arrived, restored): the manager's location directory.
    /// Sent after the tick that saw them.
    Spawned(Vec<crate::EntityId>),
    /// Entities no longer owned here, departures excepted (despawned,
    /// parked, left): the manager forgets their location.
    Despawned(Vec<crate::EntityId>),
    /// Reply to `SaveAt(id, tick)`: the region's portable snapshot at the
    /// opening of slot `tick` (only the chunks changed since its previous
    /// snapshot). Everything the region sent before it on the output channel
    /// happened before that point — the Tick's departures and the intents
    /// sent so far are flushed first — and everything after it, after.
    CutSnapshot(crate::CutId, Tick, Box<crate::PortableRegion>),
    /// Reply to `SaveAt(id, tick)` from a region already past the opening of
    /// slot `tick`: the manager aborts that cut and retries with a longer
    /// lead.
    CutMissed(crate::CutId),
    /// The region's stored state could not be read from the world store
    /// (the spawner's load at start; the reason): its thread ends without
    /// running. The manager keeps it stopped until a retry
    /// ([`crate::STORE_LOAD_RETRY_SECS`]) — never regenerated over stored data.
    LoadFailed(String),
    /// Sent just before a `CutSnapshot`: where the people with a body here
    /// stand (region-local translations), for the history
    /// (`History::note_positions`) — the snapshot itself holds no people.
    PeoplePositions(Vec<(crate::history::PersonId, [f32; 3])>),
    /// The answer to [`RegionInput::ListPeople`] of this epoch: exactly the history
    /// people with a body here (region-local translations).
    PeopleListed(u64, Vec<(crate::history::PersonId, [f32; 3])>),
    /// Never sent by a runner: the manager puts it on the output channel
    /// just before it spawns a region's runner, so it can tell a dead
    /// runner's late outputs (before the marker) from the new one's.
    Incarnation(u64),
}

impl RegionOutput {
    /// The variant's name, for logs.
    pub fn name(&self) -> &'static str {
        match self {
            RegionOutput::EventProcessed(_) => "EventProcessed",
            RegionOutput::EventProcessedHashed(..) => "EventProcessedHashed",
            RegionOutput::Snapshot(..) => "Snapshot",
            RegionOutput::SyncClock { .. } => "SyncClock",
            RegionOutput::Stopped(..) => "Stopped",
            RegionOutput::Departures(..) => "Departures",
            RegionOutput::HaloEntities(..) => "HaloEntities",
            RegionOutput::HaloDeltas(..) => "HaloDeltas",
            RegionOutput::PeopleParked(_) => "PeopleParked",
            RegionOutput::PlayerParked(..) => "PlayerParked",
            RegionOutput::Forward(..) => "Forward",
            RegionOutput::Intents(_) => "Intents",
            RegionOutput::Spawned(_) => "Spawned",
            RegionOutput::Despawned(_) => "Despawned",
            RegionOutput::CutSnapshot(..) => "CutSnapshot",
            RegionOutput::CutMissed(_) => "CutMissed",
            RegionOutput::LoadFailed(_) => "LoadFailed",
            RegionOutput::PeoplePositions(_) => "PeoplePositions",
            RegionOutput::PeopleListed(..) => "PeopleListed",
            RegionOutput::Incarnation(_) => "Incarnation",
        }
    }
}

/// What a region is built from when it spawns.
pub enum RegionSeed {
    /// Not run in this process, or not since its parked state was given up:
    /// the spawner restores it from its store when that holds it, else builds
    /// it from its stud genesis.
    Fresh,
    /// It ran in this process and stopped: its parked blob, its whole state.
    Parked(SerializedRegion),
}

impl RegionSeed {
    /// The region itself. `Fresh` (not in the spawner's store): built empty
    /// and seeded with `genesis()` when that gives terrain — before any
    /// runner takes its baseline hash. `Parked`: restored from its blob;
    /// `genesis` is not called.
    ///
    /// `Err` when the blob does not decode. That is a failed load like a
    /// store's: the spawner answers [`RegionOutput::LoadFailed`] and the
    /// region does not run. It is never regenerated — the blob was its whole
    /// state (edits, entities, piles), and fresh ground in its place would be
    /// saved over the stored region by the next cut.
    pub fn into_region(
        self,
        id: RegionId,
        genesis: impl FnOnce() -> Option<crate::StudRegionSeed>,
    ) -> Result<Region, String> {
        match self {
            RegionSeed::Fresh => {
                let mut region = Region::new(Rollback::new(None), None, id, None);
                if let Some(terrain) = genesis() {
                    region.seed_stud_world(terrain);
                }
                Ok(region)
            }
            RegionSeed::Parked(serialized) => match serialized.to_rollback() {
                Ok(rollback) => Ok(Region::new(rollback, None, id, None)),
                Err(e) => Err(format!("parked state corrupt: {e}")),
            },
        }
    }
}

/// Worldgen of a never-run neighbour's STUD terrain for a halo seed: what its chunk
/// columns `xs × zs` (its own frame) would start from — the recipes, and the feature
/// bricks of those columns (`stud_halo.rs`): the same ones its own genesis makes.
pub type StudHaloGenerator = std::sync::Arc<
    dyn Fn(RegionCoords, std::ops::Range<usize>, std::ops::Range<usize>) -> crate::StudRegionSeed + Send + Sync,
>;

/// A client's body buffered in a runner's inbox: its arrival (stamp, entity),
/// or its creation (a create or restore held past a save cut).
#[derive(Debug, Clone, Copy)]
enum HeldBody {
    Arrival(Tick, crate::EntityId),
    Creation(Tick),
}

impl HeldBody {
    fn stamp(self) -> Tick {
        match self {
            HeldBody::Arrival(t, _) | HeldBody::Creation(t) => t,
        }
    }
}

pub struct RegionRunner {
    id: RegionCoords,
    /// See [`RegionRunner::set_stud_halo_generator`].
    stud_halo_generator: Option<StudHaloGenerator>,
    /// See [`RegionRunner::set_stored_terrain`].
    stored_terrain: Option<std::sync::Arc<dyn crate::StoredTerrain>>,
    region: Region,
    out: Sender<(RegionCoords, RegionOutput)>,
    /// Client events waiting for the tick they were predicted after, in
    /// arrival order.
    held: std::collections::VecDeque<(GameEventKind, u64)>,
    /// [`RegionInput::Transfer`]s waiting for the end of their stamped slot,
    /// applied in canonical order right before the `Tick` that closes it
    /// (`crate::slot_order`). Also holds, behind them, the events of a client
    /// whose body's arrival or creation is in here, so they follow the body
    /// (see [`RegionRunner::route_input`]). Runner state: never hashed or
    /// saved — `Shutdown` applies all of it.
    inbox: crate::SlotInbox,
    /// A [`RegionInput::ListPeople`] (its epoch) waiting for the transfers
    /// buffered when it came, until the slot of the latest of them closed.
    people_list_due: Option<(u64, Tick)>,
    /// Clients whose body left this region during the last
    /// [`MAX_INPUT_LEAD_TICKS`] ticks: where to and the departure stamp.
    /// Their late inputs come back as [`RegionOutput::Forward`]. Transient,
    /// not region state.
    departed: std::collections::BTreeMap<ClientId, (RegionCoords, Tick)>,
    /// Cost of the every-`state_hash_interval()` state hashes since the last
    /// `take_state_hash_cost` (metrics only): count, total, slowest.
    state_hash_cost: (u32, std::time::Duration, std::time::Duration),
    /// Test support ([`RegionRunner::record_slot_start_hashes`]): the state
    /// hash right after each Tick event, before the slot's held events —
    /// what the desync check hashes and what a client holds at that tick.
    slot_start_hashes: Option<Vec<(Tick, u32)>>,
    /// The entity ids owned here at the last report (`Spawned` /
    /// `Despawned` are its diffs). Starts empty, so a region's first tick
    /// reports everything it holds.
    known_ids: std::collections::BTreeSet<crate::EntityId>,
    /// The chunk terms of this region's previous portable snapshot (empty:
    /// none yet in this incarnation, so the next one is whole). Set by the
    /// spawner for a region restored from the store
    /// ([`RegionRunner::set_saved_terms`]).
    saved_terms: crate::SaveTerms,
    /// A `SaveAt` waiting for its slot to open.
    pending_cut: Option<(crate::CutId, Tick)>,
    /// The last slot [`RegionRunner::tick`] opened (`None`: not ticked yet
    /// in this incarnation).
    opened: Option<Tick>,
}

/// Furthest ahead of a region's tick a client event may be stamped and still
/// be held for it (one second). Past that the stamp is a runaway client
/// clock or a lie, and the event applies on arrival rather than parking.
pub const MAX_INPUT_LEAD_TICKS: u64 = 1000 / TICK_RATE;

impl RegionRunner {
    pub fn new(
        id: RegionCoords,
        region: Region,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Self {
        Self {
            id,
            stud_halo_generator: None,
            stored_terrain: None,
            region,
            out,
            held: Default::default(),
            inbox: crate::SlotInbox::new(),
            people_list_due: None,
            departed: Default::default(),
            state_hash_cost: Default::default(),
            slot_start_hashes: None,
            known_ids: Default::default(),
            saved_terms: Default::default(),
            pending_cut: None,
            opened: None,
        }
    }

    /// The chunk terms of the stored state this region was restored from
    /// ([`crate::restore_portable`]): its first snapshot then sends only
    /// what changed since.
    pub fn set_saved_terms(&mut self, terms: crate::SaveTerms) {
        self.saved_terms = terms;
    }

    /// Takes the portable snapshot against the previous one's terms, and
    /// advances them.
    fn portable_snapshot(&mut self) -> Box<crate::PortableRegion> {
        let (portable, terms) = self.region.data().portable_snapshot(&self.saved_terms);
        self.saved_terms = terms;
        Box::new(portable)
    }

    /// The cut snapshot, at the opening of slot `tick`: the intents sent so
    /// far go out first (so the manager knows them as sent before it), then
    /// the people's positions, then the snapshot.
    fn send_cut_snapshot(&mut self, id: crate::CutId, tick: Tick) {
        let intents = self.region.take_intents();
        if !intents.is_empty() {
            let _ = self.out.send((self.id, RegionOutput::Intents(intents)));
        }
        let people = self.region.data().people_here();
        if !people.is_empty() {
            let _ = self.out.send((self.id, RegionOutput::PeoplePositions(people)));
        }
        let snapshot = self.portable_snapshot();
        let _ = self.out.send((self.id, RegionOutput::CutSnapshot(id, tick, snapshot)));
    }

    /// Worldgen for [`RegionInput::SeedHaloFromWorldgen`]; without one such a
    /// seed is skipped (logged).
    pub fn set_stud_halo_generator(&mut self, generator: StudHaloGenerator) {
        self.stud_halo_generator = Some(generator);
    }

    /// The world store's terrain for [`RegionInput::SeedHaloFromStore`];
    /// without one such seeds come from worldgen.
    pub fn set_stored_terrain(&mut self, stored: std::sync::Arc<dyn crate::StoredTerrain>) {
        self.stored_terrain = Some(stored);
    }

    /// Test support: from now on record `(tick, state hash)` right after each
    /// Tick event (see [`RegionRunner::take_slot_start_hashes`]).
    pub fn record_slot_start_hashes(&mut self) {
        self.slot_start_hashes.get_or_insert_with(Vec::new);
    }

    /// The slot-start hashes recorded since the last call.
    pub fn take_slot_start_hashes(&mut self) -> Vec<(Tick, u32)> {
        self.slot_start_hashes.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// `(checks, total, slowest)` of the every-`state_hash_interval()` state
    /// hashes since the previous call; resets it. Like the terrain hash it
    /// runs inside a tick, so `slowest` is a floor on that tick's duration.
    pub fn take_state_hash_cost(&mut self) -> (u32, std::time::Duration, std::time::Duration) {
        std::mem::take(&mut self.state_hash_cost)
    }

    /// Items the region's pile caps destroyed since the previous call
    /// (`Region::take_items_destroyed`); resets it. Metrics only.
    pub fn take_items_destroyed(&mut self) -> u64 {
        self.region.take_items_destroyed()
    }

    /// The region's terrain as the manager keeps it (halo seeds of starting
    /// neighbours are cut from it).
    fn terrain(&self) -> TerrainSnapshot {
        self.region.terrain_snapshot()
    }

    /// Returns false when the runner should stop (after Shutdown).
    pub fn handle_input(&mut self, input: RegionInput) -> bool {
        match input {
            RegionInput::Event(kind) if kind.input_client().is_some() => self.route_input(kind, None),
            RegionInput::Event(kind) => match self.behind_held_body(&kind) {
                Some(body) => self.queue_behind(kind, body, body.stamp()),
                None => self.apply_input(kind),
            },
            RegionInput::Transfer(kind, stamp) => match self.behind_held_body(&kind) {
                Some(body) => self.queue_behind(kind, body, stamp),
                None => self.inbox.push(kind, stamp),
            },
            RegionInput::EventAfterTick(kind, after_tick) if kind.input_client().is_some() => {
                self.route_input(kind, Some(after_tick))
            }
            RegionInput::EventAfterTick(kind, after_tick) => match self.behind_held_body(&kind) {
                Some(body) => self.queue_behind(kind, body, after_tick),
                None => self.hold_or_apply(kind, after_tick),
            },
            RegionInput::RequestSnapshot(client_id) => {
                let _ = self.out.send((
                    self.id,
                    RegionOutput::Snapshot(client_id, self.region.data().clone()),
                ));
            }
            RegionInput::StartAt(tick) => self.region.start_at(tick),
            RegionInput::SetRouteMap(route) => self.region.set_route_map(route),
            RegionInput::SaveAt(id, tick) => {
                let now = self.region.current_tick();
                if now < tick {
                    // Answered when `tick` brings the counter to it.
                    self.pending_cut = Some((id, tick));
                } else if now == tick && self.opened != Some(tick) {
                    // At `tick`, its slot not opened yet (a region started at
                    // `tick`, or restored there, before its first tick).
                    self.send_cut_snapshot(id, tick);
                } else {
                    let _ = self.out.send((self.id, RegionOutput::CutMissed(id)));
                }
            }
            RegionInput::HaloRequest(to) => {
                if let Some(seed) = self.region.data().halo_seed_for(to) {
                    let stamp = self.region.current_tick();
                    let _ = self.out.send((self.id, RegionOutput::HaloDeltas(vec![(seed, to)], stamp)));
                }
            }
            RegionInput::SeedHaloFromWorldgen(neighbours) => self.seed_halo_from_worldgen(neighbours),
            RegionInput::SeedHaloFromStore(neighbours) => self.seed_halo_from_store(neighbours),
            RegionInput::ListPeople(epoch) => match self.inbox.latest_stamp() {
                // Answered once every buffered transfer has been applied: at the
                // end of the latest one's slot (`tick`).
                Some(t) => self.people_list_due = Some((epoch, t)),
                None => self.send_people_listed(epoch),
            },
            RegionInput::Shutdown => {
                // A save cut at a tick this region will not reach now: it
                // parks there, so its answer is its state with what is held
                // for the slots before the cut tick applied (the slots it
                // would have run first), taken before what is held for the
                // cut's slot or later.
                if let Some((id, tick)) = self.pending_cut.take() {
                    self.apply_buffered(Some(tick));
                    self.send_cut_snapshot(id, tick);
                }
                // Input must not be lost to a park.
                self.apply_buffered(None);
                let people = self.region.data().people_here();
                for (person, _) in &people {
                    self.apply(GameEventKind::DespawnPerson(*person));
                }
                let _ = self.out.send((self.id, RegionOutput::PeopleParked(people)));
                // A parked region holds no unanswered intent; these and the
                // intents sent since the last tick go out.
                self.region.refuse_queued_intents();
                let intents = self.region.take_intents();
                if !intents.is_empty() {
                    let _ = self.out.send((self.id, RegionOutput::Intents(intents)));
                }
                // Its mirrors in the neighbours end with it.
                let stamp = self.region.current_tick();
                let gone = self.region.data().halo_entities_gone_all();
                if !gone.is_empty() {
                    let _ = self.out.send((self.id, RegionOutput::HaloEntities(gone, stamp)));
                }
                let serialized = SerializedRegion::from_rollback(self.region.data());
                let far = self.terrain();
                let portable = self.portable_snapshot();
                let _ = self.out.send((self.id, RegionOutput::Stopped(serialized, far, stamp, portable)));
                return false;
            }
        }
        true
    }

    /// The stamp of `client`'s body buffered in the inbox — its arrival (a
    /// crossing waiting for the end of its slot), or its creation (a create or
    /// restore the manager held past a save cut's tick) — if any.
    fn held_body(&self, client: ClientId) -> Option<HeldBody> {
        let (stamp, kind) = self.inbox.holds_entry(|k| match k {
            GameEventKind::EntityArrived(b) => b.client.as_ref().is_some_and(|(c, _)| *c == client),
            GameEventKind::CreateClient(c) | GameEventKind::CreateClientAt(c, ..) | GameEventKind::RestorePlayer(c, _) => {
                *c == client
            }
            _ => false,
        })?;
        Some(match kind {
            GameEventKind::EntityArrived(b) => HeldBody::Arrival(stamp, b.id),
            _ => HeldBody::Creation(stamp),
        })
    }

    /// Queue `kind` (stamped `stamp`) behind `body`: directly after a buffered
    /// arrival — after the inputs it carries, before the slot's other messages
    /// (`SlotInbox::push_after_arrival`) — unless it is for a later slot than
    /// the arrival's (then behind everything of its own stamp); behind a held
    /// creation, among class `Other` (`push_behind`).
    fn queue_behind(&mut self, kind: GameEventKind, body: HeldBody, stamp: Tick) {
        match body {
            HeldBody::Arrival(arrives, id) if stamp <= arrives => self.inbox.push_after_arrival(kind, arrives, id),
            held => self.inbox.push_behind(kind, held.stamp().max(stamp)),
        }
    }

    /// No event for a client is applied before its buffered body: the body
    /// to queue `kind` (a server event for a client — park, leave, …) behind
    /// ([`Self::queue_behind`]), when there is one. Any other plain `Event` overtakes the inbox (see
    /// [`RegionInput::Event`]).
    fn behind_held_body(&self, kind: &GameEventKind) -> Option<HeldBody> {
        let client = match kind {
            GameEventKind::ParkPlayer(c) | GameEventKind::ClientLeft(c) => Some(*c),
            k => k.origin_client(),
        }?;
        self.held_body(client)
    }

    /// Applies what is buffered — the inbox in canonical order, then the
    /// held client inputs — stamped before `before` (`None`: everything),
    /// until nothing such is left (an applied arrival may hold the inputs it
    /// carried). For a region about to park: the inbox first, as its
    /// messages for the open slot close it before any later slot's input.
    fn apply_buffered(&mut self, before: Option<Tick>) {
        // A stamp is a tick (or a held input's, at most `MAX_INPUT_LEAD_TICKS`
        // ahead), never `Tick::MAX`: `None` takes everything.
        let before = before.unwrap_or(Tick::MAX);
        loop {
            let transfers = self.inbox.drain_before(before);
            let (inputs, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held).into_iter().partition(|(_, t)| *t < before);
            self.held = rest.into();
            if inputs.is_empty() && transfers.is_empty() {
                return;
            }
            for kind in transfers {
                self.apply_input(kind);
            }
            for (kind, _) in inputs {
                self.apply(kind);
            }
        }
    }

    /// Halo seeds read from the world store here, applied at once (before
    /// the first tick); a neighbour the store cannot serve (logged) is
    /// seeded from worldgen. A region that has ticked already (another host saved a
    /// neighbour: `WorldManager::stored_terrain_changed`) keeps its mirror of a neighbour
    /// the store cannot serve — worldgen would be older still.
    fn seed_halo_from_store(&mut self, neighbours: Vec<RegionCoords>) {
        let mut generate = Vec::new();
        for n in neighbours {
            let snap = match &self.stored_terrain {
                Some(stored) => crate::load_stored_or_warn(stored.as_ref(), n, "halo seed"),
                None => {
                    log::warn!("region {:?}: no stored terrain source; halo of {n:?} from worldgen", self.id);
                    None
                }
            };
            match snap {
                Some(snap) => {
                    if let Some(seed) = crate::halo::halo_seed_from_snapshot(n, self.id, &snap) {
                        self.apply(GameEventKind::HaloUpdate(seed));
                    }
                }
                None => generate.push(n),
            }
        }
        if !generate.is_empty() && self.opened.is_some() {
            log::warn!("region {:?}: halo of {generate:?} not re-seeded (the store could not serve it); kept", self.id);
        } else if !generate.is_empty() {
            self.seed_halo_from_worldgen(generate);
        }
    }

    /// Halo seeds generated here, applied at once (before the first tick).
    fn seed_halo_from_worldgen(&mut self, neighbours: Vec<RegionCoords>) {
        // Only a spawner that seeds halos on its region threads asks for
        // this, and every such spawner hands its runners a generator.
        let Some(studs) = self.stud_halo_generator.clone() else {
            debug_assert!(false, "region {:?}: halo seed from worldgen without a halo generator", self.id);
            log::error!("region {:?}: no halo generator; {} neighbour seeds skipped", self.id, neighbours.len());
            return;
        };
        for n in neighbours {
            // The whole facing strip in one call: the trees past the strip that decide
            // its bricks are worked out once (cut into windows they cost 4x; measured
            // 2026-11-11, `worldgen` `stud_trees::halo_window_cost` — the strip's
            // region-wide patch costs nothing next to them).
            let (xs, zs) = crate::halo::facing_columns(n, self.id);
            let seed = studs(n, xs, zs);
            let snap = TerrainSnapshot::of_stud_edge(crate::StudEdge::from_seed(&seed));
            if let Some(seed) = crate::halo::halo_seed_from_snapshot(n, self.id, &snap) {
                self.apply(GameEventKind::HaloUpdate(seed));
            }
        }
    }

    /// An event predicted after tick `after_tick`: held for that slot, or
    /// applied now when the slot is open or past (or the stamp is a runaway).
    fn hold_or_apply(&mut self, kind: GameEventKind, after_tick: u64) {
        let now = self.region.current_tick() as u64;
        if after_tick > now && after_tick - now <= MAX_INPUT_LEAD_TICKS {
            self.held.push_back((kind, after_tick));
        } else {
            self.apply(kind);
        }
    }

    /// A client input (`stamp`: the tick it was predicted after, if any)
    /// follows the client's body:
    /// - body here: held for its slot or applied, as any event;
    /// - body on its way here (its arrival, or its creation, is buffered):
    ///   queued right behind it in the inbox, or held for its own later slot
    ///   (by then the body is in);
    /// - body left recently: sent back as [`RegionOutput::Forward`];
    /// - otherwise (no such client here at all) handled as before.
    fn route_input(&mut self, kind: GameEventKind, stamp: Option<u64>) {
        let client = kind.input_client().expect("route_input takes client inputs");
        if !self.region.data().clients.contains_key(&client) {
            // Its arrival, or its creation held past a save cut
            // (`WorldManager::send_play_event`).
            if let Some(body) = self.held_body(client) {
                let arrives = body.stamp();
                let now = self.region.current_tick() as u64;
                match stamp {
                    // A later slot than the arrival's, not open yet: the
                    // arrival is applied before the Tick that opens it.
                    Some(s) if s > arrives && s > now && s - now <= MAX_INPUT_LEAD_TICKS => {
                        self.held.push_back((kind, s))
                    }
                    _ => self.queue_behind(kind, body, arrives),
                }
                return;
            }
            if let Some(&(_, left)) = self.departed.get(&client) {
                let stamp = stamp.map_or(left, |s| s.max(left));
                let _ = self.out.send((self.id, RegionOutput::Forward(kind, stamp)));
                return;
            }
        }
        match stamp {
            Some(s) => self.hold_or_apply(kind, s),
            None => self.apply(kind),
        }
    }

    /// A manager-routed event ([`RegionInput::Event`], or a
    /// [`RegionInput::Transfer`] the inbox released).
    fn apply_input(&mut self, kind: GameEventKind) {
        match kind {
            // The manager filters these; drop defensively rather than
            // double-ticking or stopping on a stray packet.
            GameEventKind::Tick | GameEventKind::Quit => {}
            // The inputs the source held for the body follow it in, right
            // after it, with their stamps; the authoritative event carries none.
            GameEventKind::EntityArrived(mut bundle) => {
                let pending = std::mem::take(&mut bundle.pending_inputs);
                if let Some((client, _)) = &bundle.client {
                    self.departed.remove(client);
                }
                self.apply(GameEventKind::EntityArrived(bundle));
                for (kind, stamp) in pending {
                    self.route_input(kind, Some(stamp));
                }
            }
            // An input queued behind its client's arrival.
            kind if kind.input_client().is_some() => self.route_input(kind, None),
            GameEventKind::ParkPlayer(client) => {
                let record = self.region.data().player_record(client, self.id).map(Box::new);
                let data = self.region.data();
                let body = data.player_entites.get(&client).and_then(|&k| data.entity_id(k));
                self.apply(GameEventKind::ParkPlayer(client));
                let _ = self.out.send((self.id, RegionOutput::PlayerParked(client, record, body)));
            }
            kind => self.apply(kind),
        }
    }

    fn apply(&mut self, kind: GameEventKind) {
        // Server-side regions never roll back: forget each event's
        // transaction immediately (undo log stays bounded), same policy as
        // the old main loop.
        let event = self
            .region
            .handle_event(kind)
            .expect("region event processing failed");
        self.region.commit_server_event();
        let _ = self.out.send((self.id, RegionOutput::EventProcessed(event)));
    }

    /// [`RegionOutput::PeopleListed`]: the people with a body here now.
    fn send_people_listed(&mut self, epoch: u64) {
        let people = self.region.data().people_here();
        let _ = self.out.send((self.id, RegionOutput::PeopleListed(epoch, people)));
    }

    /// One sim tick + the every-10-ticks SyncClock self-report. The caller
    /// owns pacing (thread timer on the server, frame accumulator on wasm).
    ///
    /// First the slot this Tick closes ends: every buffered neighbour message
    /// stamped for it or earlier is applied, in canonical order
    /// (`crate::slot_order`) — so a slot's events are its `Tick`, its client
    /// inputs, then its neighbour messages sorted.
    pub fn tick(&mut self) {
        let closing = self.region.current_tick();
        for kind in self.inbox.drain_due(closing) {
            self.apply_input(kind);
        }
        if let Some((epoch, due)) = self.people_list_due {
            if due <= closing {
                self.people_list_due = None;
                self.send_people_listed(epoch);
            }
        }
        let event = self
            .region
            .handle_event(GameEventKind::Tick)
            .expect("region tick failed");
        self.region.commit_server_event();
        if let Some(hashes) = self.slot_start_hashes.as_mut() {
            hashes.push((self.region.current_tick(), crate::state_hash(self.region.data())));
        }
        // Hashed before the held events below, so it describes the state the
        // client holds right after confirming this Tick.
        let interval = crate::state_hash_interval() as crate::Tick;
        let output = if interval > 0 && self.region.current_tick() % interval == 0 {
            let watch = crate::perf::Stopwatch::start();
            let hash = crate::state_hash(self.region.data());
            let took = watch.elapsed();
            self.state_hash_cost.0 += 1;
            self.state_hash_cost.1 += took;
            self.state_hash_cost.2 = self.state_hash_cost.2.max(took);
            RegionOutput::EventProcessedHashed(event, self.region.current_tick(), hash)
        } else {
            RegionOutput::EventProcessed(event)
        };
        let _ = self.out.send((self.id, output));
        let now = self.region.current_tick() as u64;
        self.opened = Some(now);
        // This tick's crossings, before the slot opens: a departing client's
        // inputs held for this slot or later go with its body.
        let (mut departures, mut halo_entities) = self.region.take_transfers();
        self.departed.retain(|_, (_, left)| now - *left <= MAX_INPUT_LEAD_TICKS);
        for (bundle, target) in &mut departures {
            let Some(client) = bundle.client.as_ref().map(|(c, _)| *c) else { continue };
            let (follow, stay): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held)
                .into_iter()
                .partition(|(k, t)| k.input_client() == Some(client) && *t >= now);
            self.held = stay.into();
            bundle.pending_inputs.extend(follow);
            self.departed.insert(client, (*target, now));
        }
        // A save cut at this tick is taken here, at the opening of the slot:
        // what departed in the Tick goes out first (it is not in the
        // snapshot), then the snapshot (`send_cut_snapshot`).
        let mut departed_ids: Vec<crate::EntityId> = Vec::new();
        match self.pending_cut {
            Some((id, tick)) if tick == now => {
                self.pending_cut = None;
                departed_ids.extend(departures.iter().map(|(b, _)| b.id));
                if !departures.is_empty() {
                    let _ = self.out.send((self.id, RegionOutput::Departures(std::mem::take(&mut departures), now)));
                }
                self.send_cut_snapshot(id, tick);
            }
            Some((id, tick)) if tick < now => {
                // Never: the counter steps one tick at a time and the cut was
                // asked for ahead. Defensive.
                self.pending_cut = None;
                let _ = self.out.send((self.id, RegionOutput::CutMissed(id)));
            }
            _ => {}
        }
        // Slot `now`: events predicted after the tick that just ran, in
        // arrival order (its neighbour messages close it, at the next tick).
        let mut i = 0;
        while i < self.held.len() {
            if self.held[i].1 <= now {
                let (kind, _) = self.held.remove(i).expect("index checked");
                self.apply(kind);
            } else {
                i += 1;
            }
        }
        // (The slot's events cross nothing; the Tick's scan did.)
        let (more_departures, more_halo) = self.region.take_transfers();
        departures.extend(more_departures);
        halo_entities.extend(more_halo);
        let stamp = self.region.current_tick();
        let halo = self.region.take_halo_updates();
        departed_ids.extend(departures.iter().map(|(b, _)| b.id));
        if !departures.is_empty() {
            let _ = self.out.send((self.id, RegionOutput::Departures(departures, stamp)));
        }
        if !halo_entities.is_empty() {
            let _ = self.out.send((self.id, RegionOutput::HaloEntities(halo_entities, stamp)));
        }
        if !halo.is_empty() {
            let _ = self.out.send((self.id, RegionOutput::HaloDeltas(halo, stamp)));
        }
        self.report_ids(&departed_ids);
        let intents = self.region.take_intents();
        if !intents.is_empty() {
            let _ = self.out.send((self.id, RegionOutput::Intents(intents)));
        }
        if self.region.current_tick() % 10 == 0 {
            let _ = self.out.send((
                self.id,
                RegionOutput::SyncClock {
                    tick_rate: TICK_RATE,
                    tick: self.region.current_tick(),
                },
            ));
        }
    }

    pub fn current_tick(&self) -> crate::Tick {
        self.region.current_tick()
    }

    /// The location directory's diffs since the last report: ids owned here
    /// now and not then (`Spawned`), and then and not now except `departed`
    /// (`Despawned` — a departure's new place comes with the `Departures`).
    fn report_ids(&mut self, departed: &[crate::EntityId]) {
        let now: std::collections::BTreeSet<crate::EntityId> = self.region.data().id_index.keys().copied().collect();
        let spawned: Vec<_> = now.difference(&self.known_ids).copied().collect();
        let gone: Vec<_> = self.known_ids.difference(&now).filter(|id| !departed.contains(id)).copied().collect();
        self.known_ids = now;
        if !spawned.is_empty() {
            let _ = self.out.send((self.id, RegionOutput::Spawned(spawned)));
        }
        if !gone.is_empty() {
            let _ = self.out.send((self.id, RegionOutput::Despawned(gone)));
        }
    }

    pub fn region_mut(&mut self) -> &mut Region {
        &mut self.region
    }
}
