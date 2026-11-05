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

pub type Tick = usize;
pub type EventId = usize;
pub type EntityId = usize;
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
pub fn rebase_isometry(iso: &Pose, from: RegionCoords, to: RegionCoords) -> Pose {
    let f = from.world_offset();
    let t = to.world_offset();
    let mut out = *iso;
    out.translation.x += f[0] - t[0];
    out.translation.z += f[2] - t[2];
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
    pub isometry: Pose,
    pub linvel: Vector,
    pub collider: ColliderSpec,
    pub has_camera: bool,
    /// Authoritative look scalars (the body quaternion is derived from them
    /// on every look change) — zeroes when `has_camera` is false.
    pub yaw: f32,
    pub pitch: f32,
    /// Movement state (fall speed, flight) — `None` for entities that don't move
    /// under the character controller.
    pub motion: Option<crate::Motion>,
    /// Slime behaviour state — `None` for every other kind.
    pub slime: Option<crate::slime::Slime>,
    /// Animation state of a rigged entity; the receiver rebuilds its bone
    /// colliders from it (`None`: no rig).
    pub animator: Option<crate::anim::Animator>,
    /// An adventurer's walk in progress (`None`: not walking). Its path is
    /// region-local to the sender, so the receiver re-plans it
    /// (`Rollback::resume_walker_safe`).
    pub walker: Option<crate::Walker>,
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
    pub isometry: Pose,
    pub linvel: Vector,
    pub collider: ColliderSpec,
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
    /// Margin mirror refresh from a neighbouring region.
    GhostUpdate(GhostData),
    /// A logical-space UI action from a client (inventory move, toggle, ...).
    UiAction(ClientId, crate::UiAction),
    /// A fortress order from a client (designate, erase, cancel, ...).
    Order(ClientId, crate::Order),
    /// Server-authored: the client's session left this region (Play moved
    /// it elsewhere). Removes its player body and `clients` entry, undoably.
    ClientLeft(ClientId),
    /// Server-authored: like `CreateClient`, but the body lands over the
    /// region-local block column `[bx, bz]` instead of the spawn column.
    CreateClientAt(ClientId, [u32; 2]),
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
    /// The client this event originated from. `Tick` and `Quit` are
    /// server/shared events with no originating client.
    pub fn origin_client(&self) -> Option<ClientId> {
        match self {
            GameEventKind::PlayerInput(id, _)
            | GameEventKind::CreateClient(id)
            | GameEventKind::CreateClientAt(id, _)
            | GameEventKind::RestorePlayer(id, _)
            | GameEventKind::UiAction(id, _)
            | GameEventKind::Order(id, _) => Some(*id),
            GameEventKind::Tick
            | GameEventKind::Quit
            | GameEventKind::EntityArrived(_)
            | GameEventKind::GhostUpdate(_)
            | GameEventKind::ClientLeft(_)
            | GameEventKind::ParkPlayer(_)
            | GameEventKind::SpawnPerson(_)
            | GameEventKind::DespawnPerson(_)
            | GameEventKind::WalkOut(..) => None,
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
