use game::{derive_voxels, voxel_index, Chunk, VoxelType};
use std::hash::Hash;

fn crc(c: &Chunk) -> u32 {
    let mut h = crc32fast::Hasher::new();
    c.hash(&mut h);
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
    assert_eq!(voxels[voxel_index(0, 7, 0)].kind, VoxelType::Black, "y<8 solid");
    assert_eq!(voxels[voxel_index(0, 8, 0)].kind, VoxelType::Air, "y>=8 air");
}
