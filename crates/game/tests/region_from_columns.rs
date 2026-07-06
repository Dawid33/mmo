use game::{Block, BlockId, Chunk, Column, ColumnCoords, RegionCoords, SubChunk};

fn stone() -> Block { Block::new(BlockId(2)) }
fn air() -> Block { Block::new(BlockId::AIR) }

fn dense_floor() -> SubChunk {
    SubChunk::Dense(Box::new(Chunk::flat_floor_with(8, BlockId(2))))
}

#[test]
fn materialized_sub_chunks_skip_air() {
    // base_sub_y = 0, one dense floor, then (trimmed) air above.
    let col = Column::new(0, stone(), air(), vec![dense_floor(), SubChunk::Air]);
    let mats = col.materialized_sub_chunks();
    assert_eq!(mats.len(), 1);
    assert_eq!(mats[0].0, 0); // sub_y
}

#[test]
fn from_columns_builds_a_region() {
    let id = RegionCoords::new(0, 0);
    let columns = vec![
        (ColumnCoords::new(0, 0), Column::new(0, stone(), air(), vec![dense_floor()])),
        (ColumnCoords::new(1, 0), Column::new(0, stone(), air(), vec![dense_floor()])),
    ];
    // Must not panic; produces a region with the two flattened sub-chunks.
    let _region = game::Region::from_columns(id, columns);
}
