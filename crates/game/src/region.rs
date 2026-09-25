use std::{
    cmp::Reverse,
    collections::{BinaryHeap, VecDeque},
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
}

impl ReconcileStats {
    /// Names of the [`Self::by_kinds`] slots.
    pub const KIND_NAMES: [&'static str; 3] = ["tick", "input", "transfer"];

    /// Slot of an event kind in [`Self::by_kinds`]: ticks, client-originated
    /// events, and everything relayed between regions.
    pub fn kind_slot(kind: &GameEventKind) -> usize {
        match kind {
            GameEventKind::Tick => 0,
            GameEventKind::PlayerInput(..) | GameEventKind::UiAction(..) => 1,
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
    /// Entities frozen into dormant cells by the LAST tick; replaced every
    /// tick, like `pending_departures`.
    pending_frozen: Vec<(crate::CellCoords, EntityBundle)>,
    /// Server only: cells dematerialized by `CellChanges`, for the store.
    pending_dormant: Vec<(crate::CellCoords, crate::DormantCell)>,
    /// Wall-time attribution for Tick executions, drained by the client's
    /// budget reporter (native only — see `RegionTickPerf`).
    #[cfg(not(target_arch = "wasm32"))]
    tick_perf: RegionTickPerf,
}

/// Compact one-line description of a game event for reconcile tracing.
fn ev_desc(e: &GameEvent) -> String {
    let kind = match &e.kind {
        GameEventKind::Tick => "Tick".to_string(),
        GameEventKind::PlayerInput(c, ie) => format!("PlayerInput(c={c:?}, {ie:?})"),
        GameEventKind::UiAction(c, a) => format!("UiAction(c={c:?}, {a:?})"),
        GameEventKind::CreateClient(c) => format!("CreateClient(c={c:?})"),
        GameEventKind::Quit => "Quit".to_string(),
        GameEventKind::EntityArrived(b) => {
            format!("EntityArrived(c={:?})", b.client.as_ref().map(|(c, _)| *c))
        }
        GameEventKind::GhostUpdate(_) => "GhostUpdate".to_string(),
        GameEventKind::CellChanges(c) => {
            format!("CellChanges(-{} +{})", c.deactivate.len(), c.activate.len())
        }
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
            pending_dormant: Vec::new(),
            pending_frozen: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            tick_perf: RegionTickPerf::default(),
        }
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
        let mut event = GameEvent::new(event, *self.data.next_game_event_id, self.id);
        self.data.new_transaction();
        self.data.next_game_event_id.update(|n| *n += 1);
        let _b = &event.kind;
        match event.kind.clone() {
            GameEventKind::Tick => {
                #[cfg(not(target_arch = "wasm32"))]
                let tick_start = Instant::now();
                for c in self.controllers.iter_mut() {
                    let data = self.data.as_refs_mut();
                    c.on_tick(data.data);
                }
                #[cfg(not(target_arch = "wasm32"))]
                let controllers_done = Instant::now();
                let data: &mut GameData = self.data.deref_mut();
                let keys: Vec<ClientId> = data.clients.keys().cloned().collect();
                for client_id in keys {
                    // get_mut logs the prior Client, so the toggle and the
                    // input step are both covered by one delta.
                    let toggled = {
                        let client = data.clients.get_mut(&client_id).unwrap();
                        let toggle = client.input.key_pressed(&Key::KeyE);
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
                #[cfg(not(target_arch = "wasm32"))]
                let input_done = Instant::now();
                let (departures, ghosts) = self.data.scan_boundaries(self.id);
                self.pending_departures = departures;
                self.pending_ghosts = ghosts;
                self.data.expire_ghosts();
                self.pending_frozen = self.data.freeze_strays_safe(self.id);
                self.data.tick.update(|t| *t += 1);
                #[cfg(not(target_arch = "wasm32"))]
                self.tick_perf.record(tick_start, controllers_done, input_done);
            }
            GameEventKind::PlayerInput(client_id, player_event) => {
                let data: &mut GameData = self.data.deref_mut();
                if let Some(c) = data.clients.get_mut(&client_id) {
                    let _ = c.input.update(player_event.clone());
                }
            }
            GameEventKind::Quit => return Err(GameError::QuitRequested),
            GameEventKind::CreateClient(client_id) => {
                info!("{:?}", event);
                self.data.clients.insert(
                    client_id,
                    Client { inventory: crate::Inventory::seeded(), ..Client::default() },
                );
                self.data.create_player_safe(client_id);
            }
            GameEventKind::CellChanges(mut changes) => 'cells: {
                if !self.data.cells.enabled() {
                    // Legacy regions have no cell grid (cell_chunks == 0).
                    log::warn!("[cells r={:?}] CellChanges on a legacy region ignored", self.id);
                    break 'cells;
                }
                // Applied at the confirmed head and forgotten at once by
                // every replica, so the raw terrain ops below never meet a
                // rollback.
                let server = self.local_client_id.is_none();
                for cell in changes.deactivate.iter().copied() {
                    if !self.data.cells.active.contains(&cell) {
                        continue;
                    }
                    if self.data.cell_has_player(cell) {
                        log::warn!(
                            "[cells r={:?}] refusing to deactivate {cell:?}: a player is in it",
                            self.id
                        );
                        continue;
                    }
                    let dormant = self.data.dematerialize_cell(cell, self.id);
                    if server {
                        self.pending_dormant.push((cell, dormant));
                    }
                }
                for payload in changes.activate.iter_mut() {
                    if self.data.cells.active.contains(&payload.cell) {
                        continue;
                    }
                    self.data.materialize_cell(payload);
                    let hash = self.data.cell_hash(payload.cell);
                    if server {
                        payload.materialized_hash = hash;
                    } else if hash != payload.materialized_hash {
                        return Err(GameError::CellHashMismatch(payload.cell));
                    }
                }
                event.kind = GameEventKind::CellChanges(changes);
            }
            GameEventKind::EntityArrived(bundle) => {
                self.data.apply_arrival(bundle);
            }
            GameEventKind::GhostUpdate(data) => {
                self.data.apply_ghost(data);
            }
            GameEventKind::UiAction(client_id, action) => {
                let data: &mut GameData = self.data.deref_mut();
                if let Some(c) = data.clients.get_mut(&client_id) {
                    match action {
                        crate::UiAction::MoveItem { from, to } => {
                            if from < c.inventory.slots.len() && to < c.inventory.slots.len() {
                                c.inventory.slots.swap(from, to);
                            }
                        }
                        crate::UiAction::ToggleInventory => {
                            c.inventory_open = !c.inventory_open;
                        }
                    }
                }
            }
        }
        self.event_log.push_back(event.clone());
        Ok(event)
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
        let chunks: Vec<(ChunkCoords, Chunk)> = columns
            .into_iter()
            .flat_map(|(col, column)| {
                column
                    .materialized_sub_chunks()
                    .into_iter()
                    .map(move |(sub_y, chunk)| (ChunkCoords::new(col.x, sub_y, col.z), chunk))
            })
            .collect();
        Region::from_chunks(id, chunks)
    }

    /// An empty cell-mode region: no terrain, no active cells. Terrain
    /// arrives only through `CellChanges`.
    pub fn new_cells(
        id: RegionId,
        cell_chunks: usize,
        game_update_send: Option<Sender<GameDataUpdate>>,
        local_client_id: Option<ClientId>,
    ) -> Self {
        crate::cells_per_side(cell_chunks); // validates the size
        let mut data = Rollback::new(None);
        data.cells.raw_mut().cell_chunks = cell_chunks;
        Region::new(data, game_update_send, id, local_client_id)
    }

    /// Entities frozen into dormant cells by the LAST tick (server: to the
    /// store; client: discard). Read right after driving a tick.
    pub fn take_frozen(&mut self) -> Vec<(crate::CellCoords, EntityBundle)> {
        std::mem::take(&mut self.pending_frozen)
    }

    /// Server: cells dematerialized since the last call, for the store.
    pub fn take_dormant(&mut self) -> Vec<(crate::CellCoords, crate::DormantCell)> {
        std::mem::take(&mut self.pending_dormant)
    }

    /// Server-side commit of the event just handled: forget its undo
    /// transaction AND drop it from `event_log`. Server regions never
    /// reconcile, so the log would otherwise grow by every event forever
    /// (and `CellChanges` carry whole cells).
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
    use crate::{Inventory, ItemKind, ItemStack, UiAction};

    #[test]
    fn move_item_swaps_slots_and_rolls_back() {
        let mut region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
        region.handle_event(GameEventKind::CreateClient(0)).unwrap();
        region.forget_last_event();
        let before = crate::state_hash(region.data());

        region
            .handle_event(GameEventKind::UiAction(0, UiAction::MoveItem { from: 0, to: 1 }))
            .unwrap();
        // Slot 0 (seeded Dirt) moved to slot 1.
        let inv: &Inventory = &region.data().clients.get(&0).unwrap().inventory;
        assert_eq!(inv.slots[0], None);
        assert_eq!(inv.slots[1], Some(ItemStack { kind: ItemKind::Dirt, count: 42 }));

        region.rollback_last_event();
        assert_eq!(before, crate::state_hash(region.data()), "UiAction must invert bit-exact");
    }
}
