//! The per-region actor core: one `RegionRunner` per running region, fed
//! `RegionInput`s and emitting `RegionOutput`s over channels. This message
//! pair is the future network seam — the runner never sees subscriber
//! lists, client sessions, or network types. On the server each runner
//! lives on its own thread with its own tick timer (crates/server); in the
//! wasm build LocalServer pumps runners inline (crates/client).

use crate::{
    Chunk, ChunkCoords, ClientId, GameEvent, GameEventKind, Region, RegionCoords, RegionId,
    Rollback, Tick, TICK_RATE,
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
    /// A new subscriber needs the full state; replied with
    /// `RegionOutput::Snapshot(client, ...)`.
    RequestSnapshot(ClientId),
    /// Graceful stop; replied with `RegionOutput::Stopped(state)`.
    Shutdown,
}

#[derive(Debug)]
pub enum RegionOutput {
    EventProcessed(GameEvent),
    Snapshot(ClientId, Rollback),
    SyncClock { tick_rate: u64, tick: Tick },
    Stopped(SerializedRegion),
}

/// How to build a region when it spawns: restored from the parking lot if
/// possible, else generated. The fallback chunks make a corrupt parked blob
/// recoverable (log + regenerate deterministically).
pub enum RegionSeed {
    Fresh(Vec<(ChunkCoords, Chunk)>),
    Parked(SerializedRegion, Vec<(ChunkCoords, Chunk)>),
}

impl RegionSeed {
    pub fn into_region(self, id: RegionId) -> Region {
        match self {
            RegionSeed::Fresh(chunks) => Region::from_chunks(id, chunks),
            RegionSeed::Parked(serialized, fallback) => match serialized.to_rollback() {
                Ok(rollback) => Region::new(rollback, None, id, None),
                Err(e) => {
                    log::error!("parked region {:?} corrupt ({e}); regenerating", id);
                    Region::from_chunks(id, fallback)
                }
            },
        }
    }
}

pub struct RegionRunner {
    id: RegionCoords,
    region: Region,
    out: Sender<(RegionCoords, RegionOutput)>,
}

impl RegionRunner {
    pub fn new(
        id: RegionCoords,
        region: Region,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Self {
        Self { id, region, out }
    }

    /// Returns false when the runner should stop (after Shutdown).
    pub fn handle_input(&mut self, input: RegionInput) -> bool {
        match input {
            RegionInput::Event(kind) => match kind {
                // The manager filters these; drop defensively rather than
                // double-ticking or stopping on a stray packet.
                GameEventKind::Tick | GameEventKind::Quit => {}
                kind => {
                    // Server-side regions never roll back: forget each
                    // event's transaction immediately (undo log stays
                    // bounded), same policy as the old main loop.
                    let event = self
                        .region
                        .handle_event(kind)
                        .expect("region event processing failed");
                    self.region.forget_last_event();
                    let _ = self.out.send((self.id, RegionOutput::EventProcessed(event)));
                }
            },
            RegionInput::RequestSnapshot(client_id) => {
                let _ = self.out.send((
                    self.id,
                    RegionOutput::Snapshot(client_id, self.region.data().clone()),
                ));
            }
            RegionInput::Shutdown => {
                let serialized = SerializedRegion::from_rollback(self.region.data());
                let _ = self.out.send((self.id, RegionOutput::Stopped(serialized)));
                return false;
            }
        }
        true
    }

    /// One sim tick + the every-10-ticks SyncClock self-report. The caller
    /// owns pacing (thread timer on the server, frame accumulator on wasm).
    pub fn tick(&mut self) {
        let event = self
            .region
            .handle_event(GameEventKind::Tick)
            .expect("region tick failed");
        self.region.forget_last_event();
        let _ = self.out.send((self.id, RegionOutput::EventProcessed(event)));
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

    pub fn current_tick(&self) -> usize {
        self.region.current_tick()
    }
}
