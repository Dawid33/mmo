use std::time::Duration;

use crate::input::InputEvent;
use crate::{Client, ClientId, EntityKey, EntityKind, IsometryReal, Rollback};
use derive_more::Debug;
use parry3d::math::{Real, Vector};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Usize {
    data: usize,
}

impl From<usize> for Usize {
    fn from(value: usize) -> Self {
        let mut temp = Self::default();
        temp.data = value;
        temp
    }
}

pub type Tick = usize;
pub type EventId = usize;
pub type EntityId = usize;
/// Regions tile the horizontal plane in fixed 256-unit squares (8×8 chunks
/// of 32 voxels). Signed and unbounded: the world grows in every direction.
/// Sims stay region-local; the world offset exists only at the render
/// boundary (region root Transform) and, later, in handoff rebasing.
pub const REGION_CHUNKS: usize = 8;
pub const REGION_SIZE: f32 = (REGION_CHUNKS * 32) as f32; // 256.0, exact in f32

#[derive(
    Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq, PartialOrd,
    Ord, Hash,
)]
pub struct RegionCoords {
    pub x: i32,
    pub z: i32,
}

impl RegionCoords {
    pub fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// World-space origin of this region: `(x*256, 0, z*256)`. Exactly
    /// representable in f32, so render offsets are lossless.
    pub fn world_offset(&self) -> [f32; 3] {
        [self.x as f32 * REGION_SIZE, 0.0, self.z as f32 * REGION_SIZE]
    }

    /// Which region owns a world-space point (floor division, so the
    /// negative side maps correctly: -0.1 is region -1, not 0).
    pub fn from_world(x: f32, z: f32) -> Self {
        Self {
            x: (x / REGION_SIZE).floor() as i32,
            z: (z / REGION_SIZE).floor() as i32,
        }
    }

    /// The 3×3 window of regions centered on `self` — the client's desired
    /// loaded set.
    pub fn window_3x3(&self) -> Vec<RegionCoords> {
        let mut out = Vec::with_capacity(9);
        for dx in -1..=1 {
            for dz in -1..=1 {
                out.push(RegionCoords::new(self.x + dx, self.z + dz));
            }
        }
        out
    }
}

/// Owned entities within this distance of a region edge are mirrored as
/// ghosts into that neighbour (up to 3 at a corner).
pub const GHOST_MARGIN: f32 = 32.0;
/// Ownership flips only this far PAST the boundary; flipping back needs 2x
/// the travel. Kills boundary thrash.
pub const FLIP_HYSTERESIS: f32 = 2.0;
/// A ghost not refreshed for this many host ticks is removed by the host's
/// tick (owner parked/died/left the margin).
pub const GHOST_TTL_TICKS: usize = 25;

/// Neighbour offset that now owns a region-local point, None while this
/// region still owns it. Pure; the same function runs on server regions and
/// in the client's predicted ticks.
pub fn departure_offset(x: f32, z: f32) -> Option<(i32, i32)> {
    let axis = |v: f32| {
        if v < -FLIP_HYSTERESIS {
            -1
        } else if v > REGION_SIZE + FLIP_HYSTERESIS {
            1
        } else {
            0
        }
    };
    match (axis(x), axis(z)) {
        (0, 0) => None,
        o => Some(o),
    }
}

/// Neighbour offsets whose margin a region-local point is inside. Order is
/// fixed (x edge, z edge, corner) — determinism requires a stable order.
pub fn ghost_offsets(x: f32, z: f32) -> Vec<(i32, i32)> {
    let dx = if x < GHOST_MARGIN { -1 } else if x > REGION_SIZE - GHOST_MARGIN { 1 } else { 0 };
    let dz = if z < GHOST_MARGIN { -1 } else if z > REGION_SIZE - GHOST_MARGIN { 1 } else { 0 };
    let mut out = Vec::new();
    if dx != 0 { out.push((dx, 0)); }
    if dz != 0 { out.push((0, dz)); }
    if dx != 0 && dz != 0 { out.push((dx, dz)); }
    out
}

/// Move an isometry from one region's local frame to another's. Offsets are
/// exact multiples of 256 so the delta is exact in f32; both the server
/// relay and the client's predicted synthesis MUST use this one function.
pub fn rebase_isometry(iso: &IsometryReal, from: RegionCoords, to: RegionCoords) -> IsometryReal {
    let f = from.world_offset();
    let t = to.world_offset();
    let mut out = *iso;
    out.translation.x += Real::from(f[0] - t[0]);
    out.translation.z += Real::from(f[2] - t[2]);
    out
}

/// Shape spec carried across the transfer seam so the receiving region can
/// rebuild the collider. Players are capsules; new kinds extend this enum.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum ColliderSpec {
    CapsuleY { half_height: f32, radius: f32 },
}

/// The unit of ownership transfer. Assembled deterministically at the
/// extraction tick; `isometry` is source-local until the relay rebases it.
/// `source_region`/`source_key` are an identity token (ghost upgrade,
/// arrival idempotency) — never dereferenced in the target's slotmap.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct EntityBundle {
    pub kind: EntityKind,
    pub isometry: IsometryReal,
    pub linvel: Vector<Real>,
    pub collider: ColliderSpec,
    pub has_camera: bool,
    pub client: Option<(ClientId, Client)>,
    pub source_region: RegionCoords,
    pub source_key: EntityKey,
}

/// Per-tick mirror of a margin entity. `collider` rides along for stage 2.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct GhostData {
    pub source_region: RegionCoords,
    pub source_key: EntityKey,
    pub kind: EntityKind,
    pub isometry: IsometryReal,
    pub linvel: Vector<Real>,
    pub collider: ColliderSpec,
}

pub type RegionId = RegionCoords;
pub type LastGameEventId = usize;

#[derive(Debug)]
pub enum GameError {
    CrashedOnServerEvent,
    QuitRequested,
}

pub enum WorldId {
    Default = 0,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum ClientPacket {
    /// Game event generated by client that needs to be processed by the server.
    GameEvent(GameEvent),
    RequestPlayerRegion,
    RequestRegionConnection(RegionId),
    /// Client no longer wants this region's events (window moved on).
    ReleaseRegionConnection(RegionId),
}

type CurrentTickRate = u64;

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub enum ServerPacket {
    SyncClock(RegionId, CurrentTickRate, Tick, Duration),
    /// Game event that was proccessed by the server.
    GameEvent(GameEvent),
    // TODO: Create player serverside and add player id here to let client know
    // who he is.
    Region(RegionId, Rollback),
    PlayerRegion(Option<RegionId>, ClientId),
}

/// A Game event
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct GameEvent {
    /// Type of game event.
    pub kind: GameEventKind,
    pub id: usize,
    pub region_id: RegionId,
}

impl Ord for GameEvent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

impl PartialOrd for GameEvent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(&other))
    }
}

impl Eq for GameEvent {}

impl GameEvent {
    pub fn new(kind: GameEventKind, id: usize, region_id: RegionId) -> Self {
        Self {
            kind,
            id,
            region_id,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum GameEventKind {
    Tick,
    PlayerInput(ClientId, InputEvent),
    CreateClient(ClientId),
    Quit,
    /// Ownership transfer into this region (manager-relayed or
    /// client-predicted). Injection is an ordinary undoable mutation.
    EntityArrived(EntityBundle),
    /// Margin mirror refresh from a neighbouring region.
    GhostUpdate(GhostData),
}

impl GameEventKind {
    /// The client this event originated from. `Tick` and `Quit` are
    /// server/shared events with no originating client.
    pub fn origin_client(&self) -> Option<ClientId> {
        match self {
            GameEventKind::PlayerInput(id, _) | GameEventKind::CreateClient(id) => Some(*id),
            GameEventKind::Tick
            | GameEventKind::Quit
            | GameEventKind::EntityArrived(_)
            | GameEventKind::GhostUpdate(_) => None,
        }
    }

    /// Reconcile's prediction-removal matcher. Transfers match on IDENTITY
    /// (source region+key), not full equality: the predicted and
    /// authoritative copies differ in pose whenever the client's extraction
    /// tick differs from the server's, and the rollback-replace path must
    /// still find and remove the prediction or it re-applies forever.
    pub fn matches_prediction(&self, other: &Self) -> bool {
        use GameEventKind::*;
        match (self, other) {
            (EntityArrived(a), EntityArrived(b)) => {
                a.source_region == b.source_region && a.source_key == b.source_key
            }
            (GhostUpdate(a), GhostUpdate(b)) => {
                a.source_region == b.source_region && a.source_key == b.source_key
            }
            _ => self == other,
        }
    }
}
