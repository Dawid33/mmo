//! Movement rig shared by the integration tests that drive a player through
//! the real region tick: a hand-built floor, one settled player, key presses
//! and per-tick body reads. Every read of the physics engine's types lives
//! here, so an engine switch migrates one file.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::ops::Range;

use game::{
    chunk_coords_from_translation, Chunk, ChunkCoords, EntityKey, GameEventKind, InputEvent, Pose,
    Key, Region, RegionCoords, Rollback, World, CHUNK_UNITS, PLAYER_SPAWN_X, PLAYER_SPAWN_Z,
};

/// A chunk's edge in world units, as `f32`.
pub const CHUNK_EDGE: f32 = CHUNK_UNITS as f32;

/// Installs the real block properties (`assets/blocks/blocks.ron`) — the
/// one process-global every test in this binary shares. It starts out as the
/// all-solid default with no drops, no hardness and no chest block, and the
/// first test that installs it changes it for every test still running, so a
/// test whose outcome depends on block props (the starting kit, mining drops
/// and hardness, leaves being non-solid) must call this BEFORE it builds any
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

/// Height (units) of the top of the one-block test floor (`Chunk::flat_floor(BLOCK_VOXELS)`
/// under every column: `seam_translate::uniform_floor`, `history_handover::floored_region`).
pub const FLOOR_TOP: f32 = game::BLOCK_VOXELS as f32;
/// The block level a body stands in (and a pile rests in) on the one-block test floor.
pub const STAND: i32 = 1;

/// The y (units) of a player body's centre when it stands on a floor whose top is
/// `top` units high.
pub fn standing_y(top: f32) -> f32 {
    top + game::movement::PLAYER_CAPSULE_HALF_HEIGHT + game::movement::PLAYER_CAPSULE_RADIUS
}

/// A body-centre height a little above the one-block test floor (the spawn
/// clearance's 3.6 units over a standing player): where the tests teleport
/// players and put slimes and bodies down "on the floor".
pub fn above_floor_y() -> f32 {
    standing_y(FLOOR_TOP) + 3.6
}

/// A manager over `spawner` whose never-run regions start from `floor(rc)` (its
/// `RegionGenerator`); far terrain is empty (an inline LOD builder with no coarse data).
pub fn floor_manager<S: game::RegionSpawner>(
    spawner: S,
    floor: impl FnMut(RegionCoords) -> Vec<(game::ColumnCoords, game::Column)> + Send + 'static,
    out: crossbeam::channel::Sender<(Option<game::ClientId>, game::ServerPacket)>,
    region_out: crossbeam::channel::Sender<(RegionCoords, game::RegionOutput)>,
) -> game::WorldManager<S> {
    let (lod_result_send, lod_result_recv) = crossbeam::channel::unbounded();
    let lod_builder: Box<dyn game::LodBuilder> = Box::new(game::InlineLodBuilder {
        coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
        water: Box::new(game::lod::LodWater::dry),
        out: lod_result_send,
    });
    game::WorldManager::new(spawner, Box::new(floor), out, region_out, lod_builder, lod_result_recv)
}

/// The chunk holding the world-unit position `units` (floor by `CHUNK_UNITS`).
pub fn chunk_of(units: game::rapier3d::math::Vector) -> ChunkCoords {
    chunk_coords_from_translation(units)
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

/// Every chunk overlapping the unit rectangle, each a `Chunk::flat_floor(depth)`.
pub fn floor_chunks(x_units: Range<f32>, z_units: Range<f32>, depth: u32) -> Vec<(ChunkCoords, Chunk)> {
    chunks_over(x_units, z_units)
        .into_iter()
        .map(|(x, z)| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(depth)))
        .collect()
}

/// Seeds `region`'s terrain halo (it stands at `here`) as if each of its 8
/// neighbours were floored like [`floor_chunks`] edge to edge: the seeds the
/// manager sends a starting region.
pub fn seed_floor_halo(region: &mut game::Region, here: game::RegionCoords) {
    let size = game::REGION_SIZE;
    let chunks = floor_chunks(0.0..size, 0.0..size, 16);
    for from in here.window_3x3().into_iter().filter(|&rc| rc != here) {
        let seed = game::halo::halo_seed_from_chunks(from, here, chunks.iter().map(|(c, ch)| (*c, ch)))
            .expect("neighbours");
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

/// A world of chunks (y 0) over the world-unit rectangle `x_units` x `z_units`
/// (`chunk(x, z)` gets each overlapping chunk's coordinates), with one player
/// whose controls are on, settled on the ground at the spawn column
/// (128, 128). The rectangle must cover the spawn.
pub fn world_of(
    x_units: Range<f32>,
    z_units: Range<f32>,
    chunk: impl Fn(usize, usize) -> Chunk,
) -> World {
    let chunks = chunks_over(x_units, z_units)
        .into_iter()
        .map(|(x, z)| (ChunkCoords::new(x, 0, z), chunk(x, z)))
        .collect();
    let mut world = World::new();
    world.load(&r0(), Region::from_chunks(r0(), chunks));
    world.handle_region_event(GameEventKind::CreateClient(0), r0()).unwrap();
    world.forget_last_event(&r0());
    key(&mut world, Key::KeyE, true);
    key(&mut world, Key::KeyE, false);
    // The spawn clearance is a few ticks of fall.
    ticks(&mut world, 12);
    assert!(body(&world).grounded, "player must settle on the floor");
    world
}

/// One-voxel terraces, a step down per chunk along +z: the gentle slopes of
/// generated terrain.
pub fn terraces(z_units: Range<f32>) -> World {
    world_of(spawn_x_band(), z_units, |_, z| Chunk::flat_floor(28 - z as u32))
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
pub mod multi_rig;
pub mod rig;
pub mod seam_translate;
pub mod water;
