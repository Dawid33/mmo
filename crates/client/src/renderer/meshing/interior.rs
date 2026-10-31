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
#[derive(Debug, Clone, PartialEq, Eq)]
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
                surfaces[i * SPAN + k] = surface(origin[0] - 1 + i as i32, origin[2] - 1 + k as i32);
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

#[cfg(test)]
mod tests {
    use super::*;

    const S: i32 = BLOCK_VOXELS as i32;

    #[test]
    fn a_brick_above_every_surface_needs_no_ground() {
        assert_eq!(InteriorGround::around([0, 11, 0], |_, _| Some(20)), None, "brick y 22..24, surface 20");
        assert_eq!(InteriorGround::around([0, 0, 0], |_, _| None), None, "no ground known");
        let g = InteriorGround::around([0, 10, 0], |_, _| Some(20)).expect("brick y 20..22: its ceilings look at y 19");
        assert!(g.is_dug_voxel([0, -1, 0]), "the air under the brick");
        assert!(!g.is_dug_voxel([0, 0, 0]), "the surface block's own air");
    }

    #[test]
    fn dug_is_strictly_below_the_column_surface() {
        // Brick y 18..20; surface 19 at block column x 1, 30 elsewhere.
        let g = InteriorGround::around([0, 9, 0], |x, _| Some(if x == 1 { 19 } else { 30 })).unwrap();
        assert!(g.is_dug_voxel([0, 0, 0]), "y 18 under a 30 surface");
        assert!(g.is_dug_voxel([S, 0, 0]), "y 18 under the 19 surface");
        assert!(!g.is_dug_voxel([S, S, 0]), "y 19 is the surface block itself (a half block's air)");
        assert!(g.is_dug_voxel([S - 1, S, 0]), "x 0 at y 19 is still under 30");
    }

    #[test]
    fn the_shell_reads_the_neighbouring_columns() {
        // Only block column (-1, *) and (2, *) have ground, high above.
        let g = InteriorGround::around([0, 0, 0], |x, _| (x == -1 || x == 2).then_some(50)).unwrap();
        assert!(g.is_dug_voxel([-1, 0, 0]), "one voxel into the -x neighbour");
        assert!(g.is_dug_voxel([2 * S, 0, 5]), "one voxel into the +x neighbour");
        assert!(!g.is_dug_voxel([0, 0, 0]), "own column: no ground known");
    }

    #[test]
    fn faces_into_dug_air_are_marked_and_merge_apart() {
        use super::super::binary::{mesh_quads_classified, CutoutTable, Face, PaddedVoxels};
        use game::BlockId;
        // A solid floor slab, voxel y 0..16 (block y 0) across the brick. The
        // brick sits at block y 0; the air above is block y 1. Surface 2 at
        // block column x 0 (so its air y 1 is dug), 1 at x 1 (y 1 is the
        // surface block: not dug).
        let mut voxels = vec![game::Voxel::default(); 32 * 32 * 32];
        for x in 0..32usize {
            for y in 0..16usize {
                for z in 0..32usize {
                    voxels[x + 32 * (y + 32 * z)] = game::Voxel::new(BlockId(1));
                }
            }
        }
        let p = PaddedVoxels::from_interior(&voxels);
        let g = InteriorGround::around([0, 0, 0], |x, _| Some(if x <= 0 { 2 } else { 1 })).unwrap();
        let tops: Vec<_> = mesh_quads_classified(&p, &CutoutTable::NONE, Some(&g))
            .into_iter()
            .filter(|q| q.face == Face::PosY)
            .collect();
        assert_eq!(tops.len(), 2, "the dug half and the open half merge apart: {tops:?}");
        for q in &tops {
            assert_eq!(q.interior, q.min[0] < 16, "{q:?}");
        }
        let plain = mesh_quads_classified(&p, &CutoutTable::NONE, None);
        assert!(plain.iter().all(|q| !q.interior));
        assert_eq!(plain.iter().filter(|q| q.face == Face::PosY).count(), 1, "unclassified: one top");
    }
}
