use std::time::Duration;

use crate::input::InputEvent;
use crate::lod::LodKey;
use crate::{
    BLOCK_VOXELS, BRICK_BLOCKS, CHUNK_BLOCKS, Chunk, ChunkCoords, Client, ClientId, Column, ColumnCoords, EntityKey, EntityKind,
    Pose, Rollback,
};
use derive_more::Debug;
use rapier3d::math::Vector;

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

pub type Tick = u64;
pub type EventId = usize;
/// Regions tile the horizontal plane in fixed squares of `REGION_BLOCKS`²
/// blocks — 64 blocks (1024 units, 64 m) at the test default. Signed and
/// unbounded: the world grows in every direction. Sims stay region-local; the
/// world offset exists only at the render boundary (region root Transform)
/// and, later, in handoff rebasing.
///
/// The dial is in BLOCKS, not chunks, so it keeps its meaning when the sim
/// chunk (`CHUNK_BLOCKS`) changes size; [`REGION_CHUNKS`] is derived from it.
///
/// A BUILD-time dial for experiments: `VOXEL_REGION_BLOCKS=128` (or 256) in
/// the environment of the build (`just region_blocks=128 …`) compiles every
/// crate for 2048-unit (4096-unit) regions. It is a constant rather than a runtime setting
/// because client, server and the wasm `LocalServer` must agree on it
/// bit-exactly (region-local coordinates, handoff rebasing, `LodKey` tiling
/// all derive from it) and it sizes const expressions. Two builds with
/// different values cannot talk to each other and NOTHING refuses the
/// connection yet (there is no connect handshake to carry a build tag) —
/// build server and client in one `just` invocation, as the recipes do.
///
/// Legal values are the ones `lod.rs`'s tiling holds for — see
/// [`REGION_BLOCKS_CHOICES`].
pub const REGION_BLOCKS: usize = parse_region_blocks(option_env!("VOXEL_REGION_BLOCKS"));
/// Region edge in sim chunks (derived: the sim-chunk columns of a region).
pub const REGION_CHUNKS: usize = REGION_BLOCKS / CHUNK_BLOCKS;
const _: () = assert!(REGION_BLOCKS % CHUNK_BLOCKS == 0, "a region must be a whole number of sim chunks");
/// Region edge in 32-voxel bricks (the LOD / render-brick grid unit).
pub const REGION_BRICKS: usize = REGION_BLOCKS / BRICK_BLOCKS;
/// Region edge in world units; a power of two, so exact in f32.
pub const REGION_SIZE: f32 = (REGION_BLOCKS * BLOCK_VOXELS) as f32;

/// Region edges this build has been made to work at. The LOD tiling contract
/// (`lod.rs`) only needs a power of two, but each size also needs its own
/// `LOD_RING_TABLE`, and the fixtures and budgets below were checked at
/// exactly these. 512 blocks (the play size) is played as a one-region
/// window; its ring table is a single ring.
pub const REGION_BLOCKS_CHOICES: [usize; 4] = [64, 128, 256, 512];

/// The old chunk-denominated env vars changed meaning when the chunk stopped
/// being a fixed size; refuse them at BUILD time rather than silently ignore.
const _: () = {
    assert!(
        option_env!("VOXEL_REGION_CHUNKS").is_none(),
        "VOXEL_REGION_CHUNKS is gone: use VOXEL_REGION_BLOCKS (blocks: 64, 128, 256 or 512); just region_blocks=N"
    );
    assert!(
        option_env!("VOXEL_CELL_CHUNKS").is_none(),
        "VOXEL_CELL_CHUNKS is gone: cell mode was removed 2026-10-30 (see docs/superpowers/plans/2026-10-30-remove-cell-mode.md)"
    );
    assert!(
        option_env!("VOXEL_CELL_BLOCKS").is_none(),
        "VOXEL_CELL_BLOCKS is gone: cell mode was removed 2026-10-30 (regions are whole; see docs/superpowers/plans/2026-10-30-remove-cell-mode.md)"
    );
};

/// `option_env!` hands over a string; unset or empty means the default.
/// Anything outside [`REGION_BLOCKS_CHOICES`] fails the BUILD, not the run.
const fn parse_region_blocks(var: Option<&str>) -> usize {
    let Some(text) = var else { return 64 };
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return 64;
    }
    let mut value = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i].is_ascii_digit(), "VOXEL_REGION_BLOCKS must be a number");
        value = value * 10 + (bytes[i] - b'0') as usize;
        i += 1;
    }
    let mut i = 0;
    while i < REGION_BLOCKS_CHOICES.len() {
        if REGION_BLOCKS_CHOICES[i] == value {
            return value;
        }
        i += 1;
    }
    panic!("VOXEL_REGION_BLOCKS must be one of REGION_BLOCKS_CHOICES (64, 128, 256, 512)")
}

/// A running region checks its terrain hash this often and pushes a
/// `TerrainChanged` only when it changed — the check cadence IS the
/// refresh throttle.
pub const TERRAIN_HASH_INTERVAL_TICKS: usize = 25;

#[derive(
    Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq, PartialOrd,
    Ord, Hash, state_hash::StateHash,
)]
pub struct RegionCoords {
    pub x: i32,
    pub z: i32,
}

impl RegionCoords {
    pub fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// World-space origin of this region: `(x, 0, z) * REGION_SIZE`. Exactly
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

    /// [`from_world`](Self::from_world) with hysteresis, for a window centre:
    /// `prev` stays the answer while `(x, z)` is inside it or less than
    /// `margin` world units past any of its edges; only then does the centre
    /// move to the region that owns the point. A viewer going back and forth
    /// across a seam therefore keeps one centre (and one window), where the
    /// plain floor division would recentre on every crossing.
    pub fn recentered(prev: Option<RegionCoords>, x: f32, z: f32, margin: f32) -> Self {
        let owner = Self::from_world(x, z);
        let Some(prev) = prev else { return owner };
        let [ox, _, oz] = prev.world_offset();
        let near = |p: f32, o: f32| p >= o - margin && p < o + REGION_SIZE + margin;
        if near(x, ox) && near(z, oz) { prev } else { owner }
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

/// A block in WORLD block coordinates: region-local blocks plus the region's
/// origin (`REGION_BLOCKS` per region). `x`/`z` are `i64` so any region of an
/// `i32` [`RegionCoords`] grid fits; `y` is the region-local block y (regions
/// tile only the plane). The seam-translation harness compares outcomes in
/// this frame.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
    state_hash::StateHash,
)]
pub struct WorldBlockPos {
    pub x: i64,
    pub y: i32,
    pub z: i64,
}

impl WorldBlockPos {
    pub fn new(x: i64, y: i32, z: i64) -> Self {
        Self { x, y, z }
    }

    /// The world block of `pos` in `rc`'s block frame. `pos` need not lie
    /// inside the region: a block past its edge maps to the neighbour's.
    pub fn from_local(rc: RegionCoords, pos: crate::volume::BlockPos) -> Self {
        let n = REGION_BLOCKS as i64;
        Self { x: rc.x as i64 * n + pos.x as i64, y: pos.y, z: rc.z as i64 * n + pos.z as i64 }
    }

    /// The region owning this block and the block in its frame
    /// (`0..REGION_BLOCKS` in x and z; floor division, so world block -1 is
    /// region -1's last block).
    pub fn to_local(self) -> (RegionCoords, crate::volume::BlockPos) {
        let n = REGION_BLOCKS as i64;
        let rc = RegionCoords::new(self.x.div_euclid(n) as i32, self.z.div_euclid(n) as i32);
        (rc, crate::volume::BlockPos::new(self.x.rem_euclid(n) as i32, self.y, self.z.rem_euclid(n) as i32))
    }
}

/// A terrain chunk in WORLD chunk coordinates (`REGION_CHUNKS` per region on
/// x and z; `y` is the region-local chunk y — regions tile only the plane).
/// The chunk counterpart of [`WorldBlockPos`]; keys the terrain halo
/// (`halo.rs`), whose chunks lie in neighbouring regions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub struct WorldChunkPos {
    pub x: i64,
    pub y: i32,
    pub z: i64,
}

impl WorldChunkPos {
    pub fn new(x: i64, y: i32, z: i64) -> Self {
        Self { x, y, z }
    }

    /// The world chunk of local chunk `[x, y, z]` in `rc`'s chunk frame. The
    /// local chunk need not lie inside the region: one past its edge maps to
    /// the neighbour's.
    pub fn from_local(rc: RegionCoords, [x, y, z]: [i32; 3]) -> Self {
        let n = crate::REGION_CHUNKS as i64;
        Self { x: rc.x as i64 * n + x as i64, y, z: rc.z as i64 * n + z as i64 }
    }

    /// The region owning this chunk and the chunk in its frame (floor
    /// division, like [`WorldBlockPos::to_local`]).
    pub fn to_local(self) -> (RegionCoords, ChunkCoords) {
        let n = crate::REGION_CHUNKS as i64;
        let rc = RegionCoords::new(self.x.div_euclid(n) as i32, self.z.div_euclid(n) as i32);
        (rc, ChunkCoords::new(self.x.rem_euclid(n) as usize, self.y, self.z.rem_euclid(n) as usize))
    }

    /// This chunk in `rc`'s chunk frame, which may lie outside it (negative,
    /// or `REGION_CHUNKS` and up).
    pub fn local_in(self, rc: RegionCoords) -> [i32; 3] {
        let n = crate::REGION_CHUNKS as i64;
        [(self.x - rc.x as i64 * n) as i32, self.y, (self.z - rc.z as i64 * n) as i32]
    }
}

/// Ownership flips only this far PAST the boundary; flipping back needs 2x
/// the travel. Kills boundary thrash.
pub const FLIP_HYSTERESIS: f32 = 2.0;

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

/// Neighbour offsets whose entity halo a region-local point is inside:
/// within [`crate::halo::HALO_UNITS`] of an edge ([`crate::halo::HALO_BLOCKS`]
/// blocks), up to 3 at a corner. Order is fixed (x edge, z edge, corner) —
/// determinism requires a stable order.
pub fn halo_offsets(x: f32, z: f32) -> Vec<(i32, i32)> {
    let m = crate::halo::HALO_UNITS;
    let dx = if x < m { -1 } else if x > REGION_SIZE - m { 1 } else { 0 };
    let dz = if z < m { -1 } else if z > REGION_SIZE - m { 1 } else { 0 };
    let mut out = Vec::new();
    if dx != 0 { out.push((dx, 0)); }
    if dz != 0 { out.push((0, dz)); }
    if dx != 0 && dz != 0 { out.push((dx, dz)); }
    out
}

/// Move an isometry from one region's local frame to another's. Offsets are
/// exact multiples of 256 so the delta is exact in f32; both the server
/// relay and the client's predicted synthesis MUST use this one function.
pub fn rebase_isometry(iso: &Pose, from: RegionCoords, to: RegionCoords) -> Pose {
    let f = from.world_offset();
    let t = to.world_offset();
    let mut out = *iso;
    out.translation.x += f[0] - t[0];
    out.translation.z += f[2] - t[2];
    out
}

/// World-unique identity of a crossing-capable entity (player, slime,
/// person): the region that first spawned it plus that region's allocation
/// serial. Allocated from rollback state (`Rollback::alloc_entity_id_safe`),
/// so a client's prediction and the server allocate the same id. Halo
/// mirrors carry their source entity's id.
#[derive(
    Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq, PartialOrd,
    Ord, Hash, state_hash::StateHash,
)]
pub struct EntityId {
    pub birth_region: RegionCoords,
    pub serial: u64,
}

/// Shape spec carried across the transfer seam so the receiving region can
/// rebuild the collider. Players are capsules; new kinds extend this enum.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, state_hash::StateHash)]
pub enum ColliderSpec {
    CapsuleY { half_height: f32, radius: f32 },
}

/// Which rapier body type a transferred body is rebuilt as. Players, slimes
/// and people are all `KinematicPosition` today; dynamic bodies join later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BodyKind {
    Dynamic,
    KinematicPosition,
}

/// Everything about a rapier body that has to survive a seam crossing,
/// read from the body at extraction ([`BodyState::of`]) and rebuilt from on
/// arrival. `pose` is region-local (rebased by [`EntityBundle::rebase`]).
/// `linvel`/`angvel` are only applied to `Dynamic` bodies: a position-based
/// kinematic body's velocity is derived from its motion.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BodyState {
    pub kind: BodyKind,
    pub pose: Pose,
    pub linvel: Vector,
    pub angvel: Vector,
    pub activation: rapier3d::dynamics::RigidBodyActivation,
    pub linear_damping: f32,
    pub angular_damping: f32,
    pub gravity_scale: f32,
    pub ccd: bool,
    pub locked_axes: rapier3d::dynamics::LockedAxes,
    /// Explicit mass properties of a dynamic body (`None`: kinematic).
    pub mass: Option<parry3d::mass_properties::MassProperties>,
}

impl BodyState {
    /// The state of the character bodies the game builds today (and of a
    /// halo mirror's body): kinematic, no gravity of its own, never sleeps.
    pub fn kinematic(pose: Pose) -> Self {
        BodyState {
            kind: BodyKind::KinematicPosition,
            pose,
            linvel: Vector::ZERO,
            angvel: Vector::ZERO,
            activation: rapier3d::dynamics::RigidBodyActivation::cannot_sleep(),
            linear_damping: 0.0,
            angular_damping: 1.0,
            gravity_scale: 0.0,
            ccd: false,
            locked_axes: rapier3d::dynamics::LockedAxes::empty(),
            mass: None,
        }
    }

    /// Read the transferable state off a live body.
    pub fn of(body: &rapier3d::dynamics::RigidBody) -> Self {
        let dynamic = body.body_type() == rapier3d::dynamics::RigidBodyType::Dynamic;
        BodyState {
            kind: if dynamic { BodyKind::Dynamic } else { BodyKind::KinematicPosition },
            pose: *body.position(),
            linvel: body.linvel(),
            angvel: body.angvel(),
            activation: *body.activation(),
            linear_damping: body.linear_damping(),
            angular_damping: body.angular_damping(),
            gravity_scale: body.gravity_scale(),
            ccd: body.is_ccd_enabled(),
            locked_axes: body.locked_axes(),
            mass: dynamic.then(|| body.mass_properties().local_mprops),
        }
    }

    /// A fresh body for entity `e` (its key rides in `user_data`).
    pub fn build(&self, e: EntityKey) -> rapier3d::dynamics::RigidBody {
        use slotmapd::Key;
        let mut b = match self.kind {
            BodyKind::Dynamic => rapier3d::dynamics::RigidBodyBuilder::dynamic(),
            BodyKind::KinematicPosition => rapier3d::dynamics::RigidBodyBuilder::kinematic_position_based(),
        }
        .pose(self.pose)
        .gravity_scale(self.gravity_scale)
        .linear_damping(self.linear_damping)
        .angular_damping(self.angular_damping)
        .locked_axes(self.locked_axes)
        .ccd_enabled(self.ccd)
        .can_sleep(false)
        .user_data(e.data().as_ffi() as u128);
        if self.kind == BodyKind::Dynamic {
            b = b.linvel(self.linvel).angvel(self.angvel);
            if let Some(m) = self.mass {
                b = b.additional_mass_properties(m);
            }
        }
        let mut body = b.build();
        *body.activation_mut() = self.activation;
        body
    }

    /// Re-apply this state to an existing body (a halo mirror upgraded
    /// in place, or a replayed arrival).
    pub fn apply_to(&self, body: &mut rapier3d::dynamics::RigidBody) {
        body.set_position(self.pose, true);
        body.set_linear_damping(self.linear_damping);
        body.set_angular_damping(self.angular_damping);
        body.set_gravity_scale(self.gravity_scale, false);
        body.enable_ccd(self.ccd);
        body.set_locked_axes(self.locked_axes, false);
        if self.kind == BodyKind::Dynamic {
            body.set_linvel(self.linvel, false);
            body.set_angvel(self.angvel, false);
        }
        *body.activation_mut() = self.activation;
    }
}

/// The unit of ownership transfer. Assembled deterministically at the
/// extraction tick; `body.pose` is source-local until [`EntityBundle::rebase`]
/// moves it into the target's frame. `id` is the identity (halo upgrade,
/// arrival idempotency); the region it left is the departure's own framing
/// (`RegionOutput::Departures`). What travels is classified exhaustively in
/// `migrate.rs` (`Carried`, `Client::arrive`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct EntityBundle {
    pub id: EntityId,
    pub kind: EntityKind,
    pub body: BodyState,
    pub collider: ColliderSpec,
    pub carried: crate::Carried,
    pub client: Option<(ClientId, Client)>,
    /// The crossing player's client inputs the source region held for a slot
    /// at or after the departure stamp, with their stamps, in arrival order.
    /// The server's target region queues them again right after applying the
    /// arrival (`RegionRunner`), so they follow the body; the authoritative
    /// `EntityArrived` event carries none. Empty on the client.
    pub pending_inputs: Vec<(crate::GameEventKind, Tick)>,
}

impl EntityBundle {
    /// Move the bundle from region `from`'s local frame into `to`'s: the
    /// body pose, and any carried region-local coordinates.
    pub fn rebase(&mut self, from: RegionCoords, to: RegionCoords) {
        self.body.pose = rebase_isometry(&self.body.pose, from, to);
        if let Some(w) = &mut self.carried.wander {
            w.home[0] += (from.x - to.x) * crate::REGION_BLOCKS as i32;
            w.home[1] += (from.z - to.z) * crate::REGION_BLOCKS as i32;
        }
    }
}

/// One owned entity in a neighbour's entity halo, as of the owner's tick:
/// sent every tick the entity is within [`crate::halo::HALO_BLOCKS`] of the
/// receiver's edge; the receiver upserts its mirror (a kinematic body with
/// this collider, so the two regions' bodies collide).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct HaloEntityDelta {
    pub id: EntityId,
    /// The region owning the real entity (rebase origin, and what the
    /// renderer hides the mirror for) — not identity.
    pub source_region: RegionCoords,
    pub kind: EntityKind,
    pub isometry: Pose,
    pub linvel: Vector,
    pub collider: ColliderSpec,
}

/// The owner's explicit end of a mirror: the entity left the receiver's
/// band, despawned, was parked, crossed elsewhere, or its region stopped.
/// (A crossing INTO the receiver sends none: the arrival takes the mirror
/// over in place.)
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct HaloEntityGone {
    pub id: EntityId,
    /// The owner that sent it; a mirror of another source is left alone.
    pub from: RegionCoords,
}

/// An entity-halo message from an owner to one neighbour, as the owner's
/// tick produced it (`RegionOutput::HaloEntities`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum HaloEntityMsg {
    Update(HaloEntityDelta),
    Gone(HaloEntityGone),
}

impl HaloEntityMsg {
    pub fn id(&self) -> EntityId {
        match self {
            HaloEntityMsg::Update(d) => d.id,
            HaloEntityMsg::Gone(g) => g.id,
        }
    }

    /// Into the receiver's frame (`from` -> `to`): only an update has a pose.
    pub fn rebase(&mut self, from: RegionCoords, to: RegionCoords) {
        if let HaloEntityMsg::Update(d) = self {
            d.isometry = rebase_isometry(&d.isometry, from, to);
        }
    }

    pub fn into_event(self) -> GameEventKind {
        match self {
            HaloEntityMsg::Update(d) => GameEventKind::HaloEntity(d),
            HaloEntityMsg::Gone(g) => GameEventKind::HaloEntityGone(g),
        }
    }
}

pub type RegionId = RegionCoords;
pub type LastGameEventId = usize;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum GameError {
    CrashedOnServerEvent,
    QuitRequested,
    /// The session ended without a requested Quit: the connect gave up, the
    /// connection was lost, or the server closed it. The string says which.
    Disconnected(String),
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
    /// The first packet of every connection: who the player is. The server's
    /// network edge resolves the token (`PlayerStore`) before the session
    /// exists; any other first packet, or an invalid token, is refused
    /// (`ServerPacket::Refused`) and the connection closed. Never reaches the
    /// `WorldManager` from a real edge.
    Hello { token: crate::PlayerToken },
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
    /// Start playing this region (picked on the world map): the server moves
    /// the player's home there or answers `ServerPacket::PlayRefused`.
    PlayRegion(RegionCoords),
    /// The client is leaving this world; treat as disconnected now.
    Leave,
    /// Ask for the people who may embark: answered by `ServerPacket::EmbarkOptions`.
    RequestEmbarkOptions,
    /// Embark: found a fortress with these people in this region; answered by
    /// `ServerPacket::EmbarkResult` (and, on success, `PlayerRegion`).
    Embark(EmbarkRequest),
    /// Stop playing the fortress: the player leaves it, it parks as usual.
    Retire,
    /// Play a retired fortress again; answered by `ServerPacket::EmbarkResult`.
    ReturnTo(crate::history::SiteId),
    /// Answered by `ServerPacket::PersonInfo`; ignored for an unknown person.
    RequestPersonInfo(crate::history::PersonId),
    /// One page of the world's legends; answered by `ServerPacket::Legends`.
    RequestLegends(crate::history::LegendsQuery),
    /// Start an adventure at a populated site; answered by `ServerPacket::AdventureResult`
    /// (and, on success, `PlayerRegion`).
    StartAdventure(crate::history::SiteId),
    /// End the adventure: the player goes back to fortress mode at the spawn region.
    EndAdventure,
}

/// The player's embark: a civ, people of that civ's sites, and the region to found in.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmbarkRequest {
    pub civ: crate::history::CivId,
    pub people: Vec<crate::history::PersonId>,
    pub region: RegionCoords,
}

/// The fortress a client plays: its site and the region it stands in (the
/// client centres its simulated neighbourhood on that region in both play
/// modes, so it needs it without waiting for the next `Sites` broadcast).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HeldFortress {
    pub site: crate::history::SiteId,
    pub region: RegionCoords,
}

/// A site as the world map shows it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SiteMarker {
    pub site: crate::history::SiteId,
    pub civ: crate::history::CivId,
    pub region: RegionCoords,
    pub name: String,
    /// Living members.
    pub population: u32,
    pub abandoned: bool,
    /// Player-founded and not abandoned: a player may return to it.
    pub returnable: bool,
}

/// A civilisation's name, as the embark tabs show it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CivMarker {
    pub civ: crate::history::CivId,
    pub name: String,
}

/// Who may embark: per civ, per site, its people free to leave.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmbarkOptions {
    pub civs: Vec<CivOption>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CivOption {
    pub civ: crate::history::CivId,
    pub name: String,
    pub sites: Vec<SiteOption>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SiteOption {
    pub site: crate::history::SiteId,
    pub name: String,
    pub region: RegionCoords,
    pub people: Vec<PersonOption>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PersonOption {
    pub person: crate::history::PersonId,
    pub name: String,
    pub age_years: u32,
    pub sex: crate::history::Sex,
    pub spouse: Option<crate::history::PersonId>,
}

/// One person, for the inspector: names rather than ids.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PersonInfo {
    pub person: crate::history::PersonId,
    pub name: String,
    /// Now, or at death.
    pub age_years: u32,
    pub alive: bool,
    /// The site they are a citizen of.
    pub site: Option<String>,
    pub spouse: Option<String>,
    pub parents: Vec<String>,
    pub children: Vec<String>,
}

/// One LOD response: (key-local cell group, RLE grid) pairs.
pub type LodPayload = Vec<([i32; 3], crate::lod::PackedLodGrid)>;

type CurrentTickRate = u64;

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub enum ServerPacket {
    /// The connection was refused (the first packet was not a valid `Hello`);
    /// the server closes it after this. The string says why.
    Refused(String),
    SyncClock(RegionId, CurrentTickRate, Tick, Duration),
    /// Game event that was proccessed by the server.
    GameEvent(GameEvent),
    /// A `GameEvent` plus the region's [`crate::state_hash`] right after it,
    /// sent for the `Tick`s on the server's `VOXEL_STATE_HASH_INTERVAL`
    /// cadence. Receiving one is the client's cue to compare its own state
    /// once it has confirmed that event.
    /// Fields: the event, the region tick right after it, the hash.
    HashedGameEvent(GameEvent, Tick, u32),
    // TODO: Create player serverside and add player id here to let client know
    // who he is.
    Region(RegionId, Rollback),
    PlayerRegion(Option<RegionId>, ClientId),
    /// Streamed LOD cells for one key (initial send AND refresh —
    /// replace-on-re-receipt).
    LodData(LodKey, LodPayload),
    /// The world map: per-region tile summaries of the forged world.
    WorldMap(crate::world_map::WorldMap),
    /// A `ClientPacket::PlayRegion` for this region was refused.
    PlayRefused(RegionCoords, crate::world_map::PlayRefusal),
    /// Every site of the history: on connect, and when the set or a population bucket
    /// changes (at most once per calendar day).
    Sites(Vec<SiteMarker>),
    /// The world calendar: on connect and once per calendar day.
    Calendar(crate::history::CalendarTick),
    /// A history log line about the site of the client's home region.
    Notice(String),
    EmbarkOptions(EmbarkOptions),
    /// The answer to `ClientPacket::Embark` and `ClientPacket::ReturnTo`: the site played.
    EmbarkResult(Result<crate::history::SiteId, crate::history::EmbarkRefusal>),
    PersonInfo(PersonInfo),
    /// The site this client plays (`None`: none): on connect and after Embark / Return / Retire.
    Fortress(Option<HeldFortress>),
    /// Every civilisation's name, by id: on connect, next to `Sites`.
    Civs(Vec<CivMarker>),
    /// The answer to `ClientPacket::RequestLegends`.
    Legends(crate::history::LegendsPage),
    /// The answer to `ClientPacket::StartAdventure`: the site the adventure starts at.
    AdventureResult(Result<crate::history::SiteId, crate::history::AdventureRefusal>),
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
    /// Entity halo: a neighbour's entity within `HALO_BLOCKS` of the shared
    /// edge, as of the neighbour's tick (upsert of its mirror here).
    HaloEntity(HaloEntityDelta),
    /// Entity halo: the neighbour ends a mirror (see [`HaloEntityGone`]).
    HaloEntityGone(HaloEntityGone),
    /// Server-authored: region `.0` was lost without stopping (its thread
    /// died), so it can send no `HaloEntityGone`: every entity mirror of it
    /// here ends.
    HaloSourceGone(RegionCoords),
    /// Terrain halo refresh from a neighbouring region (`halo.rs`): its
    /// chunks within `HALO_BLOCKS` of the shared edge, changed (or, to seed
    /// the halo, all of them).
    HaloUpdate(crate::HaloUpdate),
    /// A logical-space UI action from a client (inventory move, toggle, ...).
    UiAction(ClientId, crate::UiAction),
    /// A fortress order from a client (designate, erase, cancel, ...).
    Order(ClientId, crate::Order),
    /// Server-authored: the client's session left this region with nothing
    /// kept (a session with no player identity ended; moves park instead).
    /// Removes its player body and `clients` entry, undoably.
    ClientLeft(ClientId),
    /// Server-authored: like `CreateClient`, but the body lands over the
    /// region-local block column `[bx, bz]` instead of the spawn column. A
    /// player moved from another region (Play, embark, adventure start /
    /// end) carries its `Client` — inventory, cursor, mode — taken from its
    /// park there; `None` = a player new to the world (the starting kit).
    CreateClientAt(ClientId, [u32; 2], Option<Box<crate::Client>>),
    /// Server-authored: give a history person a body here (and optionally
    /// start a walk). Never accepted from a client.
    SpawnPerson(PersonSpawn),
    /// Server-authored: remove a person's body from this region.
    DespawnPerson(crate::history::PersonId),
    /// Server-authored: walk a person out of this region toward a neighbour.
    WalkOut(crate::history::PersonId, RegionCoords),
    /// Server-authored: the player's session ended. Removes its body and
    /// `clients` entry like [`GameEventKind::ClientLeft`]; the server keeps
    /// what it removed as the player's [`crate::PlayerRecord`].
    ParkPlayer(ClientId),
    /// Server-authored: a returning player's `Client` and body are put back
    /// from its record (the body at the record's pose, lifted onto the nearest
    /// standable tile if that is now inside terrain).
    RestorePlayer(ClientId, Box<crate::PlayerRecord>),
    /// An intent or its reply from another region (`intents.rs`), relayed
    /// as a stamped transfer: joins this region's queue, applied / delivered
    /// at the opening of its due tick. Never accepted from a client.
    Intent(crate::IntentMsg),
}

/// Where a spawned person appears in a region.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum SpawnAt {
    /// A region-local block column.
    Column([u32; 2]),
    /// Next to that person's body here (within 2 blocks); the region
    /// landing if they have no body here.
    NearPerson(crate::history::PersonId),
    /// On the region edge facing `from`, `along_per_mille` along it
    /// (0 = the lower x / z end), at the nearest standable column.
    Edge { from: RegionCoords, along_per_mille: u32 },
}

/// A [`GameEventKind::SpawnPerson`] payload.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PersonSpawn {
    pub person: crate::history::PersonId,
    pub at: SpawnAt,
    pub walk_to: Option<crate::Goal>,
}

impl GameEventKind {
    /// The client whose input this is: the kinds a client sends and the
    /// manager routes to its home (`PlayerInput`, `UiAction`, `Order`). They
    /// follow the client's body across a seam.
    pub fn input_client(&self) -> Option<ClientId> {
        match self {
            GameEventKind::PlayerInput(id, _) | GameEventKind::UiAction(id, _) | GameEventKind::Order(id, _) => {
                Some(*id)
            }
            _ => None,
        }
    }

    /// The client this event originated from. `Tick` and `Quit` are
    /// server/shared events with no originating client.
    pub fn origin_client(&self) -> Option<ClientId> {
        match self {
            GameEventKind::PlayerInput(id, _)
            | GameEventKind::CreateClient(id)
            | GameEventKind::CreateClientAt(id, ..)
            | GameEventKind::RestorePlayer(id, _)
            | GameEventKind::UiAction(id, _)
            | GameEventKind::Order(id, _) => Some(*id),
            GameEventKind::Tick
            | GameEventKind::Quit
            | GameEventKind::EntityArrived(_)
            | GameEventKind::HaloEntity(_)
            | GameEventKind::HaloEntityGone(_)
            | GameEventKind::HaloSourceGone(_)
            | GameEventKind::HaloUpdate(_)
            | GameEventKind::ClientLeft(_)
            | GameEventKind::ParkPlayer(_)
            | GameEventKind::SpawnPerson(_)
            | GameEventKind::DespawnPerson(_)
            | GameEventKind::WalkOut(..)
            | GameEventKind::Intent(_) => None,
        }
    }

    /// Reconcile's prediction-removal matcher. Transfers match on IDENTITY
    /// (`EntityId`), not full equality: the predicted and
    /// authoritative copies differ in pose whenever the client's extraction
    /// tick differs from the server's, and the rollback-replace path must
    /// still find and remove the prediction or it re-applies forever.
    pub fn matches_prediction(&self, other: &Self) -> bool {
        use GameEventKind::*;
        match (self, other) {
            (EntityArrived(a), EntityArrived(b)) => a.id == b.id,
            (HaloEntity(a), HaloEntity(b)) => a.id == b.id,
            (HaloEntityGone(a), HaloEntityGone(b)) => a == b,
            (HaloUpdate(a), HaloUpdate(b)) => a.same_chunks(b),
            _ => self == other,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::volume::BlockPos;

    #[test]
    fn world_block_pos_round_trips_across_negative_regions() {
        let n = REGION_BLOCKS as i32;
        for rc in [RegionCoords::new(0, 0), RegionCoords::new(-1, 0), RegionCoords::new(0, -1), RegionCoords::new(-3, 2)] {
            for local in [BlockPos::new(0, 5, 0), BlockPos::new(n - 1, -7, 0), BlockPos::new(3, 0, n - 1)] {
                let w = WorldBlockPos::from_local(rc, local);
                assert_eq!(w.to_local(), (rc, local), "{rc:?} {local:?} -> {w:?}");
            }
        }
        // Floor division: the block just west/north of the origin is region -1's last.
        let (rc, local) = WorldBlockPos::new(-1, 0, -1).to_local();
        assert_eq!((rc, local), (RegionCoords::new(-1, -1), BlockPos::new(n - 1, 0, n - 1)));
        // A local block past the edge lands in the neighbour.
        let w = WorldBlockPos::from_local(RegionCoords::new(-1, 0), BlockPos::new(n, 0, -1));
        assert_eq!(w.to_local(), (RegionCoords::new(0, -1), BlockPos::new(0, 0, n - 1)));
    }

    #[test]
    fn legal_region_sizes_parse() {
        assert_eq!(parse_region_blocks(None), 64);
        assert_eq!(parse_region_blocks(Some("512")), 512);
    }

    #[test]
    #[should_panic(expected = "REGION_BLOCKS_CHOICES")]
    fn region_sizes_above_512_are_refused() {
        parse_region_blocks(Some("1024"));
    }

    #[test]
    fn the_packet_cap_never_overflows() {
        assert_eq!(max_decompressed_packet_bytes(64), 256 * 1024 * 1024);
        assert_eq!(max_decompressed_packet_bytes(65536), 2 * 1024 * 1024 * 1024);
    }

    use super::*;

    /// The window centre moves only `margin` past its region, both ways, and
    /// a far jump lands straight on the owner.
    #[test]
    fn recentered_holds_the_centre_inside_the_margin() {
        let m = 256.0;
        let a = RegionCoords::new(0, 0);
        assert_eq!(RegionCoords::recentered(None, -1.0, 5.0, m), RegionCoords::new(-1, 0));
        // Just across the seam into (0,-1), and back: still a.
        assert_eq!(RegionCoords::recentered(Some(a), 10.0, -1.0, m), a);
        assert_eq!(RegionCoords::recentered(Some(a), 10.0, -m + 1.0, m), a);
        // Past the margin: the owner.
        assert_eq!(RegionCoords::recentered(Some(a), 10.0, -m - 1.0, m), RegionCoords::new(0, -1));
        // Far side, too, and on x.
        assert_eq!(RegionCoords::recentered(Some(a), REGION_SIZE + m - 1.0, 5.0, m), a);
        assert_eq!(RegionCoords::recentered(Some(a), REGION_SIZE + m, 5.0, m), RegionCoords::new(1, 0));
        // Teleport.
        assert_eq!(RegionCoords::recentered(Some(a), 10.0 * REGION_SIZE, 0.0, m), RegionCoords::new(10, 0));
        // Zero margin is plain from_world.
        assert_eq!(RegionCoords::recentered(Some(a), 5.0, -0.5, 0.0), RegionCoords::new(0, -1));
    }

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
/// before anything is allocated. A region snapshot is ~40 MiB at the default
/// 64 blocks and scales with region AREA (~160 MiB at 128, ~640 MiB at
/// 256), so the cap scales with it: 256 MiB × (REGION_BLOCKS / 64)², held
/// under the u32 length prefix (and wasm32's address space) at 2 GiB.
pub const MAX_DECOMPRESSED_PACKET_BYTES: usize = max_decompressed_packet_bytes(REGION_BLOCKS);

/// Computed in u64: at large region sizes the unclamped product overflows
/// wasm32's usize (it did at compile time from 256 up).
pub const fn max_decompressed_packet_bytes(region_blocks: usize) -> usize {
    let r = (region_blocks / 64) as u64;
    let scaled = 256 * 1024 * 1024 * r * r;
    let cap: u64 = 2 * 1024 * 1024 * 1024;
    let v = if scaled > cap { cap } else { scaled };
    if v > usize::MAX as u64 { usize::MAX } else { v as usize }
}

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
