use game::{Block, BlockId, Column, ColumnGround, UNDERGROUND_DEPTH};

fn id(n: u16) -> BlockId {
    BlockId(n)
}

#[test]
fn bedrock_starts_sixty_below_each_columns_surface() {
    let (stone, bedrock) = (id(1), id(2));
    let g = ColumnGround::plain([100, 90, 100, 100], 0, stone, bedrock);
    assert_eq!(UNDERGROUND_DEPTH, 60);
    assert_eq!(g.block_at(0, 0, 41).id, stone);
    assert_eq!(g.block_at(0, 0, 40).id, bedrock);
    assert_eq!(g.block_at(0, 1, 30).id, bedrock);
    assert_eq!(g.block_at(0, 1, 31).id, stone);
}

#[test]
fn strata_lookup_is_by_stratigraphic_coordinate() {
    let (a, b, c) = (id(10), id(11), id(12));
    let mut g = ColumnGround::plain([0; 4], 0, a, id(2));
    g.strata = vec![(10, a), (0, b), (-20, c)];
    g.geo = [5; 4];
    for (y, want) in [(20, a), (5, a), (4, b), (-15, b), (-16, c), (-40, c)] {
        assert_eq!(g.block_at(1, 1, y).id, want, "y={y}");
    }
}

#[test]
fn column_ground_survives_bincode() {
    let g = ColumnGround::plain([7; 4], -3, id(1), id(2));
    let col = Column::new(0, Block::new(id(1)), Block::new(id(0)), vec![]).with_ground(g.clone());
    let bytes = bincode::serialize(&col).unwrap();
    let back: Column = bincode::deserialize(&bytes).unwrap();
    assert_eq!(back.ground(), Some(&g));
    assert!(g.is_implied(-4) && !g.is_implied(-3));
}
