//! Binary greedy meshing: occupancy packed as per-axis u64 column masks over
//! the padded 34³ space; face culling via bitshifts; greedy merging (task 3)
//! over per-merge-key bitplanes. Pure functions — no Bevy types here.

use game::{Voxel, CHUNK_VOXEL_COUNT};

/// Padded chunk edge: 32 interior + 1 shell cell each side.
pub const PADDED: usize = 34;
/// Only interior positions (bits 1..=32) may emit faces.
const INTERIOR_MASK: u64 = ((1u64 << 32) - 1) << 1;

/// A 34³ voxel snapshot: the chunk's 32³ interior at `1..=32` per axis plus a
/// 1-voxel shell of neighbor data (task 5) or air (`from_interior`).
pub struct PaddedVoxels(pub Vec<Voxel>);

impl PaddedVoxels {
    pub fn index(x: usize, y: usize, z: usize) -> usize {
        x + PADDED * (y + PADDED * z)
    }

    pub fn get(&self, x: usize, y: usize, z: usize) -> Voxel {
        self.0[Self::index(x, y, z)]
    }

    pub fn is_solid(&self, x: usize, y: usize, z: usize) -> bool {
        !self.get(x, y, z).is_air()
    }

    /// Interior from a chunk's 32³ voxels, shell all-air (isolated chunk).
    pub fn from_interior(voxels: &[Voxel]) -> Self {
        debug_assert_eq!(voxels.len(), CHUNK_VOXEL_COUNT);
        let mut p = vec![Voxel::default(); PADDED * PADDED * PADDED];
        for (i, v) in voxels.iter().enumerate() {
            let (x, y, z) = (i % 32, (i / 32) % 32, i / (32 * 32));
            p[Self::index(x + 1, y + 1, z + 1)] = *v;
        }
        Self(p)
    }
}

/// The fixed cross-axes (U, V) each face axis merges over. Emission (task 4)
/// derives winding from these, so they must never change independently.
pub fn cross_axes(axis: usize) -> (usize, usize) {
    match axis {
        0 => (1, 2), // X faces: U=Y, V=Z
        1 => (0, 2), // Y faces: U=X, V=Z
        2 => (0, 1), // Z faces: U=X, V=Y
        _ => unreachable!("axis is 0..3"),
    }
}

/// Solidity packed as u64 columns along each axis: `cols[axis][u][v]` has bit
/// `a` set iff the voxel at position `a` along `axis` (cross coords `(u, v)`
/// per `cross_axes`) is solid. 34 ≤ 64, so one word per column.
pub(crate) struct Occupancy {
    cols: Box<[[[u64; PADDED]; PADDED]; 3]>,
}

impl Occupancy {
    pub fn new(p: &PaddedVoxels) -> Self {
        let mut cols: Box<[[[u64; PADDED]; PADDED]; 3]> =
            vec![[[0u64; PADDED]; PADDED]; 3].try_into().unwrap();
        for z in 0..PADDED {
            for y in 0..PADDED {
                for x in 0..PADDED {
                    if p.is_solid(x, y, z) {
                        cols[0][y][z] |= 1 << x;
                        cols[1][x][z] |= 1 << y;
                        cols[2][x][y] |= 1 << z;
                    }
                }
            }
        }
        Self { cols }
    }

    /// Visible-face bits along `axis` at cross coords `(u, v)`: bit `a` set
    /// iff the voxel at `a` is solid and its `positive`/negative neighbor
    /// along `axis` is air. Shell positions are masked out.
    pub fn face_bits(&self, axis: usize, positive: bool, u: usize, v: usize) -> u64 {
        let col = self.cols[axis][u][v];
        let vis = if positive { col & !(col >> 1) } else { col & !(col << 1) };
        vis & INTERIOR_MASK
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::BlockId;

    fn interior_with(cells: &[([usize; 3], BlockId)]) -> Vec<Voxel> {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for ([x, y, z], id) in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(*id);
        }
        v
    }

    #[test]
    fn padded_roundtrip_and_shell_is_air() {
        let p = PaddedVoxels::from_interior(&interior_with(&[([0, 5, 31], BlockId(1))]));
        assert!(p.is_solid(1, 6, 32), "chunk-local (0,5,31) is padded (1,6,32)");
        assert!(!p.is_solid(0, 6, 32), "shell stays air with from_interior");
        assert!(!p.is_solid(1, 6, 33), "shell stays air with from_interior");
    }

    #[test]
    fn lone_voxel_has_six_visible_faces() {
        let p = PaddedVoxels::from_interior(&interior_with(&[([5, 6, 7], BlockId(1))]));
        let occ = Occupancy::new(&p);
        // padded coords (6, 7, 8)
        assert_eq!(occ.face_bits(0, true, 7, 8) & (1 << 6), 1 << 6, "+X visible");
        assert_eq!(occ.face_bits(0, false, 7, 8) & (1 << 6), 1 << 6, "-X visible");
        assert_eq!(occ.face_bits(1, true, 6, 8) & (1 << 7), 1 << 7, "+Y visible");
        assert_eq!(occ.face_bits(1, false, 6, 8) & (1 << 7), 1 << 7, "-Y visible");
        assert_eq!(occ.face_bits(2, true, 6, 7) & (1 << 8), 1 << 8, "+Z visible");
        assert_eq!(occ.face_bits(2, false, 6, 7) & (1 << 8), 1 << 8, "-Z visible");
    }

    #[test]
    fn touching_voxels_cull_shared_faces() {
        // Two voxels adjacent along X at chunk-local (5,6,7) and (6,6,7).
        let p = PaddedVoxels::from_interior(&interior_with(&[
            ([5, 6, 7], BlockId(1)),
            ([6, 6, 7], BlockId(1)),
        ]));
        let occ = Occupancy::new(&p);
        // +X of the left voxel (padded x=6) faces the right voxel: culled.
        assert_eq!(occ.face_bits(0, true, 7, 8) & (1 << 6), 0);
        // -X of the right voxel (padded x=7): culled.
        assert_eq!(occ.face_bits(0, false, 7, 8) & (1 << 7), 0);
        // Outer X faces remain.
        assert_eq!(occ.face_bits(0, false, 7, 8) & (1 << 6), 1 << 6);
        assert_eq!(occ.face_bits(0, true, 7, 8) & (1 << 7), 1 << 7);
    }

    #[test]
    fn solid_shell_neighbor_culls_boundary_face() {
        // Solid voxel at chunk-local x=31 (padded x=32); a solid PAD cell at
        // padded x=33 (neighbor data) must cull its +X face.
        let mut p = PaddedVoxels::from_interior(&interior_with(&[([31, 5, 5], BlockId(1))]));
        p.0[PaddedVoxels::index(33, 6, 6)] = Voxel::new(BlockId(1));
        let occ = Occupancy::new(&p);
        assert_eq!(occ.face_bits(0, true, 6, 6) & (1 << 32), 0, "+X culled by neighbor");
        assert_eq!(occ.face_bits(0, false, 6, 6) & (1 << 32), 1 << 32, "-X still visible");
    }

    #[test]
    fn shell_cells_never_emit_faces() {
        // A solid cell in the shell itself (padded x=33) must not produce
        // visible faces — only interior bits 1..=32 may emit.
        let mut p = PaddedVoxels::from_interior(&vec![Voxel::default(); CHUNK_VOXEL_COUNT]);
        p.0[PaddedVoxels::index(33, 6, 6)] = Voxel::new(BlockId(1));
        let occ = Occupancy::new(&p);
        for u in 0..PADDED {
            for v in 0..PADDED {
                assert_eq!(occ.face_bits(0, true, u, v), 0);
                assert_eq!(occ.face_bits(0, false, u, v), 0);
            }
        }
    }
}
