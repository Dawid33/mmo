//! Which terrain faces line a DUG space — the faces the cut-off x-ray view
//! opens the terrain in front of (`renderer/voxel_pull/xray.rs`, spec
//! `docs/superpowers/specs/2026-10-31-cave-xray-design.md`).
//!
//! A face is interior when the air it looks into is dug: strictly below its
//! block column's original worldgen surface ([`game::is_dug_air`], the same
//! rule the x-ray pick uses). The mesh task gets the surfaces it needs as an
//! [`InteriorGround`]: the brick's own block columns plus a one-block ring,
//! since a side face on the brick's edge looks into the neighbouring column.

use game::{BLOCK_VOXELS, BRICK_VOXELS};

/// Blocks along one brick edge (2: a 32³-voxel brick of 16-voxel blocks).
pub const BRICK_BLOCKS: i32 = (BRICK_VOXELS / BLOCK_VOXELS) as i32;
/// Columns along one edge of an [`InteriorGround`]: the brick's plus one each side.
const SPAN: usize = BRICK_BLOCKS as usize + 2;

/// The original surfaces around one brick, for classifying its faces.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InteriorGround {
    /// The brick's minimum block, world block coords.
    pub origin: [i32; 3],
    /// Surface block y of block column `(origin.x - 1 + i, origin.z - 1 + k)`
    /// at `[i * SPAN + k]`; `None` where no ground is known (nothing there
    /// counts as dug).
    surfaces: [Option<i16>; SPAN * SPAN],
}

impl InteriorGround {
    /// The ground around brick `brick` (world brick key), from `surface`:
    /// the original surface of a world block column, `None` where unknown.
    /// `None` when no column the brick can touch has ground — nothing to mark.
    pub fn around(brick: [i32; 3], surface: impl Fn(i32, i32) -> Option<i16>) -> Option<Self> {
        let origin = brick.map(|b| b * BRICK_BLOCKS);
        let mut surfaces = [None; SPAN * SPAN];
        for i in 0..SPAN {
            for k in 0..SPAN {
                surfaces[i * SPAN + k] =
                    surface(origin[0] - 1 + i as i32, origin[2] - 1 + k as i32);
            }
        }
        // Dug air lies below a surface: a brick whose faces look into nothing
        // under the highest surface around (its lowest look is one block
        // below it, a ceiling's) has no interior face.
        let highest = surfaces.iter().flatten().max()?;
        (origin[1] - 1 < i32::from(*highest)).then_some(Self { origin, surfaces })
    }

    /// Whether the voxel at brick-local `v` (each axis -1..=32: the padded
    /// shell reaches one voxel into the neighbours) is in a dug block cell —
    /// asked only for air the mesher emits a face against.
    pub fn is_dug_voxel(&self, v: [i32; 3]) -> bool {
        let block = v.map(|c| c.div_euclid(BLOCK_VOXELS as i32));
        let (i, k) = ((block[0] + 1) as usize, (block[2] + 1) as usize);
        if i >= SPAN || k >= SPAN {
            return false;
        }
        self.surfaces[i * SPAN + k]
            .is_some_and(|s| game::is_dug_air(self.origin[1] + block[1], i32::from(s)))
    }
}
