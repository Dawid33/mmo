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
use crate::{Chunk, Client, ClientId, GameData, Rollback};

use crate::{
    camera::CameraController, physics::PhysicsController, ChunkCoords, Column, ColumnCoords,
    Controller, EntityBundle, GameDataUpdate, GameError, GameEvent, GameEventKind, GhostData,
    RegionCoords, RegionId, ServerPacket,
};

/// Cumulative wall-time breakdown of `handle_event(Tick)` executions since
/// the last `take_tick_perf()`. Native-only: `Instant::now()` panics on
/// wasm32-unknown-unknown, so the accumulator and its call sites compile away
/// there. Reconcile replays run through `handle_event` too, so `ticks` counts
/// tick *executions* (main-loop + replayed), not sim-time ticks — exactly the
/// cost the client's keep-up budget cares about. The server accumulates but
/// never drains; a few `Instant::now()` reads per tick are noise next to the
/// ~ms tick itself.
/// Opens or closes the inventory. Closing in Adventure returns the cursor
/// stack to the inventory; what does not fit is returned, for the caller to
/// drop as a pile at the player's feet ([`Rollback::drop_at_feet_safe`]).
fn toggle_inventory(c: &mut Client) -> Option<crate::ItemStack> {
    c.inventory_open = !c.inventory_open;
    if !c.inventory_open && c.mode == crate::PlayMode::Adventure {
        if let Some(mut stack) = c.cursor.take() {
            stack.count = c.inventory.add(stack);
            return (stack.count > 0).then_some(stack);
        }
    }
    None
}

impl Rollback {
    /// Drops `stack` as a pile at the cell of `client`'s feet, pickable after
    /// [`crate::PICKUP_DELAY_TICKS`]. With no body to drop it from, it goes
    /// back on the cursor instead, so nothing is lost.
    fn drop_at_feet_safe(&mut self, client: ClientId, stack: crate::ItemStack) {
        let feet = self.data.player_entites.get(&client).and_then(|&e| self.body_feet(e));
        match feet {
            Some(feet) => {
                let tick = *self.data.tick as u64;
                // A loss to the caps is counted where it happens.
                let _destroyed =
                    self.deposit_items_safe(crate::feet_tile(feet), stack, tick, crate::PICKUP_DELAY_TICKS);
            }
            None => {
                if let Some(c) = self.data.clients.get_mut(&client) {
                    c.cursor = Some(stack);
                }
            }
        }
    }
}

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
    /// Boundary scan + ghost expiry + tick bump.
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
            GameEventKind::PlayerInput(..) | GameEventKind::UiAction(..) | GameEventKind::Order(..) => 1,
            _ => 2,
        }
    }
}

/// A region represents an portion of the world that processes game events at
/// its own tick rate separately from other regions.
#[allow(unused)]
pub struct Region {
    event_log: VecDeque<GameEvent>,
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
    pending_ghosts: Vec<(GhostData, RegionCoords)>,
    /// The finite world's regions, when it has an edge: nothing crosses
    /// out of it (walks stop at the edge tile, bodies are held at the outer
    /// seam). Not sim state — set by whoever runs the region (the manager
    /// on the server, the client from its `WorldMap`), the same on both.
    map_extent: Option<crate::MapExtent>,
    /// Client-only: the server's state hash after each of these event ids
    /// (`id -> (tick, hash)`), checked when reconcile confirms that event.
    /// Ids at or below the confirmed head are dropped as missed.
    expected_hashes: BTreeMap<usize, (usize, u32)>,
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
        GameEventKind::CreateClient(c) => format!("CreateClient(c={c:?})"),
        GameEventKind::CreateClientAt(c, col) => format!("CreateClientAt(c={c:?}, col={col:?})"),
        GameEventKind::ClientLeft(c) => format!("ClientLeft(c={c:?})"),
        GameEventKind::ParkPlayer(c) => format!("ParkPlayer(c={c:?})"),
        GameEventKind::RestorePlayer(c, r) => format!("RestorePlayer(c={c:?}, region={:?})", r.region),
        GameEventKind::Quit => "Quit".to_string(),
        GameEventKind::EntityArrived(b) => {
            format!("EntityArrived(c={:?})", b.client.as_ref().map(|(c, _)| *c))
        }
        GameEventKind::GhostUpdate(_) => "GhostUpdate".to_string(),
        GameEventKind::SpawnPerson(s) => format!("SpawnPerson({s:?})"),
        GameEventKind::DespawnPerson(p) => format!("DespawnPerson({p:?})"),
        GameEventKind::WalkOut(p, to) => format!("WalkOut({p:?}, {to:?})"),
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
            pending_ghosts: Vec::new(),
            map_extent: None,
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
    pub fn expect_state_hash(&mut self, event_id: usize, tick: usize, hash: u32) {
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
            let kinds: Vec<GameEventKind> = self.event_log.iter().map(|e| e.kind.clone()).collect();
            while self.event_log.pop_back().is_some() {
                self.data.rollback();
            }
            let hash = crate::state_hash(&self.data);
            for kind in kinds {
                let _ = self.handle_event(kind);
            }
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
    /// [`crate::MapExtent`]. Takes effect from the next event.
    pub fn set_map_extent(&mut self, extent: Option<crate::MapExtent>) {
        self.map_extent = extent;
    }

    pub fn map_extent(&self) -> Option<crate::MapExtent> {
        self.map_extent
    }

    /// Check if event from network matches client event history. rollback the
    /// game state as neccessary and re-simulate to current time.
    pub fn reconcile(&mut self, server_event: GameEvent) -> Result<ReconcileStats, GameError> {
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
                .map(ev_desc)
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

        // Set when this reconcile pass integrates the AUTHORITATIVE arrival of
        // the local player into this region (the server-confirmed handoff).
        // Triggers eviction of the orphan lag-window inputs below.
        let mut applied_local_arrival = false;

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
                        if let GameEventKind::EntityArrived(bundle) = &server_event.0.kind {
                            let is_local = self.local_client_id.is_some()
                                && bundle.client.as_ref().map(|(c, _)| *c)
                                    == self.local_client_id;
                            if is_local {
                                applied_local_arrival = true;
                            }
                        }
                        if event != server_event.0 {
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
                                let len = temp_log.len();
                                let mut i = 0;
                                let mut removed_id = None;
                                while i < temp_log.len() {
                                    let e = &mut temp_log[i];
                                    let local_origin = match e.kind.origin_client() {
                                        Some(o) => Some(o) == self.local_client_id,
                                        None => true,
                                    };
                                    if local_origin && e.kind.matches_prediction(&server_event.0.kind) {
                                        removed_id = Some(e.id);
                                        temp_log.remove(i);
                                        break;
                                    }
                                    e.id += 1;
                                    i += 1;
                                }
                                if len == temp_log.len() {
                                    debug!(
                                        "[reconcile r={:?}] NO MATCHING PREDICTION for server {} — client behind; kept all {} predictions, NOTHING removed/forgotten",
                                        self.id,
                                        ev_desc(&server_event.0),
                                        len
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

                            for event in &mut temp_log {
                                let replayed = self.handle_event(event.clone().kind);
                                match replayed {
                                    Ok(r) if r.id != event.id => debug!(
                                        "[reconcile r={:?}] REPLAY-ID-SKEW: temp_log says id={} but handle_event assigned id={}",
                                        self.id, event.id, r.id
                                    ),
                                    Err(ref e) => debug!(
                                        "[reconcile r={:?}] REPLAY-ERR for {}: {:?}",
                                        self.id,
                                        ev_desc(event),
                                        e
                                    ),
                                    _ => {}
                                }
                            }
                            self.event_log = temp_log;
                        } else {
                            debug!(
                                "[reconcile r={:?}] MATCH+FORGET {}",
                                self.id,
                                ev_desc(&server_event.0)
                            );
                            self.data.forget();
                            self.confirmed(server_event.0.id, true, &mut stats);
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

        if applied_local_arrival {
            debug!(
                "[reconcile r={:?}] applied local player arrival; evicting orphan lag-window inputs",
                self.id
            );
            self.evict_orphan_local_inputs();
        }

        // Id-accounting invariant: predictions are appended with ids from
        // next_game_event_id, so the log tail must sit exactly one below it.
        // A violation means the replay/evict machinery desynced the counter.
        if let Some(back) = self.event_log.back() {
            let next = *self.data.next_game_event_id;
            if back.id + 1 != next {
                log::warn!(
                    "[reconcile r={:?}] ID-DESYNC: event_log back id={} but next_game_event_id={} (expected back+1==next); front={}",
                    self.id,
                    back.id,
                    next,
                    self.event_log.front().map(ev_desc).unwrap_or_default(),
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

    /// Client-only: the authoritative handoff of the LOCAL player into this
    /// region has just been confirmed (reconcile integrated the server's
    /// `EntityArrived` for our client). The local-player `PlayerInput`s still
    /// pending in this region's event log were predicted during the flip RTT,
    /// while the server still routed our inputs to the SOURCE region by
    /// `homes`. Those inputs resolved in the source and will NEVER echo back
    /// for this region, so no server event can ever match/remove them: they
    /// are orphans that would otherwise re-apply on every future rollback and
    /// accumulate on every crossing. Evict them; the arrival bundle already
    /// carries the post-window pose (the source applied them before extraction).
    ///
    /// Legitimate inputs predicted AFTER this confirmation carry higher ids,
    /// are not yet in the log at this point, and reconcile normally.
    ///
    /// Removal reuses the exact-inverse rollback machinery: roll every pending
    /// prediction back (each is one un-forgotten transaction, which also
    /// rewinds `next_game_event_id` to the first pending slot), then replay the
    /// non-orphan tail. The replayed events re-append with fresh, gap-free ids
    /// that mirror the server's arrival-onward sequence — closing the id holes
    /// the orphans left so later authoritative events still match. State stays
    /// hash-exact because every mutation goes through the same logged deltas.
    fn evict_orphan_local_inputs(&mut self) {
        let Some(local) = self.local_client_id else { return };
        let is_orphan = |e: &GameEvent| {
            matches!(&e.kind, GameEventKind::PlayerInput(cid, _) if *cid == local)
        };
        if !self.event_log.iter().any(is_orphan) {
            return;
        }

        let kept_kinds: Vec<GameEventKind> = self
            .event_log
            .iter()
            .filter(|e| !is_orphan(e))
            .map(|e| e.kind.clone())
            .collect();
        while self.event_log.pop_back().is_some() {
            self.data.rollback();
        }
        for kind in kept_kinds {
            let _ = self.handle_event(kind);
        }
    }

    /// Handle a client event.
    pub fn handle_event(&mut self, event: GameEventKind) -> Result<GameEvent, GameError> {
        let event = GameEvent::new(event, *self.data.next_game_event_id, self.id);
        let destroyed_before = crate::item_piles::items_destroyed_on_thread();
        self.data.new_transaction();
        self.data.next_game_event_id.update(|n| *n += 1);
        let _b = &event.kind;
        match event.kind.clone() {
            GameEventKind::Tick => {
                #[cfg(not(target_arch = "wasm32"))]
                let tick_start = Instant::now();
                // Mobs set kinematic targets, like the camera controller;
                // the physics controller's step consumes them.
                self.data.tick_slimes_safe(self.id);
                // Adventurers' walks advance before the camera controller
                // steers their bodies from them.
                self.data.tick_walkers_safe(self.id, self.map_extent);
                // People wander and walk their walks (kinematic targets for
                // the physics step below).
                self.data.tick_people_safe(self.id, self.map_extent);
                for c in self.controllers.iter_mut() {
                    let data = self.data.as_refs_mut();
                    c.on_tick(data.data);
                }
                // After the step: clips follow how bodies moved, and bone
                // colliders take the new pose for the next step.
                self.data.tick_animators_safe();
                // Voxel bodies that came to rest (or left) are settled.
                let tick = *self.data.tick as u64;
                self.data.tick_voxel_bodies_safe(tick);
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
                    self.data.player_edit_safe(client_id, action);
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
                    self.data.tick_mining_safe(client_id, tick);
                    if place {
                        self.data.survival_place_safe(client_id);
                    }
                    if let Some(whole_stack) = throw {
                        self.data.throw_selected_safe(client_id, whole_stack, tick);
                    }
                }
                // Item piles: Adventure bodies pick up what they reach, then
                // due piles despawn.
                self.data.tick_pickups_safe(tick);
                self.data.expire_piles_safe(tick);
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
                let (departures, ghosts) = self.data.scan_boundaries(self.id, self.map_extent);
                self.pending_departures = departures;
                self.pending_ghosts = ghosts;
                self.data.expire_ghosts();
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
                                let step = if y > 0.0 { 1 } else { -1 };
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
            GameEventKind::CreateClientAt(client_id, column) => {
                info!("{:?}", event);
                self.data.clients.insert(
                    client_id,
                    Client { inventory: crate::Inventory::starting_kit(), ..Client::default() },
                );
                self.data.create_player_at_safe(client_id, crate::block_column_xz(column));
            }
            GameEventKind::RestorePlayer(client_id, record) => {
                info!("{}", ev_desc(&event));
                self.data.restore_player_safe(client_id, &record);
            }
            GameEventKind::SpawnPerson(spawn) => {
                self.data.apply_person_spawn_safe(spawn, self.id, self.map_extent);
            }
            GameEventKind::DespawnPerson(person) => {
                self.data.despawn_person_safe(person);
            }
            GameEventKind::WalkOut(person, toward) => {
                self.data.walk_out_safe(person, toward, self.id, self.map_extent);
            }
            GameEventKind::ClientLeft(client_id) | GameEventKind::ParkPlayer(client_id) => {
                info!("{:?}", event);
                match self.data.player_entites.get(&client_id).copied() {
                    // The bundle is for handoffs; a leaving session drops it.
                    Some(key) => drop(self.data.extract_entity_safe(key, self.id)),
                    None => log::warn!(
                        "[region r={:?}] ClientLeft({client_id}): no player body here",
                        self.id
                    ),
                }
            }
            GameEventKind::EntityArrived(bundle) => {
                // A walk in progress carries over, re-planned in this region.
                if let Some(e) = self.data.apply_arrival(bundle) {
                    self.data.resume_walker_safe(e, self.id, self.map_extent);
                }
            }
            GameEventKind::GhostUpdate(data) => {
                self.data.apply_ghost(data);
            }
            GameEventKind::UiAction(client_id, action) => {
                let data: &mut GameData = self.data.deref_mut();
                if let Some(c) = data.clients.get_mut(&client_id) {
                    match action {
                        crate::UiAction::Click { slot, button, shift } => {
                            let Client { cursor, inventory, .. } = c;
                            crate::click(cursor, inventory, None, slot, button, shift);
                        }
                        crate::UiAction::ToggleInventory => {
                            if let Some(rest) = toggle_inventory(c) {
                                self.data.drop_at_feet_safe(client_id, rest);
                            }
                        }
                    }
                }
            }
            // A walk is planned against this region's identity.
            GameEventKind::Order(client_id, crate::Order::MoveTo(goal)) => {
                self.data.move_to_safe(client_id, goal, self.id, self.map_extent);
            }
            GameEventKind::Order(client_id, order) => {
                self.data.apply_order_safe(client_id, order);
            }
        }
        self.items_destroyed += crate::item_piles::items_destroyed_on_thread() - destroyed_before;
        self.event_log.push_back(event.clone());
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

    pub fn current_tick(&self) -> usize {
        *self.data.tick
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

    pub fn data(&self) -> &Rollback {
        &self.data
    }

    pub fn terrain_snapshot(&self) -> crate::TerrainSnapshot {
        self.data.terrain_snapshot()
    }

    /// See [`Rollback::terrain_fingerprint`].
    pub fn terrain_fingerprint(&self) -> u32 {
        self.data.terrain_fingerprint()
    }

    pub fn create_basic(&mut self, coords: ChunkCoords) {
        self.data.create_mesh(coords, Chunk::flat_floor(8));
    }

    /// Build a server-side region from generated chunk contents. Mirrors
    /// what `World::basic` does for the origin region, for any region.
    pub fn from_chunks(id: RegionId, chunks: Vec<(ChunkCoords, Chunk)>) -> Self {
        let mut region = Region::new(Rollback::new(None), None, id, None);
        // Genesis bulk build: unlogged, O(N) inserts with full-chunk cuboid
        // collapse. See Rollback::seed_terrain_genesis.
        region.data.seed_terrain_genesis(chunks);
        region
    }

    /// Seed a region from vertical columns: flatten each column's materialized
    /// (Dense) sub-chunks into the existing per-chunk mesh/collider path.
    pub fn from_columns(id: RegionId, columns: Vec<(ColumnCoords, Column)>) -> Self {
        let ground: Vec<(ColumnCoords, crate::ColumnGround)> =
            columns.iter().filter_map(|(c, col)| col.ground().map(|g| (*c, g.clone()))).collect();
        let chunks: Vec<(ChunkCoords, Chunk)> = columns
            .into_iter()
            .flat_map(|(col, column)| {
                column
                    .materialized_sub_chunks()
                    .into_iter()
                    .map(move |(sub_y, chunk)| (ChunkCoords::new(col.x, sub_y, col.z), chunk))
            })
            .collect();
        let mut region = Region::from_chunks(id, chunks);
        region.data.seed_ground_genesis(ground);
        region
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
    ) -> (Vec<(EntityBundle, RegionCoords)>, Vec<(GhostData, RegionCoords)>) {
        (
            std::mem::take(&mut self.pending_departures),
            std::mem::take(&mut self.pending_ghosts),
        )
    }

    /// Test hook: run an undo-safe mutation in its own forgotten
    /// transaction (teleports, state surgery in integration tests).
    pub fn with_data(&mut self, f: impl FnOnce(&mut Rollback)) {
        self.data.new_transaction();
        f(&mut self.data);
        self.data.forget();
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
        let mut region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
        region.handle_event(GameEventKind::CreateClient(0)).unwrap();
        region.forget_last_event();
        region.with_data(|d| {
            d.clients.get_mut(&0).unwrap().inventory.slots[0] =
                Some(ItemStack { item: ItemId::block(BlockId(1)), count: 42 });
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
