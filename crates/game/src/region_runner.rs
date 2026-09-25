//! The per-region actor core: one `RegionRunner` per running region, fed
//! `RegionInput`s and emitting `RegionOutput`s over channels. This message
//! pair is the future network seam — the runner never sees subscriber
//! lists, client sessions, or network types. On the server each runner
//! lives on its own thread with its own tick timer (crates/server); in the
//! wasm build LocalServer pumps runners inline (crates/client).

use crate::{
    required_cells, CellChanges, CellConfig, CellCoords, CellStore, CellTickets, ClientId, Column,
    ColumnCoords, EntityBundle, TerrainSnapshot, GameEvent, GameEventKind, GhostData, Region,
    RegionCoords, RegionId, Rollback, Tick,
    TERRAIN_HASH_INTERVAL_TICKS, TICK_RATE,
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
    Event(GameEventKind),
    /// A client event the client predicted right after its own tick number
    /// `.1` of this region. Held until this region has run that tick, so it
    /// lands in the slot the client predicted instead of wherever the two
    /// clocks happen to stand on arrival; a client level with or behind the
    /// server gets it applied on arrival, like [`RegionInput::Event`].
    EventAfterTick(GameEventKind, u64),
    /// A new subscriber needs the full state; replied with
    /// `RegionOutput::Snapshot(client, ...)`.
    RequestSnapshot(ClientId),
    /// An LOD build needs read-only terrain; replied with
    /// `RegionOutput::Terrain(client, ...)`. Does not affect sim state.
    RequestTerrain(ClientId),
    /// Graceful stop; replied with `RegionOutput::Stopped(state)`.
    Shutdown,
}

#[derive(Debug)]
pub enum RegionOutput {
    EventProcessed(GameEvent),
    Snapshot(ClientId, Rollback),
    /// Reply to `RequestTerrain`: read-only terrain for one requester.
    Terrain(ClientId, TerrainSnapshot),
    /// Throttled push when this region's terrain changed; manager fans out.
    TerrainChanged(TerrainSnapshot),
    SyncClock { tick_rate: u64, tick: Tick },
    Stopped(SerializedRegion, TerrainSnapshot),
    /// Boundary crossings this tick: bundles + ABSOLUTE target regions.
    /// Isometries are still source-local; the manager rebases.
    Departures(Vec<(EntityBundle, RegionCoords)>),
    /// Margin mirrors this tick, same framing.
    GhostUpdates(Vec<(GhostData, RegionCoords)>),
}

/// How to build a region when it spawns: restored from the parking lot if
/// possible, else generated. The fallback chunks make a corrupt parked blob
/// recoverable (log + regenerate deterministically).
pub enum RegionSeed {
    Fresh(Vec<(ColumnCoords, Column)>),
    Parked(SerializedRegion, Vec<(ColumnCoords, Column)>),
    /// The spawner generates the columns itself, on the region's own thread
    /// (`RegionSpawner::generates_columns`): `None` = never run, `Some` = the
    /// parked blob to restore. Worldgen then costs the manager thread nothing
    /// (it measured 0.1-0.9 s per region start, sequentially — 6-7 s for a 3x3
    /// join), and a parked region generates only if its blob is corrupt.
    Deferred(Option<SerializedRegion>),
}

impl RegionSeed {
    /// For spawners that never receive a [`RegionSeed::Deferred`] (they do not
    /// `generates_columns`).
    pub fn into_region(self, id: RegionId) -> Region {
        self.into_region_with(id, &|rc| {
            unreachable!("Deferred seed for {rc:?} reached a spawner that does not generate columns")
        })
    }

    /// `generate` is only called for a deferred seed that actually needs
    /// columns: a never-run region, or a parked one whose blob is corrupt.
    pub fn into_region_with(
        self,
        id: RegionId,
        generate: &dyn Fn(RegionCoords) -> Vec<(ColumnCoords, Column)>,
    ) -> Region {
        let restore = |serialized: SerializedRegion, fallback: &mut dyn FnMut() -> Vec<(ColumnCoords, Column)>| {
            match serialized.to_rollback() {
                Ok(rollback) => Region::new(rollback, None, id, None),
                Err(e) => {
                    log::error!("parked region {:?} corrupt ({e}); regenerating", id);
                    Region::from_columns(id, fallback())
                }
            }
        };
        match self {
            RegionSeed::Fresh(columns) => Region::from_columns(id, columns),
            RegionSeed::Parked(serialized, fallback) => {
                let mut fallback = Some(fallback);
                restore(serialized, &mut || fallback.take().unwrap_or_default())
            }
            RegionSeed::Deferred(None) => Region::from_columns(id, generate(id)),
            RegionSeed::Deferred(Some(serialized)) => restore(serialized, &mut || generate(id)),
        }
    }
}

impl RegionSeed {
    /// Build this seed's runner: legacy (fully materialized) when
    /// `cell_chunks == 0`, else a cell-mode runner whose store holds the
    /// generated columns. Cell regions never park, so a parked seed in cell
    /// mode is unexpected and restores legacy.
    pub fn into_runner(
        self,
        id: RegionId,
        out: Sender<(RegionCoords, RegionOutput)>,
        generate: &dyn Fn(RegionCoords) -> Vec<(ColumnCoords, Column)>,
        cell_chunks: usize,
    ) -> RegionRunner {
        if cell_chunks == 0 {
            return RegionRunner::new(id, self.into_region_with(id, generate), out);
        }
        let columns = match self {
            RegionSeed::Fresh(columns) => columns,
            RegionSeed::Deferred(None) => generate(id),
            parked => {
                log::error!("cell-mode region {id:?} got a parked seed; restoring legacy");
                return RegionRunner::new(id, parked.into_region_with(id, generate), out);
            }
        };
        RegionRunner::new_with_cells(
            id,
            Region::new_cells(id, cell_chunks, None, None),
            out,
            CellStore::from_columns(columns, cell_chunks),
            CellConfig::new(cell_chunks),
        )
    }
}

/// Cell-mode state of a runner (spec §1, §3): tickets + the dormant store.
pub struct CellMode {
    store: CellStore,
    tickets: CellTickets,
    config: CellConfig,
}

pub struct RegionRunner {
    id: RegionCoords,
    region: Region,
    out: Sender<(RegionCoords, RegionOutput)>,
    /// Last-pushed terrain hash; a change past the throttle window pushes a
    /// `TerrainChanged`. Seeded at construction so the initial state is a
    /// baseline, not a spurious push.
    last_terrain_hash: u32,
    /// Cost of the cadence terrain-hash checks since the last
    /// `take_terrain_hash_cost` (metrics only): count, total, slowest.
    terrain_hash_cost: (u32, std::time::Duration, std::time::Duration),
    /// Client events waiting for the tick they were predicted after, in
    /// arrival order.
    held: std::collections::VecDeque<(GameEventKind, u64)>,
    /// `Some` for a cell-mode region (`RegionRunner::new_with_cells`).
    cells: Option<CellMode>,
    /// See [`RegionRunner::baseline_hash_time`].
    baseline_hash_time: std::time::Duration,
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
        let hash_started = crate::perf::Stopwatch::start();
        let last_terrain_hash = region.terrain_fingerprint();
        let baseline_hash_time = hash_started.elapsed();
        Self {
            id,
            region,
            out,
            last_terrain_hash,
            terrain_hash_cost: Default::default(),
            held: Default::default(),
            cells: None,
            baseline_hash_time,
        }
    }

    /// A cell-mode runner: `region` must come from `Region::new_cells` with
    /// the same cell size as `config`. Terrain loads through tickets.
    pub fn new_with_cells(
        id: RegionCoords,
        region: Region,
        out: Sender<(RegionCoords, RegionOutput)>,
        store: CellStore,
        config: CellConfig,
    ) -> Self {
        assert_eq!(
            region.data().cells.cell_chunks,
            config.cell_chunks,
            "region/config cell size mismatch"
        );
        let mut runner = Self::new(id, region, out);
        runner.cells = Some(CellMode { store, tickets: CellTickets::default(), config });
        runner
    }

    /// Wall time of the baseline `terrain_fingerprint` taken in `new`
    /// (the `terrain_hash_ms` of the server's `region_genesis` line).
    pub fn baseline_hash_time(&self) -> std::time::Duration {
        self.baseline_hash_time
    }

    /// The region's whole terrain, for LOD: the sim's chunks, plus — in cell
    /// mode — every dormant cell's stored chunks. Without the dormant part a
    /// released cell would become a hole in the LOD rings.
    fn terrain(&self) -> TerrainSnapshot {
        let mut snap = self.region.terrain_snapshot();
        if let Some(mode) = self.cells.as_ref() {
            snap.chunks.extend(mode.store.dormant_chunks(&self.region.data().cells.active));
            snap.chunks.sort_by_key(|(c, _)| (c.x, c.y, c.z));
        }
        snap
    }

    pub fn cell_store(&self) -> Option<&CellStore> {
        self.cells.as_ref().map(|c| &c.store)
    }

    /// Emit one CellChanges (if non-empty) for `deactivate` + `activate`,
    /// then file dematerialized cells back into the store.
    fn apply_cell_changes(&mut self, deactivate: Vec<CellCoords>, activate: Vec<CellCoords>) {
        let Some(mode) = self.cells.as_mut() else { return };
        let mut changes = CellChanges { deactivate, activate: Vec::new() };
        for cell in activate {
            match mode.store.payload(cell) {
                Some(p) => changes.activate.push(p),
                None => log::error!("[cells r={:?}] no store entry for {cell:?}; skipped", self.id),
            }
        }
        if changes.is_empty() {
            return;
        }
        self.apply(GameEventKind::CellChanges(changes));
        let dormant = self.region.take_dormant();
        if let Some(mode) = self.cells.as_mut() {
            for (cell, content) in dormant {
                mode.store.put(cell, content);
            }
        }
    }

    /// Required cells around `pos` that are not active yet, activated now,
    /// outside the budget (joins and arrivals).
    fn ensure_required(&mut self, pos: (f32, f32)) {
        let Some(mode) = self.cells.as_ref() else { return };
        let active = &self.region.data().cells.active;
        let missing: Vec<CellCoords> = required_cells(pos, mode.config.cell_chunks)
            .into_iter()
            .filter(|c| !active.contains(c))
            .collect();
        self.apply_cell_changes(Vec::new(), missing);
    }

    /// The per-tick ticket pass (before the Tick event).
    fn run_tickets(&mut self) {
        let Some(mode) = self.cells.as_mut() else { return };
        let players = self.region.data().player_positions();
        let plan = mode.tickets.plan(&self.region.data().cells.active, &players, &mode.config);
        self.apply_cell_changes(plan.deactivate, plan.activate);
    }

    /// `(checks, total, slowest)` of the every-`TERRAIN_HASH_INTERVAL_TICKS`
    /// terrain snapshot + hash since the previous call; resets it. Each check
    /// runs inside a tick, so `slowest` is a floor on that tick's duration.
    pub fn take_terrain_hash_cost(&mut self) -> (u32, std::time::Duration, std::time::Duration) {
        std::mem::take(&mut self.terrain_hash_cost)
    }

    /// Returns false when the runner should stop (after Shutdown).
    pub fn handle_input(&mut self, input: RegionInput) -> bool {
        match input {
            RegionInput::Event(kind) => match kind {
                // The manager filters these; drop defensively rather than
                // double-ticking or stopping on a stray packet.
                GameEventKind::Tick | GameEventKind::Quit => {}
                kind => {
                    // Cell mode: a joining or arriving body needs its cells
                    // under it before it lands.
                    match &kind {
                        GameEventKind::CreateClient(_) => {
                            self.ensure_required(crate::player_spawn_xz(true))
                        }
                        GameEventKind::EntityArrived(b) => {
                            let t = b.isometry.translation;
                            self.ensure_required((t.x.0, t.z.0));
                        }
                        _ => {}
                    }
                    self.apply(kind)
                }
            },
            RegionInput::EventAfterTick(kind, after_tick) => {
                let now = self.region.current_tick() as u64;
                if after_tick > now && after_tick - now <= MAX_INPUT_LEAD_TICKS {
                    self.held.push_back((kind, after_tick));
                } else {
                    self.apply(kind);
                }
            }
            RegionInput::RequestSnapshot(client_id) => {
                let _ = self.out.send((
                    self.id,
                    RegionOutput::Snapshot(client_id, self.region.data().clone()),
                ));
            }
            RegionInput::RequestTerrain(client_id) => {
                let _ = self.out.send((
                    self.id,
                    RegionOutput::Terrain(client_id, self.terrain()),
                ));
            }
            RegionInput::Shutdown => {
                // Input must not be lost to a park.
                for (kind, _) in std::mem::take(&mut self.held) {
                    self.apply(kind);
                }
                let serialized = SerializedRegion::from_rollback(self.region.data());
                let far = self.terrain();
                let _ = self.out.send((self.id, RegionOutput::Stopped(serialized, far)));
                return false;
            }
        }
        true
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

    /// One sim tick + the every-10-ticks SyncClock self-report. The caller
    /// owns pacing (thread timer on the server, frame accumulator on wasm).
    pub fn tick(&mut self) {
        self.run_tickets();
        let event = self
            .region
            .handle_event(GameEventKind::Tick)
            .expect("region tick failed");
        self.region.commit_server_event();
        let _ = self.out.send((self.id, RegionOutput::EventProcessed(event)));
        // Events predicted after the tick that just ran, in arrival order.
        let now = self.region.current_tick() as u64;
        let mut i = 0;
        while i < self.held.len() {
            if self.held[i].1 <= now {
                let (kind, _) = self.held.remove(i).expect("index checked");
                self.apply(kind);
            } else {
                i += 1;
            }
        }
        let (departures, ghosts) = self.region.take_transfers();
        let frozen = self.region.take_frozen();
        if let Some(mode) = self.cells.as_mut() {
            for (cell, bundle) in frozen {
                mode.store.add_entity(cell, bundle);
            }
        }
        if !departures.is_empty() {
            let _ = self.out.send((self.id, RegionOutput::Departures(departures)));
        }
        if !ghosts.is_empty() {
            let _ = self.out.send((self.id, RegionOutput::GhostUpdates(ghosts)));
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
        // Push-on-change, throttled: the check cadence is the throttle. The
        // manager invalidates this region's LOD keys and rebuilds the
        // subscribed ones.
        if self.region.current_tick() % TERRAIN_HASH_INTERVAL_TICKS == 0 {
            let watch = crate::perf::Stopwatch::start();
            // In-place fingerprint: the (large) snapshot is only built below,
            // once something actually changed.
            let hash = self.region.terrain_fingerprint();
            let took = watch.elapsed();
            self.terrain_hash_cost.0 += 1;
            self.terrain_hash_cost.1 += took;
            self.terrain_hash_cost.2 = self.terrain_hash_cost.2.max(took);
            if hash != self.last_terrain_hash {
                self.last_terrain_hash = hash;
                let snap = self.terrain();
                let _ = self.out.send((self.id, RegionOutput::TerrainChanged(snap)));
            }
        }
    }

    pub fn current_tick(&self) -> usize {
        self.region.current_tick()
    }

    pub fn region_mut(&mut self) -> &mut Region {
        &mut self.region
    }
}
