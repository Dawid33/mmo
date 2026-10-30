//! 0fps-style baked ambient occlusion, computed from the padded occupancy at
//! mesh time. Render-derived data only — never part of rollback state.
//! Reference: https://0fps.net/2013/07/03/ambient-occlusion-for-minecraft-like-worlds/

use super::binary::{cross_axes, CutoutTable, PaddedVoxels};

/// Vertex color multiplier per AO level (0 = fully occluded corner). Tunable.
pub const AO_CURVE: [f32; 4] = [0.55, 0.70, 0.85, 1.0];

/// Classic 0fps corner rule. `side1`/`side2` are the two face-plane-adjacent
/// neighbors of the corner, `corner` the diagonal one.
pub fn vertex_ao(side1: bool, side2: bool, corner: bool) -> u8 {
    if side1 && side2 {
        0
    } else {
        3 - (side1 as u8 + side2 as u8 + corner as u8)
    }
}

/// `vertex_ao` as a table indexed `side1 << 2 | side2 << 1 | corner`, for the
/// mesher's hot loop where the three samples arrive as bits.
pub(crate) const VERTEX_AO_LUT: [u8; 8] = [3, 2, 2, 1, 2, 1, 0, 0];

/// AO for the 4 corners of the (`axis`, `positive`) face of the voxel at
/// padded coords `pos`. Corner order (cu, cv) = (0,0), (1,0), (0,1), (1,1)
/// over `cross_axes(axis)` — the same order emission (task 4) writes vertices.
/// Only opaque voxels occlude: a cutout block (`cutout`) darkens no corner.
pub fn face_ao(
    p: &PaddedVoxels,
    cutout: &CutoutTable,
    axis: usize,
    positive: bool,
    pos: [usize; 3],
) -> [u8; 4] {
    let (ua, va) = cross_axes(axis);
    // The occlusion samples live in the voxel layer the face looks into.
    let mut layer = [pos[0] as i32, pos[1] as i32, pos[2] as i32];
    layer[axis] += if positive { 1 } else { -1 };

    let solid = |c: [i32; 3]| -> bool {
        // Not reachable from the mesher: every position it passes here stays
        // within 0..=33, since the AO samples are one shell cell out from a
        // face voxel that itself stops at the shell (0/33). This guard is
        // defensive for callers outside the mesher's contract who might pass
        // a `pos` closer to (or past) the buffer edge; out-of-buffer counts
        // as open (no occlusion) — consistent with "missing neighbor = air"
        // (task 5).
        if c.iter()
            .any(|&v| v < 0 || v as usize >= super::binary::PADDED)
        {
            return false;
        }
        p.is_opaque(c[0] as usize, c[1] as usize, c[2] as usize, cutout)
    };

    let mut ao = [0u8; 4];
    for (i, (cu, cv)) in [(0i32, 0i32), (1, 0), (0, 1), (1, 1)]
        .into_iter()
        .enumerate()
    {
        let du = if cu == 0 { -1 } else { 1 };
        let dv = if cv == 0 { -1 } else { 1 };
        let mut s1 = layer;
        s1[ua] += du;
        let mut s2 = layer;
        s2[va] += dv;
        let mut c = layer;
        c[ua] += du;
        c[va] += dv;
        ao[i] = vertex_ao(solid(s1), solid(s2), solid(c));
    }
    ao
}

/// 0fps anisotropy fix: the default triangulation splits along the 0-3
/// diagonal; when the 0-3 corner pair is brighter than 1-2, flip to split
/// along 1-2 so interpolation crosses the darker pair consistently.
pub fn ao_flips_quad(ao: &[u8; 4]) -> bool {
    ao[0] + ao[3] > ao[1] + ao[2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{BlockId, Voxel, BRICK_VOXEL_COUNT};

    #[test]
    fn lut_matches_vertex_ao() {
        for s1 in [false, true] {
            for s2 in [false, true] {
                for c in [false, true] {
                    let idx = (s1 as usize) << 2 | (s2 as usize) << 1 | c as usize;
                    assert_eq!(VERTEX_AO_LUT[idx], vertex_ao(s1, s2, c));
                }
            }
        }
    }

    #[test]
    fn vertex_ao_truth_table() {
        assert_eq!(vertex_ao(false, false, false), 3, "open corner");
        assert_eq!(vertex_ao(false, false, true), 2, "corner only");
        assert_eq!(vertex_ao(true, false, false), 2, "one side");
        assert_eq!(vertex_ao(true, false, true), 1, "side + corner");
        assert_eq!(vertex_ao(true, true, false), 0, "both sides occlude fully");
        assert_eq!(vertex_ao(true, true, true), 0, "both sides + corner");
    }

    #[test]
    fn lone_voxel_top_face_is_unoccluded() {
        let mut v = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        v[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let p = PaddedVoxels::from_interior(&v);
        assert_eq!(face_ao(&p, &CutoutTable::NONE, 1, true, [6, 6, 6]), [3, 3, 3, 3]);
    }

    #[test]
    fn side_neighbor_darkens_the_two_corners_it_touches() {
        // Voxel at (5,5,5); a second voxel at (4,6,5) sits diagonally above
        // along -X: for the top (+Y) face it occludes the two cu=0 corners.
        let mut v = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        v[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        v[4 + 32 * 6 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let p = PaddedVoxels::from_interior(&v);
        // +Y face cross axes are (U=X, V=Z). The occluder at padded (5,7,6)
        // is the side1 (U-direction) sample for both cu=0 corners, so
        // corners (0,0) and (0,1) darken to 2 while (1,0) and (1,1) stay 3.
        assert_eq!(face_ao(&p, &CutoutTable::NONE, 1, true, [6, 6, 6]), [2, 3, 2, 3]);
    }

    #[test]
    fn flip_rule_uses_diagonal_sums() {
        assert!(ao_flips_quad(&[3, 0, 0, 3]), "bright 0-3 diagonal flips");
        assert!(
            !ao_flips_quad(&[0, 3, 3, 0]),
            "bright 1-2 diagonal keeps default"
        );
        assert!(!ao_flips_quad(&[3, 3, 3, 3]), "uniform never flips");
    }
}
