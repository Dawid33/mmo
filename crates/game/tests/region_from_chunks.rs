use game::{Chunk, ChunkCoords, Region, RegionCoords, REGION_CHUNKS};

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn from_chunks_builds_a_region_with_all_chunk_entities() {
    let chunks: Vec<(ChunkCoords, Chunk)> = (0..REGION_CHUNKS)
        .flat_map(|x| {
            (0..REGION_CHUNKS).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8)))
        })
        .collect();
    let region = Region::from_chunks(RegionCoords::new(0, 0), chunks);
    // One sim entity per chunk (World::basic parity: REGION_CHUNKS² grid).
    assert_eq!(region.data().ecs.entities.len(), REGION_CHUNKS * REGION_CHUNKS);
}
