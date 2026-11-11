use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap, VecDeque},
    ops::DerefMut,
};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

use crossbeam::channel::Sender;
use log::{debug, info};
use crate::input::Key;
use crate::{Client, ClientId, GameData, Rollback, Tick};

use crate::{
    camera::CameraController, physics::PhysicsController,
    Controller, EntityBundle, GameDataUpdate, GameError, GameEvent, GameEventKind, HaloEntityMsg,
    RegionCoords, RegionId, ServerPacket,
};

/// A region started at world tick `t` ([`Region::start_at`]) allocates its
/// `EntityId` serials from `t << ENTITY_SERIAL_TICK_SHIFT`: 2^20 ids per
/// tick of headroom between incarnations, and 2^44 ticks (~28,000 years at
/// 50 ms) before the product leaves `u64`.
pub const ENTITY_SERIAL_TICK_SHIFT: u32 = 20;

/// Opens or closes the inventory. Closing (in either play mode) closes the
/// chest shown in it and returns the cursor stack to the inventory; what does
/// not fit is returned, for the caller to drop as a pile at the player's feet
/// ([`Rollback::drop_at_feet_safe`]).
pub(crate) fn toggle_inventory(c: &mut Client) -> Option<crate::ItemStack> {
    if c.inventory_open {
        return close_inventory(c);
    }
    c.inventory_open = true;
    None
}

/// Closes the inventory if it is open (else a no-op), as
/// [`toggle_inventory`] closes it.
pub(crate) fn close_inventory(c: &mut Client) -> Option<crate::ItemStack> {
    if !c.inventory_open {
        return None;
    }
    c.inventory_open = false;
    c.open_container = None;
    c.stow_cursor()
}

impl Rollback {
    /// Drops `stack` as a pile at the cell of `client`'s feet, pickable after
    /// [`crate::PICKUP_DELAY_TICKS`]. With no body to drop it from, it goes
    /// back on the cursor instead, so nothing is lost.
    pub(crate) fn drop_at_feet_safe(&mut self, client: ClientId, stack: crate::ItemStack) {
        let feet = self.data.player_entites.get(&client).and_then(|&e| self.body_feet(e));
        match feet {
            Some(feet) => {
                let tick = *self.data.tick;
                let at = crate::feet_tile(feet);
                // From the body: a part a neighbour's piles cannot hold
                // comes back to it. A loss to the caps is counted where it
                // happens.
                let by = self.client_body_id(client).unwrap_or(crate::EntityId::of_region(*self.data.region));
                let _destroyed = self.deposit_items_by_safe(by, at, at, stack, tick, crate::PICKUP_DELAY_TICKS);
            }
            None => {
                if let Some(c) = self.data.clients.get_mut(&client) {
                    c.cursor = Some(stack);
                }
            }
        }
    }
}

/// Cumulative wall-time breakdown of `handle_event(Tick)` executions since
/// the last `take_tick_perf()`. Native-only: `Instant::now()` panics on
/// wasm32-unknown-unknown, so the accumulator and its call sites compile away
/// there. Reconcile replays run through `handle_event` too, so `ticks` counts
/// tick *executions* (main-loop + replayed), not sim-time ticks — exactly the
/// cost the client's keep-up budget cares about. The server accumulates but
/// never drains; a few `Instant::now()` reads per tick are noise next to the
/// ~ms tick itself.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct RegionTickPerf {
    /// Tick executions recorded (main-loop + reconcile replays).
    pub ticks: u32,
    pub total: Duration,
    /// Slowest single execution.
    pub max: Duration,
    /// Controller pass (camera + rapier physics step; physics dominates).
    pub controllers: Duration,
    /// Per-client input step + fps-cam toggle.
    pub input: Duration,
    /// Boundary scan (departures, entity halo) + tick bump.
    pub scan: Duration,
}

#[cfg(not(target_arch = "wasm32"))]
impl RegionTickPerf {
    fn record(&mut self, start: Instant, controllers_done: Instant, input_done: Instant) {
        let end = Instant::now();
        let total = end - start;
        self.ticks += 1;
        self.total += total;
        if total > self.max {
            self.max = total;
        }
        self.controllers += controllers_done - start;
        self.input += input_done - controllers_done;
        self.scan += end - input_done;
    }

    /// Fold another region's accumulator into this one (world-wide sum).
    pub fn merge(&mut self, o: &RegionTickPerf) {
        self.ticks += o.ticks;
        self.total += o.total;
        if o.max > self.max {
            self.max = o.max;
        }
        self.controllers += o.controllers;
        self.input += o.input;
        self.scan += o.scan;
    }
}

/// What one `reconcile()` pass cost the client, for keep-up metrics.
/// A single pass can drain several buffered server events, so the counts
/// accumulate across every misprediction handled in that call.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReconcileStats {
    /// Number of predicted events that diverged from server authority and
    /// triggered a rollback+replay in this pass.
    pub mispredictions: u32,
    /// Sum of replay depths (predicted ticks rolled back and re-simulated)
    /// across those mispredictions — divide by `mispredictions` for the mean.
    pub replay_depth_sum: usize,
    /// Deepest single rollback in this pass.
    pub max_replay_depth: usize,
    /// Mispredictions by what was predicted at the id and what the server
    /// had there instead: `by_kinds[predicted][server]`, indexed by
    /// [`ReconcileStats::kind_slot`]. Says which way the timelines disagree —
    /// a predicted tick displaced by our own input is the client running
    /// ahead of the server, the reverse is it running behind.
    pub by_kinds: [[u32; 3]; 3],
    /// Server state hashes (`ServerPacket::HashedGameEvent`) compared
    /// against this client's confirmed state in this pass, and how many of
    /// them differed (each is also logged as an error).
    pub state_hash_checks: u32,
    pub state_hash_mismatches: u32,
}

impl ReconcileStats {
    /// Names of the [`Self::by_kinds`] slots.
    pub const KIND_NAMES: [&'static str; 3] = ["tick", "input", "transfer"];

    /// Slot of an event kind in [`Self::by_kinds`]: ticks, client-originated
    /// events, and everything relayed between regions.
    pub fn kind_slot(kind: &GameEventKind) -> usize {
        match kind {
            GameEventKind::Tick => 0,
            GameEventKind::PlayerInput(..)
            | GameEventKind::UiAction(..)
            | GameEventKind::Order(..)
            | GameEventKind::Prefab(..) => 1,
            _ => 2,
        }
    }
}

/// A predicted event in [`Region`]'s log, with the slot it was predicted in:
/// the region's tick when it was first handled — what a client input was
/// stamped with for the server. Replays keep it, wherever the event lands.
#[derive(Debug, Clone)]
struct Logged {
    event: GameEvent,
    stamp: Tick,
}

impl std::ops::Deref for Logged {
    type Target = GameEvent;
    fn deref(&self) -> &GameEvent {
        &self.event
    }
}

impl std::ops::DerefMut for Logged {
    fn deref_mut(&mut self) -> &mut GameEvent {
        &mut self.event
    }
}

/// A region represents an portion of the world that processes game events at
/// its own tick rate separately from other regions.
#[allow(unused)]
pub struct Region {
    event_log: VecDeque<Logged>,
    data: Rollback,
    id: RegionId,
    input_buffer: BinaryHeap<Reverse<GameEvent>>,
    controllers: Vec<Box<dyn Controller>>,
    synchronized: bool,
    /// `Some` on a client (which client's predictions live in `event_log`);
    /// `None` on the server, which never reconciles.
    local_client_id: Option<ClientId>,
    /// `next_game_event_id` of the snapshot this region was built from.
    /// Server events below this id are already baked into the state.
    base_event_id: usize,
    /// Boundary-scan results from the LAST executed tick. REPLACED by each
    /// tick, never appended: consumers read immediately after driving a
    /// tick; reconcile replays overwrite these with stale scans that are
    /// never read (see the plan's read-after-progress invariant).
    pending_departures: Vec<(EntityBundle, RegionCoords)>,
    pending_halo_entities: Vec<(HaloEntityMsg, RegionCoords)>,
    /// Own pile and chest cells in the pile halo band written since the last
    /// [`Region::take_halo_updates`] (`item_piles.rs`). Not state: a client's
    /// reconcile clears it (its replays re-write cells whose updates the
    /// forward ticks already sent).
    halo_pile_dirty: std::collections::BTreeSet<crate::WorldBlockPos>,
    /// Own stud chunks in the halo band written since the last
    /// [`Region::take_halo_updates`] (`stud_halo.rs`); cleared like `halo_pile_dirty`.
    halo_stud_dirty: std::collections::BTreeSet<crate::StudChunkCoords>,
    /// Intents and replies this region's events sent to other regions
    /// (`intents.rs`), each with its own due tick; taken after each tick
    /// ([`Region::take_intents`]). Not state, and not filled by a client's
    /// reconcile (its forward events already sent theirs).
    pending_intents: Vec<crate::IntentMsg>,
    /// Inside [`Region::reconcile`]: replays send no intents out.
    replaying: bool,
    /// The finite world's regions, when it has an edge: nothing crosses
    /// out of it (walks stop at the edge tile, bodies are held at the outer
    /// seam). Not sim state — set by whoever runs the region (the manager
    /// on the server, the client from its `WorldMap`), the same on both.
    map_extent: Option<crate::MapExtent>,
    /// The coarse route's view of the world (`path_plan.rs`), bounded by
    /// `map_extent`; walks plan over it.
    route_map: Option<std::sync::Arc<crate::RouteMap>>,
    /// Client-only: the server's state hash after each of these event ids
    /// (`id -> (tick, hash)`), checked when reconcile confirms that event.
    /// Ids at or below the confirmed head are dropped as missed.
    expected_hashes: BTreeMap<usize, (Tick, u32)>,
    /// Client-only: id of the newest server event reconcile has confirmed.
    last_confirmed_id: Option<usize>,
    /// Wall-time attribution for Tick executions, drained by the client's
    /// budget reporter (native only — see `RegionTickPerf`).
    #[cfg(not(target_arch = "wasm32"))]
    tick_perf: RegionTickPerf,
    /// Items the pile caps destroyed in this region's events since the last
    /// `take_items_destroyed` (metrics only; not state, not hashed — a
    /// client replay counts again).
    items_destroyed: u64,
}

/// Compact one-line description of a game event for reconcile tracing.
fn ev_desc(e: &GameEvent) -> String {
    let kind = match &e.kind {
        GameEventKind::Tick => "Tick".to_string(),
        GameEventKind::PlayerInput(c, ie) => format!("PlayerInput(c={c:?}, {ie:?})"),
        GameEventKind::UiAction(c, a) => format!("UiAction(c={c:?}, {a:?})"),
        GameEventKind::Order(c, o) => format!("Order(c={c:?}, {o:?})"),
        GameEventKind::Prefab(c, p) => format!("Prefab(c={c:?}, {p:?})"),
        GameEventKind::CreateClient(c) => format!("CreateClient(c={c:?})"),
        GameEventKind::CreateClientAt(c, col, carried) => {
            format!("CreateClientAt(c={c:?}, col={col:?}, carried={})", carried.is_some())
        }
        GameEventKind::ClientLeft(c) => format!("ClientLeft(c={c:?})"),
        GameEventKind::ParkPlayer(c) => format!("ParkPlayer(c={c:?})"),
        GameEventKind::RestorePlayer(c, r) => format!("RestorePlayer(c={c:?}, region={:?})", r.region),
        GameEventKind::Quit => "Quit".to_string(),
        GameEventKind::EntityArrived(b) => {
            format!("EntityArrived(c={:?})", b.client.as_ref().map(|(c, _)| *c))
        }
        GameEventKind::HaloEntity(d) => format!("HaloEntity({:?})", d.id),
        GameEventKind::HaloEntityGone(g) => format!("HaloEntityGone({:?})", g.id),
        GameEventKind::HaloSourceGone(rc) => format!("HaloSourceGone({rc:?})"),
        GameEventKind::HaloUpdate(u) => {
            format!("HaloUpdate(from={:?}, studs={})", u.from, u.studs.len())
        }
        GameEventKind::SpawnPerson(s) => format!("SpawnPerson({s:?})"),
        GameEventKind::DespawnPerson(p) => format!("DespawnPerson({p:?})"),
        GameEventKind::WalkOut(p, to) => format!("WalkOut({p:?}, {to:?})"),
        GameEventKind::Intent(m) => format!("Intent({m:?})"),
    };
    format!("[id={} r=({},{}) {}]", e.id, e.region_id.x, e.region_id.z, kind)
}

impl Region {
    /// Create new region
    pub fn new(
        mut data: Rollback,
        game_update_send: Option<Sender<GameDataUpdate>>,
        id: RegionId,
        local_client_id: Option<ClientId>,
    ) -> Self {
        data.reinitialize(game_update_send);
        if *data.region != id {
            // The id namespace of this state (`EntityId::birth_region`); its
            // own forgotten transaction, like `jump_clock_to`.
            data.new_transaction();
            data.region.update(|r| *r = id);
            data.forget();
        }
        let base_event_id = *data.next_game_event_id;
        Self {
            data,
            event_log: VecDeque::new(),
            input_buffer: BinaryHeap::new(),
            controllers: Vec::from([CameraController::new(), PhysicsController::new()]),
            id,
            synchronized: false,
            local_client_id,
            base_event_id,
            pending_departures: Vec::new(),
            pending_halo_entities: Vec::new(),
            halo_pile_dirty: Default::default(),
            halo_stud_dirty: Default::default(),
            pending_intents: Vec::new(),
            replaying: false,
            map_extent: None,
            route_map: None,
            expected_hashes: BTreeMap::new(),
            last_confirmed_id: None,
            #[cfg(not(target_arch = "wasm32"))]
            tick_perf: RegionTickPerf::default(),
            items_destroyed: 0,
        }
    }

    /// Client: the server's [`crate::state_hash`] after event `event_id`
    /// (tick `tick`). Checked by [`Region::reconcile`] when it confirms that
    /// event; an event already confirmed, or baked into the snapshot this
    /// region was built from, can no longer be checked and is dropped.
    pub fn expect_state_hash(&mut self, event_id: usize, tick: Tick, hash: u32) {
        let confirmed = self.last_confirmed_id.is_some_and(|c| event_id <= c);
        if event_id < self.base_event_id || confirmed {
            debug!(
                "[state-hash r={:?}] server hash for id={event_id} tick={tick} arrived after that event was confirmed; not checked",
                self.id
            );
            return;
        }
        self.expected_hashes.insert(event_id, (tick, hash));
    }

    /// Reconcile just confirmed server event `id`: the state is the
    /// authoritative state after it, plus — when `predictions_on_top` — the
    /// still-unconfirmed predictions in `event_log`. If the server sent its
    /// hash for `id`, compare. Predictions are rolled back for the hash and
    /// replayed after it (exact inverses, like a misprediction), which is why
    /// the server only hashes every `state_hash_interval()` ticks.
    fn confirmed(&mut self, id: usize, predictions_on_top: bool, stats: &mut ReconcileStats) {
        self.last_confirmed_id = Some(id);
        if self.expected_hashes.is_empty() {
            return;
        }
        // Anything older was never confirmed in place (snapshot, skipped id).
        while let Some((&first, _)) = self.expected_hashes.first_key_value() {
            if first >= id {
                break;
            }
            self.expected_hashes.remove(&first);
        }
        let Some((tick, expected)) = self.expected_hashes.remove(&id) else { return };
        let actual = if predictions_on_top && !self.event_log.is_empty() {
            let logged: Vec<Logged> = self.event_log.iter().cloned().collect();
            while self.event_log.pop_back().is_some() {
                self.data.rollback();
            }
            let hash = crate::state_hash(&self.data);
            self.replay(logged);
            hash
        } else {
            crate::state_hash(&self.data)
        };
        stats.state_hash_checks += 1;
        if actual != expected {
            stats.state_hash_mismatches += 1;
            log::error!(
                "[state-hash r=({},{})] DESYNC after event id={id} tick={tick}: server {expected:#010x} != client {actual:#010x}",
                self.id.x,
                self.id.z
            );
            crate::perf_event!(
                "state_hash_mismatch",
                "region={},{} id={id} tick={tick} server={expected:#010x} client={actual:#010x}",
                self.id.x,
                self.id.z
            );
        } else {
            debug!("[state-hash r={:?}] ok id={id} tick={tick} hash={actual:#010x}", self.id);
        }
    }

    /// Bounds this region's sim to `extent` (`None` = unbounded): see
    /// [`crate::MapExtent`], with nothing for a walk's coarse route to walk
    /// around ([`crate::RouteMap::open`]). Takes effect from the next event.
    pub fn set_map_extent(&mut self, extent: Option<crate::MapExtent>) {
        self.set_route_map(extent.map(|e| std::sync::Arc::new(crate::RouteMap::open(e))));
    }

    /// Bounds this region's sim to `route`'s extent and routes walks over it
    /// (`path_plan.rs`); `None` = unbounded, straight-line routes. Takes
    /// effect from the next event.
    pub fn set_route_map(&mut self, route: Option<std::sync::Arc<crate::RouteMap>>) {
        self.map_extent = route.as_ref().map(|r| r.extent());
        self.route_map = route;
    }

    pub fn map_extent(&self) -> Option<crate::MapExtent> {
        self.map_extent
    }

    /// Check if event from network matches client event history. rollback the
    /// game state as neccessary and re-simulate to current time.
    pub fn reconcile(&mut self, server_event: GameEvent) -> Result<ReconcileStats, GameError> {
        self.replaying = true;
        let result = self.reconcile_inner(server_event);
        self.replaying = false;
        result
    }

    fn reconcile_inner(&mut self, server_event: GameEvent) -> Result<ReconcileStats, GameError> {
        let mut stats = ReconcileStats::default();
        // Events older than the snapshot this region was constructed from
        // are already baked into its state.
        if server_event.id < self.base_event_id {
            debug!(
                "[reconcile r={:?}] IGNORE {} below base_event_id={} (already baked in)",
                self.id,
                ev_desc(&server_event),
                self.base_event_id
            );
            return Ok(stats);
        }
        self.input_buffer.push(Reverse(server_event.clone()));

        if self.input_buffer.len() > 1000 {
            panic!("Server event input buffer too big.");
        }

        debug!(
            "[reconcile r={:?}] RECV {} | event_log.len={} front={} back={} next_id={} input_buffer.len={}",
            self.id,
            ev_desc(&server_event),
            self.event_log.len(),
            self.event_log
                .front()
                .map(|e| ev_desc(e))
                .unwrap_or_else(|| "none".to_string()),
            self.event_log
                .back()
                .map(|e| e.id.to_string())
                .unwrap_or_else(|| "none".to_string()),
            *self.data.next_game_event_id,
            self.input_buffer.len(),
        );

        if self.event_log.len() == 0 {
            debug!(
                "[reconcile r={:?}] no predictions in event_log; {} parked in input_buffer (len={})",
                self.id,
                ev_desc(&server_event),
                self.input_buffer.len()
            );
            return Ok(stats);
        }

        // The slot the authoritative arrival of the local player landed in,
        // if this pass integrated one (see `drop_local_inputs_before`).
        let mut local_arrival_slot = None;

        'outer: while !self.input_buffer.is_empty() && !self.event_log.is_empty() {
            if let Some(server_event) = self.input_buffer.pop() {
                if let Some(event) = self.event_log.pop_front() {
                    debug!(
                        "[reconcile r={:?}] COMPARE predicted {} vs server {}",
                        self.id,
                        ev_desc(&event),
                        ev_desc(&server_event.0)
                    );
                    if event.id == server_event.0.id {
                        if event.event != server_event.0 {
                            debug!(
                                "[reconcile r={:?}] MISPREDICT at id={}: predicted {} != server {} -> rollback+replay",
                                self.id,
                                event.id,
                                ev_desc(&event),
                                ev_desc(&server_event.0)
                            );
                            if matches!(
                                (&event.kind, &server_event.0.kind),
                                (GameEventKind::PlayerInput(..), GameEventKind::PlayerInput(..))
                            ) {
                                debug!(
                                    "[reconcile r={:?}] INPUT-VS-INPUT FULL DIFF:\n  predicted: {:?}\n  server:    {:?}",
                                    self.id, event, server_event.0
                                );
                            }
                            stats.by_kinds[ReconcileStats::kind_slot(&event.kind)]
                                [ReconcileStats::kind_slot(&server_event.0.kind)] += 1;
                            self.event_log.push_front(event);
                            // Replay depth = predicted ticks about to be rolled
                            // back and re-simulated for this misprediction.
                            let depth = self.event_log.len();
                            stats.mispredictions += 1;
                            stats.replay_depth_sum += depth;
                            if depth > stats.max_replay_depth {
                                stats.max_replay_depth = depth;
                            }
                            let mut temp_log = self.event_log.clone();

                            // rollback whole event log
                            while let Some(_e) = self.event_log.pop_back() {
                                self.data.rollback();
                            }

                            // apply server event that was different from expected.
                            self.handle_event(server_event.clone().0.kind)?;
                            self.data.forget();
                            // Every prediction is rolled back and none is
                            // replayed yet: the state is exactly the server's
                            // after this event.
                            self.confirmed(server_event.0.id, false, &mut stats);

                            let foreign = match server_event.0.kind.origin_client() {
                                Some(origin) => Some(origin) != self.local_client_id,
                                None => false,
                            };

                            if foreign {
                                // Another client's event is an *insertion* into
                                // our timeline: keep every prediction, shift
                                // them all one id later.
                                debug!(
                                    "[reconcile r={:?}] FOREIGN insert {}: keeping all {} predictions, shifting +1 id",
                                    self.id,
                                    ev_desc(&server_event.0),
                                    temp_log.len()
                                );
                                for event in &mut temp_log {
                                    event.id += 1;
                                }
                            } else {
                                // Our own event came back at a different id
                                // than predicted: remove our matching
                                // prediction, shifting everything before it.
                                // Only locally-originated predictions are
                                // candidates for removal.
                                let server_kind = &server_event.0.kind;
                                let mut i = 0;
                                let mut removed_id = None;
                                let mut dropped_mirrors = 0;
                                while i < temp_log.len() {
                                    let e = &temp_log[i];
                                    let local_origin = match e.kind.origin_client() {
                                        Some(o) => Some(o) == self.local_client_id,
                                        None => true,
                                    };
                                    if local_origin && e.kind.matches_prediction(server_kind) {
                                        removed_id = Some(e.id);
                                        temp_log.remove(i);
                                        break;
                                    }
                                    // Margin mirrors are per slot (the events
                                    // between two Ticks): a server Tick closes
                                    // the slot, so a mirror predicted in it
                                    // that the server did not have is dropped
                                    // (never carried into the next slot) ...
                                    if matches!(server_kind, GameEventKind::Tick)
                                        && matches!(e.kind, GameEventKind::HaloEntity(_) | GameEventKind::HaloEntityGone(_) | GameEventKind::HaloUpdate(_) | GameEventKind::Intent(_))
                                    {
                                        temp_log.remove(i);
                                        dropped_mirrors += 1;
                                        continue;
                                    }
                                    // ... and a server mirror never takes the
                                    // place of a later slot's prediction.
                                    if matches!(server_kind, GameEventKind::HaloEntity(_) | GameEventKind::HaloEntityGone(_) | GameEventKind::HaloUpdate(_) | GameEventKind::Intent(_))
                                        && matches!(e.kind, GameEventKind::Tick)
                                    {
                                        break;
                                    }
                                    i += 1;
                                }
                                // The predictions follow the server's event,
                                // gap-free.
                                for (k, e) in temp_log.iter_mut().enumerate() {
                                    e.id = server_event.0.id + 1 + k;
                                }
                                if dropped_mirrors > 0 {
                                    debug!(
                                        "[reconcile r={:?}] dropped {} halo update(s) predicted in the slot server {} closed",
                                        self.id,
                                        dropped_mirrors,
                                        ev_desc(&server_event.0)
                                    );
                                }
                                if removed_id.is_none() {
                                    debug!(
                                        "[reconcile r={:?}] NO MATCHING PREDICTION for server {} — client behind; kept {} predictions, none removed",
                                        self.id,
                                        ev_desc(&server_event.0),
                                        temp_log.len()
                                    );
                                } else {
                                    debug!(
                                        "[reconcile r={:?}] removed local prediction id={:?} matching server {}; {} predictions remain, shifted",
                                        self.id,
                                        removed_id,
                                        ev_desc(&server_event.0),
                                        temp_log.len()
                                    );
                                }
                            }

                            self.replay(temp_log);
                            if self.is_local_arrival(&server_event.0) {
                                local_arrival_slot = Some(self.confirmed_tick());
                            }
                        } else {
                            debug!(
                                "[reconcile r={:?}] MATCH+FORGET {}",
                                self.id,
                                ev_desc(&server_event.0)
                            );
                            self.data.forget();
                            self.confirmed(server_event.0.id, true, &mut stats);
                            if self.is_local_arrival(&server_event.0) {
                                local_arrival_slot = Some(self.confirmed_tick());
                            }
                        }
                    } else {
                        // IDs are incorrect, game event arrived out of order.
                        debug!(
                            "[reconcile r={:?}] OUT-OF-ORDER: predicted {} vs server {} (ids differ); re-buffer both, break",
                            self.id,
                            ev_desc(&event),
                            ev_desc(&server_event.0)
                        );
                        self.input_buffer.push(server_event);
                        self.event_log.push_front(event);
                        break 'outer;
                    }
                }
            }
        }

        if let Some(slot) = local_arrival_slot {
            self.drop_local_inputs_before(slot);
        }
        // Replays re-wrote chunks whose halo updates their forward ticks
        // already sent.
        self.halo_pile_dirty.clear();
        self.halo_stud_dirty.clear();

        // Id-accounting invariant: predictions are appended with ids from
        // next_game_event_id, so the log tail must sit exactly one below it.
        // A violation means the replay machinery desynced the counter.
        if let Some(back) = self.event_log.back() {
            let next = *self.data.next_game_event_id;
            if back.id + 1 != next {
                log::warn!(
                    "[reconcile r={:?}] ID-DESYNC: event_log back id={} but next_game_event_id={} (expected back+1==next); front={}",
                    self.id,
                    back.id,
                    next,
                    self.event_log.front().map(|e| ev_desc(e)).unwrap_or_default(),
                );
            }
        }
        debug!(
            "[reconcile r={:?}] DONE: event_log.len={} input_buffer.len={} buffered_min_id={:?} mispredicts_this_pass={}",
            self.id,
            self.event_log.len(),
            self.input_buffer.len(),
            self.input_buffer.peek().map(|Reverse(e)| e.id),
            stats.mispredictions
        );
        Ok(stats)
    }

    /// Is `event` the arrival of this client's own player?
    fn is_local_arrival(&self, event: &GameEvent) -> bool {
        matches!(&event.kind, GameEventKind::EntityArrived(b)
            if self.local_client_id.is_some() && b.client.as_ref().map(|(c, _)| *c) == self.local_client_id)
    }

    /// The tick of the confirmed state: the current tick less the predicted
    /// ticks on top of it.
    fn confirmed_tick(&self) -> Tick {
        let predicted = self.event_log.iter().filter(|l| matches!(l.kind, GameEventKind::Tick)).count();
        self.current_tick() - predicted as Tick
    }

    /// Re-apply `log` (rolled back) on the current state, keeping each
    /// prediction's stamp and renumbering ids as `handle_event` assigns them.
    ///
    /// Inputs follow the body: the server applies this client's input only
    /// where its body is (a region its body left forwards it), so a local
    /// input replayed where the local body is not can never be confirmed
    /// and is dropped; its server copy reaches the body's region on its own.
    /// This covers a body that left here EARLIER on the server than predicted
    /// (`drop_local_inputs_before` covers one that arrived later).
    fn replay(&mut self, log: impl IntoIterator<Item = Logged>) {
        let mut replayed = VecDeque::new();
        for mut logged in log {
            if let Some(local) = self.local_client_id {
                if logged.kind.input_client() == Some(local) && !self.data.clients.contains_key(&local) {
                    debug!("[reconcile r={:?}] dropped {}: the local body is not here", self.id, ev_desc(&logged));
                    continue;
                }
            }
            let id = *self.data.next_game_event_id;
            match self.handle_event(logged.kind.clone()) {
                Ok(r) => {
                    if r.id != logged.id {
                        debug!(
                            "[reconcile r={:?}] REPLAY-ID-SKEW: log says id={} but handle_event assigned id={}",
                            self.id, logged.id, r.id
                        );
                    }
                    logged.id = r.id;
                }
                Err(ref e) => {
                    debug!("[reconcile r={:?}] REPLAY-ERR for {}: {:?}", self.id, ev_desc(&logged), e);
                    logged.id = id;
                }
            }
            replayed.push_back(logged);
        }
        self.event_log = replayed;
    }

    /// The authoritative arrival of the local player was just integrated
    /// here in `slot`. A local input still predicted here with an earlier
    /// stamp was predicted after an arrival the client expected sooner: the
    /// server's body was still in the source region in that slot, so the
    /// server applied it there and the arrival bundle carries its effect. It
    /// is never confirmed here — drop it. Stamps are world ticks, so this is
    /// exact: inputs stamped `slot` or later followed the body here.
    fn drop_local_inputs_before(&mut self, slot: Tick) {
        let Some(local) = self.local_client_id else { return };
        let stale = |l: &Logged| l.kind.input_client() == Some(local) && l.stamp < slot;
        if !self.event_log.iter().any(stale) {
            return;
        }
        let kept: Vec<Logged> = self.event_log.iter().filter(|l| !stale(l)).cloned().collect();
        debug!(
            "[reconcile r={:?}] dropping {} local input(s) stamped before the arrival slot {slot}",
            self.id,
            self.event_log.len() - kept.len()
        );
        while self.event_log.pop_back().is_some() {
            self.data.rollback();
        }
        self.replay(kept);
    }

    /// Handle a client event.
    pub fn handle_event(&mut self, event: GameEventKind) -> Result<GameEvent, GameError> {
        let event = GameEvent::new(event, *self.data.next_game_event_id, self.id);
        // The slot it is predicted in (see `Logged::stamp`).
        let stamp = *self.data.tick;
        let destroyed_before = crate::item_piles::items_destroyed_on_thread();
        // Writes left over from outside an event are no region's.
        crate::stud_halo::take_touched_stud_chunks();
        crate::item_piles::take_touched_piles();
        crate::item_piles::take_pile_intents();
        self.data.new_transaction();
        self.data.next_game_event_id.update(|n| *n += 1);
        // Intents this event sends to other regions (`intents.rs`).
        let mut intents_out = Vec::new();
        let _b = &event.kind;
        match event.kind.clone() {
            GameEventKind::Tick => {
                #[cfg(not(target_arch = "wasm32"))]
                let tick_start = Instant::now();
                // The tick opens with the intents due at it (sent at the
                // previous tick, here or across a seam) and their replies.
                self.data.tick_intents_safe(&mut intents_out);
                // Every mover plans this tick with the water it is in now.
                self.data.tick_immersion_safe();
                // Bodies in water float and are dragged (before the step).
                self.data.tick_buoyancy_safe();
                // Mobs set kinematic targets, like the camera controller;
                // the physics controller's step consumes them.
                self.data.tick_slimes_safe(self.id);
                // Adventurers' walks advance before the camera controller
                // steers their bodies from them.
                self.data.tick_walkers_safe(self.id, self.route_map.as_deref());
                // People wander and walk their walks (kinematic targets for
                // the physics step below).
                self.data.tick_people_safe(self.id, self.route_map.as_deref());
                for c in self.controllers.iter_mut() {
                    let data = self.data.as_refs_mut();
                    c.on_tick(data.data);
                }
                // After the step: clips follow how bodies moved, and bone
                // colliders take the new pose for the next step.
                self.data.tick_animators_safe();
                // Stud bodies that came to rest (or left) are settled.
                let tick = *self.data.tick;
                self.data.tick_stud_bodies_safe(tick);
                #[cfg(not(target_arch = "wasm32"))]
                let controllers_done = Instant::now();
                // Block edits: a click this tick breaks or places, from the
                // pose the controllers just produced, before the input step
                // consumes the press.
                let edits: Vec<(ClientId, crate::edit::EditAction)> = self
                    .data
                    .clients
                    .iter()
                    // Not while the inventory is open: its clicks are UI.
                    // Adventure (survival) clients mine instead, below.
                    .filter(|(_, c)| c.mode != crate::PlayMode::Adventure && c.fps_cam_mode && !c.inventory_open)
                    .filter_map(|(id, c)| {
                        if c.input.mouse_pressed(&crate::edit::BREAK_BUTTON) {
                            Some((*id, crate::edit::EditAction::Break))
                        } else if c.input.mouse_pressed(&crate::edit::PLACE_BUTTON) {
                            Some((*id, crate::edit::EditAction::Place))
                        } else {
                            None
                        }
                    })
                    .collect();
                for (client_id, action) in edits {
                    self.data.player_edit_safe(client_id, action, &mut intents_out);
                }
                // Survival places their owner refused give their item back.
                let clients: Vec<ClientId> = self.data.clients.keys().copied().collect();
                for client_id in clients {
                    self.data.refund_refused_places_safe(client_id);
                    // Pile takes granted, deposits refused (`item_piles.rs`).
                    self.data.settle_pile_replies_safe(client_id);
                    // Chest takes granted, puts refused (`containers.rs`).
                    self.data.settle_container_replies_safe(client_id);
                }
                // Survival (Adventure): left held mines with break times,
                // a right press places the selected block, Q / Ctrl+Q throw.
                let survivors: Vec<(ClientId, bool, Option<bool>)> = self
                    .data
                    .clients
                    .iter()
                    .filter(|(_, c)| c.mode == crate::PlayMode::Adventure)
                    .map(|(id, c)| {
                        let hands = c.fps_cam_mode && !c.inventory_open;
                        let place = hands && c.input.mouse_pressed(&crate::edit::PLACE_BUTTON);
                        let throw = (hands && c.input.key_pressed(&Key::KeyQ))
                            .then(|| c.input.key_held(&Key::ControlLeft));
                        (*id, place, throw)
                    })
                    .collect();
                for (client_id, place, throw) in survivors {
                    self.data.tick_mining_safe(client_id, tick, &mut intents_out);
                    if place {
                        self.data.survival_place_safe(client_id, &mut intents_out);
                    }
                    if let Some(whole_stack) = throw {
                        self.data.throw_selected_safe(client_id, whole_stack, tick);
                    }
                }
                // Item piles: Adventure bodies pick up what they reach, then
                // due piles despawn.
                self.data.tick_pickups_safe(tick);
                self.data.expire_piles_safe(tick);
                // Chests left behind (or gone) close.
                self.data.close_far_containers_safe();
                let mut drops: Vec<(ClientId, crate::ItemStack)> = Vec::new();
                let data: &mut GameData = self.data.deref_mut();
                let keys: Vec<ClientId> = data.clients.keys().cloned().collect();
                for client_id in keys {
                    // get_mut logs the prior Client, so the toggle and the
                    // input step are both covered by one delta.
                    let toggled = {
                        let client = data.clients.get_mut(&client_id).unwrap();
                        let adventure = client.mode == crate::PlayMode::Adventure;
                        let e_pressed = client.input.key_pressed(&Key::KeyE);
                        if adventure {
                            // Survival: E is the inventory, not a camera mode.
                            if e_pressed {
                                if let Some(rest) = toggle_inventory(client) {
                                    drops.push((client_id, rest));
                                }
                            }
                            if let Some(slot) = Key::DIGITS.iter().find_map(|k| {
                                client.input.key_pressed(k).then(|| k.hotbar_slot()).flatten()
                            }) {
                                client.selected = slot;
                            }
                        }
                        let toggle = !adventure && e_pressed;
                        if toggle {
                            client.fps_cam_mode = !client.fps_cam_mode;
                        }
                        let _ = client.input.step();
                        toggle.then_some(client.fps_cam_mode)
                    };
                    if let Some(mode) = toggled {
                        data.ecs.send(GameDataUpdate::new(
                            crate::GameDataTransactionKind::Do,
                            crate::GameDataUpdateKind::SetFreeCam(client_id, mode),
                        ));
                    }
                }
                for (client_id, rest) in drops {
                    self.data.drop_at_feet_safe(client_id, rest);
                }
                #[cfg(not(target_arch = "wasm32"))]
                let input_done = Instant::now();
                let (departures, halo) = self.data.scan_boundaries(self.id, self.map_extent);
                // Replies queued for a departing entity go with it.
                for (bundle, target) in &departures {
                    self.data.forward_replies_safe(bundle.id, *target, &mut intents_out);
                }
                self.pending_departures = departures;
                self.pending_halo_entities = halo;
                self.data.tick.update(|t| *t += 1);
                #[cfg(not(target_arch = "wasm32"))]
                self.tick_perf.record(tick_start, controllers_done, input_done);
            }
            GameEventKind::PlayerInput(client_id, player_event) => {
                let data: &mut GameData = self.data.deref_mut();
                if let Some(c) = data.clients.get_mut(&client_id) {
                    // get_mut logged the prior Client: both arms undo.
                    match player_event {
                        crate::InputEvent::FpsCam(on) => c.fps_cam_mode = on,
                        crate::InputEvent::MouseWheel { y, .. } => {
                            // The wheel steps the hotbar while the body is
                            // driven and no inventory window holds the pointer.
                            if c.mode == crate::PlayMode::Adventure && c.fps_cam_mode && !c.inventory_open && y != 0.0 {
                                let n = crate::HOTBAR_SLOTS as i32;
                                // Minecraft: wheel down (negative y) = next slot.
                                let step = if y > 0.0 { -1 } else { 1 };
                                c.selected = (c.selected as i32 + step).rem_euclid(n) as u8;
                            }
                        }
                        _ => {
                            let _ = c.input.update(player_event.clone());
                        }
                    }
                }
            }
            GameEventKind::Quit => return Err(GameError::QuitRequested),
            GameEventKind::CreateClient(client_id) => {
                info!("{:?}", event);
                self.data.clients.insert(
                    client_id,
                    Client { inventory: crate::Inventory::starting_kit(), ..Client::default() },
                );
                self.data.create_player_safe(client_id);
            }
            GameEventKind::CreateClientAt(client_id, column, carried) => {
                info!("{}", ev_desc(&event));
                // A moved player brings its `Client` (inventory, cursor,
                // mode); only a player new to the world gets the kit.
                let client = match carried {
                    // Session and region-local state reset.
                    Some(c) => crate::parked_client(&c),
                    None => Client { inventory: crate::Inventory::starting_kit(), ..Client::default() },
                };
                self.data.clients.insert(client_id, client);
                self.data.create_player_at_safe(client_id, crate::block_column_xz(column));
            }
            GameEventKind::RestorePlayer(client_id, record) => {
                info!("{}", ev_desc(&event));
                self.data.restore_player_safe(client_id, &record);
            }
            GameEventKind::SpawnPerson(spawn) => {
                self.data.apply_person_spawn_safe(spawn, self.id, self.route_map.as_deref());
            }
            GameEventKind::DespawnPerson(person) => {
                self.data.despawn_person_safe(person);
            }
            GameEventKind::WalkOut(person, toward) => {
                self.data.walk_out_safe(person, toward, self.id, self.route_map.as_deref());
            }
            GameEventKind::ClientLeft(client_id) | GameEventKind::ParkPlayer(client_id) => {
                info!("{:?}", event);
                match self.data.player_entites.get(&client_id).copied() {
                    // The bundle is for handoffs; a leaving session drops it.
                    Some(key) => drop(self.data.extract_entity_safe(key)),
                    None => log::warn!(
                        "[region r={:?}] ClientLeft({client_id}): no player body here",
                        self.id
                    ),
                }
            }
            GameEventKind::EntityArrived(bundle) => {
                // A walk in progress walks on (`path_plan.rs`).
                if let Some(e) = self.data.apply_arrival(bundle) {
                    self.data.continue_walk_safe(e, self.id, self.route_map.as_deref());
                }
            }
            GameEventKind::HaloEntity(data) => {
                self.data.apply_halo_entity_safe(data);
            }
            GameEventKind::HaloEntityGone(gone) => {
                self.data.apply_halo_entity_gone_safe(gone);
            }
            GameEventKind::HaloSourceGone(from) => {
                self.data.drop_halo_source_safe(from);
                // Its pile and chest mirrors too: nobody answers takes of
                // them now (an open chest view of one closes next tick).
                self.data.drop_halo_piles_of_safe(from);
                self.data.drop_halo_containers_of_safe(from);
            }
            GameEventKind::HaloUpdate(update) => {
                self.data.apply_halo_update_safe(&update);
            }
            GameEventKind::UiAction(client_id, action) => {
                let data: &mut GameData = self.data.deref_mut();
                if let Some(c) = data.clients.get_mut(&client_id) {
                    match action {
                        // Clicks land only on an open screen.
                        crate::UiAction::Click { .. } if !c.inventory_open => {}
                        // With a chest open: what moves in or out of it is an
                        // intent to its owner, here or across a seam
                        // (`containers.rs`).
                        crate::UiAction::Click { slot, button, shift } if c.open_container.is_some() => {
                            self.data.container_click_safe(client_id, slot, button, shift, &mut intents_out);
                        }
                        crate::UiAction::Click { slot, button, shift } => {
                            let Client { cursor, inventory, .. } = c;
                            crate::click(cursor, inventory, None, slot, button, shift);
                        }
                        crate::UiAction::ToggleInventory => {
                            if let Some(rest) = toggle_inventory(c) {
                                self.data.drop_at_feet_safe(client_id, rest);
                            }
                        }
                        crate::UiAction::CloseInventory => {
                            if let Some(rest) = close_inventory(c) {
                                self.data.drop_at_feet_safe(client_id, rest);
                            }
                        }
                    }
                }
            }
            GameEventKind::Order(client_id, crate::Order::DevBreakBlock(p)) => {
                self.data.dev_break_block_safe(client_id, p, &mut intents_out);
            }
            // Stud edits that may reach a neighbour: its part goes as intents.
            GameEventKind::Order(client_id, crate::Order::CarveStud { lo, hi }) => {
                self.data.carve_stud_order_safe(client_id, lo, hi, &mut intents_out);
            }
            GameEventKind::Order(client_id, crate::Order::FillStud { lo, hi, material }) => {
                self.data.fill_stud_order_safe(client_id, lo, hi, material, &mut intents_out);
            }
            GameEventKind::Order(client_id, crate::Order::PlaceStudAt { chunk, brick }) => {
                self.data.place_stud_order_safe(client_id, chunk, brick, &mut intents_out);
            }
            GameEventKind::Order(client_id, crate::Order::RemoveStudAt { chunk, cell }) => {
                self.data.remove_stud_order_safe(client_id, chunk, cell, &mut intents_out);
            }
            GameEventKind::Intent(msg) => self.data.receive_intent_safe(msg),
            // A walk is planned against this region's identity.
            GameEventKind::Order(client_id, crate::Order::MoveTo(goal)) => {
                self.data.move_to_safe(client_id, goal, self.id, self.route_map.as_deref());
            }
            GameEventKind::Order(client_id, order) => {
                self.data.apply_order_safe(client_id, order);
            }
            GameEventKind::Prefab(client_id, crate::PrefabCommand::Register(prefab)) => {
                self.data.register_prefab_safe(client_id, *prefab);
            }
            GameEventKind::Prefab(client_id, crate::PrefabCommand::Deregister(key)) => {
                self.data.deregister_prefab_safe(client_id, key);
            }
            // The runtime stamp: not built yet.
            GameEventKind::Prefab(_, crate::PrefabCommand::Stamp { .. }) => {}
        }
        self.items_destroyed += crate::item_piles::items_destroyed_on_thread() - destroyed_before;
        self.note_halo_writes();
        // Pile intents sent from inside writes (`item_piles.rs`).
        intents_out.extend(crate::item_piles::take_pile_intents());
        if !self.replaying {
            self.pending_intents.extend(intents_out);
        }
        self.event_log.push_back(Logged { event: event.clone(), stamp });
        Ok(event)
    }

    /// Items the pile caps destroyed in this region's events since the
    /// previous call; resets it (metrics only).
    pub fn take_items_destroyed(&mut self) -> u64 {
        std::mem::take(&mut self.items_destroyed)
    }

    pub fn build_region_server_packet(&self, region_id: &RegionId) -> ServerPacket {
        ServerPacket::Region(*region_id, self.data.clone())
    }

    pub fn current_tick(&self) -> Tick {
        *self.data.tick
    }

    /// Start this region's tick count at the world tick `tick` (a region woken
    /// late joins the world's numbering). Unlogged state surgery: only before
    /// the first tick, and a no-op when `tick` is not ahead of the region.
    /// A runner's start ([`crate::RegionInput::StartAt`]): the region joins
    /// the world's tick numbering ([`Self::jump_clock_to`]) and its
    /// `EntityId` serials move to at least `tick << ENTITY_SERIAL_TICK_SHIFT`.
    /// A region generated afresh (procedural respawn after a thread death, a
    /// forged region before its first save, a set-aside file) starts at
    /// serial 0, so without that floor it would hand out the ids of its
    /// earlier incarnation's entities, which may still live elsewhere. Every
    /// incarnation starts at a later world tick, so their serials are
    /// disjoint while no region allocates `1 << ENTITY_SERIAL_TICK_SHIFT` ids
    /// per tick. Server-only: the client gets the floor in the snapshot.
    pub fn start_at(&mut self, tick: Tick) {
        self.jump_clock_to(tick);
        let floor = tick.saturating_mul(1 << ENTITY_SERIAL_TICK_SHIFT);
        if *self.data.next_entity_serial < floor {
            // Its own forgotten transaction, like `jump_clock_to`.
            self.data.new_transaction();
            self.data.next_entity_serial.update(|n| *n = floor);
            self.data.forget();
        }
    }

    pub fn jump_clock_to(&mut self, tick: Tick) {
        let now = self.current_tick();
        if tick <= now {
            return;
        }
        debug_assert!(self.event_log.is_empty(), "jump_clock_to after the region ticked");
        // Own forgotten transaction (R8): `update` always logs an undo entry.
        self.data.new_transaction();
        self.data.tick.update(|t| *t = tick);
        self.data.on_clock_jump(tick - now);
        self.data.forget();
    }

    /// Drain the cumulative Tick wall-time breakdown (native only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn take_tick_perf(&mut self) -> RegionTickPerf {
        std::mem::take(&mut self.tick_perf)
    }

    /// Ids of locally-predicted events awaiting server confirmation.
    /// Exposed for tests.
    pub fn pending_event_ids(&self) -> Vec<usize> {
        self.event_log.iter().map(|e| e.id).collect()
    }

    /// The locally-predicted events awaiting server confirmation, in order.
    /// Exposed for tests.
    pub fn pending_events(&self) -> impl Iterator<Item = &GameEvent> {
        self.event_log.iter().map(|l| &l.event)
    }

    pub fn data(&self) -> &Rollback {
        &self.data
    }

    pub fn terrain_snapshot(&self) -> crate::TerrainSnapshot {
        self.data.terrain_snapshot()
    }

    /// A region with no terrain (tests; a region about to be seeded).
    pub fn empty(id: RegionId) -> Self {
        Region::new(Rollback::new(None), None, id, None)
    }

    /// Genesis for the stud world: seeds the stud chunks raw
    /// (`Rollback::seed_stud_genesis`) and sets the region's player body to
    /// `BodyScale::STUD`. Fresh regions only — base state, unlogged.
    pub fn seed_studs(&mut self, chunks: Vec<(crate::StudChunkCoords, crate::StudChunk)>) {
        self.data.seed_stud_genesis(chunks);
        self.data.set_body_scale_raw(crate::movement::BodyScale::STUD);
    }

    /// Genesis for a stud world from ground recipes
    /// (`Rollback::seed_stud_ground_genesis`), plus `features`: extra bricks
    /// (trees, structures) placed into their chunks, skipped where they would
    /// overlap. Sets the player body to `BodyScale::STUD`. Fresh regions only.
    pub fn seed_stud_world(&mut self, seed: crate::StudRegionSeed) {
        // Features go into the pristine chunk before it is seeded: one collider build.
        let extra = seed.feature_chunks();
        let ground = seed.ground;
        self.data.seed_stud_ground_genesis(ground, extra);
        self.data.set_body_scale_raw(crate::movement::BodyScale::STUD);
    }

    /// Server-side commit of the event just handled: forget its undo
    /// transaction AND drop it from `event_log`. Server regions never
    /// reconcile, so the log would otherwise grow by every event forever.
    pub fn commit_server_event(&mut self) {
        self.data.forget();
        self.event_log.pop_back();
    }

    pub fn forget_last_event(&mut self) {
        self.data.forget();
    }

    /// Boundary-scan results from the LAST executed tick. REPLACED (not
    /// appended) each tick; call immediately after driving a tick and at no
    /// other time — see the read-after-progress invariant on the fields.
    pub fn take_transfers(
        &mut self,
    ) -> (Vec<(EntityBundle, RegionCoords)>, Vec<(HaloEntityMsg, RegionCoords)>) {
        (
            std::mem::take(&mut self.pending_departures),
            std::mem::take(&mut self.pending_halo_entities),
        )
    }

    /// The intents and replies sent to other regions since the previous call
    /// (each carries its due tick). Taken right after a tick and its slot's
    /// events, like [`Region::take_transfers`].
    pub fn take_intents(&mut self) -> Vec<crate::IntentMsg> {
        std::mem::take(&mut self.pending_intents)
    }

    /// Server, at `Shutdown`: refuse every intent still queued here
    /// (`OwnerNotRunning`), as its own forgotten transaction; the replies to
    /// other regions join the outbox.
    pub fn refuse_queued_intents(&mut self) {
        let mut out = Vec::new();
        self.data.new_transaction();
        self.data.refuse_queued_intents_safe(&mut out);
        self.data.forget();
        self.pending_intents.extend(out);
    }

    /// Test hook: run an undo-safe mutation in its own forgotten
    /// transaction (teleports, state surgery in integration tests). Pile
    /// intents it sends join the outbox.
    pub fn with_data(&mut self, f: impl FnOnce(&mut Rollback)) {
        crate::stud_halo::take_touched_stud_chunks();
        crate::item_piles::take_touched_piles();
        crate::item_piles::take_pile_intents();
        self.data.new_transaction();
        f(&mut self.data);
        self.data.forget();
        self.note_halo_writes();
        self.pending_intents.extend(crate::item_piles::take_pile_intents());
    }

    /// The chunk and pile writes of the event just handled that a neighbour
    /// mirrors join `halo_stud_dirty` / `halo_pile_dirty`.
    fn note_halo_writes(&mut self) {
        for c in crate::stud_halo::take_touched_stud_chunks() {
            if !crate::halo::halo_targets(c.x as i32, c.z as i32).is_empty() {
                self.halo_stud_dirty.insert(c);
            }
        }
        for p in crate::item_piles::take_touched_piles() {
            let l = crate::item_piles::local_cell(self.id, p);
            if !crate::item_piles::pile_halo_targets(l[0], l[2]).is_empty() {
                self.halo_pile_dirty.insert(p);
            }
        }
    }

    /// Halo updates for the neighbours, from the band chunks and piles
    /// written since the previous call (their content now), with each
    /// target. Taken right after a tick and its slot's events, like
    /// [`Region::take_transfers`].
    pub fn take_halo_updates(&mut self) -> Vec<(crate::HaloUpdate, RegionCoords)> {
        if self.halo_pile_dirty.is_empty() && self.halo_stud_dirty.is_empty() {
            return Vec::new();
        }
        let piles = std::mem::take(&mut self.halo_pile_dirty);
        let studs = std::mem::take(&mut self.halo_stud_dirty);
        self.data.halo_updates_for(&piles, &studs)
    }

    /// Test hook: roll back the most recent (unforgotten) event.
    pub fn rollback_last_event(&mut self) {
        self.data.rollback();
        self.event_log.pop_back();
    }
}

#[cfg(test)]
mod ui_action_tests {
    use super::*;
    use crate::{BlockId, Inventory, ItemId, ItemStack, UiAction};

    #[test]
    fn click_picks_up_and_rolls_back() {
        let mut region = Region::empty(RegionCoords::new(0, 0));
        region.handle_event(GameEventKind::CreateClient(0)).unwrap();
        region.forget_last_event();
        region.with_data(|d| {
            let c = d.clients.get_mut(&0).unwrap();
            c.inventory.slots[0] = Some(ItemStack { item: ItemId::block(BlockId(1)), count: 42 });
            // Clicks need an open inventory.
            c.inventory_open = true;
        });
        let before = crate::state_hash(region.data());

        let click = UiAction::Click {
            slot: crate::SlotRef::Player(0),
            button: crate::ClickButton::Left,
            shift: false,
        };
        region.handle_event(GameEventKind::UiAction(0, click)).unwrap();
        let c = region.data().clients.get(&0).unwrap();
        let inv: &Inventory = &c.inventory;
        assert_eq!(inv.slots[0], None);
        assert_eq!(c.cursor, Some(ItemStack { item: ItemId::block(BlockId(1)), count: 42 }));

        region.rollback_last_event();
        assert_eq!(before, crate::state_hash(region.data()), "UiAction must invert bit-exact");
    }
}
