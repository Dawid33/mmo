//! A body walking over flat terrain must move level and at constant speed,
//! whatever the terrain is assembled from. Terrain colliders are many boxes
//! laid flush — voxels inside a chunk collider, chunk colliders side by side,
//! block-scale next to chisel-scale next to collapsed full-chunk cuboids — and
//! every seam between two boxes used to read as an edge: the character
//! controller slid down its leaning normal into the floor, fell through the
//! skin for a tick, then tripped on the next box and popped up ungrounded.
mod common;

use common::{
    approach_seam, body, key, r0, seam_row, seam_z, spawn_x_band, spawn_z_walk, terraces, ticks, world_of,
    CHUNK_EDGE,
};
use game::movement::{TICK_DT, UNITS_PER_METER, WALK_SPEED};
use game::{
    Block, BlockId, BlockIndex, ChiselData, Chunk, ChunkCoords, GameEventKind, Key, Region,
    World, BLOCK_VOXELS, CHUNK_BLOCKS, CHUNK_UNITS,
};

const SOLID: BlockId = BlockId(1);
/// Distance covered by one tick of walking.
const STEP: f32 = WALK_SPEED * UNITS_PER_METER * TICK_DT;
/// Slack on per-tick motion: the controller re-settles onto its skin by a
/// fraction of a millimetre now and then.
const LEVEL: f32 = 0.005;

/// The z extent (world units) a walk of that many ticks from the spawn needs.
/// Voxel-scale colliders are slow to build, so worlds are no bigger than the
/// walk.
fn rows_for(walk_ticks: usize) -> std::ops::Range<f32> {
    spawn_z_walk(walk_ticks as f32 * STEP)
}

/// A world one chunk either side of the spawn column in x, over `z_units`
/// along z: all a walk along z from the spawn touches.
fn world(z_units: std::ops::Range<f32>, chunk: impl Fn(usize, usize) -> Chunk) -> World {
    world_of(spawn_x_band(), z_units, chunk)
}

/// Walks +z (S at yaw 0) for `n` ticks after sidestepping `side` ticks along
/// +x, asserting level, straight, constant-speed, grounded motion throughout.
/// `side = 0` walks the x = 128 seam between two chunk columns.
fn assert_level_walk(world: &mut World, side: usize, n: usize) {
    // Up to three chunk borders; a walk too short for that crosses as many as
    // it is long.
    assert_level_walk_across(world, side, n, 3.min((n as f32 * STEP / CHUNK_EDGE) as usize));
}

/// [`assert_level_walk`] that must cross at least `borders` chunk borders.
fn assert_level_walk_across(world: &mut World, side: usize, n: usize, borders: usize) {
    key(world, Key::KeyD, true);
    ticks(world, side);
    key(world, Key::KeyD, false);
    ticks(world, 2);
    key(world, Key::KeyS, true);
    ticks(world, 1);
    let start = body(world);
    let mut prev = start;
    for tick in 0..n {
        ticks(world, 1);
        let now = body(world);
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "lost the ground at {at}");
        assert!((now.y - prev.y).abs() < LEVEL, "bobbed at {at}");
        assert!((now.z - prev.z - STEP).abs() < LEVEL, "speed hitched at {at}");
        assert!((now.x - start.x).abs() < LEVEL, "drifted sideways at {at}");
        prev = now;
    }
    let crossed = (prev.z / CHUNK_EDGE).floor() - (start.z / CHUNK_EDGE).floor();
    assert!(crossed >= borders as f32, "the walk must cross chunk borders: {start:?} -> {prev:?}");
}

/// A floor of `BLOCK_VOXELS` height built from chiselled blocks: the same
/// surface as `flat_floor(16)`, but in the voxel-scale collider. Colliders
/// take the coarsest solid cell of a chisel, so a full chisel would collapse
/// to a block cell; one hidden void at the bottom of each upper 8³ octant
/// keeps the octants holding the walking surface at voxel scale.
fn chiselled_floor() -> Chunk {
    let mut chunk = Chunk::flat_floor(BLOCK_VOXELS as u32);
    let half = BLOCK_VOXELS / 2;
    for bx in 0..2 {
        for bz in 0..2 {
            let voided = ChiselData::from_fn(vec![SOLID], |vx, vy, vz| {
                let void = vy == half && vx % half == 0 && vz % half == 0;
                (!void).then_some(0)
            });
            chunk.chisel.insert(BlockIndex::from_xyz(bx, 0, bz), voided);
        }
    }
    chunk
}

/// Ticks of a walk long enough to also catch what only shows up now and then.
const LONG: usize = 80;
/// Ticks of a walk down three terraces.
const SHORT: usize = 30;

#[test]
fn block_scale_floor_is_level_across_chunk_borders() {
    for side in [0, 4] {
        assert_level_walk(&mut world(rows_for(LONG), |_, _| Chunk::flat_floor(16)), side, LONG);
    }
}

#[test]
fn voxel_scale_floor_is_level_across_a_chunk_border() {
    // Small and short: voxel-scale colliders are slow to build and sweep.
    // A chunk either side of the seam ahead of the spawn (z 128..192 at
    // 2-block chunks), the walk starting just short of it.
    let mut world = world(seam_z() - 32.0..seam_z() + 32.0, |_, _| Chunk::flat_floor(8));
    approach_seam(&mut world, 32.0);
    assert_level_walk_across(&mut world, 0, 12, 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn voxel_scale_floor_is_level_on_a_long_walk() {
    for side in [0, 4] {
        assert_level_walk(&mut world(rows_for(LONG), |_, _| Chunk::flat_floor(8)), side, LONG);
    }
}

#[test]
fn diagonal_walk_is_level() {
    let x = common::spawn_x_band();
    let mut world = world_of(x.start..x.end + CHUNK_EDGE, rows_for(20), |_, _| Chunk::flat_floor(16));
    key(&mut world, Key::KeyS, true);
    key(&mut world, Key::KeyD, true);
    ticks(&mut world, 1);
    let mut prev = body(&world);
    for tick in 0..20 {
        ticks(&mut world, 1);
        let now = body(&world);
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "lost the ground at {at}");
        assert!((now.y - prev.y).abs() < LEVEL, "bobbed at {at}");
        let moved = ((now.x - prev.x).powi(2) + (now.z - prev.z).powi(2)).sqrt();
        assert!((moved - STEP).abs() < LEVEL, "speed hitched at {at}");
        prev = now;
    }
}

/// Block-scale and voxel-scale colliders in alternate rows.
fn mixed_scales(_: usize, z: usize) -> Chunk {
    if z % 2 == 0 { Chunk::flat_floor(16) } else { chiselled_floor() }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn block_scale_to_voxel_scale_seam_is_level() {
    for side in [0, 4] {
        assert_level_walk(&mut world(rows_for(LONG), mixed_scales), side, LONG);
    }
}

#[test]
fn collapsed_cuboid_to_voxels_seam_is_level() {
    // Full chunks collapse to one cuboid collider; knocking out a top block
    // away from the walking line keeps the neighbour a voxels collider with
    // the same surface.
    let mixed = |x: usize, z: usize| {
        let mut chunk = Chunk::uniform(Block::new(SOLID));
        if z % 2 == 1 {
            // The top block farthest from the walking line (x = spawn).
            let centre = |bx: usize| (x * CHUNK_UNITS + bx * BLOCK_VOXELS + BLOCK_VOXELS / 2) as f32;
            let far = (0..CHUNK_BLOCKS)
                .max_by(|&a, &b| {
                    let off = |bx| (centre(bx) - game::PLAYER_SPAWN_X).abs();
                    off(a).total_cmp(&off(b))
                })
                .unwrap();
            chunk.set_block(far, CHUNK_BLOCKS - 1, 0, Block::new(BlockId::AIR));
        }
        chunk
    };
    assert_level_walk(&mut world(rows_for(LONG), mixed), 0, LONG);
}

#[test]
fn low_step_at_a_chunk_border_is_walked_up() {
    // 4 units (0.25 m) up at the seam ahead of the spawn (z = 160 at 2-block
    // chunks), under the 0.6 m step height.
    let stepped = |_, z: usize| Chunk::flat_floor(if z >= seam_row() { 20 } else { 16 });
    let mut world = world(rows_for(25), stepped);
    approach_seam(&mut world, 32.0);
    let low = body(&world);
    key(&mut world, Key::KeyS, true);
    ticks(&mut world, 25);
    let high = body(&world);
    assert!(high.z > seam_z() + 40.0, "the step must not stop the walk: {high:?}");
    assert!(high.grounded);
    assert!((high.y - low.y - 4.0).abs() < 0.05, "must stand on the step: {low:?} -> {high:?}");
}

#[test]
fn full_block_wall_at_a_chunk_border_blocks() {
    // 16 units (1 m) up at the seam ahead of the spawn: above the step height.
    let walled = |_, z: usize| Chunk::flat_floor(if z >= seam_row() { 32 } else { 16 });
    let mut world = world(rows_for(25), walled);
    approach_seam(&mut world, 32.0);
    let before = body(&world);
    key(&mut world, Key::KeyS, true);
    ticks(&mut world, 25);
    let after = body(&world);
    assert!(after.z < seam_z() - 6.0, "walked into the wall: {after:?}");
    assert!(after.z > seam_z() - 7.0, "stopped short of the wall: {after:?}");
    assert!(after.grounded);
    assert!((after.y - before.y).abs() < LEVEL, "the wall must not lift the body: {after:?}");
}

/// A floor one block deep and, from the chunk seam ahead of the spawn on
/// (z = 160 at 2-block chunks), a one-block-thick ceiling whose underside is
/// `clear_blocks` blocks above the floor.
fn tunnel(clear_blocks: usize) -> World {
    let floor = BLOCK_VOXELS;
    let ceiling = floor + clear_blocks * BLOCK_VOXELS;
    // Chunk layers up to the one holding the ceiling's top face.
    let top_layer = (ceiling + BLOCK_VOXELS - 1) / CHUNK_UNITS;
    let mut chunks = Vec::new();
    for (x, z) in common::chunks_over(spawn_x_band(), rows_for(25)) {
        for cy in 0..=top_layer {
            let mut chunk = if cy == 0 { Chunk::flat_floor(floor as u32) } else { Chunk::default() };
            if z >= seam_row() {
                for bx in 0..CHUNK_BLOCKS {
                    for by in 0..CHUNK_BLOCKS {
                        for bz in 0..CHUNK_BLOCKS {
                            let y0 = cy * CHUNK_UNITS + by * BLOCK_VOXELS;
                            if y0 >= ceiling && y0 < ceiling + BLOCK_VOXELS {
                                chunk.set_block(bx, by, bz, Block::new(SOLID));
                            }
                        }
                    }
                }
            }
            chunks.push((ChunkCoords::new(x, cy as i32, z), chunk));
        }
    }
    let mut world = World::new();
    world.load(&r0(), Region::from_chunks(r0(), chunks));
    world.handle_region_event(GameEventKind::CreateClient(0), r0()).unwrap();
    world.forget_last_event(&r0());
    key(&mut world, Key::KeyE, true);
    key(&mut world, Key::KeyE, false);
    ticks(&mut world, 12);
    assert!(body(&world).grounded, "player must settle on the floor");
    approach_seam(&mut world, 32.0);
    world
}

#[test]
fn player_walks_through_a_two_block_tunnel() {
    // The body fills two blocks but must still fit a two-block-high gap.
    let mut world = tunnel(2);
    let before = body(&world);
    key(&mut world, Key::KeyS, true);
    ticks(&mut world, 25);
    let after = body(&world);
    assert!(after.z > seam_z() + 15.0, "a two-block tunnel must not stop the body: {after:?}");
    assert!(after.grounded);
    assert!((after.y - before.y).abs() < LEVEL, "the ceiling must not push the body: {after:?}");
}

#[test]
fn player_does_not_fit_a_one_block_tunnel() {
    let mut world = tunnel(1);
    key(&mut world, Key::KeyS, true);
    ticks(&mut world, 25);
    let after = body(&world);
    assert!(after.z < seam_z(), "a one-block gap is lower than the body: {after:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walking_down_terraces_stays_on_the_ground() {
    // The slide along level ground ends with no downward part, which used to
    // keep snap-to-ground from engaging: a tick of hovering past each edge,
    // ungrounded, then a drop.
    // Three seams ahead: the walk is as long as it takes to pass them.
    let end_z = seam_z() + 2.0 * CHUNK_EDGE;
    let walk = ((end_z - game::PLAYER_SPAWN_Z) / STEP).ceil() as usize + 2;
    let mut world = terraces(rows_for(walk));
    key(&mut world, Key::KeyS, true);
    let mut prev = body(&world);
    for tick in 0..walk {
        ticks(&mut world, 1);
        let now = body(&world);
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "left the ground at {at}");
        assert!(now.y - prev.y < LEVEL, "rose on the way down at {at}");
        assert!(prev.y - now.y < 1.1, "fell more than a terrace at {at}");
        prev = now;
    }
    assert!(prev.z > end_z, "must have walked down three terraces: {prev:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walking_up_terraces_stays_on_the_ground() {
    // Sliding over a terrace's edge overshoots it; the body must be put back
    // down on the terrace within the tick rather than fall for the next one.
    // Start a few ticks short of a chunk border, going back toward -z: the
    // spawn itself at 2-block chunks (z = 128 is a border), 20 units into the
    // second chunk row otherwise.
    let z0 = game::PLAYER_SPAWN_Z.max(CHUNK_EDGE + 20.0);
    let border = ((z0 / CHUNK_EDGE).ceil() - 1.0) * CHUNK_EDGE;
    let mut world = terraces((border - CHUNK_EDGE).max(0.0)..z0 + CHUNK_EDGE);
    if z0 != game::PLAYER_SPAWN_Z {
        let b = body(&world);
        common::teleport(&mut world, b.x, b.y, z0);
        ticks(&mut world, 4);
    }
    let start = body(&world);
    key(&mut world, Key::KeyW, true);
    let mut prev = start;
    for tick in 0..15 {
        ticks(&mut world, 1);
        let now = body(&world);
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "left the ground at {at}");
        assert!(now.y - prev.y > -LEVEL, "dropped on the way up at {at}");
        prev = now;
    }
    // Standing on the terrace it has reached: floor + capsule half-extent + skin.
    let floor = 28.0 - (prev.z / CHUNK_EDGE).floor();
    assert!(prev.z < border && prev.y > start.y + 0.9, "must have climbed: {start:?} -> {prev:?}");
    let half_extent = game::movement::PLAYER_CAPSULE_HALF_HEIGHT + game::movement::PLAYER_CAPSULE_RADIUS;
    assert!((prev.y - (floor + half_extent + game::movement::CHARACTER_SKIN)).abs() < 0.05, "must stand on the terrace: {prev:?}");
}
