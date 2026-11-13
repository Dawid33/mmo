use game::{Chunk, ChunkCoords, Region, RegionCoords};

#[test]
fn from_chunks_builds_an_entity_for_every_input_chunk() {
    let chunks = vec![
        (ChunkCoords::new(0, 0, 0), Chunk::flat_floor(8)),
        (ChunkCoords::new(31, 0, 0), Chunk::flat_floor(8)),
        (ChunkCoords::new(0, 0, 31), Chunk::flat_floor(8)),
        (ChunkCoords::new(31, 0, 31), Chunk::flat_floor(8)),
    ];
    let region = Region::from_chunks(RegionCoords::new(0, 0), chunks);
    assert_eq!(region.data().ecs.entities.len(), 4);
}
