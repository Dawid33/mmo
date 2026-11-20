//! 0fps-style baked ambient occlusion, computed from the padded occupancy at
//! mesh time. Render-derived data only — never part of rollback state.
//! Reference: https://0fps.net/2013/07/03/ambient-occlusion-for-minecraft-like-worlds/

use super::binary::{CutoutTable, PaddedVoxels, cross_axes};

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
