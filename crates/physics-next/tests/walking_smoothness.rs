//! Port of `crates/game/tests/walking_smoothness.rs` to the fork (Task 3.4):
//! the real seam pin. A body walking over flat terrain must move level and at
//! constant speed, whatever the terrain is assembled from — voxels inside a
//! chunk collider, chunk colliders side by side, block-scale next to
//! chisel-scale next to collapsed full-chunk cuboids. Every seam between two
//! boxes used to read as an edge: the character controller slid down its
//! leaning normal into the floor, fell through the skin for a tick, then
//! tripped on the next box and popped up ungrounded. Terraces pin the KCC
//! snap gate. The rig (`common::Walker`) is the game's own movement code
//! driving the fork. The game file's four `slow` cases run untagged here:
//! the whole file takes ~0.1 s on the fork (test profile), and two of them are
//! the snap gate's pins.
mod common;

use common::{Body, CHUNK, Walker};
use game::movement::{CHARACTER_SKIN, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, TICK_DT, UNITS_PER_METER, WALK_SPEED};
use game::{BLOCK_VOXELS, Block, BlockId, BlockIndex, ChiselData, Chunk};

const SOLID: BlockId = BlockId(1);
/// Distance covered by one tick of walking.
const STEP: f32 = WALK_SPEED * UNITS_PER_METER * TICK_DT;
/// Slack on per-tick motion: the controller re-settles onto its skin by a
/// fraction of a millimetre now and then.
const LEVEL: f32 = 0.005;

/// Chunk rows along z a walk of that many ticks from the spawn needs.
fn rows_for(walk_ticks: usize) -> std::ops::RangeInclusive<i32> {
    3..=(4 + (walk_ticks as f32 * STEP / CHUNK) as i32 + 1)
}

/// Chunk columns x 3..=4 over `rows` along z: all a walk along z from the spawn touches.
fn world(rows: std::ops::RangeInclusive<i32>, chunk: impl Fn(i32, i32) -> Chunk) -> Walker {
    Walker::on(3..=4, rows, chunk)
}

fn chunk_row(z: f32) -> f32 {
    (z / CHUNK).floor()
}

/// Walks +z (S at yaw 0) for `n` ticks after sidestepping `side` ticks along
/// +x, asserting level, straight, constant-speed, grounded motion throughout.
/// `side = 0` walks the x = 128 seam between two chunk columns.
fn assert_level_walk(w: &mut Walker, side: usize, n: usize) {
    assert_level_walk_across(w, side, n, 3);
}

/// [`assert_level_walk`] that must cross at least `borders` chunk borders.
fn assert_level_walk_across(w: &mut Walker, side: usize, n: usize, borders: usize) {
    w.keys.right = true;
    w.ticks(side);
    w.keys.right = false;
    w.ticks(2);
    w.keys.back = true;
    w.ticks(1);
    let start = w.body();
    let mut prev = start;
    for tick in 0..n {
        w.tick();
        let now = w.body();
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "lost the ground at {at}");
        assert!((now.y - prev.y).abs() < LEVEL, "bobbed at {at}");
        assert!((now.z - prev.z - STEP).abs() < LEVEL, "speed hitched at {at}");
        assert!((now.x - start.x).abs() < LEVEL, "drifted sideways at {at}");
        prev = now;
    }
    let crossed = chunk_row(prev.z) - chunk_row(start.z);
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
    assert_level_walk_across(&mut world(4..=5, |_, _| Chunk::flat_floor(8)), 0, 12, 1);
}

#[test]
fn voxel_scale_floor_is_level_on_a_long_walk() {
    for side in [0, 4] {
        assert_level_walk(&mut world(rows_for(LONG), |_, _| Chunk::flat_floor(8)), side, LONG);
    }
}

#[test]
fn diagonal_walk_is_level() {
    let mut w = Walker::on(3..=5, rows_for(20), |_, _| Chunk::flat_floor(16));
    w.keys.back = true;
    w.keys.right = true;
    w.ticks(1);
    let mut prev = w.body();
    for tick in 0..20 {
        w.tick();
        let now = w.body();
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "lost the ground at {at}");
        assert!((now.y - prev.y).abs() < LEVEL, "bobbed at {at}");
        let moved = ((now.x - prev.x).powi(2) + (now.z - prev.z).powi(2)).sqrt();
        assert!((moved - STEP).abs() < LEVEL, "speed hitched at {at}");
        prev = now;
    }
}

/// Block-scale and voxel-scale colliders in alternate rows.
fn mixed_scales(_: i32, z: i32) -> Chunk {
    if z % 2 == 0 { Chunk::flat_floor(16) } else { chiselled_floor() }
}

#[test]
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
    let mixed = |x: i32, z: i32| {
        let mut chunk = Chunk::uniform(Block::new(SOLID));
        if z % 2 == 1 {
            let far = if x <= 3 { 0 } else { 1 };
            chunk.set_block(far, 1, 0, Block::new(BlockId::AIR));
        }
        chunk
    };
    assert_level_walk(&mut world(rows_for(LONG), mixed), 0, LONG);
}

#[test]
fn low_step_at_a_chunk_border_is_walked_up() {
    // 4 units (0.25 m) up at z = 160, under the 0.6 m step height.
    let mut w = world(rows_for(25), |_, z| Chunk::flat_floor(if z >= 5 { 20 } else { 16 }));
    let low = w.body();
    w.keys.back = true;
    w.ticks(25);
    let high = w.body();
    assert!(high.z > 200.0, "the step must not stop the walk: {high:?}");
    assert!(high.grounded);
    assert!((high.y - low.y - 4.0).abs() < 0.05, "must stand on the step: {low:?} -> {high:?}");
}

#[test]
fn full_block_wall_at_a_chunk_border_blocks() {
    // 16 units (1 m) up at z = 160: above the step height.
    let mut w = world(rows_for(25), |_, z| Chunk::flat_floor(if z >= 5 { 32 } else { 16 }));
    let before = w.body();
    w.keys.back = true;
    w.ticks(25);
    let after = w.body();
    let wall = 5.0 * CHUNK;
    assert!(after.z < wall - 6.0, "walked into the wall: {after:?}");
    assert!(after.z > wall - 7.0, "stopped short of the wall: {after:?}");
    assert!(after.grounded);
    assert!((after.y - before.y).abs() < LEVEL, "the wall must not lift the body: {after:?}");
}

/// A floor one block deep and, from row 5 on, a one-block-thick ceiling
/// whose underside is `clear_blocks` blocks above the floor.
fn tunnel(clear_blocks: usize) -> Walker {
    let floor = BLOCK_VOXELS;
    let ceiling = floor + clear_blocks * BLOCK_VOXELS;
    let mut chunks = Vec::new();
    for x in 3..=4 {
        for z in rows_for(25) {
            for cy in 0..=1usize {
                let mut chunk = if cy == 0 { Chunk::flat_floor(floor as u32) } else { Chunk::default() };
                if z >= 5 {
                    for bx in 0..2 {
                        for by in 0..2 {
                            for bz in 0..2 {
                                let y0 = cy * (CHUNK as usize) + by * BLOCK_VOXELS;
                                if y0 >= ceiling && y0 < ceiling + BLOCK_VOXELS {
                                    chunk.set_block(bx, by, bz, Block::new(SOLID));
                                }
                            }
                        }
                    }
                }
                chunks.push(((x, cy as i32, z), chunk));
            }
        }
    }
    Walker::new(chunks)
}

#[test]
fn player_walks_through_a_two_block_tunnel() {
    // The body fills two blocks but must still fit a two-block-high gap.
    let mut w = tunnel(2);
    let before = w.body();
    w.keys.back = true;
    w.ticks(25);
    let after = w.body();
    assert!(after.z > 175.0, "a two-block tunnel must not stop the body: {after:?}");
    assert!(after.grounded);
    assert!((after.y - before.y).abs() < LEVEL, "the ceiling must not push the body: {after:?}");
}

#[test]
fn player_does_not_fit_a_one_block_tunnel() {
    let mut w = tunnel(1);
    w.keys.back = true;
    w.ticks(25);
    let after = w.body();
    assert!(after.z < 5.0 * CHUNK, "a one-block gap is lower than the body: {after:?}");
}

/// Not in the game's file — pins the snap gate's other half (`// mmo-fork:`
/// `heading_down` in rapier-next's controller): snap-to-ground only follows
/// a body that means to go down. A jump under a low ceiling rises less than
/// `GROUND_SNAP` before bumping its head; snapping it would pull it straight
/// back onto the floor, cancelling the jump.
#[test]
fn a_jump_under_a_low_ceiling_leaves_the_ground() {
    let mut w = tunnel(2);
    w.keys.back = true;
    w.ticks(15);
    w.keys.back = false;
    w.ticks(2);
    let before = w.body();
    assert!(before.z > 5.0 * CHUNK + 10.0 && before.grounded, "must stand in the tunnel: {before:?}");
    w.keys.jump_pressed = true;
    w.keys.jump_held = true;
    w.tick();
    w.keys.jump_held = false;
    let after = w.body();
    assert!(after.y > before.y + 1.0 && !after.grounded, "the jump must rise to the ceiling: {before:?} -> {after:?}");
}

/// One-voxel terraces, a step down per chunk along +z: the gentle slopes of
/// generated terrain.
fn terraces(rows: std::ops::RangeInclusive<i32>) -> Walker {
    Walker::on(3..=4, rows, |_, z| Chunk::flat_floor(28 - z as u32))
}

#[test]
fn walking_down_terraces_stays_on_the_ground() {
    // The slide along level ground ends with no downward part, which used to
    // keep snap-to-ground from engaging: a tick of hovering past each edge,
    // ungrounded, then a drop.
    let mut w = terraces(rows_for(SHORT));
    w.keys.back = true;
    let mut prev = w.body();
    for tick in 0..SHORT {
        w.tick();
        let now = w.body();
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "left the ground at {at}");
        assert!(now.y - prev.y < LEVEL, "rose on the way down at {at}");
        assert!(prev.y - now.y < 1.1, "fell more than a terrace at {at}");
        prev = now;
    }
    assert!(prev.z > 7.0 * CHUNK, "must have walked down three terraces: {prev:?}");
}

#[test]
fn walking_up_terraces_stays_on_the_ground() {
    // Sliding over a terrace's edge overshoots it; the body must be put back
    // down on the terrace within the tick rather than fall for the next one.
    let mut w = terraces(2..=4);
    let start = w.body();
    w.keys.forward = true;
    let mut prev: Body = start;
    for tick in 0..15 {
        w.tick();
        let now = w.body();
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "left the ground at {at}");
        assert!(now.y - prev.y > -LEVEL, "dropped on the way up at {at}");
        prev = now;
    }
    // Standing on the terrace it has reached: floor + capsule half-extent + skin.
    let floor = 28.0 - chunk_row(prev.z);
    assert!(prev.z < 3.0 * CHUNK && prev.y > start.y + 0.9, "must have climbed: {start:?} -> {prev:?}");
    let half_extent = PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS;
    assert!((prev.y - (floor + half_extent + CHARACTER_SKIN)).abs() < 0.05, "must stand on the terrace: {prev:?}");
}
