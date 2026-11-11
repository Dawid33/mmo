//! Movement rig shared by the integration tests that drive a player through
//! the real region tick: a hand-built stud floor, one settled player, key presses
//! and per-tick body reads. Every read of the physics engine's types lives
//! here, so an engine switch migrates one file.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::ops::Range;

use game::{
    ColumnCoords, EntityKey, GameEventKind, InputEvent, Key, Pose, Region,
    RegionCoords, Rollback, StudColumnGround, StudProfile, StudRegionSeed, World, CHUNK_UNITS, PLAYER_SPAWN_X,
    PLAYER_SPAWN_Z, REGION_CHUNKS, STUD_CHUNK_STUDS, STUD_UNITS, TILE_PLATES, TILE_STUDS,
};

/// A chunk's edge in world units, as `f32`.
pub const CHUNK_EDGE: f32 = CHUNK_UNITS as f32;

/// A tile's width (x and z) in sim units: `game::tile_width_units()`.
pub const TILE_W: f32 = (TILE_STUDS * STUD_UNITS) as f32;
/// A tile's height in sim units: `game::tile_height_units()`.
pub fn tile_h() -> f32 {
    game::plate_units(TILE_PLATES as i32)
}

/// Surface plate of the shared flat test ground: the top of tile level
/// `STAND - 1`, so a body stands (and a pile rests) in level [`STAND`].
pub const FLOOR_PLATES: i32 = 30;
/// The tile level a body stands in on the flat test ground.
pub const STAND: i32 = FLOOR_PLATES / TILE_PLATES as i32;
/// Bedrock of the shared test ground: far below anything a test digs.
pub const FLOOR_BEDROCK: i32 = -2000;
/// Grass over dirt: the soil of the shared test ground.
pub const SOIL: StudProfile =
    StudProfile { cover: game::MAT_GRASS, cover_plates: 1, soil: game::MAT_DIRT, soil_plates: 5 };

/// Height (sim units) of the top of a floor whose surface plate is `plates`.
pub fn floor_y(plates: i32) -> f32 {
    game::plate_units(plates)
}

/// The y (sim units) of a `BodyScale::STUD` body's centre when it stands on a
/// smooth floor at `plates` (studded ground adds up to `game::stud_cap_units()`).
pub fn standing_y(plates: i32) -> f32 {
    {
        let s = game::movement::BodyScale::STUD;
        floor_y(plates) + s.capsule_half_height() + s.capsule_radius()
    }
}

/// A body-centre height a little above the shared flat floor (the spawn
/// clearance's 3.6 units over a standing `BodyScale::STUD` body): where the
/// tests teleport players and put slimes and bodies down "on the floor".
pub fn above_floor_y() -> f32 {
    standing_y(FLOOR_PLATES) + 3.6
}

/// A flat, dry stud column recipe with its surface at `plates` ([`SOIL`] over stone).
pub fn stud_flat(plates: i32) -> StudColumnGround {
    StudColumnGround::flat(plates, SOIL, game::MAT_STONE, FLOOR_BEDROCK)
}

/// A dry stud column recipe whose surface at stud column `(x, z)` of the chunk
/// column is `surface(x, z)` plates ([`SOIL`] over stone).
pub fn stud_ground(surface: impl Fn(usize, usize) -> i32) -> StudColumnGround {
    let n = STUD_CHUNK_STUDS;
    let tops: Vec<i32> = (0..n * n).map(|i| surface(i % n, i / n)).collect();
    StudColumnGround::new(
        tops.into(),
        vec![game::STUD_NO_WATER; n * n].into(),
        vec![0; n * n].into(),
        vec![SOIL],
        vec![0; n * n].into(),
        vec![(i32::MAX, game::MAT_STONE)],
        FLOOR_BEDROCK,
    )
    .expect("a recipe of known materials")
}

/// A stud region seed: `ground(x, z)` for every chunk column overlapping the
/// world-unit rectangle, no features.
pub fn stud_seed_over(
    x_units: Range<f32>,
    z_units: Range<f32>,
    ground: impl Fn(usize, usize) -> StudColumnGround,
) -> StudRegionSeed {
    let ground = chunks_over(x_units, z_units).into_iter().map(|(x, z)| (ColumnCoords::new(x, z), ground(x, z))).collect();
    StudRegionSeed { ground, features: Vec::new() }
}

/// Flat ground at `plates` under every chunk column overlapping the rectangle.
pub fn floor_seed(x_units: Range<f32>, z_units: Range<f32>, plates: i32) -> StudRegionSeed {
    stud_seed_over(x_units, z_units, |_, _| stud_flat(plates))
}

/// Flat ground at [`FLOOR_PLATES`] under every chunk column of a region: the
/// same ground in the world frame in every region.
pub fn uniform_seed() -> StudRegionSeed {
    let n = REGION_CHUNKS;
    let ground = (0..n).flat_map(|x| (0..n).map(move |z| (ColumnCoords::new(x, z), stud_flat(FLOOR_PLATES)))).collect();
    StudRegionSeed { ground, features: Vec::new() }
}

/// Stud chunk coordinates (`y` = stud layer).
pub fn sc(x: usize, y: i32, z: usize) -> game::StudChunkCoords {
    game::StudChunkCoords { x, y, z }
}

/// A stud chunk holding one stone filler slab `plates` thick over its whole
/// footprint: the hand-made chunk of the tests that only need "a terrain
/// chunk entity" (`Rollback::create_stud_chunk_safe`, `seed_stud_genesis`).
pub fn slab_chunk(plates: u8) -> game::StudChunk {
    let n = STUD_CHUNK_STUDS as u8;
    let slab = game::StudBrick::filler([0, 0, 0], [n, plates, n], game::MAT_STONE);
    game::StudChunk::new([slab]).expect("a filler slab is a valid chunk")
}

/// A server-side stud region (no render sink) started from `seed`; its bodies
/// are `BodyScale::STUD`. An empty seed gives a region without terrain.
pub fn region_of(rc: RegionCoords, seed: StudRegionSeed) -> Region {
    let mut region = Region::new(Rollback::new(None), None, rc, None);
    region.seed_stud_world(seed);
    region
}

/// A stud region without any terrain.
pub fn empty_region(rc: RegionCoords) -> Region {
    region_of(rc, StudRegionSeed::default())
}

/// A stud region floored flat at `plates` over the unit rectangle.
pub fn floor_region(rc: RegionCoords, x_units: Range<f32>, z_units: Range<f32>, plates: i32) -> Region {
    region_of(rc, floor_seed(x_units, z_units, plates))
}

/// Seeds terrain into a bare sim (`Rollback`) as `Region::seed_stud_world` does.
pub fn seed_studs(r: &mut Rollback, seed: StudRegionSeed) {
    let extra = seed.feature_chunks();
    r.seed_stud_ground_genesis(seed.ground, extra);
    r.set_body_scale_raw(game::movement::BodyScale::STUD);
}

/// A terrain snapshot holding the stud edge `edge` (what the world store's
/// `load_terrain` or a parked region hands a neighbour's halo).
pub fn stud_terrain_snapshot(edge: game::StudEdge) -> game::TerrainSnapshot {
    game::TerrainSnapshot { stud: edge }
}

/// The halo seed a region at `to` gets from a never-run neighbour at `from`
/// whose genesis is `seed` (what the manager sends a starting region).
pub fn stud_halo_seed(from: RegionCoords, to: RegionCoords, seed: &StudRegionSeed) -> Option<game::halo::HaloUpdate> {
    game::halo::halo_seed_from_snapshot(from, to, &stud_terrain_snapshot(game::StudEdge::from_seed(seed)))
}

/// Installs the real block properties (`assets/blocks/blocks.ron`) — the
/// one process-global every test in this binary shares. It starts out as the
/// all-solid default with no drops, no hardness and no chest block, and the
/// first test that installs it changes it for every test still running, so a
/// test whose outcome depends on block props (the starting kit, mining drops
/// and hardness, material names) must call this BEFORE it builds any
/// state — else it sees the default in one run and the real props in the
/// next. Idempotent: every installer in this binary installs the same props.
pub fn install_real_block_props() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let reg = game::BlockRegistry::from_ron(include_str!("../../../../assets/blocks/blocks.ron"))
            .expect("assets/blocks/blocks.ron parses");
        game::install_block_props(reg.props());
    });
}

/// The far-terrain builder of the test worlds: the inline stud one, every
/// tile flat at [`FLOOR_PLATES`].
pub fn stud_lod_builder() -> Box<dyn game::LodBuilder> {
    Box::new(game::InlineStudLodBuilder::new(Box::new(|key| {
        let side = game::StudLodTile::side_for(key);
        let n = usize::from(side) + 1;
        let sample = game::StudLodSample::of_column(FLOOR_PLATES, game::MAT_GRASS, None);
        game::StudLodTile::encode(side, &vec![sample; n * n])
    })))
}

/// A manager of a stud world over `spawner` (give it its genesis with
/// `InlineSpawner::with_stud_genesis`): far terrain comes from [`stud_lod_builder`].
pub fn stud_manager<S: game::RegionSpawner>(
    spawner: S,
    out: crossbeam::channel::Sender<(Option<game::ClientId>, game::ServerPacket)>,
    region_out: crossbeam::channel::Sender<(RegionCoords, game::RegionOutput)>,
) -> game::WorldManager<S> {
    game::WorldManager::new(spawner, out, region_out, stud_lod_builder())
}

/// A far-terrain key the stud path serves for region `rc` at this region size.
pub fn stud_lod_key(rc: RegionCoords) -> game::lod::LodKey {
    game::lod::LodKey::for_region(game::stud_ring_table(game::lod::LOD_RING_TABLE)[0].1, rc)
}

/// Body-centre position (region-local sim units) of a `BodyScale::STUD` body
/// standing in tile `at` on a studded floor at the tile's bottom: the column's
/// centre, its feet on the stud caps.
pub fn body_on_tile(at: game::TilePos) -> game::rapier3d::math::Vector {
    let (x, z) = game::column_centre([at[0], at[2]]);
    let s = game::movement::BodyScale::STUD;
    let feet = game::level_bottom_units(at[1]) + game::stud_cap_units();
    game::rapier3d::math::Vector::new(x, feet + s.capsule_half_height() + s.capsule_radius(), z)
}

/// The feet of a `BodyScale::STUD` body whose centre is at `t`.
pub fn feet_of(t: game::rapier3d::math::Vector) -> game::rapier3d::math::Vector {
    let s = game::movement::BodyScale::STUD;
    t - game::rapier3d::math::Vector::Y * (s.capsule_half_height() + s.capsule_radius())
}

/// The tile a `BodyScale::STUD` body whose centre is at `t` stands in.
pub fn feet_tile(t: game::rapier3d::math::Vector) -> game::TilePos {
    game::feet_tile(feet_of(t))
}

/// Whether tile `p` (region-local; own ∪ stud halo) holds nothing.
pub fn tile_open(d: &Rollback, p: game::TilePos) -> bool {
    game::TileSource::tile_fill(d, p) == game::TileFill::Open
}

/// Whether world tile `p` holds anything, as `d`'s region sees it (own ∪ stud halo).
pub fn world_tile_filled(d: &Rollback, p: game::WorldBlockPos) -> bool {
    !tile_open(d, game::local_cell(*d.region, p))
}

/// Fills tile `at` of a region with stone bricks, undo-safely (a built block).
pub fn fill_tile(d: &mut Rollback, at: game::TilePos) -> u32 {
    let (lo, hi) = game::stud_tile_box(at).expect("a tile of the region");
    d.fill_stud_box_safe(lo, hi, game::MAT_STONE)
}

/// Carves tile `at` out of a region, undo-safely (a dug block). Returns
/// whether anything was removed.
pub fn carve_tile(d: &mut Rollback, at: game::TilePos) -> bool {
    let (lo, hi) = game::stud_tile_box(at).expect("a tile of the region");
    !d.carve_stud_safe(lo, hi).is_empty()
}

/// An inline spawner whose never-run regions start from `floor(rc)`.
pub fn stud_spawner(floor: impl Fn(RegionCoords) -> StudRegionSeed + Send + Sync + 'static) -> game::InlineSpawner {
    game::InlineSpawner::default().with_stud_genesis(std::sync::Arc::new(floor))
}

/// Chunk index along one horizontal axis of the world-unit coordinate `u`.
pub fn chunk_index_of(u: f32) -> usize {
    (u / CHUNK_EDGE).floor() as usize
}

/// Chunk coordinates whose footprint overlaps the half-open world-unit
/// rectangle (negative edges clamp to 0). Fixtures state their terrain in
/// units through this, never in chunk indices, so they build the same
/// physical world at any `CHUNK_BLOCKS`.
pub fn chunks_over(x_units: Range<f32>, z_units: Range<f32>) -> Vec<(usize, usize)> {
    let span = |r: Range<f32>| {
        let lo = chunk_index_of(r.start.max(0.0));
        let hi = (r.end / CHUNK_EDGE).ceil() as usize;
        lo..hi
    };
    let zs = span(z_units);
    span(x_units).flat_map(|x| zs.clone().map(move |z| (x, z))).collect()
}

/// Seeds `region`'s stud halo (it stands at `here`) as if each of its 8
/// neighbours were floored like [`uniform_seed`] edge to edge: the seeds the
/// manager sends a starting region.
pub fn seed_floor_halo(region: &mut game::Region, here: game::RegionCoords) {
    let neighbour = uniform_seed();
    for from in here.window_3x3().into_iter().filter(|&rc| rc != here) {
        let seed = stud_halo_seed(from, here, &neighbour).expect("neighbours");
        region.handle_event(game::GameEventKind::HaloUpdate(seed)).unwrap();
        region.forget_last_event();
    }
}

/// The first chunk row past the spawn's: where a chunk seam lies ahead of the
/// player walking +z / +x from the spawn column (row 5, z = 160, at 2-block
/// chunks).
pub fn seam_row() -> usize {
    chunk_index_of(PLAYER_SPAWN_Z) + 1
}
/// World z of [`seam_row`]'s border.
pub fn seam_z() -> f32 {
    seam_row() as f32 * CHUNK_EDGE
}
/// World x of the first chunk border ahead of the spawn along +x.
pub fn seam_x() -> f32 {
    (chunk_index_of(PLAYER_SPAWN_X) + 1) as f32 * CHUNK_EDGE
}

/// Unit range one chunk either side of the spawn column in x (`3..=4` at
/// 2-block chunks): all a walk straight along z touches.
pub fn spawn_x_band() -> Range<f32> {
    PLAYER_SPAWN_X - CHUNK_EDGE..PLAYER_SPAWN_X + CHUNK_EDGE
}
/// Unit range from one chunk behind the spawn to `ahead` units past it,
/// padded by two chunks — the z extent of a walk of `ahead` units.
pub fn spawn_z_walk(ahead: f32) -> Range<f32> {
    let end = (chunk_index_of(PLAYER_SPAWN_Z) + 2 + (ahead / CHUNK_EDGE) as usize) as f32 * CHUNK_EDGE;
    PLAYER_SPAWN_Z - CHUNK_EDGE..end
}

pub fn r0() -> RegionCoords {
    RegionCoords::new(0, 0)
}

/// A stud world over the world-unit rectangle `x_units` x `z_units`
/// (`ground(x, z)` gives each overlapping chunk column's recipe), with one
/// player whose controls are on, settled on the ground at the spawn column
/// (128, 128). The rectangle must cover the spawn.
pub fn world_of(
    x_units: Range<f32>,
    z_units: Range<f32>,
    ground: impl Fn(usize, usize) -> StudColumnGround,
) -> World {
    let mut world = World::new();
    world.load(&r0(), region_of(r0(), stud_seed_over(x_units, z_units, ground)));
    world.handle_region_event(GameEventKind::CreateClient(0), r0()).unwrap();
    world.forget_last_event(&r0());
    key(&mut world, Key::KeyE, true);
    key(&mut world, Key::KeyE, false);
    // The spawn clearance is a few ticks of fall.
    ticks(&mut world, 12);
    assert!(body(&world).grounded, "player must settle on the floor");
    world
}

/// One-plate terraces, a step down per chunk along +z: the gentle slopes of
/// generated terrain.
pub fn terraces(z_units: Range<f32>) -> World {
    world_of(spawn_x_band(), z_units, |_, z| stud_flat(FLOOR_PLATES + 12 - z as i32))
}

pub fn key(world: &mut World, key: Key, pressed: bool) {
    world
        .handle_region_event(GameEventKind::PlayerInput(0, InputEvent::Key { key, pressed }), r0())
        .unwrap();
    world.forget_last_event(&r0());
}

pub fn ticks(world: &mut World, n: usize) {
    let mut results = BTreeMap::new();
    for _ in 0..n {
        world.progress_world_one_tick(&mut results);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Body {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub grounded: bool,
    pub vel_y: f32,
}

/// Position and motion of any entity with a body.
pub fn entity_body(data: &Rollback, e: EntityKey) -> Body {
    let t = data.physics.bodies.get(*data.ecs.rigidbody.get(e)).unwrap().translation();
    let m = data.ecs.motion.try_get(e).unwrap_or_default();
    Body { x: t.x, y: t.y, z: t.z, grounded: m.grounded, vel_y: m.vel_y }
}

/// The player's body.
pub fn body(world: &World) -> Body {
    let data = world.data(&r0());
    entity_body(data, *data.player_entites.get(&0).unwrap())
}

/// Puts the player's body at `(x, y, z)`, undo-safely.
pub fn teleport(world: &mut World, x: f32, y: f32, z: f32) {
    world.regions.get_mut(&r0()).unwrap().with_data(|d| {
        let e = *d.data.player_entites.get(&0).unwrap();
        d.set_body_pose_safe(e, Pose::translation(x, y, z));
    });
}

/// Moves a settled player forward along +z so it stands `gap` units short of
/// the chunk seam at [`seam_z`]. Where the spawn already is within `gap` of the
/// seam (2-block chunks: 32 away) the player stays put.
pub fn approach_seam(world: &mut World, gap: f32) {
    let z = seam_z() - gap;
    let now = body(world);
    if z > now.z {
        teleport(world, now.x, now.y, z);
        ticks(world, 2);
    }
}

/// Spawns a slime of `size` with its body centre at `pos`, undo-safely.
/// `tick` seeds its roll sequence.
pub fn spawn_slime(region: &mut Region, pos: [f32; 3], size: u8, yaw: f32, tick: u64) -> EntityKey {
    let mut slime = None;
    region.with_data(|d| {
        let pos = game::parry::math::Vector::new(pos[0], pos[1], pos[2]);
        slime = Some(d.spawn_slime_safe(pos, size, yaw.into(), tick));
    });
    slime.unwrap()
}

/// `(fixed bodies, fixed bodies with mass)`. Terrain bodies are fixed and
/// must skip collider mass properties: for `Voxels` those go through
/// `SymmetricEigen`'s std `acos`/`cos`, which is not bit-identical across
/// targets.
pub fn fixed_body_mass_count(data: &Rollback) -> (usize, usize) {
    let fixed: Vec<_> = data.physics.bodies.iter().filter(|(_, b)| b.is_fixed()).collect();
    let massive = fixed.iter().filter(|(_, b)| b.mass() != 0.0).count();
    (fixed.len(), massive)
}

pub mod cut_store;
pub mod rig;
pub mod seam_translate;
pub mod water;

/// Brute-force reference of re-bricking on exposure (`game::stud_rebrick`) for one
/// chunk of `layer`: every diggable filler cell with an open face neighbour — inside
/// the chunk by its occupancy, outside it by `outside_open` (chunk cell coordinates
/// past the chunk's box) — has its slot turned into bricks.
#[allow(dead_code)]
pub fn rebrick_reference(chunk: &mut game::StudChunk, layer: i32, outside_open: impl Fn([i32; 3]) -> bool) {
    let (n, h) = (STUD_CHUNK_STUDS as i32, game::STUD_CHUNK_PLATES as i32);
    let occ = chunk.occupancy();
    let mut slots = std::collections::BTreeSet::new();
    let fillers: Vec<game::StudBrick> = chunk
        .bricks()
        .filter(|b| matches!(b.shape, game::StudShape::Filler { .. }))
        .filter(|b| game::stud_material(b.material).is_some_and(|m| m.hardness != u16::MAX))
        .copied()
        .collect();
    for b in fillers {
        for cell in b.cells() {
            let c = cell.map(i32::from);
            let open = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]].iter().any(|d| {
                let q = [c[0] + d[0], c[1] + d[1], c[2] + d[2]];
                let inside = (0..n).contains(&q[0]) && (0..h).contains(&q[1]) && (0..n).contains(&q[2]);
                if inside { !occ.get(q) } else { outside_open(q) }
            });
            if open {
                slots.insert(game::rebrick_slot(layer, cell));
            }
        }
    }
    for (lo, hi) in slots {
        chunk.rebrick_box(lo, hi);
    }
}
