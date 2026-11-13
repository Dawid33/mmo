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
use crate::common;

use common::{Body, CHUNK, Walker, chunks_over, seam_row, seam_z, spawn_x_band, spawn_z_walk};
use game::movement::{CHARACTER_SKIN, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, TICK_DT, UNITS_PER_METER, WALK_SPEED};
use game::{BLOCK_VOXELS, Block, BlockId, BlockIndex, CHUNK_BLOCKS, ChiselData, Chunk};

const SOLID: BlockId = BlockId(1);
/// Distance covered by one tick of walking.
const STEP: f32 = WALK_SPEED * UNITS_PER_METER * TICK_DT;
/// Slack on per-tick motion: the controller re-settles onto its skin by a
/// fraction of a millimetre now and then.
const LEVEL: f32 = 0.005;

/// The z extent (world units) a walk of that many ticks from the spawn needs.
fn rows_for(walk_ticks: usize) -> std::ops::Range<f32> {
    spawn_z_walk(walk_ticks as f32 * STEP)
}

/// One chunk either side of the spawn column in x, over `z_units` along z: all
/// a walk along z from the spawn touches.
fn world(z_units: std::ops::Range<f32>, chunk: impl Fn(i32, i32) -> Chunk) -> Walker {
    Walker::on(spawn_x_band(), z_units, chunk)
}

fn chunk_row(z: f32) -> f32 {
    (z / CHUNK).floor()
}

/// Walks +z (S at yaw 0) for `n` ticks after sidestepping `side` ticks along
/// +x, asserting level, straight, constant-speed, grounded motion throughout.
/// `side = 0` walks the x = 128 seam between two chunk columns.
fn assert_level_walk(w: &mut Walker, side: usize, n: usize) {
    // Up to three chunk borders; a walk too short for that crosses as many as
    // it is long.
    assert_level_walk_across(w, side, n, 3.min((n as f32 * STEP / CHUNK) as usize));
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
    let mut w = world(seam_z() - 32.0..seam_z() + 32.0, |_, _| Chunk::flat_floor(8));
    w.approach_seam(32.0);
    assert_level_walk_across(&mut w, 0, 12, 1);
}

#[test]
fn voxel_scale_floor_is_level_on_a_long_walk() {
    for side in [0, 4] {
        assert_level_walk(&mut world(rows_for(LONG), |_, _| Chunk::flat_floor(8)), side, LONG);
    }
}

#[test]
fn diagonal_walk_is_level() {
    let x = spawn_x_band();
    let mut w = Walker::on(x.start..x.end + CHUNK, rows_for(20), |_, _| Chunk::flat_floor(16));
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
            // The top block farthest from the walking line (x = spawn).
            let centre = |bx: usize| x as f32 * CHUNK + (bx * BLOCK_VOXELS + BLOCK_VOXELS / 2) as f32;
            let far = (0..CHUNK_BLOCKS)
                .max_by(|&a, &b| {
                    let off = |bx| (centre(bx) - common::SPAWN[0]).abs();
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
    let mut w = world(rows_for(25), |_, z| Chunk::flat_floor(if z >= seam_row() { 20 } else { 16 }));
    w.approach_seam(32.0);
    let low = w.body();
    w.keys.back = true;
    w.ticks(25);
    let high = w.body();
    assert!(high.z > seam_z() + 40.0, "the step must not stop the walk: {high:?}");
    assert!(high.grounded);
    assert!((high.y - low.y - 4.0).abs() < 0.05, "must stand on the step: {low:?} -> {high:?}");
}

#[test]
fn full_block_wall_at_a_chunk_border_blocks() {
    // 16 units (1 m) up at the seam ahead of the spawn: above the step height.
    let mut w = world(rows_for(25), |_, z| Chunk::flat_floor(if z >= seam_row() { 32 } else { 16 }));
    w.approach_seam(32.0);
    let before = w.body();
    w.keys.back = true;
    w.ticks(25);
    let after = w.body();
    let wall = seam_z();
    assert!(after.z < wall - 6.0, "walked into the wall: {after:?}");
    assert!(after.z > wall - 7.0, "stopped short of the wall: {after:?}");
    assert!(after.grounded);
    assert!((after.y - before.y).abs() < LEVEL, "the wall must not lift the body: {after:?}");
}

/// A floor one block deep and, from the chunk seam ahead of the spawn on, a
/// one-block-thick ceiling whose underside is `clear_blocks` blocks above the
/// floor.
fn tunnel(clear_blocks: usize) -> Walker {
    let floor = BLOCK_VOXELS;
    let ceiling = floor + clear_blocks * BLOCK_VOXELS;
    // Chunk layers up to the one holding the ceiling's top face.
    let top_layer = (ceiling + BLOCK_VOXELS - 1) / CHUNK as usize;
    let mut chunks = Vec::new();
    for (x, z) in chunks_over(spawn_x_band(), rows_for(25)) {
        for cy in 0..=top_layer {
            let mut chunk = if cy == 0 { Chunk::flat_floor(floor as u32) } else { Chunk::default() };
            if z >= seam_row() {
                for bx in 0..CHUNK_BLOCKS {
                    for by in 0..CHUNK_BLOCKS {
                        for bz in 0..CHUNK_BLOCKS {
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
    let mut w = Walker::new(chunks);
    w.approach_seam(32.0);
    w
}

#[test]
fn player_walks_through_a_two_block_tunnel() {
    // The body fills two blocks but must still fit a two-block-high gap.
    let mut w = tunnel(2);
    let before = w.body();
    w.keys.back = true;
    w.ticks(25);
    let after = w.body();
    assert!(after.z > seam_z() + 15.0, "a two-block tunnel must not stop the body: {after:?}");
    assert!(after.grounded);
    assert!((after.y - before.y).abs() < LEVEL, "the ceiling must not push the body: {after:?}");
}

#[test]
fn player_does_not_fit_a_one_block_tunnel() {
    let mut w = tunnel(1);
    w.keys.back = true;
    w.ticks(25);
    let after = w.body();
    assert!(after.z < seam_z(), "a one-block gap is lower than the body: {after:?}");
}

/// Not in the game's file — pins the snap gate's other half (`// mmo-fork:`
/// `heading_down` in the rapier fork's controller): snap-to-ground only follows
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
    assert!(before.z > seam_z() + 10.0 && before.grounded, "must stand in the tunnel: {before:?}");
    w.keys.jump_pressed = true;
    w.keys.jump_held = true;
    w.tick();
    w.keys.jump_held = false;
    let after = w.body();
    assert!(after.y > before.y + 1.0 && !after.grounded, "the jump must rise to the ceiling: {before:?} -> {after:?}");
}

/// One-voxel terraces, a step down per chunk along +z: the gentle slopes of
/// generated terrain.
fn terraces(z_units: std::ops::Range<f32>) -> Walker {
    Walker::on(spawn_x_band(), z_units, |_, z| Chunk::flat_floor(28 - z as u32))
}

#[test]
fn walking_down_terraces_stays_on_the_ground() {
    // The slide along level ground ends with no downward part, which used to
    // keep snap-to-ground from engaging: a tick of hovering past each edge,
    // ungrounded, then a drop.
    // Three seams ahead: the walk is as long as it takes to pass them.
    let end_z = seam_z() + 2.0 * CHUNK;
    let walk = ((end_z - common::SPAWN[1]) / STEP).ceil() as usize + 2;
    let mut w = terraces(rows_for(walk));
    w.keys.back = true;
    let mut prev = w.body();
    for tick in 0..walk {
        w.tick();
        let now = w.body();
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "left the ground at {at}");
        assert!(now.y - prev.y < LEVEL, "rose on the way down at {at}");
        assert!(prev.y - now.y < 1.1, "fell more than a terrace at {at}");
        prev = now;
    }
    assert!(prev.z > end_z, "must have walked down three terraces: {prev:?}");
}

#[test]
fn walking_up_terraces_stays_on_the_ground() {
    // Sliding over a terrace's edge overshoots it; the body must be put back
    // down on the terrace within the tick rather than fall for the next one.
    // Start a few ticks short of a chunk border, going back toward -z: the
    // spawn itself at 2-block chunks (z = 128 is a border), 20 units into the
    // second chunk row otherwise.
    let z0 = common::SPAWN[1].max(CHUNK + 20.0);
    let border = ((z0 / CHUNK).ceil() - 1.0) * CHUNK;
    let mut w = terraces((border - CHUNK).max(0.0)..z0 + CHUNK);
    if z0 != common::SPAWN[1] {
        let b = w.body();
        w.teleport(rapier3d::prelude::Vector::new(b.x, b.y, z0));
        w.ticks(4);
    }
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
    assert!(prev.z < border && prev.y > start.y + 0.9, "must have climbed: {start:?} -> {prev:?}");
    let half_extent = PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS;
    assert!((prev.y - (floor + half_extent + CHARACTER_SKIN)).abs() < 0.05, "must stand on the terrace: {prev:?}");
}

/// The rig must step exactly as a region does (H9 review I1): its integration
/// parameters are the game's, not a hand-built set (it used `dt = TICK_DT`
/// while the game runs rapier's default `dt`).
#[test]
fn walker_steps_with_the_games_integration_parameters() {
    let w = Walker::on(spawn_x_band(), 96.0..160.0, |_, _| Chunk::flat_floor(16));
    assert_eq!(w.w.params, game::PhysicsParams::default().0);
}
