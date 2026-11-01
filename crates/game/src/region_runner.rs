//! The per-region actor core: one `RegionRunner` per running region, fed
//! `RegionInput`s and emitting `RegionOutput`s over channels. This message
//! pair is the future network seam — the runner never sees subscriber
//! lists, client sessions, or network types. On the server each runner
//! lives on its own thread with its own tick timer (crates/server); in the
//! wasm build LocalServer pumps runners inline (crates/client).

use crate::{
    ClientId, Column, ColumnCoords, EntityBundle, TerrainSnapshot, GameEvent, GameEventKind, GhostData, Region,
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
    /// The finite world's extent (`Region::set_map_extent`): the manager
    /// sends it right after spawning, before any tick, when it has a map.
    SetMapExtent(Option<crate::MapExtent>),
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
    /// Build this seed's runner.
    pub fn into_runner(
        self,
        id: RegionId,
        out: Sender<(RegionCoords, RegionOutput)>,
        generate: &dyn Fn(RegionCoords) -> Vec<(ColumnCoords, Column)>,
    ) -> RegionRunner {
        RegionRunner::new(id, self.into_region_with(id, generate), out)
    }
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
            baseline_hash_time,
        }
    }

    /// Wall time of the baseline `terrain_fingerprint` taken in `new`
    /// (the `terrain_hash_ms` of the server's `region_genesis` line).
    pub fn baseline_hash_time(&self) -> std::time::Duration {
        self.baseline_hash_time
    }

    /// The region's whole terrain, for LOD.
    fn terrain(&self) -> TerrainSnapshot {
        self.region.terrain_snapshot()
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
                kind => self.apply(kind),
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
            RegionInput::SetMapExtent(extent) => self.region.set_map_extent(extent),
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
