use game::{derive_voxels, voxel_index, BlockId, Chunk};
use game::StateHash;

fn crc(c: &Chunk) -> u32 {
    let mut h = crc32fast::Hasher::new();
    c.state_hash(&mut h);
    h.finalize()
}

#[test]
fn flat_floor_is_block_based_with_a_chiseled_slab() {
    let c = Chunk::flat_floor(8);
    assert!(c.blocks.uniform().is_none(), "a floor mixes solid and air blocks");
    // depth 8 leaves the 4 bottom blocks partially filled → chiseled;
    // the 4 top blocks are fully air.
    assert_eq!(c.chisel.len(), 4, "the 4 bottom blocks are partial → chiseled");
}

#[test]
fn chunk_hash_is_stable_and_bincode_roundtrips() {
    let c = Chunk::flat_floor(12);
    assert_eq!(crc(&c), crc(&c.clone()), "clone must hash identically");
    let bytes = bincode::serialize(&c).unwrap();
    let back: Chunk = bincode::deserialize(&bytes).unwrap();
    assert_eq!(crc(&c), crc(&back), "bincode round-trip must hash identically");
}

#[test]
fn derive_reproduces_floor_height() {
    let c = Chunk::flat_floor(8);
    let voxels = derive_voxels(&c);
    assert_eq!(voxels[voxel_index(0, 7, 0)].block, BlockId(1), "y<8 solid");
    assert_eq!(voxels[voxel_index(0, 8, 0)].block, BlockId::AIR, "y>=8 air");
}

/// Oracle: topmost non-air voxel of column `(vx, vz)` in the dense derivation.
fn oracle_top(voxels: &[game::Voxel], vx: u32, vz: u32) -> Option<(u32, BlockId)> {
    (0..game::CHUNK_UNITS as u32).rev().find_map(|vy| {
        let b = voxels[voxel_index(vx as usize, vy as usize, vz as usize)].block;
        (b != BlockId::AIR).then_some((vy, b))
    })
}

#[test]
fn column_top_matches_derived_voxels() {
    use game::{Block, BlockIndex, ChiselData};
    let mut chunks: Vec<Chunk> = [0u32, 1, 8, 16, 17, game::CHUNK_UNITS as u32]
        .into_iter()
        .map(Chunk::flat_floor)
        .collect();
    let mut half = Chunk::uniform(Block::new(BlockId::AIR));
    half.set_block(1, 0, 1, Block::new(BlockId(2)));
    half.chisel
        .insert(BlockIndex::from_xyz(1, 0, 1), ChiselData::cuboid(BlockId(2), [0, 0, 0], [16, 8, 16]));
    chunks.push(half);
    for chunk in &chunks {
        let voxels = derive_voxels(chunk);
        for vx in 0..game::CHUNK_UNITS as u32 {
            for vz in 0..game::CHUNK_UNITS as u32 {
                assert_eq!(chunk.column_top(vx, vz), oracle_top(&voxels, vx, vz), "column ({vx},{vz})");
            }
        }
    }
}

fn hash_of<T: StateHash>(v: &T) -> u32 {
    let mut h = crc32fast::Hasher::new();
    v.state_hash(&mut h);
    h.finalize()
}

#[test]
fn chunk_blocks_normalise_to_uniform() {
    use game::{Block, BlockIndex, ChunkBlocks, CHUNK_BLOCK_COUNT};
    let stone = Block::new(BlockId(2));
    let fresh = ChunkBlocks::from_vec(vec![stone; CHUNK_BLOCK_COUNT]);
    assert_eq!(fresh, ChunkBlocks::Uniform(stone));
    let mut b = fresh.clone();
    let i = BlockIndex::from_linear(CHUNK_BLOCK_COUNT - 1);
    b.set(i, Block::new(BlockId::AIR));
    assert!(matches!(b, ChunkBlocks::Dense(_)), "a differing write densifies");
    assert_eq!(b.get(i), Block::new(BlockId::AIR));
    assert_eq!(b.uniform(), None);
    b.set(i, stone);
    assert_eq!(b, ChunkBlocks::Uniform(stone), "all-equal dense collapses back");
    assert_eq!(hash_of(&b), hash_of(&fresh));
    assert_eq!(bincode::serialize(&b).unwrap(), bincode::serialize(&fresh).unwrap());
}

#[test]
fn dense_round_trips_bincode_and_hash() {
    use game::{Block, CHUNK_BLOCKS};
    // At 2-block chunks the 17-deep floor is all solid blocks (its upper
    // layer chiselled), i.e. `Uniform`; clearing the top corner block makes
    // it `Dense` at every chunk size.
    let floor = Chunk::flat_floor(17);
    let mut dense = floor.clone();
    let top = CHUNK_BLOCKS - 1;
    dense.set_block(top, top, top, Block::new(BlockId::AIR));
    assert!(matches!(dense.blocks, game::ChunkBlocks::Dense(_)));
    for c in [floor, dense] {
        let bytes = game::encode_packet(&c).unwrap();
        let back: Chunk = game::decode_packet(&bytes).unwrap();
        assert_eq!(back.blocks, c.blocks);
        assert_eq!(back.chisel, c.chisel);
        assert_eq!(crc(&back), crc(&c));
    }
}

#[test]
fn block_index_covers_chunk() {
    use game::{BlockIndex, CHUNK_BLOCK_COUNT};
    for i in 0..CHUNK_BLOCK_COUNT {
        let bi = BlockIndex::from_linear(i);
        let (x, y, z) = bi.xyz();
        assert_eq!(BlockIndex::from_xyz(x, y, z), bi, "index {i}");
        assert_eq!(bi.0 as usize, i);
    }
}
