use std::time::Duration;

use crate::input::InputEvent;
use crate::lod::LodKey;
use crate::{
    Chunk, ChunkCoords, Client, ClientId, Column, ColumnCoords, EntityKey, EntityKind,
    IsometryReal, Rollback,
};
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
/// Regions tile the horizontal plane in fixed 1024-unit squares (32×32
/// chunks of 32 voxels). Signed and unbounded: the world grows in every
/// direction. Sims stay region-local; the world offset exists only at the
/// render boundary (region root Transform) and, later, in handoff rebasing.
pub const REGION_CHUNKS: usize = 32;
pub const REGION_SIZE: f32 = (REGION_CHUNKS * 32) as f32; // 1024.0, exact in f32

/// A running region checks its terrain hash this often and pushes a
/// `TerrainChanged` only when it changed — the check cadence IS the
/// refresh throttle.
pub const TERRAIN_HASH_INTERVAL_TICKS: usize = 25;

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
        self.window_radius(1)
    }

    /// The (2r+1)² window of regions centered on `self`. Radius 0 is `self`
    /// alone — headless tests use it to avoid paying for eight neighbours they
    /// never look at.
    pub fn window_radius(&self, r: i32) -> Vec<RegionCoords> {
        let span = 2 * r + 1;
        let mut out = Vec::with_capacity((span * span) as usize);
        for dx in -r..=r {
            for dz in -r..=r {
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
    /// Authoritative look scalars (the body quaternion is derived from them
    /// on every look change) — zeroes when `has_camera` is false.
    pub yaw: Real,
    pub pitch: Real,
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
    /// A client materialized a cell differently from the server (spec §1).
    CellHashMismatch(crate::CellCoords),
}

pub enum WorldId {
    Default = 0,
}

/// The visual-only subset of a region's data: terrain chunks, nothing else.
/// Much smaller than a `Rollback` (a `Chunk` is 8 blocks + a sparse chisel
/// map). The source for server-side LOD builds; never carries
/// entities/physics/undo state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TerrainSnapshot {
    pub chunks: Vec<(ChunkCoords, Chunk)>,
}

impl TerrainSnapshot {
    /// Materialize worldgen columns into the same `(ChunkCoords, Chunk)` form
    /// `Region::from_columns` uses — how never-run regions are served.
    pub fn from_columns(columns: Vec<(ColumnCoords, Column)>) -> Self {
        let mut chunks: Vec<_> = columns
            .into_iter()
            .flat_map(|(col, column)| {
                column
                    .materialized_sub_chunks()
                    .into_iter()
                    .map(move |(sub_y, chunk)| (ChunkCoords::new(col.x, sub_y, col.z), chunk))
            })
            .collect();
        // Sort by coordinate: keeps `terrain_hash` order-insensitive and
        // stable relative to `Rollback::terrain_snapshot`'s
        // ECS-insertion-order output, since both are hashed positionally via
        // bincode.
        chunks.sort_by_key(|(c, _)| (c.x, c.y, c.z));
        Self { chunks }
    }

    /// crc32 of the terrain, for cheap change detection on the region thread.
    pub fn terrain_hash(&self) -> u32 {
        let mut h = crc32fast::Hasher::new();
        h.update(&bincode::serialize(self).expect("terrain snapshot serializes"));
        h.finalize()
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum ClientPacket {
    /// Game event generated by client that needs to be processed by the server.
    GameEvent(GameEvent),
    /// [`ClientPacket::GameEvent`] that the client predicted right after its
    /// own tick number `.1` of the event's region; see
    /// `RegionInput::EventAfterTick`. u64: crosses the 64-bit/wasm32 seam.
    GameEventAfterTick(GameEvent, u64),
    RequestPlayerRegion,
    RequestRegionConnection(RegionId),
    /// Client no longer wants this region's events (window moved on).
    ReleaseRegionConnection(RegionId),
    /// Subscribe to one LOD key: the server replies with `LodData` (from
    /// cache or after building) and re-pushes on terrain change.
    RequestLod(LodKey),
    /// Unsubscribe; removal is client-driven, there is no "gone" packet.
    ReleaseLod(LodKey),
}

/// One LOD response: (key-local cell group, RLE grid) pairs.
pub type LodPayload = Vec<([i32; 3], crate::lod::PackedLodGrid)>;

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
    /// Streamed LOD cells for one key (initial send AND refresh —
    /// replace-on-re-receipt).
    LodData(LodKey, LodPayload),
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
    /// A logical-space UI action from a client (inventory move, toggle, ...).
    UiAction(ClientId, crate::UiAction),
    /// Server-authored cell loading (cell-mode regions only): applied at the
    /// confirmed head by every replica, never predicted.
    CellChanges(crate::CellChanges),
}

impl GameEventKind {
    /// The client this event originated from. `Tick` and `Quit` are
    /// server/shared events with no originating client.
    pub fn origin_client(&self) -> Option<ClientId> {
        match self {
            GameEventKind::PlayerInput(id, _)
            | GameEventKind::CreateClient(id)
            | GameEventKind::UiAction(id, _) => Some(*id),
            GameEventKind::Tick
            | GameEventKind::Quit
            | GameEventKind::EntityArrived(_)
            | GameEventKind::GhostUpdate(_)
            | GameEventKind::CellChanges(_) => None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lod_packets_round_trip_bincode() {
        use crate::lod::{LodKey, PackedLodGrid};
        let key = LodKey { factor: 64, coords: [-1, 2] };
        let req = ClientPacket::RequestLod(key);
        let bytes = bincode::serialize(&req).unwrap();
        assert!(matches!(
            bincode::deserialize::<ClientPacket>(&bytes).unwrap(),
            ClientPacket::RequestLod(k) if k == key
        ));
        let payload: LodPayload = vec![([0, -1, 0], PackedLodGrid(vec![(100, crate::BlockId(2))]))];
        let data = ServerPacket::LodData(key, payload.clone());
        let bytes = bincode::serialize(&data).unwrap();
        match bincode::deserialize::<ServerPacket>(&bytes).unwrap() {
            ServerPacket::LodData(k, p) => {
                assert_eq!(k, key);
                assert_eq!(p, payload);
            }
            other => panic!("wrong variant {other:?}"),
        }
    }
}

// ---- Wire framing -----------------------------------------------------------

/// Payloads at or below this many bincode bytes go out raw. Tick traffic
/// (inputs, game events, clock syncs) is tens of bytes and must not pay for
/// compression; region snapshots and LOD payloads are far above it.
pub const PACKET_COMPRESS_THRESHOLD: usize = 4096;

/// Cap on the decompressed size a frame may claim. The length prefix is
/// untrusted wire data and is what sizes the decode buffer, so it is checked
/// before anything is allocated. A region snapshot is ~40 MiB.
pub const MAX_DECOMPRESSED_PACKET_BYTES: usize = 256 * 1024 * 1024;

const FRAME_RAW: u8 = 0;
const FRAME_LZ4: u8 = 1;

#[derive(Debug)]
pub enum PacketError {
    Empty,
    UnknownFormat(u8),
    /// A compressed frame shorter than its own header.
    Truncated,
    /// A compressed frame claiming more than [`MAX_DECOMPRESSED_PACKET_BYTES`].
    TooLarge(usize),
    Decompress(lz4_flex::block::DecompressError),
    Decode(bincode::Error),
}

impl std::fmt::Display for PacketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PacketError::Empty => write!(f, "empty packet"),
            PacketError::UnknownFormat(tag) => write!(f, "unknown packet format tag {tag}"),
            PacketError::Truncated => write!(f, "compressed packet shorter than its header"),
            PacketError::TooLarge(n) => write!(f, "packet claims {n} decompressed bytes (cap {MAX_DECOMPRESSED_PACKET_BYTES})"),
            PacketError::Decompress(e) => write!(f, "lz4: {e}"),
            PacketError::Decode(e) => write!(f, "bincode: {e}"),
        }
    }
}

impl std::error::Error for PacketError {}

/// Frame `value` for the wire: one format byte, then either the bincode bytes
/// (`0`) or a little-endian `u32` decompressed length and an lz4 block (`1`).
///
/// Every transport edge uses this pair — server, native client, wasm client —
/// so they must change together. Why it exists: a region snapshot is ~40 MiB
/// of highly redundant state (voxel collider shapes, per-voxel chisel material
/// arrays) and a production join ships nine; lz4 packs one 43x in ~7 ms, which
/// is a net win even on loopback (2026-09-20 measurement,
/// `worldgen` example `snapshot_size`).
pub fn encode_packet<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, bincode::Error> {
    let raw = bincode::serialize(value)?;
    if raw.len() > PACKET_COMPRESS_THRESHOLD && raw.len() <= MAX_DECOMPRESSED_PACKET_BYTES {
        let compressed = lz4_flex::block::compress(&raw);
        // Incompressible payloads go out raw: never grow a packet.
        if compressed.len() + 4 < raw.len() {
            let mut frame = Vec::with_capacity(5 + compressed.len());
            frame.push(FRAME_LZ4);
            frame.extend_from_slice(&(raw.len() as u32).to_le_bytes());
            frame.extend_from_slice(&compressed);
            return Ok(frame);
        }
    }
    let mut frame = Vec::with_capacity(1 + raw.len());
    frame.push(FRAME_RAW);
    frame.extend_from_slice(&raw);
    Ok(frame)
}

/// Inverse of [`encode_packet`]. Wire data is untrusted: every malformed input
/// is an error, never a panic or an unbounded allocation.
pub fn decode_packet<T: serde::de::DeserializeOwned>(frame: &[u8]) -> Result<T, PacketError> {
    let (&tag, body) = frame.split_first().ok_or(PacketError::Empty)?;
    match tag {
        FRAME_RAW => bincode::deserialize(body).map_err(PacketError::Decode),
        FRAME_LZ4 => {
            if body.len() < 4 {
                return Err(PacketError::Truncated);
            }
            let (len, block) = body.split_at(4);
            let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
            if len > MAX_DECOMPRESSED_PACKET_BYTES {
                return Err(PacketError::TooLarge(len));
            }
            let raw = lz4_flex::block::decompress(block, len).map_err(PacketError::Decompress)?;
            bincode::deserialize(&raw).map_err(PacketError::Decode)
        }
        other => Err(PacketError::UnknownFormat(other)),
    }
}
