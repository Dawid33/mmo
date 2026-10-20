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

fn ground_region() -> game::Region {
    let ground = game::ColumnGround::plain([10; 4], 2, BlockId(2), BlockId(3));
    let col = Column::new(2, stone(), air(), vec![dense_floor()]).with_ground(ground);
    game::Region::from_columns(RegionCoords::new(0, 0), vec![(ColumnCoords::new(0, 0), col)])
}

#[test]
fn implied_ground_below_the_band() {
    let region = ground_region();
    let d = region.data();
    assert_eq!(d.implied_block(game::ChunkCoords::new(0, 1, 0), (0, 0, 0)), Some(stone()));
    assert_eq!(
        d.implied_block(game::ChunkCoords::new(0, -30, 0), (0, 0, 0)),
        Some(Block::new(BlockId(3)))
    );
    // Materialised (a chunk entity exists) and above the band: not implied.
    assert_eq!(d.implied_block(game::ChunkCoords::new(0, 2, 0), (0, 0, 0)), None);
    assert_eq!(d.implied_block(game::ChunkCoords::new(0, 9, 0), (0, 0, 0)), None);
    // A column without ground contributes nothing.
    assert_eq!(d.implied_block(game::ChunkCoords::new(5, 0, 5), (0, 0, 0)), None);
}

#[test]
fn ground_survives_park_and_restore() {
    let region = ground_region();
    let parked = game::SerializedRegion::from_rollback(region.data());
    let back = parked.to_rollback().expect("restores");
    for c in [(0, 1, 0), (0, -30, 0), (0, 2, 0)] {
        let c = game::ChunkCoords::new(c.0, c.1, c.2);
        assert_eq!(region.data().implied_block(c, (1, 1, 1)), back.implied_block(c, (1, 1, 1)));
    }
    assert_eq!(game::state_hash(region.data()), game::state_hash(&back));
}

#[test]
fn cell_mode_has_no_implied_ground() {
    let region = game::Region::new_cells(RegionCoords::new(0, 0), 16, None, None);
    assert_eq!(region.data().implied_block(game::ChunkCoords::new(0, 1, 0), (0, 0, 0)), None);
}
