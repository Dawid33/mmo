//! Cave-culling visibility graph (Tommo/MCPE "Advanced Cave Culling"):
//! per-chunk face-pair connectivity computed by flood fill over the chunk's
//! own 32³ non-solid space, and the per-frame camera BFS that hides chunks
//! no sight line can reach. All state here is render-derived — recomputed on
//! remesh, never rollback-hashed.
//! Reference: https://tomcc.github.io/2014/08/31/visibility-1.html

use super::binary::{Face, PaddedVoxels};

/// All 15 face pairs connected — the mask of an all-air (or absent) chunk.
pub const ALL_CONNECTED: u16 = 0x7FFF;

/// Index of the unordered face pair (a, b), a < b, faces in `Face::index()`
/// order. Bijection onto 0..15 (test-pinned).
pub fn pair_index(a: u32, b: u32) -> u32 {
    debug_assert!(a < b && b < 6);
    // Formula rewritten to avoid underflow: a * (11 - a) / 2 + (b - a - 1)
    // (equivalent to a * 5 - a * (a - 1) / 2 + (b - a - 1))
    a * (11 - a) / 2 + (b - a - 1)
}

/// Bit for the unordered pair (a, b) in either order.
pub fn pair_bit(a: u32, b: u32) -> u16 {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    1 << pair_index(lo, hi)
}

/// Which of a cell's chunk-local coords sit on which face, in Face::index()
/// order: +X = x == 31, -X = x == 0, +Y = y == 31, -Y = y == 0, +Z/-Z same.
fn touched_faces(x: usize, y: usize, z: usize) -> u8 {
    let mut f = 0u8;
    if x == 31 { f |= 1 << 0; }
    if x == 0  { f |= 1 << 1; }
    if y == 31 { f |= 1 << 2; }
    if y == 0  { f |= 1 << 3; }
    if z == 31 { f |= 1 << 4; }
    if z == 0  { f |= 1 << 5; }
    f
}

/// Flood-fill the chunk's own 32³ non-solid space (padded interior 1..=32;
/// the 0/33 neighbor shell is deliberately excluded) and record, for each
/// connected air component, which chunk faces it touches; every pair of
/// touched faces is connected. O(32³) worst case, a few hundred µs — runs
/// inside the async mesh task, never on the main thread.
pub fn face_connectivity(p: &PaddedVoxels) -> u16 {
    const N: usize = 32;
    let idx = |x: usize, y: usize, z: usize| x + N * (y + N * z);
    let mut visited = vec![false; N * N * N];
    let mut mask = 0u16;
    let mut stack: Vec<(usize, usize, usize)> = Vec::new();

    for sz in 0..N {
        for sy in 0..N {
            for sx in 0..N {
                if visited[idx(sx, sy, sz)] || p.is_solid(sx + 1, sy + 1, sz + 1) {
                    continue;
                }
                // New air component: flood it, accumulate touched faces.
                let mut faces = 0u8;
                visited[idx(sx, sy, sz)] = true;
                stack.push((sx, sy, sz));
                while let Some((x, y, z)) = stack.pop() {
                    faces |= touched_faces(x, y, z);
                    let mut push = |nx: usize, ny: usize, nz: usize| {
                        if !visited[idx(nx, ny, nz)] && !p.is_solid(nx + 1, ny + 1, nz + 1) {
                            visited[idx(nx, ny, nz)] = true;
                            stack.push((nx, ny, nz));
                        }
                    };
                    if x + 1 < N { push(x + 1, y, z); }
                    if x > 0     { push(x - 1, y, z); }
                    if y + 1 < N { push(x, y + 1, z); }
                    if y > 0     { push(x, y - 1, z); }
                    if z + 1 < N { push(x, y, z + 1); }
                    if z > 0     { push(x, y, z - 1); }
                }
                // Union all pairs among the touched faces.
                for a in 0..6u32 {
                    if faces >> a & 1 == 0 { continue; }
                    for b in (a + 1)..6u32 {
                        if faces >> b & 1 == 1 {
                            mask |= pair_bit(a, b);
                        }
                    }
                }
                if mask == ALL_CONNECTED {
                    return mask; // can't grow further
                }
            }
        }
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

    fn interior(cells: &[[usize; 3]]) -> PaddedVoxels {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for [x, y, z] in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(1));
        }
        PaddedVoxels::from_interior(&v)
    }

    fn solid_except(air: &[[usize; 3]]) -> PaddedVoxels {
        let mut v = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
        for [x, y, z] in air {
            v[x + 32 * y + 32 * 32 * z] = Voxel::default();
        }
        PaddedVoxels::from_interior(&v)
    }

    #[test]
    fn pair_index_is_a_bijection_onto_0_15() {
        let mut seen = [false; 15];
        for a in 0..6u32 {
            for b in (a + 1)..6u32 {
                let i = pair_index(a, b) as usize;
                assert!(!seen[i], "pair ({a},{b}) collides at {i}");
                seen[i] = true;
            }
        }
        assert!(seen.iter().all(|&s| s), "all 15 indices hit");
    }

    #[test]
    fn empty_chunk_connects_everything() {
        assert_eq!(face_connectivity(&interior(&[])), ALL_CONNECTED);
    }

    #[test]
    fn solid_chunk_connects_nothing() {
        assert_eq!(face_connectivity(&solid_except(&[])), 0);
    }

    #[test]
    fn straight_tunnel_connects_exactly_its_two_faces() {
        // 1-voxel tunnel along X at y=5, z=5 through an otherwise solid chunk.
        let air: Vec<[usize; 3]> = (0..32).map(|x| [x, 5, 5]).collect();
        let mask = face_connectivity(&solid_except(&air));
        assert_eq!(mask, pair_bit(Face::PosX.index(), Face::NegX.index()));
    }

    #[test]
    fn l_tunnel_connects_exactly_its_corner_pair() {
        // Air along -X to center, then down to -Y: connects NegX <-> NegY only.
        let mut air: Vec<[usize; 3]> = (0..=16).map(|x| [x, 16, 16]).collect();
        air.extend((0..=16).map(|y| [16, y, 16]));
        let mask = face_connectivity(&solid_except(&air));
        assert_eq!(mask, pair_bit(Face::NegX.index(), Face::NegY.index()));
    }

    #[test]
    fn open_column_connects_vertical_pair() {
        let air: Vec<[usize; 3]> = (0..32).map(|y| [7, y, 9]).collect();
        let mask = face_connectivity(&solid_except(&air));
        assert_eq!(mask, pair_bit(Face::PosY.index(), Face::NegY.index()));
    }

    #[test]
    fn two_isolated_rooms_union_their_pairs_without_cross_connecting() {
        // Room A: tunnel X (touches ±X). Room B: tunnel Z (touches ±Z),
        // far apart, no shared air. Mask = both pairs, and NOT e.g. PosX<->PosZ.
        let mut air: Vec<[usize; 3]> = (0..32).map(|x| [x, 3, 3]).collect();
        air.extend((0..32).map(|z| [28, 28, z]));
        let mask = face_connectivity(&solid_except(&air));
        let expect = pair_bit(Face::PosX.index(), Face::NegX.index())
            | pair_bit(Face::PosZ.index(), Face::NegZ.index());
        assert_eq!(mask, expect);
    }

    #[test]
    fn interior_pocket_touching_no_face_adds_nothing() {
        // A sealed air pocket in the middle: connects no faces.
        let air: Vec<[usize; 3]> = vec![[15, 15, 15], [16, 15, 15]];
        assert_eq!(face_connectivity(&solid_except(&air)), 0);
    }
}
