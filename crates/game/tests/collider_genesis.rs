//! Genesis collider build (Region::from_chunks -> seed_terrain_genesis):
//! fully-solid chunks collapse to a single cuboid, partial chunks keep the
//! parry Voxels shape, and the build is deterministic (bit-exact hash).
use game::parry::shape::ShapeType;
use game::{Chunk, ChunkCoords, Region, RegionId};
use std::hash::Hash;

fn hash_region(r: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    r.data().data.hash(&mut h);
    h.finalize()
}

// flat_floor(depth) fills the bottom `depth` voxel-layers. 32 -> every one of
// the 32^3 cells solid (full chunk); 8 -> a partial (surface-like) chunk.
fn full_chunk() -> Chunk {
    Chunk::flat_floor(32)
}
fn partial_chunk() -> Chunk {
    Chunk::flat_floor(8)
}

fn build() -> Region {
    Region::from_chunks(
        RegionId::new(0, 0),
        vec![
            (ChunkCoords::new(0, 0, 0), full_chunk()),
            (ChunkCoords::new(1, 0, 0), partial_chunk()),
        ],
    )
}

#[test]
fn full_chunk_becomes_cuboid_partial_stays_voxels() {
    let region = build();
    let shapes: Vec<ShapeType> = region
        .data()
        .physics
        .colliders
        .iter()
        .map(|(_, c)| c.shape().shape_type())
        .collect();
    assert!(
        shapes.contains(&ShapeType::Cuboid),
        "fully-solid chunk should collapse to a cuboid; got {shapes:?}"
    );
    assert!(
        shapes.contains(&ShapeType::Voxels),
        "partial chunk should keep a voxels shape; got {shapes:?}"
    );
    // Exactly one collider per non-empty chunk (both chunks are non-empty).
    assert_eq!(shapes.len(), 2, "one collider per chunk");
}

#[test]
fn genesis_is_deterministic() {
    assert_eq!(
        hash_region(&build()),
        hash_region(&build()),
        "independent genesis builds must hash bit-exactly identical"
    );
}
