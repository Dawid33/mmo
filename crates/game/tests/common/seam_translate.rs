//! Seam-translation invariance: the acceptance oracle of seam-transparent
//! regions. A [`Scenario`] is played on a uniform floor (or its own ground,
//! [`Scenario::floor`]) three ways — centred
//! in a region, across a straight seam ([`Placement::Seam`]) and through a
//! four-region corner ([`Placement::Corner`]) — and must end in the same
//! outcome in the WORLD frame, measured from the placement's origin.
//!
//! Deviation from the plan's `world_state_hash` (controller ruling R18):
//! bit-exact equality across placements is unachievable for float physics,
//! since rapier rounds by the local coordinate magnitude, which necessarily
//! differs between a centred and a seam placement. A placement's outcome is
//! a [`WorldSnapshot`] instead, compared by [`compare`]: DISCRETE state
//! exactly (the set of `EntityId`s and kinds, each entity's feet tile in
//! world blocks, whether its body sleeps, its walk goal, a client's mode and
//! inventory, changed terrain chunks, item piles, chests) and CONTINUOUS
//! state (world poses, rotations, velocities) within named [`Tolerances`]
//! (a scenario may set its own: [`Scenario::lockstep_tolerances`]).
//! Ownership is checked inside [`snapshot`] instead of compared: every
//! entity must be owned by the region its world position lies in.
//!
//! Scope of the snapshot: entities the scenario plays with. Slimes are left
//! out — the spawner places them by region-local rules, so they do not move
//! with the placement (they are scenery here, not the scenario).
//!
//! Every placement starts in the spawn region (the rig's client spawns
//! there): a scenario starts on the +x/+z side of the origin and moves toward
//! −x/−z through it, so its motion crosses the seam / the corner.
use std::collections::BTreeMap;

use super::rig::Rig;
use game::{
    Chunk, ColumnCoords, Column, EntityId, EntityKind, InlineSchedule, Inventory, ItemStack, PlayMode,
    RegionCoords, StateHash, StateHasher, SubChunk, WorldBlockPos, BLOCK_VOXELS, CHUNK_BLOCKS, REGION_BLOCKS,
    REGION_CHUNKS, REGION_SIZE, SPAWN_REGION,
};

/// A straight seam's axis: `X` is the seam crossed moving along x (x = 0
/// between region −1 and 0), `Z` the one crossed moving along z.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    X,
    Z,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// In the middle of the spawn region, far from every seam.
    Centre,
    /// On the spawn region's −x (`Axis::X`) or −z (`Axis::Z`) seam, halfway
    /// along it.
    Seam(Axis),
    /// On the spawn region's −x/−z corner.
    Corner,
}

/// Every placement, the reference ([`Placement::Centre`]) first.
pub const PLACEMENTS: [Placement; 4] =
    [Placement::Centre, Placement::Seam(Axis::X), Placement::Seam(Axis::Z), Placement::Corner];

/// The block level a body stands on over [`uniform_floor`] (the floor is one
/// block thick at block y 0).
pub const STAND_LEVEL: i32 = 1;

impl Placement {
    /// The world block the scenario is centred on.
    pub fn origin(self) -> WorldBlockPos {
        let mid = REGION_BLOCKS as i64 / 2;
        let (x, z) = match self {
            Placement::Centre => (mid, mid),
            Placement::Seam(Axis::X) => (0, mid),
            Placement::Seam(Axis::Z) => (mid, 0),
            Placement::Corner => (0, 0),
        };
        WorldBlockPos::new(x, STAND_LEVEL, z)
    }

    /// The region past the seam / corner a scenario's motion must reach
    /// (it starts in the spawn region and moves toward −x/−z).
    pub fn beyond(self) -> Option<RegionCoords> {
        match self {
            Placement::Centre => None,
            Placement::Seam(Axis::X) => Some(RegionCoords::new(-1, 0)),
            Placement::Seam(Axis::Z) => Some(RegionCoords::new(0, -1)),
            Placement::Corner => Some(RegionCoords::new(-1, -1)),
        }
    }
}

/// `origin` moved by whole blocks.
pub fn offset(origin: WorldBlockPos, dx: i64, dy: i32, dz: i64) -> WorldBlockPos {
    WorldBlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz)
}

/// `pos` as a tile of the spawn region; panics if it lies outside it (every
/// scenario starts there).
pub fn spawn_tile(pos: WorldBlockPos) -> game::TilePos {
    let (rc, local) = pos.to_local();
    assert_eq!(rc, SPAWN_REGION, "a scenario starts in the spawn region: {pos:?}");
    [local.x, local.y, local.z]
}

/// A one-block floor under every chunk column of every region: the same
/// ground in the world frame wherever a scenario is placed.
pub fn uniform_floor(_rc: RegionCoords) -> Vec<(ColumnCoords, Column)> {
    let air = game::Block::new(game::BlockId::AIR);
    let mut out = Vec::new();
    for x in 0..REGION_CHUNKS {
        for z in 0..REGION_CHUNKS {
            let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(BLOCK_VOXELS as u32)));
            out.push((ColumnCoords::new(x, z), Column::new(0, air, air, vec![sub])));
        }
    }
    out
}

/// A region generator: the ground a scenario is played on.
pub type Floor = Box<dyn FnMut(RegionCoords) -> Vec<(ColumnCoords, Column)> + Send>;

/// A gameplay scenario the harness places three ways. `setup` gets the
/// placement's origin (and may run rounds itself, e.g. to let a body land);
/// then `drive(round)` runs before each of `rounds()` scheduling rounds.
pub trait Scenario {
    fn setup(&self, rig: &mut Rig, origin: WorldBlockPos);
    fn drive(&self, rig: &mut Rig, round: u32);
    fn rounds(&self) -> u32;
    /// The ground recipes the scenario is played on, placed at `origin`
    /// (the regions' genesis columns): [`uniform_floor`] by default. A
    /// scenario with recipe water builds it with
    /// `common::water::floor_with_water`, its boxes relative to `origin`.
    fn floor(&self, _origin: WorldBlockPos) -> Floor {
        Box::new(uniform_floor)
    }
    /// Whether the scenario's motion crosses the seam (`run` then asserts
    /// it reached the region past it). A scenario that reaches across a
    /// seam without moving there (an edit through an intent) says `false`.
    fn crosses(&self) -> bool {
        true
    }
    /// The kind of the one entity whose motion crosses: `None` = the
    /// client's player. Every round it must be owned by at most one region.
    fn crosser(&self) -> Option<EntityKind> {
        None
    }
    /// What a jittery run is held to against the lockstep centred one.
    fn jitter_tolerances(&self) -> Tolerances {
        JITTER_TOLERANCES
    }
    /// What a lockstep placement is held to against the centred one.
    fn lockstep_tolerances(&self) -> Tolerances {
        LOCKSTEP_TOLERANCES
    }
}

/// How closely two snapshots must agree.
#[derive(Debug, Clone, Copy)]
pub struct Tolerances {
    /// Largest world-frame position difference, per axis, in blocks.
    pub pose_blocks: f32,
    /// Largest velocity difference, per axis, in blocks per second.
    pub vel_blocks_per_s: f32,
    /// Feet tiles must be equal (off when lag may legitimately move a body
    /// into the next tile; the pose tolerance still holds it).
    pub tiles: bool,
    /// Largest world-frame rotation difference, degrees. Infinite = not
    /// compared (the walking scenarios: a capsule's facing is not what they
    /// test).
    pub angle_deg: f32,
}

/// Lockstep placements vs the centred one: world poses within this many
/// blocks. Measured ×2: the largest spread was 5.3e-5 blocks (2026-11-06,
/// with the terrain halo; REGION_BLOCKS 64, CHUNK_BLOCKS 16; adventure walk
/// at Corner — the key walk's is 6.3e-6 at Seam(X)/Seam(Z), 3.3e-6 at
/// Corner). Before the halo it was 0.0123 (tolerance 0.025): the body dipped
/// past the seam (no floor there) and rested at another height. The adventure
/// walk at Seam(X)/Seam(Z) was 0.17 until walks planned across seams
/// (`path_plan.rs`, Task 6.5); see `adventure_walk_is_seam_invariant`.
pub const POSE_TOL_BLOCKS: f32 = 1.1e-4;
/// Lockstep velocities within this many blocks per second (bodies are at
/// rest when the snapshot is taken). Measured ×2: 1.5e-4 (2026-11-06,
/// before the halo; with it the adventure walk at a seam was 1.5e-4 lockstep
/// and 2.3e-4 under jitter until walks planned across seams — since then
/// (Task 6.5) the adventure walk is 7.6e-5 in every placement, lockstep and
/// jittery, the key walk 0). Shared with [`JITTER_TOLERANCES`].
pub const VEL_TOL_BLOCKS_PER_S: f32 = 3.1e-4;
pub const LOCKSTEP_TOLERANCES: Tolerances = Tolerances {
    pose_blocks: POSE_TOL_BLOCKS,
    vel_blocks_per_s: VEL_TOL_BLOCKS_PER_S,
    tiles: true,
    angle_deg: f32::INFINITY,
};

/// A jittery run vs the lockstep centred one: late inputs and missed ticks
/// shift WHEN a key takes effect, so a held-key walk may end up to a few
/// ticks of walking off (one tick of walking is ~0.22 blocks). Measured ×2:
/// the key walk's largest spread over 4 jittery seeds was 0.763 blocks
/// (2026-11-06, as above).
pub const JITTER_POSE_TOL_BLOCKS: f32 = 1.53;
pub const JITTER_TOLERANCES: Tolerances =
    Tolerances {
        pose_blocks: JITTER_POSE_TOL_BLOCKS,
        vel_blocks_per_s: VEL_TOL_BLOCKS_PER_S,
        tiles: false,
        angle_deg: f32::INFINITY,
    };

/// Seeds a jittery [`assert_seam_invariant`] sweeps per placement.
pub const JITTER_SEEDS: u64 = 4;
/// Lockstep rounds after the scenario (any schedule turned off) so lagging
/// regions catch up, transfers land and bodies come to rest.
pub const SETTLE_ROUNDS: usize = 20;

/// One entity of a [`WorldSnapshot`]. Positions are in blocks from the
/// placement's origin.
#[derive(Debug, Clone, PartialEq)]
pub struct EntitySnap {
    pub kind: EntityKind,
    /// Feet tile, world blocks relative to the origin.
    pub tile: [i64; 3],
    /// Body centre, blocks relative to the origin.
    pub pos: [f32; 3],
    /// Linear velocity, blocks per second.
    pub vel: [f32; 3],
    /// Body rotation (quaternion x, y, z, w): regions share axes, so the
    /// local rotation is the world one.
    pub rot: [f32; 4],
    /// Whether the body is asleep (compared exactly).
    pub sleeping: bool,
    /// A walk in progress: its goal column relative to the origin, and level.
    pub walk_goal: Option<([i64; 2], Option<i32>)>,
    /// A client's body: its play mode, inventory and cursor stack.
    pub client: Option<(PlayMode, Inventory, Option<ItemStack>)>,
}

/// A placement's outcome in the world frame, relative to its origin.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldSnapshot {
    pub entities: BTreeMap<EntityId, EntitySnap>,
    /// Terrain chunks that differ from the start of the run, by world chunk
    /// coordinates relative to the origin: the new content's digest, `None`
    /// when the chunk is gone.
    pub terrain: BTreeMap<[i64; 3], Option<u32>>,
    /// Item piles by cell, world blocks relative to the origin.
    pub piles: BTreeMap<[i64; 3], Vec<ItemStack>>,
    /// Chests by cell, world blocks relative to the origin: their slots.
    pub containers: BTreeMap<[i64; 3], Vec<Option<ItemStack>>>,
}

impl WorldSnapshot {
    /// The client's body (the only entity with a client).
    pub fn player(&self) -> &EntitySnap {
        let mut players = self.entities.values().filter(|e| e.client.is_some());
        let p = players.next().expect("a player body");
        assert!(players.next().is_none(), "one player body");
        p
    }
}

/// World chunk coordinates (absolute) -> content digest of every terrain
/// chunk in the running regions.
fn terrain_digests(rig: &mut Rig) -> BTreeMap<[i64; 3], u32> {
    let mut out = BTreeMap::new();
    let n = REGION_CHUNKS as i64;
    for rc in rig.manager.running_regions() {
        rig.manager.spawner_mut().with_region(rc, |region| {
            let data = region.data();
            for (&(x, z, y), &key) in data.chunk_index.iter() {
                if let Some(chunk) = data.ecs.chunk.try_get(key) {
                    let mut h = StateHasher::new();
                    chunk.state_hash(&mut h);
                    out.insert([rc.x as i64 * n + x as i64, y as i64, rc.z as i64 * n + z as i64], h.finalize());
                }
            }
        });
    }
    out
}

/// The rig's world relative to `origin`. Panics if an entity is owned by a
/// region its world position does not lie in (a body resting within
/// `FLIP_HYSTERESIS` past a seam would trip this: scenarios end clear of
/// seams).
pub fn snapshot(rig: &mut Rig, origin: WorldBlockPos, baseline: &BTreeMap<[i64; 3], u32>) -> WorldSnapshot {
    let bv = BLOCK_VOXELS as f64;
    let o = [origin.x as f64 * bv, origin.y as f64 * bv, origin.z as f64 * bv];
    let mut entities = BTreeMap::new();
    let mut piles = BTreeMap::new();
    let mut containers = BTreeMap::new();
    let rel = |w: WorldBlockPos| [w.x - origin.x, (w.y - origin.y) as i64, w.z - origin.z];
    for rc in rig.manager.running_regions() {
        rig.manager.spawner_mut().with_region(rc, |region| {
            let data = region.data();
            let client_of: BTreeMap<_, _> = data.player_entites.iter().map(|(c, k)| (*k, *c)).collect();
            for (id, &key) in data.id_index.iter() {
                let kind = (*data.ecs.kind.try_get(key)).expect("an entity with an id has a kind");
                if matches!(kind, EntityKind::Slime { .. }) {
                    continue;
                }
                let handle = (*data.ecs.rigidbody.try_get(key)).expect("an entity with an id has a body");
                let body = data.physics.bodies.get(handle).expect("its body exists");
                let (t, v, q) = (body.translation(), body.linvel(), *body.rotation());
                let world = [
                    rc.x as f64 * REGION_SIZE as f64 + t.x as f64,
                    t.y as f64,
                    rc.z as f64 * REGION_SIZE as f64 + t.z as f64,
                ];
                let owner = RegionCoords::new(
                    (world[0] / REGION_SIZE as f64).floor() as i32,
                    (world[2] / REGION_SIZE as f64).floor() as i32,
                );
                assert_eq!(owner, rc, "{id:?} is owned by {rc:?} but stands in {owner:?} (world {world:?})");
                let tile = game::feet_tile(game::feet_of(t));
                let tile = WorldBlockPos::from_local(rc, game::BlockPos::new(tile[0], tile[1], tile[2]));
                let walk_goal = data.ecs.walker.try_get(key).as_ref().map(|w| {
                    let g = w.goal;
                    ([g.column[0] - origin.x, g.column[1] - origin.z], g.y)
                });
                let client = client_of.get(&key).map(|c| {
                    let c = data.clients.get(c).expect("a player's client");
                    (c.mode, c.inventory.clone(), c.cursor)
                });
                let blocks = |v: f64| (v / bv) as f32;
                let snap = EntitySnap {
                    kind,
                    tile: rel(tile),
                    pos: [blocks(world[0] - o[0]), blocks(world[1] - o[1]), blocks(world[2] - o[2])],
                    vel: [blocks(v.x as f64), blocks(v.y as f64), blocks(v.z as f64)],
                    rot: [q.x, q.y, q.z, q.w],
                    sleeping: body.is_sleeping(),
                    walk_goal,
                    client,
                };
                assert!(entities.insert(*id, snap).is_none(), "{id:?} owned twice");
            }
            for (cell, pile) in data.item_piles.iter() {
                assert_eq!(cell.to_local().0, rc, "{rc:?} holds a pile it does not own: {cell:?}");
                piles.insert(rel(*cell), pile.stacks.clone());
            }
            for (cell, c) in data.containers().iter() {
                assert_eq!(cell.to_local().0, rc, "{rc:?} holds a chest it does not own: {cell:?}");
                containers.insert(rel(*cell), c.slots.clone());
            }
        });
    }
    let chunk = CHUNK_BLOCKS as i64;
    assert!(origin.x % chunk == 0 && origin.z % chunk == 0, "origins sit on the chunk grid");
    let co = [origin.x / chunk, 0, origin.z / chunk];
    let now = terrain_digests(rig);
    let mut terrain = BTreeMap::new();
    for (c, d) in &now {
        if baseline.get(c) != Some(d) {
            terrain.insert([c[0] - co[0], c[1], c[2] - co[2]], Some(*d));
        }
    }
    for c in baseline.keys().filter(|c| !now.contains_key(*c)) {
        terrain.insert([c[0] - co[0], c[1], c[2] - co[2]], None);
    }
    WorldSnapshot { entities, terrain, piles, containers }
}

/// The first difference between `a` and `b` beyond `tol`, readably.
pub fn compare(a: &WorldSnapshot, b: &WorldSnapshot, tol: &Tolerances) -> Result<(), String> {
    let ids = |s: &WorldSnapshot| s.entities.keys().copied().collect::<Vec<_>>();
    if ids(a) != ids(b) {
        return Err(format!("entity sets differ: {:?} vs {:?}", ids(a), ids(b)));
    }
    for (id, x) in &a.entities {
        let y = &b.entities[id];
        if x.kind != y.kind {
            return Err(format!("{id:?}: kind {:?} vs {:?}", x.kind, y.kind));
        }
        if tol.tiles && x.tile != y.tile {
            return Err(format!("{id:?}: feet tile {:?} vs {:?} (relative world blocks)", x.tile, y.tile));
        }
        if x.walk_goal != y.walk_goal {
            return Err(format!("{id:?}: walk goal {:?} vs {:?}", x.walk_goal, y.walk_goal));
        }
        if x.client != y.client {
            return Err(format!("{id:?}: client state {:?} vs {:?}", x.client, y.client));
        }
        if x.sleeping != y.sleeping {
            return Err(format!("{id:?}: asleep {} vs {}", x.sleeping, y.sleeping));
        }
        let turn = angle_deg(x.rot, y.rot);
        if turn > tol.angle_deg {
            return Err(format!(
                "{id:?}: rotation {:?} vs {:?} — {turn}° apart, tolerance {}°",
                x.rot, y.rot, tol.angle_deg
            ));
        }
        let off = max_diff(x.pos, y.pos);
        if off > tol.pose_blocks {
            return Err(format!(
                "{id:?}: position {:?} vs {:?} blocks — {off} apart, tolerance {}",
                x.pos, y.pos, tol.pose_blocks
            ));
        }
        let off = max_diff(x.vel, y.vel);
        if off > tol.vel_blocks_per_s {
            return Err(format!(
                "{id:?}: velocity {:?} vs {:?} blocks/s — {off} apart, tolerance {}",
                x.vel, y.vel, tol.vel_blocks_per_s
            ));
        }
    }
    if a.terrain != b.terrain {
        let first = a.terrain.iter().find(|(k, v)| b.terrain.get(*k) != Some(*v)).map(|(k, _)| *k);
        let first = first.or_else(|| b.terrain.keys().find(|k| !a.terrain.contains_key(*k)).copied());
        return Err(format!(
            "terrain differs first at relative world chunk {first:?}: {:?} vs {:?}",
            first.and_then(|k| a.terrain.get(&k)),
            first.and_then(|k| b.terrain.get(&k))
        ));
    }
    if a.piles != b.piles {
        return Err(format!("item piles differ: {:?} vs {:?}", a.piles, b.piles));
    }
    if a.containers != b.containers {
        return Err(format!("chests differ: {:?} vs {:?}", a.containers, b.containers));
    }
    Ok(())
}

fn max_diff(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| (a[i] - b[i]).abs()).fold(0.0, f32::max)
}

/// The angle (degrees) of the rotation between two unit quaternions.
pub fn angle_deg(a: [f32; 4], b: [f32; 4]) -> f32 {
    let dot = (0..4).map(|i| a[i] as f64 * b[i] as f64).sum::<f64>().abs().min(1.0);
    (2.0 * dot.acos()).to_degrees() as f32
}

/// Largest position, velocity and rotation spread between the entities two
/// snapshots share (blocks, blocks/s, degrees): what the tolerances are
/// measured from.
pub fn spread(a: &WorldSnapshot, b: &WorldSnapshot) -> (f32, f32, f32) {
    let mut out = (0.0f32, 0.0f32, 0.0f32);
    for (id, x) in &a.entities {
        if let Some(y) = b.entities.get(id) {
            out.0 = out.0.max(max_diff(x.pos, y.pos));
            out.1 = out.1.max(max_diff(x.vel, y.vel));
            out.2 = out.2.max(angle_deg(x.rot, y.rot));
        }
    }
    out
}

/// Play `s` at `placement` (under `schedule`, then lockstep to settle) and
/// snapshot the outcome. Panics unless the crossing entity
/// ([`Scenario::crosser`], the player by default) was owned by the region
/// past the placement's seam at some point — the motion must cross it — or
/// by two regions in one round.
pub fn run<S: Scenario>(s: &S, placement: Placement, schedule: Option<InlineSchedule>) -> WorldSnapshot {
    let origin = placement.origin();
    let mut rig = Rig::with_floor(schedule, s.floor(origin));
    rig.join_spawn();
    let baseline = terrain_digests(&mut rig);
    s.setup(&mut rig, origin);
    let mut owners = std::collections::BTreeSet::new();
    let mut track = |rig: &mut Rig| {
        let now = match s.crosser() {
            None => rig.owners(),
            Some(kind) => rig.owners_of(kind),
        };
        assert!(now.len() <= 1, "{placement:?}: the crossing entity is owned by several regions: {now:?}");
        owners.extend(now);
    };
    for round in 0..s.rounds() {
        s.drive(&mut rig, round);
        rig.round();
        track(&mut rig);
    }
    rig.manager.spawner_mut().set_schedule(None);
    for _ in 0..SETTLE_ROUNDS {
        rig.round();
        track(&mut rig);
    }
    if let Some(beyond) = placement.beyond().filter(|_| s.crosses()) {
        assert!(owners.contains(&beyond), "{placement:?}: the scenario never reached {beyond:?} (owners {owners:?})");
    }
    assert_eq!(rig.manager.in_flight_count(), 0, "{placement:?}: a transfer still in flight after settling");
    snapshot(&mut rig, origin, &baseline)
}

/// Seam-translation invariance of `s`. Lockstep (`schedule == None`): every
/// placement matches the centred run within `s.lockstep_tolerances()`
/// ([`LOCKSTEP_TOLERANCES`] by default).
/// Jittery: each placement under `schedule(seed)` for [`JITTER_SEEDS`] seeds
/// keeps the entity set (no loss, no duplicate — `Rig::round` asserts
/// conservation every round) and matches the lockstep centred run within
/// `s.jitter_tolerances()`. Returns the lockstep centred snapshot, so the
/// caller can check the outcome itself.
pub fn assert_seam_invariant<S: Scenario>(s: &S, schedule: Option<fn(u64) -> InlineSchedule>) -> WorldSnapshot {
    let reference = run(s, Placement::Centre, None);
    match schedule {
        None => {
            for placement in &PLACEMENTS[1..] {
                let snap = run(s, *placement, None);
                let (pose, vel, turn) = spread(&reference, &snap);
                eprintln!(
                    "[seam-invariant] lockstep {placement:?}: pose spread {pose} blocks, velocity {vel} blocks/s, rotation {turn}°"
                );
                if let Err(e) = compare(&reference, &snap, &s.lockstep_tolerances()) {
                    panic!("lockstep {placement:?} differs from Centre: {e}");
                }
            }
        }
        Some(schedule) => {
            let tol = s.jitter_tolerances();
            for seed in 0..JITTER_SEEDS {
                for placement in PLACEMENTS {
                    let snap = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run(s, placement, Some(schedule(seed)))
                    }))
                    .unwrap_or_else(|_| panic!("{placement:?} broke under schedule seed {seed}"));
                    let (pose, vel, turn) = spread(&reference, &snap);
                    eprintln!(
                        "[seam-invariant] seed {seed} {placement:?}: pose spread {pose} blocks, velocity {vel} blocks/s, rotation {turn}°"
                    );
                    if let Err(e) = compare(&reference, &snap, &tol) {
                        panic!("{placement:?} under schedule seed {seed} differs from lockstep Centre: {e}");
                    }
                }
            }
        }
    }
    reference
}
