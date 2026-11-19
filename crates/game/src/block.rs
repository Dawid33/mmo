//! The block layer: a `Block` (16³ voxels = 1 m³) is the authoritative gameplay
//! unit; voxels are a subdivision derived on demand via [`derive_voxels`].
//! Sub-block detail ("chiseling") is sparse — only chiseled blocks carry a
//! [`ChiselData`], a canonical cuboid list. This module is Bevy-free and
//! deterministic.

use crate::block_props;
use block_mesh::ndshape::ConstShape;
use parry3d::math::IVector;

use crate::registry::BlockId;
use crate::voxel::{BrickShape, Chunk, Voxel, BRICK_VOXEL_COUNT};

pub const BLOCK_VOXELS: usize = 16; // 1 block = 16³ voxels = 1 m³
/// Blocks per axis of a sim chunk — the unit that owns one entity, one rapier
/// body and one collider set. A BUILD-time dial (`VOXEL_CHUNK_BLOCKS`, set in
/// `.cargo/config.toml [env]`, `just chunk_blocks=N …`) for the same reason as
/// `REGION_BLOCKS`: client, server and the wasm LocalServer must agree on it
/// bit-exactly. Legal values are exactly 2 and 16 — the only sizes with pinned
/// goldens (2 = the pre-2026-10-29 size); anything else fails the BUILD.
pub const CHUNK_BLOCKS: usize = parse_chunk_blocks(option_env!("VOXEL_CHUNK_BLOCKS"));

/// `option_env!` hands over a string; unset or empty means the default.
const fn parse_chunk_blocks(var: Option<&str>) -> usize {
    const DEFAULT: usize = 16;
    let Some(text) = var else { return DEFAULT };
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return DEFAULT;
    }
    let mut value = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i].is_ascii_digit(), "VOXEL_CHUNK_BLOCKS must be a number");
        value = value * 10 + (bytes[i] - b'0') as usize;
        i += 1;
    }
    assert!(
        value == 2 || value == 16,
        "VOXEL_CHUNK_BLOCKS must be 2 or 16 (the only chunk sizes with pinned goldens)"
    );
    value
}
pub const CHUNK_BLOCK_COUNT: usize = CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS;
/// Sim-chunk edge in voxels = world units. Not the render brick (`BRICK_VOXELS`).
pub const CHUNK_UNITS: usize = CHUNK_BLOCKS * BLOCK_VOXELS;
/// Bricks along one chunk edge.
pub const BRICKS_PER_CHUNK_EDGE: usize = CHUNK_UNITS / crate::voxel::BRICK_VOXELS;
const _: () = assert!(CHUNK_UNITS % crate::voxel::BRICK_VOXELS == 0);
pub const BLOCK_VOXEL_COUNT: usize = BLOCK_VOXELS * BLOCK_VOXELS * BLOCK_VOXELS; // = 4096
/// Edge of a half block, and the cell size of the half-block collider grid.
pub const HALF_BLOCK_VOXELS: usize = BLOCK_VOXELS / 2; // = 8

/// Linear voxel index within a 32³ render brick, matching `BrickShape` (x fastest).
/// Reuses `voxel::BrickShape` so the 32³ layout has a single source of truth.
pub fn voxel_index(x: usize, y: usize, z: usize) -> usize {
    BrickShape::linearize([x as u32, y as u32, z as u32]) as usize
}

/// Index of a block within a chunk, linear over `CHUNK_BLOCKS³`
/// (x fastest, then y, then z). `u16` so it covers a 16³ chunk (4096).
/// `Ord` so it can key a `BTreeMap` deterministically.
#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, state_hash::StateHash, serde::Serialize, serde::Deserialize, Debug,
)]
pub struct BlockIndex(pub u16);

const _: () = assert!(CHUNK_BLOCK_COUNT <= u16::MAX as usize + 1);

impl BlockIndex {
    /// The block at linear position `i` (`0..CHUNK_BLOCK_COUNT`).
    pub fn from_linear(i: usize) -> Self {
        debug_assert!(i < CHUNK_BLOCK_COUNT, "block index {i} out of a {CHUNK_BLOCK_COUNT}-block chunk");
        BlockIndex(i as u16)
    }
    /// Linear position of this block, `0..CHUNK_BLOCK_COUNT`.
    pub fn linear(self) -> usize {
        self.0 as usize
    }
    pub fn from_xyz(bx: usize, by: usize, bz: usize) -> Self {
        Self::from_linear(bx + by * CHUNK_BLOCKS + bz * CHUNK_BLOCKS * CHUNK_BLOCKS)
    }
    pub fn xyz(self) -> (usize, usize, usize) {
        let i = self.0 as usize;
        (
            i % CHUNK_BLOCKS,
            (i / CHUNK_BLOCKS) % CHUNK_BLOCKS,
            i / (CHUNK_BLOCKS * CHUNK_BLOCKS),
        )
    }
}

/// Bits of `Block.data` holding a tree id.
pub const TREE_ID_BITS: u32 = 18;
pub const TREE_ID_MASK: u32 = (1 << TREE_ID_BITS) - 1;
/// Largest limb index (the 6 bits above the tree id).
pub const MAX_TREE_LIMB: u8 = 63;

/// The authoritative per-cell gameplay value. Compact; `data` is block state:
/// for a tree block, its tree id in bits 0..18 (0 = not a tree) and its limb
/// in bits 18..24 (0 = the trunk; little-endian), else zero. Hashes
/// trivially.
#[derive(
    Copy, Clone, Default, PartialEq, Eq, Hash, state_hash::StateHash, serde::Serialize, serde::Deserialize, Debug,
)]
pub struct Block {
    pub id: BlockId,
    pub data: [u8; 3],
}

impl Block {
    pub fn new(id: BlockId) -> Self {
        Self { id, data: [0; 3] }
    }

    /// A trunk block (limb 0) of tree `tree`: [`Block::with_tree`].
    pub fn with_tree_id(id: BlockId, tree: u32) -> Self {
        Self::with_tree(id, tree, 0)
    }

    /// A block of tree `tree` (non-zero, at most [`TREE_ID_BITS`]) — log,
    /// branch or leaves — on limb `limb` (0 = the trunk, at most
    /// [`MAX_TREE_LIMB`]), both in `data`.
    pub fn with_tree(id: BlockId, tree: u32, limb: u8) -> Self {
        debug_assert!(tree <= TREE_ID_MASK, "tree id {tree:#X} wider than {TREE_ID_BITS} bits");
        debug_assert!(limb <= MAX_TREE_LIMB, "limb {limb} over {MAX_TREE_LIMB}");
        let [a, b, c, _] = ((tree & TREE_ID_MASK) | u32::from(limb & MAX_TREE_LIMB) << TREE_ID_BITS).to_le_bytes();
        Self { id, data: [a, b, c] }
    }

    fn data_u32(&self) -> u32 {
        let [a, b, c] = self.data;
        u32::from_le_bytes([a, b, c, 0])
    }

    /// The tree this block belongs to, 0 if none.
    pub fn tree_id(&self) -> u32 {
        self.data_u32() & TREE_ID_MASK
    }

    /// The tree limb this block belongs to (0 = the trunk, or not a tree).
    pub fn limb(&self) -> u8 {
        (self.data_u32() >> TREE_ID_BITS) as u8 & MAX_TREE_LIMB
    }
}

/// A chiseled block's 16³ voxels, decoded: per voxel (x fastest, then y,
/// then z) the palette index it holds, `None` where it is empty.
pub type ChiselGrid = Vec<Option<u8>>;

/// Sub-block detail for a chiseled block, stored the way Vintage Story stores
/// microblocks: axis-aligned cuboids on the 16³ voxel grid, one `u32` each
/// (min x/y/z and max-1 x/y/z in 4 bits apiece, palette index in the top 8),
/// plus a small material palette. A half block is one cuboid.
///
/// The list is canonical: every constructor and edit goes through the dense
/// [`ChiselGrid`] and one fixed greedy merge ([`ChiselData::from_grid`]), so
/// equal voxels always give an equal list — equal serialization, equal
/// hashes. Undo restores a block's whole chisel entry, so no per-voxel inverse is
/// needed. Edit in bulk through the grid; [`ChiselData::set`] re-merges on
/// every call.
#[derive(Clone, Default, PartialEq, Eq, Hash, state_hash::StateHash, serde::Serialize, serde::Deserialize, Debug)]
pub struct ChiselData {
    cuboids: Vec<u32>,
    palette: Vec<BlockId>, // ≤ 256 materials, indexed by a cuboid's top byte
}

pub(crate) fn local_index(vx: usize, vy: usize, vz: usize) -> usize {
    vx + vy * BLOCK_VOXELS + vz * BLOCK_VOXELS * BLOCK_VOXELS
}

fn pack_cuboid(min: [usize; 3], max: [usize; 3], material: u8) -> u32 {
    (min[0] | min[1] << 4 | min[2] << 8 | (max[0] - 1) << 12 | (max[1] - 1) << 16 | (max[2] - 1) << 20)
        as u32
        | (material as u32) << 24
}

/// (min, exclusive max, palette index) of a packed cuboid.
fn unpack_cuboid(c: u32) -> ([usize; 3], [usize; 3], u8) {
    let f = |shift: u32| ((c >> shift) & 0xF) as usize;
    ([f(0), f(4), f(8)], [f(12) + 1, f(16) + 1, f(20) + 1], (c >> 24) as u8)
}

impl ChiselData {
    /// No voxels yet.
    pub fn new(palette: Vec<BlockId>) -> Self {
        assert!(palette.len() <= 256, "a chisel palette holds at most 256 materials");
        Self { cuboids: Vec::new(), palette }
    }

    /// One solid cuboid of `material`, voxels `min..max` (exclusive). Packed
    /// directly: a lone box is already its own canonical merge (the greedy
    /// walk starts at its min corner and grows to exactly its bounds).
    pub fn cuboid(material: BlockId, min: [usize; 3], max: [usize; 3]) -> Self {
        let empty = (0..3).any(|a| min[a] >= max[a]);
        debug_assert!(max.iter().all(|&m| m <= BLOCK_VOXELS), "cuboid past the block: {max:?}");
        let cuboids = if empty { Vec::new() } else { vec![pack_cuboid(min, max, 0)] };
        Self { cuboids, palette: vec![material] }
    }

    /// Built voxel by voxel: `f(x, y, z)` is the palette index there, or
    /// `None` for empty.
    pub fn from_fn(palette: Vec<BlockId>, f: impl Fn(usize, usize, usize) -> Option<u8>) -> Self {
        let mut grid = vec![None; BLOCK_VOXEL_COUNT];
        for vz in 0..BLOCK_VOXELS {
            for vy in 0..BLOCK_VOXELS {
                for vx in 0..BLOCK_VOXELS {
                    grid[local_index(vx, vy, vz)] = f(vx, vy, vz);
                }
            }
        }
        Self::from_grid(palette, &grid)
    }

    /// The canonical cuboid list of a dense grid. Visits voxels in grid order
    /// (x fastest); each filled voxel not yet covered starts a 1³ box that
    /// grows +x, +y, +z in turn, one layer at a time, while the new layer is
    /// entirely uncovered voxels of the same palette index.
    pub fn from_grid(palette: Vec<BlockId>, grid: &ChiselGrid) -> Self {
        assert!(palette.len() <= 256, "a chisel palette holds at most 256 materials");
        debug_assert_eq!(grid.len(), BLOCK_VOXEL_COUNT);
        let mut covered = vec![false; BLOCK_VOXEL_COUNT];
        let mut cuboids = Vec::new();
        for start in 0..BLOCK_VOXEL_COUNT {
            let Some(m) = grid[start] else { continue };
            if covered[start] {
                continue;
            }
            let min = [start % BLOCK_VOXELS, (start / BLOCK_VOXELS) % BLOCK_VOXELS, start / (BLOCK_VOXELS * BLOCK_VOXELS)];
            let mut max = [min[0] + 1, min[1] + 1, min[2] + 1];
            // The layer just past `max` on `axis` is growable.
            let growable = |max: &[usize; 3], axis: usize| {
                if max[axis] == BLOCK_VOXELS {
                    return false;
                }
                let (mut lo, mut hi) = (min, *max);
                lo[axis] = max[axis];
                hi[axis] = max[axis] + 1;
                (lo[2]..hi[2]).all(|z| {
                    (lo[1]..hi[1]).all(|y| {
                        (lo[0]..hi[0]).all(|x| {
                            let i = local_index(x, y, z);
                            grid[i] == Some(m) && !covered[i]
                        })
                    })
                })
            };
            loop {
                let mut grew = false;
                for axis in 0..3 {
                    if growable(&max, axis) {
                        max[axis] += 1;
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
            for z in min[2]..max[2] {
                for y in min[1]..max[1] {
                    for x in min[0]..max[0] {
                        covered[local_index(x, y, z)] = true;
                    }
                }
            }
            cuboids.push(pack_cuboid(min, max, m));
        }
        Self { cuboids, palette }
    }

    /// The dense grid (x fastest, then y, then z).
    pub fn grid(&self) -> ChiselGrid {
        let mut grid = vec![None; BLOCK_VOXEL_COUNT];
        for &c in &self.cuboids {
            let (min, max, m) = unpack_cuboid(c);
            for z in min[2]..max[2] {
                for y in min[1]..max[1] {
                    for x in min[0]..max[0] {
                        grid[local_index(x, y, z)] = Some(m);
                    }
                }
            }
        }
        grid
    }

    /// Per voxel (grid order) the material it shows: `AIR` where empty, or
    /// where its palette index has no material.
    pub fn materials(&self) -> Vec<BlockId> {
        self.grid().into_iter().map(|m| m.map_or(BlockId::AIR, |m| self.material(m))).collect()
    }

    fn material(&self, index: u8) -> BlockId {
        self.palette.get(index as usize).copied().unwrap_or(BlockId::AIR)
    }

    pub fn palette(&self) -> &[BlockId] {
        &self.palette
    }

    /// The cuboids as (min, exclusive max, material).
    pub fn cuboids(&self) -> impl Iterator<Item = ([usize; 3], [usize; 3], BlockId)> + '_ {
        self.cuboids.iter().map(|&c| {
            let (min, max, m) = unpack_cuboid(c);
            (min, max, self.material(m))
        })
    }

    pub fn is_empty(&self) -> bool {
        self.cuboids.is_empty()
    }

    /// Fill (`solid`) or clear one voxel. Decodes and re-merges the whole
    /// block: for many voxels, edit a [`ChiselData::grid`] and rebuild with
    /// [`ChiselData::from_grid`] instead.
    pub fn set(&mut self, vx: usize, vy: usize, vz: usize, solid: bool, material_idx: u8) {
        let mut grid = self.grid();
        grid[local_index(vx, vy, vz)] = solid.then_some(material_idx);
        *self = Self::from_grid(std::mem::take(&mut self.palette), &grid);
    }

    fn index_at(&self, vx: usize, vy: usize, vz: usize) -> Option<u8> {
        self.cuboids.iter().find_map(|&c| {
            let (min, max, m) = unpack_cuboid(c);
            let inside = (min[0]..max[0]).contains(&vx)
                && (min[1]..max[1]).contains(&vy)
                && (min[2]..max[2]).contains(&vz);
            inside.then_some(m)
        })
    }

    pub fn is_solid(&self, vx: usize, vy: usize, vz: usize) -> bool {
        self.index_at(vx, vy, vz).is_some()
    }

    pub fn material_at(&self, vx: usize, vy: usize, vz: usize) -> BlockId {
        self.index_at(vx, vy, vz).map_or(BlockId::AIR, |m| self.material(m))
    }
}

/// Chunk collider cells split by grid scale, each solid region at the
/// coarsest aligned cell it fills: whole blocks on the block grid, 8³
/// half-block cells on the half-block grid, and the remaining chisel voxels
/// on the voxel grid. Together they cover exactly the solid cells of
/// [`derive_voxels`] — this is that function's collider-facing counterpart,
/// kept coarse instead of expanded to a dense voxel grid. Chiseled and plain blocks
/// go through the same rule: a chisel that fills its block is a block cell, a
/// half-block chisel is four half cells.
#[derive(Debug, Clone, PartialEq)]
pub struct ColliderCells {
    /// Block-grid keys (0..CHUNK_BLOCKS per axis) of wholly solid blocks.
    pub blocks: Vec<IVector>,
    /// Half-block-grid keys (0..CHUNK_BLOCKS*2 per axis) of wholly solid 8³
    /// octants of chiseled blocks.
    pub halves: Vec<IVector>,
    /// Voxel-grid keys (0..CHUNK_BLOCKS*BLOCK_VOXELS per axis) of the other
    /// solid chisel voxels (occupied with a non-air material).
    pub voxels: Vec<IVector>,
}

impl ColliderCells {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty() && self.halves.is_empty() && self.voxels.is_empty()
    }

    /// The non-empty grids as (cell edge in voxels, keys), coarsest first:
    /// one parry `Voxels` collider each. Every collider builder walks this,
    /// so all of them attach the same shapes in the same order.
    pub fn grids(&self) -> impl Iterator<Item = (f32, &[IVector])> {
        [
            (BLOCK_VOXELS as f32, self.blocks.as_slice()),
            (HALF_BLOCK_VOXELS as f32, self.halves.as_slice()),
            (1.0, self.voxels.as_slice()),
        ]
        .into_iter()
        .filter(|(_, cells)| !cells.is_empty())
    }

    /// True iff every block of the chunk is wholly solid, i.e. its collision
    /// volume is exactly the full chunk cube (`CHUNK_UNITS`³ voxels).
    pub fn is_full_cube(&self) -> bool {
        self.blocks.len() == CHUNK_BLOCK_COUNT
    }
}

/// Decompose a chunk's block layer into collider cells. Solidity matches
/// [`derive_voxels`]: an unchiseled block is solid iff its id is non-air; a
/// chisel voxel is solid iff occupied with a non-air palette material.
/// Deterministic: blocks in `BlockIndex` order; within a chiseled block,
/// octants x-fastest, and an octant's voxels x-outer/z-inner.
pub fn collider_cells(chunk: &Chunk) -> ColliderCells {
    let mut out = ColliderCells { blocks: Vec::new(), halves: Vec::new(), voxels: Vec::new() };
    let chisel = &chunk.chisel;
    // A uniform unchiseled chunk: one block key per block, or none for air.
    if let (Some(block), true) = (chunk.blocks.uniform(), chisel.is_empty()) {
        if block_props().is_solid(block.id) && block.id != BlockId::AIR {
            out.blocks = (0..CHUNK_BLOCK_COUNT)
                .map(|i| {
                    let (bx, by, bz) = BlockIndex::from_linear(i).xyz();
                    IVector::new(bx as i32, by as i32, bz as i32)
                })
                .collect();
        }
        return out;
    }
    for (bi, block) in chunk.blocks.iter() {
        push_block_cells(&mut out, bi, block, chisel.get(&bi));
    }
    out
}

/// The collider cells of block `i` of `chunk` alone, in the chunk-local keys
/// [`collider_cells`] uses: a block edit diffs these before and after the
/// write instead of decomposing the whole chunk again.
pub fn collider_cells_of_block(chunk: &Chunk, i: BlockIndex) -> ColliderCells {
    let mut out = ColliderCells { blocks: Vec::new(), halves: Vec::new(), voxels: Vec::new() };
    push_block_cells(&mut out, i, chunk.blocks.get(i), chunk.chisel.get(&i));
    out
}

/// The collider cells of one block on its own, keyed as block (0, 0, 0) of a
/// chunk — the same rule a chunk applies to it (non-solid blocks none, tree
/// wood on the half-block grid). For shapes built outside a chunk
/// (`voxel_body`).
pub fn block_collider_cells(block: Block, chisel: Option<&ChiselData>) -> ColliderCells {
    let mut out = ColliderCells { blocks: Vec::new(), halves: Vec::new(), voxels: Vec::new() };
    push_block_cells(&mut out, BlockIndex::from_xyz(0, 0, 0), block, chisel);
    out
}

/// Appends block `bi`'s cells to `out`: one block key for a solid block,
/// half keys for a partial chisel's solid octants and voxel keys for its
/// other solid voxels, or nothing for air. A `ColliderScale::Half` block
/// (tree wood) has no voxel keys: an octant is one half key when at least
/// `HALF_CELL_FILL_MIN` of its voxels are solid, else open.
fn push_block_cells(out: &mut ColliderCells, bi: BlockIndex, block: Block, chisel: Option<&ChiselData>) {
    let h = HALF_BLOCK_VOXELS;
    let (bx, by, bz) = bi.xyz();
    let Some(c) = chisel else {
        if block.id != BlockId::AIR && block_props().is_solid(block.id) {
            out.blocks.push(IVector::new(bx as i32, by as i32, bz as i32));
        }
        return;
    };
    let props = block_props();
    let solid: Vec<bool> =
        c.materials().into_iter().map(|m| m != BlockId::AIR && props.is_solid(m)).collect();
    if solid.iter().all(|&s| s) {
        out.blocks.push(IVector::new(bx as i32, by as i32, bz as i32));
        return;
    }
    let half_grid = props.collider_scale(block.id) == crate::ColliderScale::Half;
    for (ox, oy, oz) in (0..8).map(|o| (o % 2, (o / 2) % 2, o / 4)) {
        let (x0, y0, z0) = (ox * h, oy * h, oz * h);
        let octant = |f: &mut dyn FnMut(usize, usize, usize)| {
            for vx in x0..x0 + h {
                for vy in y0..y0 + h {
                    for vz in z0..z0 + h {
                        f(vx, vy, vz);
                    }
                }
            }
        };
        let mut filled = 0u32;
        octant(&mut |x, y, z| filled += solid[local_index(x, y, z)] as u32);
        let cell_min = if half_grid { crate::HALF_CELL_FILL_MIN } else { (h * h * h) as u32 };
        if filled >= cell_min {
            out.halves.push(IVector::new(
                (bx * 2 + ox) as i32,
                (by * 2 + oy) as i32,
                (bz * 2 + oz) as i32,
            ));
            continue;
        }
        if half_grid {
            continue; // an octant under the threshold is open
        }
        octant(&mut |x, y, z| {
            if solid[local_index(x, y, z)] {
                out.voxels.push(IVector::new(
                    (bx * BLOCK_VOXELS + x) as i32,
                    (by * BLOCK_VOXELS + y) as i32,
                    (bz * BLOCK_VOXELS + z) as i32,
                ));
            }
        });
    }
}

/// Derive the dense 32³ voxels of render brick `brick` (each axis
/// `0..BRICKS_PER_CHUNK_EDGE`) straight from the chunk's blocks and chisel —
/// an oracle independent of [`crate::BrickView::voxels`], kept for tests and
/// tools. Output order is `BrickShape` (index with [`voxel_index`]). Never a
/// whole chunk: dense voxel arrays are per brick.
pub fn derive_voxels(chunk: &Chunk, brick: [usize; 3]) -> Vec<Voxel> {
    use crate::voxel::BRICK_BLOCKS;
    let chisel = &chunk.chisel;
    let origin = brick.map(|b| b * BRICK_BLOCKS);
    let mut voxels = vec![Voxel::new(BlockId::AIR); BRICK_VOXEL_COUNT];
    for lz in 0..BRICK_BLOCKS {
        for ly in 0..BRICK_BLOCKS {
            for lx in 0..BRICK_BLOCKS {
                let bi = BlockIndex::from_xyz(origin[0] + lx, origin[1] + ly, origin[2] + lz);
                let block = chunk.blocks.get(bi);
                let materials = chisel.get(&bi).map(ChiselData::materials);
                for vz in 0..BLOCK_VOXELS {
                    for vy in 0..BLOCK_VOXELS {
                        for vx in 0..BLOCK_VOXELS {
                            let id = match &materials {
                                Some(m) => m[local_index(vx, vy, vz)],
                                None => block.id,
                            };
                            let li = voxel_index(
                                lx * BLOCK_VOXELS + vx,
                                ly * BLOCK_VOXELS + vy,
                                lz * BLOCK_VOXELS + vz,
                            );
                            voxels[li] = Voxel::new(id);
                        }
                    }
                }
            }
        }
    }
    voxels
}

