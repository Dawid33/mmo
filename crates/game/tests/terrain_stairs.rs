//! Walking up terrain slopes: a staircase of slope blocks (`game::slope`,
//! half a block of rise per half a block of tread) through the real region
//! tick.
use crate::common::{body, key, teleport, ticks, world_of};
use game::slope::{Side, SlopeShape, slope_chisel};
use game::{BLOCK_VOXELS, Block, BlockId, BlockIndex, CHUNK_BLOCKS, Chunk, Key, World};

const SOLID: BlockId = BlockId(1);
/// The first stair's block row along z: 2 blocks past the spawn row.
const FIRST_ROW: usize = (game::PLAYER_SPAWN_Z as usize) / BLOCK_VOXELS + 2;
/// Stairs in the flight; each rises one block.
const STAIRS: usize = 6;
/// World units of rise of the whole flight.
const RISE: f32 = (STAIRS * BLOCK_VOXELS) as f32;

/// A one-block floor with a flight of [`STAIRS`] slope blocks rising along +z
/// from block row [`FIRST_ROW`], as wide as the floor, and a plateau behind it.
fn staircase() -> World {
    let floor_z = 0.0..((FIRST_ROW + STAIRS + 6) * BLOCK_VOXELS) as f32;
    world_of(0.0..512.0, floor_z, |_, cz| {
        let mut chunk = Chunk::flat_floor(BLOCK_VOXELS as u32);
        for bz in 0..CHUNK_BLOCKS {
            let Some(stair) = (cz * CHUNK_BLOCKS + bz).checked_sub(FIRST_ROW) else {
                continue;
            };
            // Whole blocks under the stair (or the plateau), then the stair.
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
    })
}

/// Holds `keys` from the spawn and returns the ticks from the first rise to
/// the top of the flight, or `None` if the body is not up after `limit`.
fn climb(keys: &[Key], limit: usize) -> Option<usize> {
    let mut world = staircase();
    let start = body(&world);
    // Mid-floor: a diagonal walk drifts along x.
    teleport(&mut world, 256.0, start.y, start.z);
    ticks(&mut world, 2);
    for k in keys {
        key(&mut world, *k, true);
    }
    let mut first = None;
    for t in 0..limit {
        ticks(&mut world, 1);
        let y = body(&world).y - start.y;
        if first.is_none() && y > 1.0 {
            first = Some(t);
        }
        if y > RISE - 1.0 {
            return Some(t - first.unwrap());
        }
    }
    None
}

/// Regression: the player could not walk up terrain stairs — it took the first half-step and
/// stopped (seen in play, 2026-11-19: "fails to climb a lot of terrain stairs"; fixed by
/// `movement::settle_sweep` keeping the momentum of a body stopped by a step, and by the
/// controller keeping a step it took — `// mmo-fork: a step taken is kept`).
#[test]
fn walking_straight_up_terrain_stairs_climbs_them() {
    if CHUNK_BLOCKS < STAIRS + 2 {
        return; // the flight does not fit one chunk layer
    }
    // 6 blocks of run at walking speed are 28 ticks; the stairs cost a few.
    let took = climb(&[Key::KeyS], 120).expect("the body must reach the top of the stairs");
    assert!(took <= 40, "the climb crawled: {took} ticks");
}

/// Regression: approaching terrain stairs at an angle, the player slid along the first riser
/// and never stepped up (seen in play, 2026-11-19, with the above; the autostep set the round
/// foot on the step's edge and the slide took it back off every tick — `// mmo-fork: a step
/// taken is kept`; a mount that fails no longer zeroes the momentum, `movement::settle_sweep`).
#[test]
fn walking_diagonally_up_terrain_stairs_climbs_them() {
    if CHUNK_BLOCKS < STAIRS + 2 {
        return; // the flight does not fit one chunk layer
    }
    // At 45° the run takes 40 ticks; without the momentum kept it took 85.
    let took =
        climb(&[Key::KeyS, Key::KeyD], 200).expect("the body must reach the top of the stairs");
    assert!(took <= 60, "the climb crawled: {took} ticks");
}
