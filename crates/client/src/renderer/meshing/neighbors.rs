//! Assembling the 34³ padded snapshot from a chunk and its up-to-26 grid
//! neighbors. Face neighbors feed culling; edge/corner neighbors feed AO.

use std::collections::BTreeMap;

use game::Voxel;

use super::super::bridge::VoxelData;
use super::binary::{PaddedVoxels, PADDED};

/// The 26 neighbor offsets in fixed enumeration order (z-outer, x-inner).
pub fn zone_offsets() -> impl Iterator<Item = [i32; 3]> {
    (-1..=1).flat_map(|z| {
        (-1..=1).flat_map(move |y| {
            (-1..=1).filter_map(move |x| {
                if [x, y, z] == [0, 0, 0] {
                    None
                } else {
                    Some([x, y, z])
                }
            })
        })
    })
}

/// Position of `off` within `zone_offsets()` (0..26).
pub fn zone_index(off: [i32; 3]) -> usize {
    let linear = (off[2] + 1) * 9 + (off[1] + 1) * 3 + (off[0] + 1);
    // Offsets after the center [0,0,0] (linear 13) shift down by one.
    if linear > 13 {
        linear as usize - 1
    } else {
        linear as usize
    }
}

fn shell_dir(c: usize) -> i32 {
    if c == 0 {
        -1
    } else if c == PADDED - 1 {
        1
    } else {
        0
    }
}

/// Padded coord → the neighbor's chunk-local coord along one axis.
fn neighbor_coord(c: usize, dir: i32) -> usize {
    match dir {
        -1 => 31, // our x=0 shell cell is the -X neighbor's x=31 plane
        1 => 0,   // our x=33 shell cell is the +X neighbor's x=0 plane
        _ => c - 1,
    }
}

/// Build the padded snapshot: interior from `center`, shell from neighbors
/// keyed by grid offset. Missing neighbors leave air (over-draws, never holes).
pub fn gather_padded(center: &[Voxel], neighbors: &BTreeMap<[i32; 3], &[Voxel]>) -> PaddedVoxels {
    let mut p = PaddedVoxels::from_interior(center);
    if neighbors.is_empty() {
        return p;
    }
    for z in 0..PADDED {
        for y in 0..PADDED {
            for x in 0..PADDED {
                let off = [shell_dir(x), shell_dir(y), shell_dir(z)];
                if off == [0, 0, 0] {
                    continue;
                }
                let Some(nv) = neighbors.get(&off) else {
                    continue;
                };
                let (nx, ny, nz) = (
                    neighbor_coord(x, off[0]),
                    neighbor_coord(y, off[1]),
                    neighbor_coord(z, off[2]),
                );
                p.0[PaddedVoxels::index(x, y, z)] = nv[nx + 32 * ny + 32 * 32 * nz];
            }
        }
    }
    p
}

/// Make a streamed LOD grid's crust solid underneath, in the padded snapshot.
///
/// Far terrain is a crust: `worldgen::coarse_tile` fills a column only from a
/// soil band under its surface up to the surface, and a downsampled region
/// ends at its columns' base. Meshed as it is, the air under it is a second,
/// inner surface — a bottom face under every column and a side face wherever
/// two neighbours' crusts end at different depths — that no view from above
/// ever shows; it was about 45 % of the far quads (2026-11-14 capture).
///
/// So each column's lowest solid cell is extended down to the bottom of the
/// snapshot (shell row included) with its own block, unless something solid
/// lies under it in the group below (`neighbors` at y − 1): then the gap is a
/// real overhang — a tree crown over ground one group down, a cave — and
/// stays. Shell columns are filled by the same rule from their own
/// neighbour's data. Where a side neighbour is not wired (another key) the
/// shell copies the fill of the interior column beside it, so the fill never
/// shows as a wall there and the border faces of the crust itself stay as
/// they were (skirts included).
///
/// The fill only ever shows where a neighbour's surface is lower than this
/// column's crust reaches — a hole in a steep face before.
pub fn fill_under_crust(p: &mut PaddedVoxels, neighbors: &BTreeMap<[i32; 3], &[Voxel]>) {
    const TOP: usize = PADDED - 2; // the interior's top row
    // Per padded column: the row its fill reaches up to (exclusive) and the
    // block it is made of.
    let mut filled: Vec<Option<(usize, Voxel)>> = vec![None; PADDED * PADDED];
    for z in 0..PADDED {
        for x in 0..PADDED {
            let Some(lowest) = (1..=TOP).find(|&y| p.is_solid(x, y, z)) else {
                continue;
            };
            if p.is_solid(x, 0, z) {
                continue;
            }
            let off = [shell_dir(x), -1, shell_dir(z)];
            let grounded = neighbors.get(&off).is_some_and(|below| {
                let (nx, nz) = (neighbor_coord(x, off[0]), neighbor_coord(z, off[2]));
                (0..32).any(|ny| !below[nx + 32 * ny + 32 * 32 * nz].is_air())
            });
            if grounded {
                continue;
            }
            let block = p.get(x, lowest, z);
            for y in 0..lowest {
                p.0[PaddedVoxels::index(x, y, z)] = block;
            }
            filled[x + PADDED * z] = Some((lowest, block));
        }
    }
    // Unwired side neighbours: mirror the interior column beside them.
    for z in 0..PADDED {
        for x in 0..PADDED {
            let off = [shell_dir(x), 0, shell_dir(z)];
            if off == [0, 0, 0] || neighbors.contains_key(&off) {
                continue;
            }
            let (ix, iz) = (x.clamp(1, TOP), z.clamp(1, TOP));
            if let Some((top, block)) = filled[ix + PADDED * iz] {
                for y in 0..top {
                    p.0[PaddedVoxels::index(x, y, z)] = block;
                }
            }
        }
    }
}

fn source_range(direction: i32) -> std::ops::Range<usize> {
    match direction {
        -1 => 0..1,
        1 => 31..32,
        _ => 0..32,
    }
}

/// Linear indices belonging to one of the 26 boundary zones, ordered exactly
/// like the chunk's dense x-major layout so an RLE cursor can stream them.
pub fn zone_indices(off: [i32; 3]) -> impl Iterator<Item = usize> {
    let (xr, yr, zr) = (
        source_range(off[0]),
        source_range(off[1]),
        source_range(off[2]),
    );
    zr.flat_map(move |z| {
        let yr = yr.clone();
        let xr = xr.clone();
        yr.flat_map(move |y| xr.clone().map(move |x| x + 32 * y + 32 * 32 * z))
    })
}

fn padded_coord(source: usize, direction: i32) -> usize {
    match direction {
        -1 => 0,
        1 => PADDED - 1,
        _ => source + 1,
    }
}

/// Build the same padded snapshot as [`gather_padded`] while keeping the
/// center and neighbors run-length encoded. Each neighbor visits only the
/// boundary plane, edge, or corner selected by its offset.
pub fn gather_padded_rle(
    center: &VoxelData,
    neighbors: &BTreeMap<[i32; 3], &VoxelData>,
) -> PaddedVoxels {
    let mut padded = PaddedVoxels(vec![Voxel::default(); PADDED * PADDED * PADDED]);
    center.for_each_index_sorted(0..game::BRICK_VOXEL_COUNT, |index, voxel| {
        let (x, y, z) = (index % 32, (index / 32) % 32, index / (32 * 32));
        padded.0[PaddedVoxels::index(x + 1, y + 1, z + 1)] = voxel;
    });

    for (&off, neighbor) in neighbors {
        let facing_center = [-off[0], -off[1], -off[2]];
        neighbor.for_each_index_sorted(zone_indices(facing_center), |index, voxel| {
            let (x, y, z) = (index % 32, (index / 32) % 32, index / (32 * 32));
            let (px, py, pz) = (
                padded_coord(x, off[0]),
                padded_coord(y, off[1]),
                padded_coord(z, off[2]),
            );
            padded.0[PaddedVoxels::index(px, py, pz)] = voxel;
        });
    }
    padded
}

#[cfg(test)]
mod tests {
    use super::super::binary::{mesh_quads, CutoutTable, Face};
    use super::*;
    use crate::renderer::bridge::VoxelData;
    use game::{BlockId, BRICK_VOXEL_COUNT};

    fn full_chunk(id: BlockId) -> Vec<Voxel> {
        vec![Voxel::new(id); BRICK_VOXEL_COUNT]
    }

    /// A crust: solid in rows `lo..=hi` of every column, air elsewhere.
    fn crust(lo: usize, hi: usize) -> Vec<Voxel> {
        let mut grid = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        for (i, v) in grid.iter_mut().enumerate() {
            if (lo..=hi).contains(&((i / 32) % 32)) {
                *v = Voxel::new(BlockId(1));
            }
        }
        grid
    }

    /// Cells covered by `face`'s quads (AO may split a wall into several).
    fn area(quads: &[super::super::binary::Quad], face: super::super::binary::Face) -> u32 {
        quads.iter().filter(|q| q.face == face).map(|q| q.w * q.h).sum()
    }

    fn filled_quads(center: &[Voxel], neighbors: &BTreeMap<[i32; 3], &[Voxel]>) -> Vec<super::super::binary::Quad> {
        let mut p = gather_padded(center, neighbors);
        fill_under_crust(&mut p, neighbors);
        mesh_quads(&p, &CutoutTable::NONE)
    }

    /// The crust's underside is not meshed: no bottom faces, and the border
    /// walls are the crust's own (the fill never shows as a wall).
    #[test]
    fn a_crust_has_no_underside() {
        let center = crust(10, 12);
        let plain = mesh_quads(&gather_padded(&center, &BTreeMap::new()), &CutoutTable::NONE);
        assert_eq!(area(&plain, Face::NegY), 32 * 32);
        let filled = filled_quads(&center, &BTreeMap::new());
        assert_eq!(area(&filled, Face::NegY), 0, "no bottom face under a crust");
        assert_eq!(area(&filled, Face::PosY), 32 * 32);
        for face in [Face::PosX, Face::NegX, Face::PosZ, Face::NegZ] {
            assert_eq!(area(&filled, face), 32 * 3, "{face:?}: the border wall is the crust's three rows");
            let lowest = filled.iter().filter(|q| q.face == face).map(|q| q.min[1]).min();
            assert_eq!(lowest, Some(10), "{face:?}: the wall starts at the crust's bottom, not at the fill's");
        }
    }

    /// Two neighbouring columns whose crusts end at different depths show no
    /// face between them under the higher surface's reach — and the step
    /// between their surfaces is whole.
    #[test]
    fn crusts_of_different_depth_show_no_inner_wall() {
        // x < 16: rows 10..=12; x >= 16: rows 14..=16 — surfaces 12 and 16,
        // and row 13 a gap in the face on the plane x = 16.
        let mut center = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        for (i, v) in center.iter_mut().enumerate() {
            let (x, y) = (i % 32, (i / 32) % 32);
            let rows = if x < 16 { 10..=12 } else { 14..=16 };
            if rows.contains(&y) {
                *v = Voxel::new(BlockId(1));
            }
        }
        let plain = mesh_quads(&gather_padded(&center, &BTreeMap::new()), &CutoutTable::NONE);
        // Unfilled: the low crust's wall facing the high one, and the border wall.
        assert_eq!(area(&plain, Face::PosX), 2 * 32 * 3);
        assert_eq!(area(&plain, Face::NegX), 2 * 32 * 3);
        let filled = filled_quads(&center, &BTreeMap::new());
        assert_eq!(area(&filled, Face::NegY), 0);
        // The low crust shows nothing toward the high one: the border wall is left.
        assert_eq!(area(&filled, Face::PosX), 32 * 3);
        // The step reaches down to the low surface (rows 13..=16), plus the border wall.
        assert_eq!(area(&filled, Face::NegX), 32 * 4 + 32 * 3);
        assert_eq!(area(&filled, Face::PosY), 32 * 32);
    }

    /// Something solid in the group below makes the gap a real overhang (a
    /// crown over ground one group down): its bottom face stays.
    #[test]
    fn an_overhang_over_the_group_below_keeps_its_bottom() {
        let center = crust(3, 5);
        let below = crust(8, 9);
        let neighbors: BTreeMap<[i32; 3], &[Voxel]> = [([0, -1, 0], below.as_slice())].into();
        assert_eq!(area(&filled_quads(&center, &neighbors), Face::NegY), 32 * 32, "the crown's underside is real");
        // An empty group below is no ground: the crust is filled as usual.
        let empty = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        let neighbors: BTreeMap<[i32; 3], &[Voxel]> = [([0, -1, 0], empty.as_slice())].into();
        assert_eq!(area(&filled_quads(&center, &neighbors), Face::NegY), 0);
    }

    /// A gap inside one grid (a crown over the ground of the same group) is
    /// untouched: only the lowest run of a column is extended.
    #[test]
    fn a_gap_above_the_crust_is_kept() {
        let mut center = crust(4, 6);
        for (i, v) in center.iter_mut().enumerate() {
            if (12..=13).contains(&((i / 32) % 32)) {
                *v = Voxel::new(BlockId(2));
            }
        }
        let quads = filled_quads(&center, &BTreeMap::new());
        assert_eq!(area(&quads, Face::NegY), 32 * 32, "the upper run's underside only");
        assert_eq!(area(&quads, Face::PosY), 2 * 32 * 32);
    }

    /// A wired side neighbour is filled by the same rule, so no wall shows
    /// between two grids' fills, nor under the neighbour's higher crust.
    #[test]
    fn the_fill_does_not_wall_off_a_wired_neighbour() {
        let center = crust(10, 12);
        let east = crust(20, 22);
        let neighbors: BTreeMap<[i32; 3], &[Voxel]> = [([1, 0, 0], east.as_slice())].into();
        let quads = filled_quads(&center, &neighbors);
        assert_eq!(area(&quads, Face::PosX), 0, "the east neighbour's fill covers the centre's whole east side");
        assert_eq!(area(&quads, Face::NegX), 32 * 3, "the unwired west side keeps the crust's wall");
    }

    #[test]
    fn zone_offsets_enumerates_26_and_indexes_roundtrip() {
        let offs: Vec<[i32; 3]> = zone_offsets().collect();
        assert_eq!(offs.len(), 26);
        assert!(!offs.contains(&[0, 0, 0]));
        for (i, off) in offs.iter().enumerate() {
            assert_eq!(zone_index(*off), i);
        }
    }

    #[test]
    fn missing_neighbors_leave_air_shell() {
        let p = gather_padded(&full_chunk(BlockId(1)), &BTreeMap::new());
        assert!(!p.is_solid(0, 5, 5));
        assert!(!p.is_solid(33, 5, 5));
        assert!(p.is_solid(1, 5, 5));
    }

    #[test]
    fn face_neighbor_boundary_fills_the_shell() {
        // Neighbor at +X: its x=0 plane becomes our x=33 shell plane.
        let mut nv = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        nv[0 + 32 * 4 + 32 * 32 * 4] = Voxel::new(BlockId(2)); // its (0,4,4)
        let mut neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        neighbors.insert([1, 0, 0], nv.as_slice());
        let p = gather_padded(&full_chunk(BlockId(1)), &neighbors);
        assert!(
            p.is_solid(33, 5, 5),
            "neighbor (0,4,4) lands at padded (33,5,5)"
        );
        assert!(!p.is_solid(33, 6, 5));
    }

    #[test]
    fn corner_neighbor_fills_the_corner_cell() {
        // Neighbor at (-1,-1,-1): its voxel (31,31,31) is our padded (0,0,0).
        let mut nv = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        nv[31 + 32 * 31 + 32 * 32 * 31] = Voxel::new(BlockId(2));
        let mut neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        neighbors.insert([-1, -1, -1], nv.as_slice());
        let p = gather_padded(&full_chunk(BlockId(1)), &neighbors);
        assert!(p.is_solid(0, 0, 0));
    }

    #[test]
    fn solid_neighbor_culls_the_whole_shared_face() {
        // THE Phase-1 payoff test: a full chunk with a full +X neighbor emits
        // zero +X faces; isolated it emits the whole 32×32 wall.
        let center = full_chunk(BlockId(1));
        let neighbor = full_chunk(BlockId(1));
        let isolated = mesh_quads(&gather_padded(&center, &BTreeMap::new()), &CutoutTable::NONE);
        assert!(isolated.iter().any(|q| q.face == Face::PosX));

        let mut neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        neighbors.insert([1, 0, 0], neighbor.as_slice());
        let meshed = mesh_quads(&gather_padded(&center, &neighbors), &CutoutTable::NONE);
        assert!(
            meshed.iter().all(|q| q.face != Face::PosX),
            "faces against a solid neighbor must be culled"
        );
        // The other five walls still render.
        assert!(meshed.iter().any(|q| q.face == Face::NegX));
    }

    #[test]
    fn rle_padding_matches_dense_padding_for_all_neighbor_zones() {
        let center_dense = (0..BRICK_VOXEL_COUNT)
            .map(|i| Voxel::new(BlockId(((i / 37) % 5) as u16)))
            .collect::<Vec<_>>();
        let center_rle = VoxelData::new(&center_dense);
        let mut dense_owned = BTreeMap::new();
        let mut rle_owned = BTreeMap::new();
        for (i, off) in zone_offsets().enumerate() {
            let voxels = vec![Voxel::new(BlockId(i as u16 + 1)); BRICK_VOXEL_COUNT];
            dense_owned.insert(off, voxels.clone());
            rle_owned.insert(off, VoxelData::new(voxels));
        }
        let dense_refs = dense_owned
            .iter()
            .map(|(&k, v)| (k, v.as_slice()))
            .collect();
        let rle_refs = rle_owned.iter().map(|(&k, v)| (k, v)).collect();

        let dense = gather_padded(&center_dense, &dense_refs);
        let rle = gather_padded_rle(&center_rle, &rle_refs);

        assert_eq!(rle.0, dense.0);
    }

    #[test]
    fn rle_padding_leaves_missing_neighbors_as_air() {
        let center = VoxelData::new(vec![Voxel::new(BlockId(1)); BRICK_VOXEL_COUNT]);
        let padded = gather_padded_rle(&center, &BTreeMap::new());
        assert_eq!(padded.get(0, 10, 10), Voxel::default());
        assert_eq!(padded.get(33, 10, 10), Voxel::default());
        assert_eq!(padded.get(1, 1, 1), Voxel::new(BlockId(1)));
    }
}
