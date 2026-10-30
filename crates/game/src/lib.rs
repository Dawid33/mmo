// #![deny(missing_docs)]
//! Game simulation code that is shared between client and server.

// Load-bearing: the #[rollback] macro expansion in `state` derives
// `crate::serde::Serialize`/`Deserialize`, so `serde` must be reachable as a
// crate-root item — this extern crate provides that binding.
extern crate serde;

#[cfg_attr(test, macro_use)]
extern crate alloc;
extern crate num_traits as num;
extern crate std;

use std::collections::BTreeMap;

pub mod block;
pub mod camera;
pub mod debug_render;
pub mod edit;
pub mod ground;
pub mod input;
pub mod lod;
pub mod lod_builder;
pub mod movement;
pub mod orders;
pub mod perf;
mod physics;
pub mod protocol;
mod region;
pub mod region_runner;
pub mod registry;
pub mod slime;
pub mod state;
pub mod ui;
pub mod volume;
pub mod voxel;
pub mod world_manager;
pub mod world_map;
pub use parry3d as parry;
pub use rapier3d;
/// The physics fork's math: glam 0.33 through glamx (`Vec3`, `Quat`, `Mat4`,
/// `Pose3`, `IVec3`, …). NOT Bevy's glam (0.32) — the render boundary
/// converts through arrays (`client/src/renderer/convert.rs`).
pub use glamx as math;
/// A body pose (translation + unit-quaternion rotation), rapier's `Pose`.
pub use rapier3d::math::Pose;
/// The rollback hash trait (plan D2); `#[derive(StateHash)]` on game types.
pub use state_hash::StateHash;

// The #[rollback] macro expansion in `state` and the borrow::Partial derives
// rely on these items being reachable at the crate root (`crate::...`).
pub use block::*;
pub use camera::*;
pub use ground::*;
pub use debug_render::{ColliderDebug, DebugFlags, DebugLine, DEBUG_COLLIDER_RADIUS};
pub use input::*;
pub use lod::*;
pub use lod_builder::*;
pub use movement::Motion;
pub use orders::*;
pub use protocol::*;
pub use region_runner::*;
pub use registry::*;
pub use state::*;
pub use ui::*;
pub use volume::*;
pub use voxel::*;
pub use world_manager::*;
pub use world_map::*;

pub const TICK_RATE: u64 = 50;
/// TESTING KNOB — deliberately 0 in normal operation. Extra client-ahead lead
/// (ms) the SyncClock pacing loop targets; raise it only to fabricate latency
/// when testing the adaptive tick. Not a production pacing target and not an
/// oversight.
pub const TEST_INDUCED_LATENCY_MS: isize = 0;

/// How often the client and server each emit their rolling keep-up metrics
/// summary line (avg/max tick duration, reconciliation load). One tunable
/// shared by both sides so their windows line up.
pub const METRICS_REPORT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Deterministic, cross-machine-stable hash of a rollback's simulation state.
/// crc32 over the `StateHash` walk (not a default-seeded DefaultHasher, not
/// std `Hash`, which writes `usize` at pointer width), so it is identical
/// across runs, machines and native/wasm32 — the basis for client/server
/// convergence assertions in tests.
pub fn state_hash(r: &Rollback) -> u32 {
    let mut h = StateHasher::new();
    r.data.state_hash(&mut h);
    h.finalize()
}

/// Bytes staged per crc32 call by [`StateHasher`].
pub const STATE_HASHER_CHUNK: usize = 4096;

/// crc32 of everything written to it, through a fixed staging buffer.
///
/// A `StateHash` walk writes a few bytes per field, and crc32fast's per-call
/// overhead is ~90% of hashing field by field (measured in plan Task 4.4:
/// bodies 2307 µs unbuffered vs 251 µs buffered). Staging the bytes and
/// feeding crc32 `STATE_HASHER_CHUNK` at a time gives the SAME value as
/// `crc32fast::Hasher` fed the same writes (crc32 is a stream: chunking does
/// not change it), so every target agrees; no heap, so a per-entry digest
/// costs nothing extra to create.
#[derive(Clone)]
pub struct StateHasher {
    crc: crc32fast::Hasher,
    len: usize,
    buf: [u8; STATE_HASHER_CHUNK],
}

impl Default for StateHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl StateHasher {
    pub fn new() -> Self {
        Self { crc: crc32fast::Hasher::new(), len: 0, buf: [0; STATE_HASHER_CHUNK] }
    }

    fn flush(&mut self) {
        self.crc.update(&self.buf[..self.len]);
        self.len = 0;
    }

    /// The crc32 of every byte written.
    pub fn finalize(mut self) -> u32 {
        self.flush();
        self.crc.finalize()
    }
}

impl std::hash::Hasher for StateHasher {
    fn write(&mut self, bytes: &[u8]) {
        if self.len + bytes.len() > STATE_HASHER_CHUNK {
            self.flush();
        }
        if bytes.len() >= STATE_HASHER_CHUNK {
            self.crc.update(bytes);
            return;
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    /// As `crc32fast::Hasher::finish`: the crc so far, widened.
    fn finish(&self) -> u64 {
        self.clone().finalize() as u64
    }
}

/// Connection/sync state surfaced to the client HUD. Derived from the
/// manager's `ready`/`is_caught_up` flags; pure `game` type so the enum can
/// cross the Bevy-free boundary in `ClientUpdateEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionState {
    #[default]
    Connecting,
    CatchingUp,
    Ready,
}

#[derive(Debug)]
pub enum ClientUpdateEvent {
    NewRegion(
        RegionId,
        GameData,
        crossbeam::channel::Receiver<GameDataUpdate>,
    ),
    /// The window moved on (or a region is being replaced): tear down this
    /// region's render state.
    RemoveRegion(RegionId),
    /// Streamed LOD cells for one key (initial send or refresh —
    /// replace-on-re-receipt). Decoded and spawned by the renderer.
    LodData(LodKey, LodPayload),
    /// The ring assignment moved on: retire this key's render root.
    /// `near_takeover`: (one of) the key's regions just joined the near
    /// window, so the replacement is authoritative terrain still on its way
    /// — the renderer keeps the LOD until that region is fully meshed rather
    /// than on a frame deadline.
    RemoveLod { key: LodKey, near_takeover: bool },
    GameCrash(GameError),
    SetPlayer(ClientId),
    /// Region + connection status for the debug HUD. Edge-triggered by the
    /// manager (only sent on change); consumed by the render bridge.
    HudStatus {
        home_region: RegionId,
        viewer_region: RegionId,
        connection: ConnectionState,
    },
    /// One-shot: the receiver half of the client-side UI projection channel.
    /// Sent once by the manager; consumed by the ui_bridge. Widget-only —
    /// separate from the per-region `GameDataUpdate` channel because widgets
    /// hang off a screen-space UI root, not a region's world-space root.
    UiChannel(crossbeam::channel::Receiver<UiUpdate>),
    /// Collider wireframe buffer for one region (region-local coords),
    /// regenerated per tick while the debug flag is on — including empty
    /// buffers, so the render side's replace-on-receipt clears stale
    /// geometry as the viewer moves. Debug-only; never serialized.
    DebugLines(RegionId, Vec<DebugLine>),
    /// The server's world map (forged worlds only; sent once on connect).
    /// Boxed: it is the one large variant.
    WorldMap(Box<WorldMap>),
    /// The server refused a Play request for this region.
    PlayRefused(RegionCoords, PlayRefusal),
}

#[cfg(not(target_arch = "wasm32"))]
pub use region::RegionTickPerf;
pub use region::{ReconcileStats, Region};

pub trait Controller: Send {
    fn on_tick<'a>(&mut self, _t: &mut Undo<GameData>) {}
}

/// Set the undo hash-verification mode from the `HASH_MODE` env var
/// (`off` | `delta` | `full`, case-insensitive), defaulting to Off — the
/// production mode, with desync caught by the periodic tick-interval state
/// hash instead — when unset (including wasm, which has no env). Client
/// and server startup call this; tests and harnesses instead pin the mode
/// explicitly (the process-global default stays Full for them). Run with
/// `HASH_MODE=delta` for per-entry footprint digests, or
/// `HASH_MODE=full` when chasing a suspected footprint/capture gap: Full
/// whole-field-hashes every entry and catches state an undo closure missed,
/// which Delta's declared-footprint digests structurally cannot see.
/// Panics on an unrecognized value — a typo silently downgrading
/// verification is worse than a startup crash.
pub fn set_hash_mode_from_env() {
    let mode = match std::env::var("HASH_MODE") {
        Ok(v) => match v.to_ascii_lowercase().as_str() {
            "off" => HashMode::Off,
            "delta" => HashMode::Delta,
            "full" => HashMode::Full,
            other => panic!("HASH_MODE must be off|delta|full, got {other:?}"),
        },
        Err(_) => HashMode::Off,
    };
    // println, not log: this runs before either binary initializes its logger.
    println!("undo hash verification mode: {mode:?}");
    set_hash_mode(mode);
}

pub struct World {
    pub regions: BTreeMap<RegionId, Region>,
}

impl World {
    pub fn new() -> Self {
        return Self {
            regions: BTreeMap::new(),
        };
    }

    pub fn basic() -> Self {
        let one = RegionCoords::new(0, 0);
        let chunks = (0..REGION_CHUNKS)
            .flat_map(|x| {
                (0..REGION_CHUNKS).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8)))
            })
            .collect();
        let data = Region::from_chunks(one, chunks);

        return Self {
            regions: BTreeMap::from([(one, data)]),
        };
    }

    pub fn current_tick(&self, id: &RegionId) -> usize {
        self.regions.get(id).unwrap().current_tick()
    }

    pub fn region_exists(&self, id: &RegionId) -> bool {
        self.regions.contains_key(id)
    }

    /// All region ids currently loaded client-side. Test/harness hook.
    pub fn loaded_regions(&self) -> Vec<RegionCoords> {
        self.regions.keys().copied().collect()
    }

    pub fn data(&self, id: &RegionId) -> &Rollback {
        self.regions.get(id).unwrap().data()
    }

    pub fn load(&mut self, id: &RegionId, region: Region) {
        self.regions.insert(*id, region);
    }

    pub fn handle_region_event(
        &mut self,
        event: GameEventKind,
        region_id: RegionId,
    ) -> Result<GameEvent, GameError> {
        let region = self.regions.get_mut(&region_id).unwrap();
        region.handle_event(event)
    }

    pub fn progress_world_one_tick(
        &mut self,
        results: &mut BTreeMap<RegionId, Result<GameEvent, GameError>>,
    ) {
        self.progress_world_one_tick_except(results, |_| false);
    }

    /// [`Self::progress_world_one_tick`] that leaves out the regions `skip`
    /// picks: a client holding one back whose timeline has run too far ahead
    /// of its server counterpart.
    pub fn progress_world_one_tick_except(
        &mut self,
        results: &mut BTreeMap<RegionId, Result<GameEvent, GameError>>,
        skip: impl Fn(&RegionId) -> bool,
    ) {
        results.clear();
        for (id, region) in &mut self.regions {
            if !skip(id) {
                results.insert(*id, region.handle_event(GameEventKind::Tick));
            }
        }
    }

    /// Used by server
    pub fn forget_last_event(&mut self, region_id: &RegionId) {
        self.regions.get_mut(region_id).unwrap().forget_last_event();
    }

    /// Aggregate transfer buffers from all loaded regions. Call immediately
    /// after progress_world_one_tick and at no other time.
    pub fn take_transfers(
        &mut self,
    ) -> (
        Vec<(EntityBundle, RegionCoords)>,
        Vec<(GhostData, RegionCoords)>,
    ) {
        let mut departures = Vec::new();
        let mut ghosts = Vec::new();
        for (_, region) in &mut self.regions {
            let (d, g) = region.take_transfers();
            departures.extend(d);
            ghosts.extend(g);
        }
        (departures, ghosts)
    }

    pub fn reconcile_event(&mut self, event: GameEvent) -> Result<ReconcileStats, GameError> {
        // Tolerate events for regions we don't hold: with a moving 3×3
        // window, an event racing a just-released region is steady-state
        // noise, not an error.
        let Some(region) = self.regions.get_mut(&event.region_id) else {
            log::debug!("dropping event for unloaded region {:?}", event.region_id);
            return Ok(ReconcileStats::default());
        };
        region.reconcile(event)
    }

    pub fn remove_region(&mut self, id: &RegionId) -> Option<Region> {
        self.regions.remove(id)
    }

    pub fn find_player(&self, client: &ClientId) -> Option<RegionId> {
        let mut region = None;
        for (id, r) in &self.regions {
            if r.data().player_entites.contains_key(client) {
                region = Some(*id);
                break;
            };
        }
        region
    }

    pub fn build_region_server_packet(&self, region_id: &RegionId) -> ServerPacket {
        self.regions
            .get(&region_id)
            .unwrap()
            .build_region_server_packet(region_id)
    }

    pub fn get_region_data(&self, region_id: &RegionId) -> Rollback {
        self.regions.get(&region_id).unwrap().data().clone()
    }
}

#[cfg(test)]
mod hud_status_tests {
    use super::*;

    #[test]
    fn connection_state_default_is_connecting() {
        assert_eq!(ConnectionState::default(), ConnectionState::Connecting);
    }

    #[test]
    fn hud_status_variant_constructs() {
        let ev = ClientUpdateEvent::HudStatus {
            home_region: RegionCoords::new(1, 2),
            viewer_region: RegionCoords::new(1, 3),
            connection: ConnectionState::Ready,
        };
        match ev {
            ClientUpdateEvent::HudStatus { connection, .. } => {
                assert_eq!(connection, ConnectionState::Ready);
            }
            _ => panic!("wrong variant"),
        }
    }
}

#[cfg(test)]
mod state_hasher_tests {
    use super::{StateHasher, STATE_HASHER_CHUNK};
    use std::hash::Hasher;

    /// The same writes through `StateHasher` and plain `crc32fast::Hasher`,
    /// including writes that straddle, fill and exceed the staging buffer.
    fn both(writes: &[Vec<u8>]) -> (u32, u32, u64, u64) {
        let (mut a, mut b) = (StateHasher::new(), crc32fast::Hasher::new());
        for w in writes {
            a.write(w);
            b.write(w);
        }
        let (fa, fb) = (a.finish(), b.finish());
        (a.finalize(), b.finalize(), fa, fb)
    }

    #[test]
    fn buffered_crc_equals_unbuffered_crc() {
        let bytes = |n: usize, seed: u8| {
            (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect::<Vec<u8>>()
        };
        let cases: Vec<Vec<Vec<u8>>> = vec![
            vec![],
            vec![bytes(1, 1)],
            (0..5000).map(|i| bytes(i % 13, i as u8)).collect(),
            vec![bytes(STATE_HASHER_CHUNK - 1, 2), bytes(2, 3)],
            vec![bytes(STATE_HASHER_CHUNK, 4)],
            vec![bytes(7, 5), bytes(3 * STATE_HASHER_CHUNK + 5, 6), bytes(9, 7)],
        ];
        for (i, writes) in cases.iter().enumerate() {
            let (a, b, fa, fb) = both(writes);
            assert_eq!(a, b, "case {i}: finalize");
            assert_eq!(fa, fb, "case {i}: finish");
        }
    }
}
