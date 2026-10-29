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
    assert_eq!(c.blocks.len(), 8, "2×2×2 blocks per chunk");
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
    let voxels = derive_voxels(&c.blocks, &c.chisel);
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
        let voxels = derive_voxels(&chunk.blocks, &chunk.chisel);
        for vx in 0..game::CHUNK_UNITS as u32 {
            for vz in 0..game::CHUNK_UNITS as u32 {
                assert_eq!(chunk.column_top(vx, vz), oracle_top(&voxels, vx, vz), "column ({vx},{vz})");
            }
        }
    }
}
