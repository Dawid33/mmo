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

