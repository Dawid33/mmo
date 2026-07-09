use std::collections::BTreeMap;

use bevy::mesh::Mesh;
use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

use super::super::block_textures::BlockTextureLayers;
use super::binary::{self, Face, Quad};
use super::emit;
use super::neighbors::gather_padded;
#[cfg(test)]
use super::pack;

/// Merge up to 2×2×2 source 32³ grids (index `sx + 2*sy + 4*sz`; `None` = all-air) into one 32³ coarse grid at 2× the cell size.
/// Each coarse cell (x,y,z) draws from source grid `(x/16, y/16, z/16)`'s 2×2×2 voxel block at `((x%16)*2, (y%16)*2, (z%16)*2)`.
/// Solid iff any source voxel solid; material = majority BlockId among solid sources, ties → lowest BlockId.
///
/// **Layout contract:** LOD grids are always 32³ `Vec<Voxel>` in the standard `x + 32*y + 32*32*z` order.
pub fn downsample2(sources: &[Option<&[Voxel]>; 8]) -> Vec<Voxel> {
    let mut output = vec![Voxel::default(); CHUNK_VOXEL_COUNT];

    // Iterate all 32³ coarse cells
    for z in 0..32 {
        for y in 0..32 {
            for x in 0..32 {
                // Determine which source octant and offset within that octant
                let sx = x / 16; // 0 or 1
                let sy = y / 16; // 0 or 1
                let sz = z / 16; // 0 or 1
                let source_idx = sx + 2 * sy + 4 * sz;

                let ox = (x % 16) * 2; // 0..30
                let oy = (y % 16) * 2; // 0..30
                let oz = (z % 16) * 2; // 0..30

                // Gather the 2×2×2 source voxels from this octant's grid
                let mut block_counts: [(BlockId, u8); 8] = [(BlockId(0), 0); 8];
                let mut block_count_len = 0;
                let mut has_solid = false;

                if let Some(source_grid) = sources[source_idx] {
                    for dz in 0..2 {
                        for dy in 0..2 {
                            for dx in 0..2 {
                                let sx_coord = ox + dx;
                                let sy_coord = oy + dy;
                                let sz_coord = oz + dz;

                                if sx_coord < 32 && sy_coord < 32 && sz_coord < 32 {
                                    let idx = sx_coord + 32 * sy_coord + 32 * 32 * sz_coord;
                                    let voxel = source_grid[idx];

                                    if !voxel.is_air() {
                                        has_solid = true;
                                        let block_id = voxel.block;

                                        // Find or add this block_id to our count array
                                        let mut found = false;
                                        for (bid, count) in block_counts[..block_count_len].iter_mut() {
                                            if *bid == block_id {
                                                *count = count.saturating_add(1);
                                                found = true;
                                                break;
                                            }
                                        }
                                        if !found && block_count_len < 8 {
                                            block_counts[block_count_len] = (block_id, 1);
                                            block_count_len += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Determine the output material
                if has_solid {
                    // Find the majority material; ties break to lowest BlockId
                    let mut max_count = 0;
                    let mut winner = BlockId(u16::MAX); // sentinel

                    for (bid, count) in block_counts[..block_count_len].iter() {
                        if *count > max_count || (*count == max_count && *bid < winner) {
                            max_count = *count;
                            winner = *bid;
                        }
                    }

                    output[x + 32 * y + 32 * 32 * z] = Voxel::new(winner);
                }
            }
        }
    }

    output
}

/// Factor-16 fast path: each source chunk contributes its 2×2×2 `blocks` directly as coarse cells.
/// Chiseled blocks count as their block id — chisel detail is invisible at 16×.
/// Output: fills only the 0..4 corner of the 32³ grid (4×4×4 cells = 8 source chunks × 8 blocks each).
///
/// **Layout contract:** LOD grids are always 32³ `Vec<Voxel>` in the standard `x + 32*y + 32*32*z` order.
pub fn downsample_blocks(chunks: &[Option<&game::Chunk>; 8]) -> Vec<Voxel> {
    let mut output = vec![Voxel::default(); CHUNK_VOXEL_COUNT];

    for (chunk_idx, chunk_opt) in chunks.iter().enumerate() {
        if let Some(chunk) = chunk_opt {
            // Determine which octant this chunk belongs to
            let sx = chunk_idx % 2;
            let sy = (chunk_idx / 2) % 2;
            let sz = (chunk_idx / 4) % 2;

            // For each of the 8 blocks in this chunk
            for bz in 0..2 {
                for by in 0..2 {
                    for bx in 0..2 {
                        let block = chunk.block_at(bx, by, bz);

                        if block.id != BlockId::AIR {
                            // Map to coarse cell position
                            let coarse_x = sx * 2 + bx;
                            let coarse_y = sy * 2 + by;
                            let coarse_z = sz * 2 + bz;

                            let idx = coarse_x + 32 * coarse_y + 32 * 32 * coarse_z;
                            output[idx] = Voxel::new(block.id);
                        }
                    }
                }
            }
        }
    }

    output
}

/// Does `q`'s plane sit on the 32-grid's outer edge along its own face axis?
/// Only X/Z side faces are ever skirted — Y (top/bottom) faces never are.
fn is_border_side_quad(q: &Quad) -> bool {
    match q.face {
        Face::NegX => q.min[0] == 0,
        Face::PosX => q.min[0] == 31, // positive face plane sits at min+1 == 32
        Face::NegZ => q.min[2] == 0,
        Face::PosZ => q.min[2] == 31,
        _ => false,
    }
}

/// Extend a border side-quad one cell downward: undo one unit of the
/// caller's +1 Y-bias (`min[1] -= 1`) and grow whichever extent maps to Y for
/// this face's axis. Per `cross_axes` (binary.rs): X faces have (U=Y, V=Z),
/// so Y rides `w`; Z faces have (U=X, V=Y), so Y rides `h`. Y faces (axis 1)
/// are never skirted — callers must not pass one.
fn skirt_extend(q: &mut Quad) {
    debug_assert_ne!(q.face.axis(), 1, "Y faces are never skirted");
    q.min[1] -= 1;
    match q.face.axis() {
        0 => q.w += 1,
        2 => q.h += 1,
        _ => unreachable!("skirt_extend called on a non-side face"),
    }
}

/// Mesh a coarse LOD grid: gather padding from same-scale neighbors, greedy
/// mesh, bias every quad up by one cell (`min[1] += 1`), then skirt border
/// side-quads one cell back down to the true floor so adjacent LOD rings
/// (or ring boundaries at different scales) never show a seam gap at y=0.
/// Resulting mesh y range is 0..=33 — callers translate `Transform.y` down
/// by the scale factor to compensate (Task 3).
pub fn mesh_lod_grid(
    grid: &[Voxel],
    neighbors: &BTreeMap<[i32; 3], &[Voxel]>,
    layers: &BlockTextureLayers,
) -> Option<Mesh> {
    let padded = gather_padded(grid, neighbors);
    let mut quads = binary::mesh_quads(&padded);
    for q in &mut quads {
        q.min[1] += 1;
    }
    for q in &mut quads {
        if q.face.axis() != 1 && is_border_side_quad(q) {
            skirt_extend(q);
        }
    }
    emit::quads_to_mesh(&quads, layers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{Block, BlockId, Voxel, CHUNK_VOXEL_COUNT};

    fn grid_with(cells: &[([usize; 3], u16)]) -> Vec<Voxel> {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for ([x, y, z], id) in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(*id));
        }
        v
    }

    #[test]
    fn empty_sources_downsample_to_air() {
        let out = downsample2(&[None; 8]);
        assert!(out.iter().all(|v| v.is_air()));
        assert_eq!(out.len(), CHUNK_VOXEL_COUNT);
    }

    #[test]
    fn any_solid_source_voxel_makes_the_coarse_cell_solid() {
        // Source grid 0 (the -x-y-z octant), voxel (0,0,0) solid: coarse (0,0,0) solid.
        let g = grid_with(&[([0, 0, 0], 1)]);
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[0] = Some(&g);
        let out = downsample2(&sources);
        assert_eq!(out[0].block, BlockId(1));
        assert!(out[1..].iter().all(|v| v.is_air()), "exactly one coarse cell");
    }

    #[test]
    fn octant_placement_maps_source_grids_to_coarse_sixteenths() {
        // Source grid 7 (+x+y+z octant), voxel (31,31,31): coarse (31,31,31).
        let g = grid_with(&[([31, 31, 31], 3)]);
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[7] = Some(&g);
        let out = downsample2(&sources);
        assert_eq!(out[31 + 32 * 31 + 32 * 32 * 31].block, BlockId(3));
    }

    #[test]
    fn majority_material_wins_ties_to_lowest() {
        // One coarse cell: 8 source voxels = 3×id2, 3×id5, 2×air → tie 3-3 → id2.
        let mut g = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for (i, (x, y, z)) in [(0, 0, 0), (1, 0, 0), (0, 1, 0)].into_iter().enumerate() {
            let _ = i;
            g[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(2));
        }
        for (x, y, z) in [(1, 1, 0), (0, 0, 1), (1, 0, 1)] {
            g[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(5));
        }
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[0] = Some(&g);
        let out = downsample2(&sources);
        assert_eq!(out[0].block, BlockId(2), "tie broken to lowest id");
    }

    #[test]
    fn downsample_is_deterministic() {
        let g = grid_with(&[([5, 6, 7], 1), ([5, 7, 7], 2), ([4, 6, 7], 2)]);
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[0] = Some(&g);
        assert_eq!(downsample2(&sources), downsample2(&sources));
    }

    #[test]
    fn blocks_fast_path_maps_blocks_to_cells() {
        // A chunk with one solid block at block-index (1,0,1): with the chunk
        // in source slot 0, the coarse cell (1,0,1) of the 4³ corner is solid.
        let mut chunk = game::Chunk::default();
        chunk.set_block(1, 0, 1, Block::new(BlockId(4)));
        let mut chunks: [Option<&game::Chunk>; 8] = [None; 8];
        chunks[0] = Some(&chunk);
        let out = downsample_blocks(&chunks);
        assert_eq!(out[1 + 32 * 0 + 32 * 32 * 1].block, BlockId(4));
        assert_eq!(out.iter().filter(|v| !v.is_air()).count(), 1);
    }

    fn base_quad(face: Face) -> Quad {
        Quad { min: [10, 20, 30], w: 4, h: 7, face, block: BlockId(1), ao: [3; 4] }
    }

    #[test]
    fn skirt_extend_grows_the_y_mapped_extent_per_face() {
        // X faces: cross_axes(0) = (U=1/Y, V=2/Z) — Y rides `w`.
        for face in [Face::NegX, Face::PosX] {
            let mut q = base_quad(face);
            skirt_extend(&mut q);
            assert_eq!(q.min, [10, 19, 30], "{face:?}: min[1] decrements by one");
            assert_eq!((q.w, q.h), (5, 7), "{face:?}: w (Y-mapped) grows, h untouched");
        }
        // Z faces: cross_axes(2) = (U=0/X, V=1/Y) — Y rides `h`.
        for face in [Face::NegZ, Face::PosZ] {
            let mut q = base_quad(face);
            skirt_extend(&mut q);
            assert_eq!(q.min, [10, 19, 30], "{face:?}: min[1] decrements by one");
            assert_eq!((q.w, q.h), (4, 8), "{face:?}: h (Y-mapped) grows, w untouched");
        }
    }

    #[test]
    fn skirt_extend_leaves_other_fields_untouched() {
        let mut q = base_quad(Face::PosX);
        skirt_extend(&mut q);
        assert_eq!(q.min[0], 10, "non-Y min components untouched");
        assert_eq!(q.min[2], 30, "non-Y min components untouched");
        assert_eq!(q.block, BlockId(1));
        assert_eq!(q.ao, [3; 4]);
    }

    #[test]
    #[should_panic]
    fn skirt_extend_rejects_y_faces_in_debug() {
        // Y faces are never side-skirted; calling this on one is a caller bug.
        let mut q = base_quad(Face::PosY);
        skirt_extend(&mut q);
    }

    #[test]
    fn is_border_side_quad_matches_the_four_side_faces_at_the_grid_edge() {
        // Border cases.
        assert!(is_border_side_quad(&Quad { min: [0, 5, 5], ..base_quad(Face::NegX) }));
        assert!(is_border_side_quad(&Quad { min: [31, 5, 5], ..base_quad(Face::PosX) }));
        assert!(is_border_side_quad(&Quad { min: [5, 5, 0], ..base_quad(Face::NegZ) }));
        assert!(is_border_side_quad(&Quad { min: [5, 5, 31], ..base_quad(Face::PosZ) }));
        // Interior (one cell in from the edge): not a border quad.
        assert!(!is_border_side_quad(&Quad { min: [1, 5, 5], ..base_quad(Face::NegX) }));
        assert!(!is_border_side_quad(&Quad { min: [30, 5, 5], ..base_quad(Face::PosX) }));
        assert!(!is_border_side_quad(&Quad { min: [5, 5, 1], ..base_quad(Face::NegZ) }));
        assert!(!is_border_side_quad(&Quad { min: [5, 5, 30], ..base_quad(Face::PosZ) }));
        // Y faces are never border side-quads, even sitting on the top/bottom plane.
        assert!(!is_border_side_quad(&Quad { min: [5, 0, 5], ..base_quad(Face::NegY) }));
        assert!(!is_border_side_quad(&Quad { min: [5, 31, 5], ..base_quad(Face::PosY) }));
    }

    fn packed_of(mesh: &Mesh) -> Vec<pack::UnpackedVertex> {
        use bevy::mesh::VertexAttributeValues;
        let VertexAttributeValues::Uint32x2(packed) = mesh
            .attribute(crate::renderer::voxel_material::ATTRIBUTE_VOXEL_PACKED)
            .unwrap()
        else {
            panic!("packed attribute is Uint32x2")
        };
        packed.iter().map(|&p| pack::unpack(p)).collect()
    }

    #[test]
    fn skirted_mesh_shifts_up_and_extends_border_side_faces_down() {
        // Full-solid 32^3 grid, no neighbors (air shell): every outer face is
        // visible and each of the 6 faces merges to one quad.
        let voxels = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
        let neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        let mesh = mesh_lod_grid(&voxels, &neighbors, &BlockTextureLayers::default())
            .expect("solid grid meshes");
        let verts = packed_of(&mesh);

        for face in [Face::NegX, Face::PosX, Face::NegZ, Face::PosZ] {
            let ys: Vec<u32> = verts.iter().filter(|v| v.face == face).map(|v| v.pos[1]).collect();
            assert!(!ys.is_empty(), "{face:?} must be present");
            assert_eq!(ys.iter().copied().min(), Some(0), "{face:?}: skirt reaches y=0");
            assert_eq!(ys.iter().copied().max(), Some(33), "{face:?}: biased top reaches y=33");
        }
        let top_ys: Vec<u32> =
            verts.iter().filter(|v| v.face == Face::PosY).map(|v| v.pos[1]).collect();
        assert!(!top_ys.is_empty());
        assert!(top_ys.iter().all(|&y| y == 33), "top face sits at biased y=33, not skirted");

        let bottom_ys: Vec<u32> =
            verts.iter().filter(|v| v.face == Face::NegY).map(|v| v.pos[1]).collect();
        assert!(!bottom_ys.is_empty());
        assert!(bottom_ys.iter().all(|&y| y == 1), "bottom face sits at biased y=1, not skirted");
    }

    #[test]
    fn interior_side_faces_are_not_skirted() {
        // A lone solid cell away from every grid edge: its side faces must
        // stay at the biased unit span, with no skirt reaching y=0.
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        voxels[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        let mesh = mesh_lod_grid(&voxels, &neighbors, &BlockTextureLayers::default())
            .expect("lone voxel meshes");
        let verts = packed_of(&mesh);

        for face in [Face::NegX, Face::PosX, Face::NegZ, Face::PosZ] {
            let ys: Vec<u32> = verts.iter().filter(|v| v.face == face).map(|v| v.pos[1]).collect();
            assert!(!ys.is_empty(), "{face:?} must be present");
            assert_eq!(ys.iter().copied().min(), Some(6), "{face:?}: biased, unextended");
            assert_eq!(ys.iter().copied().max(), Some(7), "{face:?}: biased, unextended");
            assert!(ys.iter().all(|&y| y != 0), "{face:?}: no skirt geometry at y=0");
        }
    }
}
