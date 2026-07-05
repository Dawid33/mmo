use game::{Chunk, ChunkCoords, Region, RegionCoords};

#[test]
fn from_chunks_builds_a_region_with_all_chunk_entities() {
    let chunks: Vec<(ChunkCoords, Chunk)> = (0..8)
        .flat_map(|x| (0..8).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8))))
        .collect();
    let region = Region::from_chunks(RegionCoords::new(0, 0), chunks);
    // One sim entity per chunk (World::basic parity: 8×8 grid).
    assert_eq!(region.data().ecs.entities.len(), 64);
}
