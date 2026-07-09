//! Binary greedy meshing: occupancy packed as per-axis u64 column masks over
//! the padded 34³ space; face culling via bitshifts; greedy merging (task 3)
//! over per-slice bitplanes with a flat merge-key grid. Pure functions — no
//! Bevy types here.

use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

use super::ao::VERTEX_AO_LUT;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    PosX,
    NegX,
    PosY,
    NegY,
    PosZ,
    NegZ,
}

impl Face {
    pub fn from_axis(axis: usize, positive: bool) -> Self {
        match (axis, positive) {
            (0, true) => Face::PosX,
            (0, false) => Face::NegX,
            (1, true) => Face::PosY,
            (1, false) => Face::NegY,
            (2, true) => Face::PosZ,
            (2, false) => Face::NegZ,
            _ => unreachable!("axis is 0..3"),
        }
    }

    pub fn axis(self) -> usize {
        match self {
            Face::PosX | Face::NegX => 0,
            Face::PosY | Face::NegY => 1,
            Face::PosZ | Face::NegZ => 2,
        }
    }

    pub fn positive(self) -> bool {
        matches!(self, Face::PosX | Face::PosY | Face::PosZ)
    }

    pub fn normal(self) -> [f32; 3] {
        let mut n = [0.0; 3];
        n[self.axis()] = if self.positive() { 1.0 } else { -1.0 };
        n
    }

    /// Packed-format face id (see meshing/pack.rs and voxel_texture.wgsl —
    /// the WGSL FACE_NORMALS table is indexed by this; order is locked).
    pub fn index(self) -> u32 {
        match self {
            Face::PosX => 0,
            Face::NegX => 1,
            Face::PosY => 2,
            Face::NegY => 3,
            Face::PosZ => 4,
            Face::NegZ => 5,
        }
    }

    pub fn from_index(i: u32) -> Self {
        match i {
            0 => Face::PosX,
            1 => Face::NegX,
            2 => Face::PosY,
            3 => Face::NegY,
            4 => Face::PosZ,
            5 => Face::NegZ,
            _ => unreachable!("face index is 0..6"),
        }
    }
}

/// A greedy-merged rectangle of identical (block, AO) faces. `min` is the
/// chunk-local voxel coord of the rectangle's minimum cell; `w` extends along
/// `cross_axes(axis).0`, `h` along `.1`. `ao` corner order (cu, cv) =
/// (0,0), (1,0), (0,1), (1,1) over (U, V).
#[derive(Debug, Clone, PartialEq)]
pub struct Quad {
    pub min: [u32; 3],
    pub w: u32,
    pub h: u32,
    pub face: Face,
    pub block: BlockId,
    pub ao: [u8; 4],
}

/// Greedy-mesh a padded chunk. Each visibility column is computed once and
/// transposed into per-slice row bitplanes; per slice, visible cells get a
/// packed (block, ao) merge key in a flat grid, then rectangles are grown
/// with bit scans — width along U within a row, height along V across rows —
/// splitting wherever the key changes.
pub fn mesh_quads(p: &PaddedVoxels) -> Vec<Quad> {
    let occ = Occupancy::new(p);
    let mut quads = Vec::new();

    // Merge-key scratch reused across all six directions: `keys[v][u]` is a
    // packed (block, ao) key, valid only for cells whose visibility bit is
    // set in the slice being merged.
    let mut keys = [[0u64; 33]; 33];
    const STRIDE: [usize; 3] = [1, PADDED, PADDED * PADDED];

    for axis in 0..3 {
        let (ua, va) = cross_axes(axis);
        let cols = &occ.cols[axis];
        for positive in [true, false] {
            let face = Face::from_axis(axis, positive);
            // Transpose: one face_bits call per column, scattered to
            // per-slice row bitplanes (`planes[a][v]` = bits over u).
            let mut planes = [[0u64; 33]; 33];
            for v in 1..=32usize {
                for u in 1..=32usize {
                    let mut bits = occ.face_bits(axis, positive, u, v);
                    while bits != 0 {
                        let a = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        planes[a][v] |= 1 << u;
                    }
                }
            }
            for a in 1..=32usize {
                // Merge keys for this slice's visible cells. AO samples live
                // in the layer the face looks into (bit `b` of the eight
                // neighboring occupancy columns) — same rule as ao::face_ao,
                // which stays the reference implementation.
                let b = if positive { a + 1 } else { a - 1 };
                let mut any = false;
                for v in 1..=32usize {
                    let mut row = planes[a][v];
                    if row == 0 {
                        continue;
                    }
                    any = true;
                    while row != 0 {
                        let u = row.trailing_zeros() as usize;
                        row &= row - 1;
                        let block =
                            p.0[a * STRIDE[axis] + u * STRIDE[ua] + v * STRIDE[va]].block;
                        let s = |uu: usize, vv: usize| (cols[uu][vv] >> b & 1) as usize;
                        // Corner order (cu, cv) = (0,0), (1,0), (0,1), (1,1);
                        // index = side_u << 2 | side_v << 1 | corner.
                        let ao = [
                            VERTEX_AO_LUT[s(u - 1, v) << 2 | s(u, v - 1) << 1 | s(u - 1, v - 1)],
                            VERTEX_AO_LUT[s(u + 1, v) << 2 | s(u, v - 1) << 1 | s(u + 1, v - 1)],
                            VERTEX_AO_LUT[s(u - 1, v) << 2 | s(u, v + 1) << 1 | s(u - 1, v + 1)],
                            VERTEX_AO_LUT[s(u + 1, v) << 2 | s(u, v + 1) << 1 | s(u + 1, v + 1)],
                        ];
                        keys[v][u] =
                            (block.0 as u64) << 32 | u32::from_le_bytes(ao) as u64;
                    }
                }
                if !any {
                    continue;
                }
                for v in 1..=32usize {
                    while planes[a][v] != 0 {
                        let row = planes[a][v];
                        let u0 = row.trailing_zeros() as usize;
                        let key = keys[v][u0];
                        // Width: the visible run, split at the first key change.
                        let run = (row >> u0).trailing_ones() as usize;
                        let mut w = 1usize;
                        while w < run && keys[v][u0 + w] == key {
                            w += 1;
                        }
                        let mask = (((1u128 << w) - 1) as u64) << u0;
                        planes[a][v] &= !mask;
                        // Grow height while the same u-run is fully visible
                        // with the same key.
                        let mut h = 1usize;
                        'grow: while v + h <= 32 && planes[a][v + h] & mask == mask {
                            for du in 0..w {
                                if keys[v + h][u0 + du] != key {
                                    break 'grow;
                                }
                            }
                            planes[a][v + h] &= !mask;
                            h += 1;
                        }
                        let mut min = [0u32; 3];
                        min[axis] = a as u32 - 1;
                        min[ua] = u0 as u32 - 1;
                        min[va] = v as u32 - 1;
                        quads.push(Quad {
                            min,
                            w: w as u32,
                            h: h as u32,
                            face,
                            block: BlockId((key >> 32) as u16),
                            ao: (key as u32).to_le_bytes(),
                        });
                    }
                }
            }
        }
    }
    quads
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
        let mut i = 0;
        for z in 0..PADDED {
            for y in 0..PADDED {
                for x in 0..PADDED {
                    if !p.0[i].is_air() {
                        cols[0][y][z] |= 1 << x;
                        cols[1][x][z] |= 1 << y;
                        cols[2][x][y] |= 1 << z;
                    }
                    i += 1;
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

    #[test]
    fn lone_voxel_yields_six_unit_quads() {
        let p = PaddedVoxels::from_interior(&interior_with(&[([5, 6, 7], BlockId(1))]));
        let quads = mesh_quads(&p);
        assert_eq!(quads.len(), 6);
        for q in &quads {
            assert_eq!((q.w, q.h), (1, 1));
            assert_eq!(q.min, [5, 6, 7]);
            assert_eq!(q.block, BlockId(1));
            assert_eq!(q.ao, [3, 3, 3, 3], "isolated voxel is unoccluded");
        }
        let mut faces: Vec<Face> = quads.iter().map(|q| q.face).collect();
        faces.dedup();
        assert_eq!(faces.len(), 6, "one quad per face direction");
    }

    #[test]
    fn full_slab_merges_to_single_quads_per_face() {
        // One-voxel-thick 32×32 slab at y=5: ±Y faces must merge to a single
        // 32×32 quad each; the four rims merge to 32×1.
        let mut cells = Vec::new();
        for x in 0..32 {
            for z in 0..32 {
                cells.push(([x, 5, z], BlockId(1)));
            }
        }
        let p = PaddedVoxels::from_interior(&interior_with(&cells));
        let quads = mesh_quads(&p);
        let top: Vec<&Quad> = quads.iter().filter(|q| q.face == Face::PosY).collect();
        assert_eq!(top.len(), 1, "uniform top face merges fully");
        assert_eq!((top[0].w, top[0].h), (32, 32));
        assert_eq!(top[0].min, [0, 5, 0]);
        // Total: 2 big faces + 4 rim strips = 6 quads.
        assert_eq!(quads.len(), 6);
    }

    #[test]
    fn differing_block_ids_do_not_merge() {
        let p = PaddedVoxels::from_interior(&interior_with(&[
            ([5, 5, 5], BlockId(1)),
            ([6, 5, 5], BlockId(2)),
        ]));
        let quads = mesh_quads(&p);
        let tops: Vec<&Quad> = quads.iter().filter(|q| q.face == Face::PosY).collect();
        assert_eq!(tops.len(), 2, "different blocks stay separate quads");
        let at = |min: [u32; 3]| tops.iter().find(|q| q.min == min).expect("quad at min");
        assert_eq!(at([5, 5, 5]).block, BlockId(1));
        assert_eq!(at([6, 5, 5]).block, BlockId(2));
    }

    #[test]
    fn differing_ao_does_not_merge() {
        // A 2×1 floor pair whose left cell gets a corner darkened by a block
        // sitting diagonally above-left: AO differs, so top faces must not merge.
        let p = PaddedVoxels::from_interior(&interior_with(&[
            ([5, 5, 5], BlockId(1)),
            ([6, 5, 5], BlockId(1)),
            ([4, 6, 5], BlockId(1)), // occluder for (5,5,5)'s top face
        ]));
        let quads = mesh_quads(&p);
        let tops: Vec<&Quad> = quads
            .iter()
            .filter(|q| q.face == Face::PosY && q.min[1] == 5)
            .collect();
        assert_eq!(tops.len(), 2, "AO mismatch splits the merge");
    }

    #[test]
    fn covered_area_matches_visible_faces() {
        // Decomposing quads into unit faces must reproduce exactly the visible
        // face set — no overlap, no gap. Deterministic pseudo-random content.
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for cell in v.iter_mut() {
            if rng() % 4 == 0 {
                *cell = Voxel::new(BlockId((rng() % 3 + 1) as u16));
            }
        }
        let p = PaddedVoxels::from_interior(&v);
        let occ = Occupancy::new(&p);
        let quads = mesh_quads(&p);

        let mut covered = std::collections::BTreeSet::new();
        for q in &quads {
            let (ua, va) = cross_axes(q.face.axis());
            for du in 0..q.w {
                for dv in 0..q.h {
                    let mut cell = q.min;
                    cell[ua] += du;
                    cell[va] += dv;
                    let fresh = covered.insert((q.face.axis(), q.face.positive(), cell));
                    assert!(fresh, "quads must not overlap");
                }
            }
        }
        let mut visible = std::collections::BTreeSet::new();
        for axis in 0..3 {
            let (ua, va) = cross_axes(axis);
            for u in 1..=32usize {
                for v2 in 1..=32usize {
                    for positive in [true, false] {
                        let bits = occ.face_bits(axis, positive, u, v2);
                        for a in 1..=32usize {
                            if bits >> a & 1 == 1 {
                                let mut cell = [0u32; 3];
                                cell[axis] = a as u32 - 1;
                                cell[ua] = u as u32 - 1;
                                cell[va] = v2 as u32 - 1;
                                visible.insert((axis, positive, cell));
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(covered, visible);
    }

    #[test]
    fn hot_path_ao_matches_face_ao_reference() {
        // The mesher computes AO from occupancy bit columns; every unit cell
        // of every quad must carry exactly what the reference ao::face_ao
        // gives for that cell (merging already requires uniform AO per quad).
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for cell in v.iter_mut() {
            if rng() % 4 == 0 {
                *cell = Voxel::new(BlockId((rng() % 3 + 1) as u16));
            }
        }
        let p = PaddedVoxels::from_interior(&v);
        for q in mesh_quads(&p) {
            let axis = q.face.axis();
            let (ua, va) = cross_axes(axis);
            for du in 0..q.w {
                for dv in 0..q.h {
                    let mut cell = q.min;
                    cell[ua] += du;
                    cell[va] += dv;
                    let pos = [
                        cell[0] as usize + 1,
                        cell[1] as usize + 1,
                        cell[2] as usize + 1,
                    ];
                    assert_eq!(
                        q.ao,
                        super::super::ao::face_ao(&p, axis, q.face.positive(), pos),
                        "face {:?} cell {cell:?}",
                        q.face
                    );
                }
            }
        }
    }

    #[test]
    fn face_index_roundtrips_in_locked_order() {
        let order = [Face::PosX, Face::NegX, Face::PosY, Face::NegY, Face::PosZ, Face::NegZ];
        for (i, f) in order.into_iter().enumerate() {
            assert_eq!(f.index(), i as u32);
            assert_eq!(Face::from_index(i as u32), f);
        }
    }
}
