//! Assembling the 34³ padded snapshot from a chunk and its up-to-26 grid
//! neighbors. Face neighbors feed culling; edge/corner neighbors feed AO.

use std::collections::BTreeMap;

use game::Voxel;

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
    if linear > 13 { linear as usize - 1 } else { linear as usize }
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
                let Some(nv) = neighbors.get(&off) else { continue };
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::binary::{mesh_quads, Face};
    use game::{BlockId, CHUNK_VOXEL_COUNT};

    fn full_chunk(id: BlockId) -> Vec<Voxel> {
        vec![Voxel::new(id); CHUNK_VOXEL_COUNT]
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
        let mut nv = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        nv[0 + 32 * 4 + 32 * 32 * 4] = Voxel::new(BlockId(2)); // its (0,4,4)
        let mut neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        neighbors.insert([1, 0, 0], nv.as_slice());
        let p = gather_padded(&full_chunk(BlockId(1)), &neighbors);
        assert!(p.is_solid(33, 5, 5), "neighbor (0,4,4) lands at padded (33,5,5)");
        assert!(!p.is_solid(33, 6, 5));
    }

    #[test]
    fn corner_neighbor_fills_the_corner_cell() {
        // Neighbor at (-1,-1,-1): its voxel (31,31,31) is our padded (0,0,0).
        let mut nv = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
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
        let isolated = mesh_quads(&gather_padded(&center, &BTreeMap::new()));
        assert!(isolated.iter().any(|q| q.face == Face::PosX));

        let mut neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        neighbors.insert([1, 0, 0], neighbor.as_slice());
        let meshed = mesh_quads(&gather_padded(&center, &neighbors));
        assert!(
            meshed.iter().all(|q| q.face != Face::PosX),
            "faces against a solid neighbor must be culled"
        );
        // The other five walls still render.
        assert!(meshed.iter().any(|q| q.face == Face::NegX));
    }
}
