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

use common::{CHUNK, Walker, chunks_over, seam_row, seam_z, spawn_x_band, spawn_z_walk};
use game::movement::{TICK_DT, UNITS_PER_METER, WALK_SPEED};
use game::{BLOCK_VOXELS, Block, BlockId, CHUNK_BLOCKS, Chunk};

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

/// Ticks of a walk long enough to also catch what only shows up now and then.
const LONG: usize = 80;

/// Gate: mmo-fork hunk parry shape_cast_voxels_shape.rs:326/357/373 trailing-row search domain (4173c4ef: voxel under the shape periodically never tested — body sank at every seam, observed)
#[test]
fn voxel_scale_floor_is_level_on_a_long_walk() {
    for side in [0, 4] {
        assert_level_walk(&mut world(rows_for(LONG), |_, _| Chunk::flat_floor(8)), side, LONG);
    }
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

/// Gate: mmo-fork hunk rapier character_controller.rs:455 snap gate heading_down half (38cd3da5; test comment names the hunk)
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

/// Gate: mmo-fork hunk rapier character_controller.rs:482 snap_to_ground no longer requires result down; observed hover past each terrace edge then drop (38cd3da5)
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

