//! The fork's autostep on terrain stairs: a flight of slope blocks
//! (`game::slope`: half a block of rise per half a block of tread) under a
//! round-footed body, approached at an angle. Upstream's autostep lifts the
//! body by the step's height, moves it on only by what travel the tick has
//! left, and lets the rest fall: short of the tread the capsule's foot comes
//! down on the step's EDGE, which it then slides back off or cannot leave.
//! The rig (`common::Walker`) is the game's own `walk_body` driving the fork.
use crate::common::{self, Walker};
use game::slope::{Side, SlopeShape, slope_chisel};
use game::{BLOCK_VOXELS, Block, BlockId, BlockIndex, CHUNK_BLOCKS, Chunk};

const SOLID: BlockId = BlockId(1);
/// A block's edge in world units.
const BLOCK: f32 = BLOCK_VOXELS as f32;
/// The first stair's block row along z: 2 blocks past the spawn row.
const FIRST_ROW: usize = (common::SPAWN[1] as usize) / BLOCK_VOXELS + 2;
/// Stairs in the flight; each rises one block.
const STAIRS: usize = 4;

/// A one-block floor with a flight of [`STAIRS`] slope blocks rising along +z
/// from block row [`FIRST_ROW`], as wide as the floor, and a plateau behind
/// it. `None` where the flight does not fit one chunk layer.
fn staircase() -> Option<Walker> {
    if CHUNK_BLOCKS < STAIRS + 2 {
        return None;
    }
    let floor_z = 0.0..(FIRST_ROW + STAIRS + 8) as f32 * BLOCK;
    Some(Walker::on(0.0..512.0, floor_z, |_, cz| {
        let mut chunk = Chunk::flat_floor(BLOCK_VOXELS as u32);
        for bz in 0..CHUNK_BLOCKS {
            let Some(stair) = (cz as usize * CHUNK_BLOCKS + bz).checked_sub(FIRST_ROW) else {
                continue;
            };
            let top = 1 + stair.min(STAIRS);
            for bx in 0..CHUNK_BLOCKS {
                for by in 1..top {
                    chunk.set_block(bx, by, bz, Block::new(SOLID));
                }
                if stair < STAIRS {
                    chunk.set_block(bx, top, bz, Block::new(SOLID));
                    let slope = slope_chisel(SOLID, SlopeShape::Straight { low: Side::NegZ });
                    chunk
                        .chisel
                        .insert(BlockIndex::from_xyz(bx, top, bz), slope);
                }
            }
        }
        chunk
    }))
}

/// Gate: mmo-fork hunk rapier character_controller.rs `move_shape` 'a step taken is kept' (the
/// step's downward remainder is set down, not slid; observed 2026-11-19: a 45° walk into terrain
/// stairs stepped and slid back off the riser's edge every tick, never rising)
#[test]
fn a_diagonal_walk_into_stairs_mounts_them() {
    let Some(mut w) = staircase() else { return };
    let start = w.body();
    w.keys.back = true;
    w.keys.right = true;
    // 2 blocks to the flight, then 2.4 units of run a tick: ~2.8 blocks up.
    w.ticks(50);
    let end = w.body();
    assert!(
        end.y - start.y > 2.0 * BLOCK,
        "must have climbed two stairs: {start:?} -> {end:?}"
    );
}

/// Gate: mmo-fork hunks rapier character_controller.rs `handle_stairs`: 'on a slant the move runs
/// into and cannot climb' + the removed 'target ramp is too steep' refusal (observed 2026-11-19:
/// a walk 72° off the stairs' axis sat on the first step's edge for good. With the first hunk
/// alone it is refused the step — set down `min_width` ahead, its round foot lands on the
/// riser's edge, which upstream took for a ramp; with the second alone it rests on that edge at
/// a contact under the climb angle: no wall, so no autostep, and too slow to slide up against
/// its own downward push)
#[test]
fn a_walk_along_stairs_still_mounts_them() {
    let Some(mut w) = staircase() else { return };
    let start = w.body();
    w.yaw = 72.0_f32.to_radians();
    w.keys.back = true;
    // 1.07 units of run a tick: 30 ticks to the flight, then ~1.9 blocks up; it sat at 0.4.
    w.ticks(60);
    let end = w.body();
    assert!(
        end.x - start.x > 2.0 * (end.z - start.z),
        "must have walked mostly along the stairs: {start:?} -> {end:?}"
    );
    assert!(
        end.y - start.y > 1.2 * BLOCK,
        "must have climbed a stair: {start:?} -> {end:?}"
    );
}
