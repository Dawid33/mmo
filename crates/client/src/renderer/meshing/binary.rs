//! Binary greedy meshing: occupancy packed as per-axis u64 column masks over
//! the padded 34³ space; face culling via bitshifts; greedy merging (task 3)
//! over per-slice bitplanes with a flat merge-key grid. Pure functions — no
//! Bevy types here.
//!
//! Cutout blocks (`game::RenderMode::Cutout`, leaves) are see-through: they
//! hide no face of a neighbour, darken no AO corner, and are open space for
//! burial and cave connectivity. A [`CutoutTable`] says which ids they are;
//! [`CutoutTable::NONE`] meshes everything opaque (the LOD rings).

use game::{BlockId, BlockProps, Voxel, BRICK_VOXEL_COUNT};

use super::ao::VERTEX_AO_LUT;

/// Whether a cutout face against ANY cutout neighbour is culled (Minecraft's
/// "fast" leaves: a crown is drawn as its outer shell). Any, not only the
/// same block: where two species' leaves touch, both double-sided faces
/// would otherwise be coplanar and z-fight; leaves are alpha-tested with
/// holes, so one face between them is too many already. `false` draws every
/// leaf/leaf face ("fancy"), at many more quads.
pub const CULL_CUTOUT_CUTOUT_FACES: bool = true;

/// Which block ids are cutout, and which are liquid, as two bitsets over
/// every `u16` id (8 KiB each): one lookup per voxel in the mesher's hot
/// loops, built once per mesh task. Liquid blocks are EMPTY to the chunk
/// mesher — no faces, never opaque, open space for burial and cave
/// connectivity — since the water pass (`renderer::water`) draws them.
pub struct CutoutTable([u64; 1024], [u64; 1024]);

impl CutoutTable {
    /// No cutout and no liquid blocks: everything non-air is opaque.
    pub const NONE: CutoutTable = CutoutTable([0; 1024], [0; 1024]);

    fn bits(ids: impl IntoIterator<Item = BlockId>) -> [u64; 1024] {
        let mut t = [0u64; 1024];
        for id in ids {
            t[id.0 as usize >> 6] |= 1 << (id.0 & 63);
        }
        t
    }

    /// Cutout `ids`, no liquids.
    pub fn from_ids(ids: impl IntoIterator<Item = BlockId>) -> Self {
        Self(Self::bits(ids), [0; 1024])
    }

    /// This table with `ids` liquid.
    pub fn with_liquids(self, ids: impl IntoIterator<Item = BlockId>) -> Self {
        Self(self.0, Self::bits(ids))
    }

    pub fn from_props(props: &BlockProps) -> Self {
        Self::from_ids(props.cutout_ids()).with_liquids(props.liquid_ids())
    }

    /// From the process-global props (`game::block_props`).
    pub fn installed() -> Self {
        Self::from_props(game::block_props())
    }

    #[inline]
    pub fn is_cutout(&self, id: BlockId) -> bool {
        self.0[id.0 as usize >> 6] >> (id.0 & 63) & 1 == 1
    }

    #[inline]
    pub fn is_liquid(&self, id: BlockId) -> bool {
        self.1[id.0 as usize >> 6] >> (id.0 & 63) & 1 == 1
    }

    /// Air or liquid: nothing the chunk mesher draws.
    #[inline]
    pub fn is_empty(&self, v: Voxel) -> bool {
        v.is_air() || self.is_liquid(v.block)
    }

    /// Not empty and not cutout: hides faces, occludes AO, buries.
    #[inline]
    pub fn is_opaque(&self, v: Voxel) -> bool {
        !self.is_empty(v) && !self.is_cutout(v.block)
    }
}

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

    /// Non-air. Only the LOD mesher (`lod::lod_quads`, with
    /// [`CutoutTable::NONE`]) asks: its grids are any-solid cells and hold
    /// no liquid (the server maps it to air, `game::lod::terrain_id`; the
    /// water rides in the key's `LodWater`), so non-air is solid there.
    pub fn is_solid(&self, x: usize, y: usize, z: usize) -> bool {
        !self.get(x, y, z).is_air()
    }

    /// Not empty and not cutout (see [`CutoutTable::is_opaque`]).
    pub fn is_opaque(&self, x: usize, y: usize, z: usize, cutout: &CutoutTable) -> bool {
        cutout.is_opaque(self.get(x, y, z))
    }

    /// Interior from a chunk's 32³ voxels, shell all-air (isolated chunk).
    pub fn from_interior(voxels: &[Voxel]) -> Self {
        debug_assert_eq!(voxels.len(), BRICK_VOXEL_COUNT);
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
/// (0,0), (1,0), (0,1), (1,1) over (U, V). `cutout`: the block is drawn
/// alpha-tested (`CutoutTable`), in the pulled path's cutout pipeline.
/// `interior`: every cell of the quad faces into a dug space
/// (`interior::InteriorGround`; the x-ray view opens what lies in front of
/// those faces) — part of the merge key, so a quad never mixes the two.
#[derive(Debug, Clone, PartialEq)]
pub struct Quad {
    pub min: [u32; 3],
    pub w: u32,
    pub h: u32,
    pub face: Face,
    pub block: BlockId,
    pub ao: [u8; 4],
    pub cutout: bool,
    pub interior: bool,
}

/// Greedy-mesh a padded chunk. Each visibility column is computed once and
/// transposed into per-slice row bitplanes; per slice, visible cells get a
/// packed (block, ao) merge key in a flat grid, then rectangles are grown
/// with bit scans — width along U within a row, height along V across rows —
/// splitting wherever the key changes. `cutout` names the see-through blocks
/// (`CutoutTable::NONE`: all opaque).
pub fn mesh_quads(p: &PaddedVoxels, cutout: &CutoutTable) -> Vec<Quad> {
    mesh_quads_classified(p, cutout, None)
}

/// Merge-key bit of [`Quad::interior`] (block ids take bits 32..48).
const INTERIOR_KEY_BIT: u64 = 1 << 48;

/// [`mesh_quads`], marking the faces whose air side is dug space per
/// `interior` (`None`: no face is interior).
pub fn mesh_quads_classified(
    p: &PaddedVoxels,
    cutout: &CutoutTable,
    interior: Option<&super::interior::InteriorGround>,
) -> Vec<Quad> {
    let occ = Occupancy::new(p, cutout);
    let mut quads = Vec::new();

    // Merge-key scratch reused across all six directions: `keys[v][u]` is a
    // packed (block, ao) key, valid only for cells whose visibility bit is
    // set in the slice being merged.
    let mut keys = [[0u64; 33]; 33];
    const STRIDE: [usize; 3] = [1, PADDED, PADDED * PADDED];

    for axis in 0..3 {
        let (ua, va) = cross_axes(axis);
        // AO samples the opaque columns: a cutout neighbour darkens nothing.
        let cols = &occ.opaque[axis];
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
                // neighboring opaque columns) — same rule as ao::face_ao,
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
                        let block = p.0[a * STRIDE[axis] + u * STRIDE[ua] + v * STRIDE[va]].block;
                        let s = |uu: usize, vv: usize| (cols[uu][vv] >> b & 1) as usize;
                        // Corner order (cu, cv) = (0,0), (1,0), (0,1), (1,1);
                        // index = side_u << 2 | side_v << 1 | corner.
                        let ao = [
                            VERTEX_AO_LUT[s(u - 1, v) << 2 | s(u, v - 1) << 1 | s(u - 1, v - 1)],
                            VERTEX_AO_LUT[s(u + 1, v) << 2 | s(u, v - 1) << 1 | s(u + 1, v - 1)],
                            VERTEX_AO_LUT[s(u - 1, v) << 2 | s(u, v + 1) << 1 | s(u - 1, v + 1)],
                            VERTEX_AO_LUT[s(u + 1, v) << 2 | s(u, v + 1) << 1 | s(u + 1, v + 1)],
                        ];
                        // The air cell the face looks into, chunk-local
                        // voxels (padded index - 1).
                        let dug = interior.is_some_and(|g| {
                            let mut air = [0i32; 3];
                            air[axis] = b as i32 - 1;
                            air[ua] = u as i32 - 1;
                            air[va] = v as i32 - 1;
                            g.is_dug_voxel(air)
                        });
                        keys[v][u] = (block.0 as u64) << 32
                            | u32::from_le_bytes(ao) as u64
                            | if dug { INTERIOR_KEY_BIT } else { 0 };
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
                        let block = BlockId((key >> 32) as u16);
                        let interior = key & INTERIOR_KEY_BIT != 0;
                        quads.push(Quad {
                            min,
                            w: w as u32,
                            h: h as u32,
                            face,
                            block,
                            ao: (key as u32).to_le_bytes(),
                            cutout: cutout.is_cutout(block),
                            interior,
                        });
                    }
                }
            }
        }
    }
    quads
}

type Columns = Box<[[[u64; PADDED]; PADDED]; 3]>;

fn zero_columns() -> Columns {
    vec![[[0u64; PADDED]; PADDED]; 3].try_into().unwrap()
}

/// Occupancy packed as u64 columns along each axis: `cols[axis][u][v]` has bit
/// `a` set iff the voxel at position `a` along `axis` (cross coords `(u, v)`
/// per `cross_axes`) is non-air; `opaque` the same for non-air, non-cutout
/// voxels; `cutout_pair` bit `a` iff voxels `a` and `a + 1` are both
/// cutout blocks (only with `CULL_CUTOUT_CUTOUT_FACES`). 34 ≤ 64, so one word
/// per column.
pub(crate) struct Occupancy {
    cols: Columns,
    opaque: Columns,
    cutout_pair: Columns,
}

impl Occupancy {
    pub fn new(p: &PaddedVoxels, cutout: &CutoutTable) -> Self {
        const STRIDE: [usize; 3] = [1, PADDED, PADDED * PADDED];
        let mut cols = zero_columns();
        let mut opaque = zero_columns();
        let mut cutout_pair = zero_columns();
        let mut i = 0;
        for z in 0..PADDED {
            for y in 0..PADDED {
                for x in 0..PADDED {
                    let v = p.0[i];
                    if !cutout.is_empty(v) {
                        cols[0][y][z] |= 1 << x;
                        cols[1][x][z] |= 1 << y;
                        cols[2][x][y] |= 1 << z;
                        if !cutout.is_cutout(v.block) {
                            opaque[0][y][z] |= 1 << x;
                            opaque[1][x][z] |= 1 << y;
                            opaque[2][x][y] |= 1 << z;
                        } else if CULL_CUTOUT_CUTOUT_FACES {
                            let pos = [x, y, z];
                            for axis in 0..3 {
                                if pos[axis] + 1 < PADDED {
                                    let n = p.0[i + STRIDE[axis]];
                                    if cutout.is_empty(n) || !cutout.is_cutout(n.block) {
                                        continue;
                                    }
                                    let (ua, va) = cross_axes(axis);
                                    cutout_pair[axis][pos[ua]][pos[va]] |= 1 << pos[axis];
                                }
                            }
                        }
                    }
                    i += 1;
                }
            }
        }
        Self { cols, opaque, cutout_pair }
    }

    /// Visible-face bits along `axis` at cross coords `(u, v)`: bit `a` set
    /// iff the voxel at `a` is non-air and its `positive`/negative neighbor
    /// along `axis` is not opaque — air, or (for an opaque voxel) a cutout
    /// block; a cutout face against another cutout block is culled, see
    /// `CULL_CUTOUT_CUTOUT_FACES`. Shell positions are masked out.
    pub fn face_bits(&self, axis: usize, positive: bool, u: usize, v: usize) -> u64 {
        let col = self.cols[axis][u][v];
        let opq = self.opaque[axis][u][v];
        let pair = self.cutout_pair[axis][u][v];
        let vis = if positive {
            col & !(opq >> 1) & !pair
        } else {
            col & !(opq << 1) & !(pair << 1)
        };
        vis & INTERIOR_MASK
    }
}

