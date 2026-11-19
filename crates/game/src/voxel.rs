use std::collections::BTreeMap;

use block_mesh::ndshape::ConstShape3u32;
use block_mesh::{MergeVoxel, VoxelVisibility};

use crate::block::{
    local_index, voxel_index, Block, BlockIndex, ChiselData, BLOCK_VOXELS, BRICKS_PER_CHUNK_EDGE, CHUNK_BLOCKS,
    CHUNK_BLOCK_COUNT,
};
use crate::registry::BlockId;

/// Edge of a render/LOD brick in voxels: the dense voxel array the mesher, the
/// LOD grids and the client consume. Not the sim chunk (see `CHUNK_UNITS`).
pub const BRICK_VOXELS: usize = 32;
pub const BRICK_BLOCKS: usize = BRICK_VOXELS / BLOCK_VOXELS;
pub type BrickShape = ConstShape3u32<32, 32, 32>;

pub const BRICK_VOXEL_COUNT: usize = BRICK_VOXELS * BRICK_VOXELS * BRICK_VOXELS;

/// A chunk's authoritative block grid, `CHUNK_BLOCK_COUNT` blocks indexed by
/// [`BlockIndex`]. Always canonical: a grid whose blocks are all equal is
/// `Uniform`, never a `Dense` box, so equal contents serialize and hash
/// equally (every constructor and [`ChunkBlocks::set`] normalise).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ChunkBlocks {
    /// Every block is this one.
    Uniform(Block),
    /// Length `CHUNK_BLOCK_COUNT`, at least two distinct blocks.
    Dense(DenseBlocks),
}

/// A dense grid's blocks and the digest of what they hash as, once asked for
/// ([`DenseBlocks::digest`]). The blocks are private: every write goes
/// through [`ChunkBlocks::set`], which forgets the digest — an undo's write
/// included. On the wire and in the store it is the block list alone.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct DenseBlocks {
    blocks: Box<[Block]>,
    #[serde(skip)]
    digest: DigestCache,
}

impl DenseBlocks {
    fn new(blocks: Box<[Block]>) -> Self {
        Self { blocks, digest: DigestCache::default() }
    }

    /// `(crc32, length)` of the grid's state-hash byte stream ([`dense_stream`]),
    /// computed on the first ask after a write.
    fn digest(&self) -> (u32, u64) {
        if let Some(found) = self.digest.get() {
            return found;
        }
        let (mut crc, mut len) = (crc32fast::Hasher::new(), 0u64);
        dense_stream(&self.blocks, |bytes| {
            crc.update(bytes);
            len += bytes.len() as u64;
        });
        let digest = (crc.finalize(), len);
        self.digest.set(digest);
        digest
    }
}

impl PartialEq for DenseBlocks {
    fn eq(&self, other: &Self) -> bool {
        self.blocks == other.blocks
    }
}
impl Eq for DenseBlocks {}
impl std::hash::Hash for DenseBlocks {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.blocks.hash(state);
    }
}

/// A [`DenseBlocks`] digest, or none: bit 63 set, the stream's length in bits
/// 32..63 and its crc32 in the low half. An atomic, not a `Cell`: region
/// snapshots are shared between threads (`Arc<GameData>`), and two threads
/// filling it write the same value.
#[derive(Debug, Default)]
struct DigestCache(std::sync::atomic::AtomicU64);

impl DigestCache {
    const SET: u64 = 1 << 63;

    fn get(&self) -> Option<(u32, u64)> {
        let v = self.0.load(std::sync::atomic::Ordering::Relaxed);
        (v & Self::SET != 0).then_some((v as u32, (v & !Self::SET) >> 32))
    }

    fn set(&self, (crc, len): (u32, u64)) {
        debug_assert!(len < 1 << 31);
        self.0.store(Self::SET | len << 32 | crc as u64, std::sync::atomic::Ordering::Relaxed);
    }

    fn clear(&mut self) {
        *self.0.get_mut() = 0;
    }
}

impl Clone for DigestCache {
    fn clone(&self) -> Self {
        Self(std::sync::atomic::AtomicU64::new(self.0.load(std::sync::atomic::Ordering::Relaxed)))
    }
}

/// The bytes a `Dense` grid hashes as, fed to `sink` in batches: exactly the
/// stream `#[derive(StateHash)]` wrote for `ChunkBlocks::Dense(Box<[Block]>)`
/// — the variant's index as `u32`, the slice's length as `u64`, then each
/// [`Block`]: `id` as `u16` and `data` as a 3-long sequence (a `u64` 3 and
/// the three bytes), all native-endian — so every state hash and golden is
/// unchanged. Batched because five hasher writes per block, 20k per chunk,
/// made the region thread's terrain fingerprint and state hash 100-190 ms of
/// a 50 ms tick in a dev build (2026-11-19 perf capture).
fn dense_stream(blocks: &[Block], mut sink: impl FnMut(&[u8])) {
    /// Bytes of one hashed [`Block`], and blocks per batch.
    const PER: usize = 2 + 8 + 3;
    const BATCH: usize = 256;
    let mut head = [0u8; 12];
    head[..4].copy_from_slice(&1u32.to_ne_bytes());
    head[4..].copy_from_slice(&(blocks.len() as u64).to_ne_bytes());
    sink(&head);
    let data_len = 3u64.to_ne_bytes();
    let mut buf = [0u8; PER * BATCH];
    for batch in blocks.chunks(BATCH) {
        for (out, block) in buf.chunks_exact_mut(PER).zip(batch) {
            out[..2].copy_from_slice(&block.id.0.to_ne_bytes());
            out[2..10].copy_from_slice(&data_len);
            out[10..].copy_from_slice(&block.data);
        }
        sink(&buf[..PER * batch.len()]);
    }
}

/// The derive's byte stream (see [`dense_stream`]), for a hasher whose
/// `write_*` are the `std` defaults (native-endian bytes into `write`), as
/// [`crate::StateHasher`]'s are. A dense grid hashed into a `StateHasher`
/// that takes digests ([`crate::StateHasher::taking_digests`]) hands over its
/// cached digest instead of its 53 kB: a region's terrain is nearly all of
/// its hashed state and almost never changes between two hashes, so only
/// edited chunks are read again.
impl state_hash::StateHash for ChunkBlocks {
    fn state_hash<H: std::hash::Hasher>(&self, h: &mut H) {
        match self {
            ChunkBlocks::Uniform(block) => {
                h.write_u32(0);
                block.state_hash(h);
            }
            ChunkBlocks::Dense(dense) => {
                if crate::digests_taken() {
                    let (crc, len) = dense.digest();
                    crate::hand_digest(h, crc, len);
                } else {
                    dense_stream(&dense.blocks, |bytes| h.write(bytes));
                }
            }
        }
    }
}

impl Default for ChunkBlocks {
    /// All air.
    fn default() -> Self {
        ChunkBlocks::Uniform(Block::new(BlockId::AIR))
    }
}

impl ChunkBlocks {
    /// The canonical form of a linear block list (`len == CHUNK_BLOCK_COUNT`).
    pub fn from_vec(blocks: Vec<Block>) -> Self {
        assert_eq!(blocks.len(), CHUNK_BLOCK_COUNT, "a chunk holds exactly CHUNK_BLOCK_COUNT blocks");
        match Self::all_equal(&blocks) {
            Some(b) => ChunkBlocks::Uniform(b),
            None => ChunkBlocks::Dense(DenseBlocks::new(blocks.into_boxed_slice())),
        }
    }

    fn all_equal(blocks: &[Block]) -> Option<Block> {
        let first = blocks[0];
        blocks.iter().all(|b| *b == first).then_some(first)
    }

    pub fn get(&self, i: BlockIndex) -> Block {
        match self {
            ChunkBlocks::Uniform(b) => *b,
            ChunkBlocks::Dense(dense) => dense.blocks[i.linear()],
        }
    }

    /// Write one block, keeping the canonical form: a differing write into a
    /// `Uniform` grid densifies it; a `Dense` grid whose blocks all become
    /// equal collapses to `Uniform` (an O(`CHUNK_BLOCK_COUNT`) check — edits
    /// are rare).
    pub fn set(&mut self, i: BlockIndex, b: Block) {
        match self {
            ChunkBlocks::Uniform(u) => {
                if *u != b {
                    let mut blocks = vec![*u; CHUNK_BLOCK_COUNT].into_boxed_slice();
                    blocks[i.linear()] = b;
                    *self = ChunkBlocks::Dense(DenseBlocks::new(blocks));
                }
            }
            ChunkBlocks::Dense(dense) => {
                dense.blocks[i.linear()] = b;
                dense.digest.clear();
                if let Some(u) = Self::all_equal(&dense.blocks) {
                    *self = ChunkBlocks::Uniform(u);
                }
            }
        }
    }

    /// The single block of a `Uniform` grid.
    pub fn uniform(&self) -> Option<Block> {
        match self {
            ChunkBlocks::Uniform(b) => Some(*b),
            ChunkBlocks::Dense(_) => None,
        }
    }

    /// Every block in `BlockIndex` order.
    pub fn iter(&self) -> impl Iterator<Item = (BlockIndex, Block)> + '_ {
        (0..CHUNK_BLOCK_COUNT).map(move |i| {
            let bi = BlockIndex::from_linear(i);
            (bi, self.get(bi))
        })
    }
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize, ::borrow::Partial, Hash, state_hash::StateHash)]
#[module(crate)]
pub struct Chunk {
    /// Authoritative block grid, `CHUNK_BLOCK_COUNT` blocks, canonical.
    pub blocks: ChunkBlocks,
    /// Sparse sub-block detail; a block is chiseled iff present here.
    pub chisel: BTreeMap<BlockIndex, ChiselData>,
}

#[cfg(test)]
thread_local! {
    /// `Chunk::clone` calls on this thread (lib tests only): lets a test pin
    /// that a hot path copies no chunk.
    pub(crate) static CHUNK_CLONES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Field-for-field what `#[derive(Clone)]` generates, plus the test-only
/// clone counter.
impl Clone for Chunk {
    fn clone(&self) -> Self {
        #[cfg(test)]
        CHUNK_CLONES.with(|c| c.set(c.get() + 1));
        Self { blocks: self.blocks.clone(), chisel: self.chisel.clone() }
    }
}

impl Default for Chunk {
    /// All-air. `Component<T>` requires `T: Default`; real content comes from
    /// constructors like [`Chunk::flat_floor`].
    fn default() -> Self {
        Self { blocks: ChunkBlocks::default(), chisel: BTreeMap::new() }
    }
}

impl Chunk {
    /// Solid floor for voxel heights `y < depth` using an arbitrary non-air
    /// material — a dev/test convenience. Production worldgen uses
    /// [`Chunk::flat_floor_with`] to place a registry-resolved material.
    pub fn flat_floor(depth: u32) -> Self {
        Self::flat_floor_with(depth, BlockId(1))
    }

    /// Solid `solid`-material floor for voxel heights `y < depth`, air above,
    /// built at block granularity: whole blocks below the floor, a chiseled
    /// slab for the block straddling it, air above.
    pub fn flat_floor_with(depth: u32, solid: BlockId) -> Self {
        let depth = depth as usize;
        let mut blocks = vec![Block::new(BlockId::AIR); CHUNK_BLOCK_COUNT];
        let mut chisel = BTreeMap::new();
        for by in 0..CHUNK_BLOCKS {
            let (y0, y1) = (by * BLOCK_VOXELS, by * BLOCK_VOXELS + BLOCK_VOXELS);
            for bx in 0..CHUNK_BLOCKS {
                for bz in 0..CHUNK_BLOCKS {
                    let bi = BlockIndex::from_xyz(bx, by, bz);
                    if y1 <= depth {
                        blocks[bi.linear()] = Block::new(solid);
                    } else if y0 >= depth {
                        // stays Air
                    } else {
                        blocks[bi.linear()] = Block::new(solid);
                        let top = [BLOCK_VOXELS, depth - y0, BLOCK_VOXELS];
                        chisel.insert(bi, ChiselData::cuboid(solid, [0, 0, 0], top));
                    }
                }
            }
        }
        Self { blocks: ChunkBlocks::from_vec(blocks), chisel }
    }

    /// Block at block-granularity coords (each axis 0..CHUNK_BLOCKS).
    pub fn block_at(&self, bx: usize, by: usize, bz: usize) -> Block {
        self.blocks.get(BlockIndex::from_xyz(bx, by, bz))
    }

    /// Set a whole block, clearing any chisel detail for that block cell
    /// (whole-block granularity — sub-block chisel is not addressed here).
    pub fn set_block(&mut self, bx: usize, by: usize, bz: usize, block: Block) {
        let bi = BlockIndex::from_xyz(bx, by, bz);
        self.blocks.set(bi, block);
        self.chisel.remove(&bi);
    }

    /// A chunk uniformly filled with `block` (no chisel detail).
    pub fn uniform(block: Block) -> Self {
        Self { blocks: ChunkBlocks::Uniform(block), chisel: BTreeMap::new() }
    }

    /// True iff every block equals `block` (full Block, incl. `data`) and there
    /// is no chisel detail. O(1): the grid is canonical.
    pub fn is_uniform(&self, block: Block) -> bool {
        self.chisel.is_empty() && self.blocks.uniform() == Some(block)
    }

    /// Topmost non-air voxel of the voxel column `(vx, vz)` (each `0..CHUNK_UNITS`):
    /// its local Y and material. Read straight from blocks plus chisel — no
    /// dense voxel array. A chiseled block shows its chisel materials (air where
    /// empty); a plain block shows its id.
    pub fn column_top(&self, vx: u32, vz: u32) -> Option<(u32, BlockId)> {
        let (vx, vz) = (vx as usize, vz as usize);
        let (bx, lx) = (vx / BLOCK_VOXELS, vx % BLOCK_VOXELS);
        let (bz, lz) = (vz / BLOCK_VOXELS, vz % BLOCK_VOXELS);
        for by in (0..CHUNK_BLOCKS).rev() {
            let bi = BlockIndex::from_xyz(bx, by, bz);
            let base = (by * BLOCK_VOXELS) as u32;
            match self.chisel.get(&bi) {
                Some(chisel) => {
                    for ly in (0..BLOCK_VOXELS).rev() {
                        let m = chisel.material_at(lx, ly, lz);
                        if m != BlockId::AIR {
                            return Some((base + ly as u32, m));
                        }
                    }
                }
                None => {
                    let id = self.blocks.get(bi).id;
                    if id != BlockId::AIR {
                        return Some((base + BLOCK_VOXELS as u32 - 1, id));
                    }
                }
            }
        }
        None
    }

    /// True iff every block is air and no chisel detail remains.
    pub fn is_all_air(&self) -> bool {
        self.is_uniform(Block::new(BlockId::AIR))
    }
}

/// One 32³ render/LOD brick of a chunk (`BRICK_BLOCKS`³ blocks), read in
/// place: LOD builders and worldgen consume chunks through this, so they stay
/// correct whether a chunk is one brick or `BRICKS_PER_CHUNK_EDGE`³ of them.
/// [`BrickView::voxels`] is the one dense form, and it is per brick.
#[derive(Clone, Copy)]
pub struct BrickView<'a> {
    chunk: &'a Chunk,
    /// Chunk-local block coords of the brick's minimum corner.
    origin: [usize; 3],
}

impl Chunk {
    /// Brick `b` (each axis `0..BRICKS_PER_CHUNK_EDGE`) of this chunk.
    pub fn brick(&self, b: [usize; 3]) -> BrickView<'_> {
        debug_assert!(b.iter().all(|&v| v < BRICKS_PER_CHUNK_EDGE), "brick {b:?} outside the chunk");
        BrickView { chunk: self, origin: b.map(|v| v * BRICK_BLOCKS) }
    }

    /// Every brick with its chunk-local brick coords, x fastest, then y, then z.
    pub fn bricks(&self) -> impl Iterator<Item = ([usize; 3], BrickView<'_>)> {
        let n = BRICKS_PER_CHUNK_EDGE;
        (0..n * n * n).map(move |i| {
            let b = [i % n, (i / n) % n, i / (n * n)];
            (b, self.brick(b))
        })
    }
}

impl BrickView<'_> {
    fn index(&self, bx: usize, by: usize, bz: usize) -> BlockIndex {
        debug_assert!(bx < BRICK_BLOCKS && by < BRICK_BLOCKS && bz < BRICK_BLOCKS);
        BlockIndex::from_xyz(self.origin[0] + bx, self.origin[1] + by, self.origin[2] + bz)
    }

    /// Block at brick-local block coords (each axis `0..BRICK_BLOCKS`).
    pub fn block_at(&self, bx: usize, by: usize, bz: usize) -> Block {
        self.chunk.blocks.get(self.index(bx, by, bz))
    }

    /// Chisel detail of a brick-local block, if it is chiseled.
    pub fn chisel(&self, bx: usize, by: usize, bz: usize) -> Option<&ChiselData> {
        self.chunk.chisel.get(&self.index(bx, by, bz))
    }

    /// The single block of a brick whose blocks are all equal and unchiseled.
    /// O(1) on a `Uniform` chunk without chisel detail.
    pub fn uniform(&self) -> Option<Block> {
        if let (Some(b), true) = (self.chunk.blocks.uniform(), self.chunk.chisel.is_empty()) {
            return Some(b);
        }
        let first = self.block_at(0, 0, 0);
        for bz in 0..BRICK_BLOCKS {
            for by in 0..BRICK_BLOCKS {
                for bx in 0..BRICK_BLOCKS {
                    if self.block_at(bx, by, bz) != first || self.chisel(bx, by, bz).is_some() {
                        return None;
                    }
                }
            }
        }
        Some(first)
    }

    /// The brick's dense `BRICK_VOXEL_COUNT` voxels in `BrickShape` order,
    /// derived from its own blocks and chisel only.
    pub fn voxels(&self) -> Vec<Voxel> {
        if let Some(b) = self.uniform() {
            return vec![Voxel::new(b.id); BRICK_VOXEL_COUNT];
        }
        let mut voxels = vec![Voxel::new(BlockId::AIR); BRICK_VOXEL_COUNT];
        for bz in 0..BRICK_BLOCKS {
            for by in 0..BRICK_BLOCKS {
                for bx in 0..BRICK_BLOCKS {
                    let id = self.block_at(bx, by, bz).id;
                    let materials = self.chisel(bx, by, bz).map(ChiselData::materials);
                    for vz in 0..BLOCK_VOXELS {
                        for vy in 0..BLOCK_VOXELS {
                            for vx in 0..BLOCK_VOXELS {
                                let m = materials.as_ref().map_or(id, |m| m[local_index(vx, vy, vz)]);
                                let li = voxel_index(
                                    bx * BLOCK_VOXELS + vx,
                                    by * BLOCK_VOXELS + vy,
                                    bz * BLOCK_VOXELS + vz,
                                );
                                voxels[li] = Voxel::new(m);
                            }
                        }
                    }
                }
            }
        }
        voxels
    }
}

/// World brick coords of brick `b` of the chunk at `chunk` (x/z as given —
/// region-local for region chunk lists — and signed y).
pub fn brick_coords(chunk: ChunkCoords, b: [usize; 3]) -> [i64; 3] {
    let n = BRICKS_PER_CHUNK_EDGE as i64;
    [
        chunk.x as i64 * n + b[0] as i64,
        chunk.y as i64 * n + b[1] as i64,
        chunk.z as i64 * n + b[2] as i64,
    ]
}

// Keep the struct's EXISTING derive list verbatim — only the `y` field type
// changes (usize -> i32). Do not add/remove derives.
#[derive(
    Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, PartialOrd, Ord, Eq,
)]
pub struct ChunkCoords {
    pub x: usize,
    pub y: i32,
    pub z: usize,
}

impl ChunkCoords {
    pub fn new(x: usize, y: i32, z: usize) -> Self {
        Self { x, y, z }
    }
}

/// Region-local (x, z) address of a vertical column, 0..REGION_CHUNKS.
#[derive(
    Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash, state_hash::StateHash, PartialOrd, Ord,
)]
pub struct ColumnCoords {
    pub x: usize,
    pub z: usize,
}

impl ColumnCoords {
    pub fn new(x: usize, z: usize) -> Self {
        Self { x, z }
    }
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Copy, Clone, Hash, state_hash::StateHash, PartialEq, Eq)]
pub struct Voxel {
    pub block: BlockId,
}

impl Voxel {
    pub fn new(block: BlockId) -> Self {
        Self { block }
    }

    pub fn is_air(&self) -> bool {
        self.block == BlockId::AIR
    }
}

impl block_mesh::Voxel for Voxel {
    fn get_visibility(&self) -> VoxelVisibility {
        if self.is_air() {
            VoxelVisibility::Empty
        } else {
            VoxelVisibility::Opaque
        }
    }
}

impl MergeVoxel for Voxel {
    type MergeValue = BlockId;
    type MergeValueFacingNeighbour = BlockId;

    fn merge_value(&self) -> Self::MergeValue {
        self.block
    }

    fn merge_value_facing_neighbour(&self) -> Self::MergeValueFacingNeighbour {
        self.block
    }
}
