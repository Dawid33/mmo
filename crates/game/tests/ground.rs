use game::{Block, BlockId, Column, ColumnGround, COLUMN_FOOTPRINT, UNDERGROUND_DEPTH};

fn cols(v: &[i16]) -> Box<[i16]> {
    assert_eq!(v.len(), COLUMN_FOOTPRINT);
    v.into()
}

fn fill(v: i16) -> Box<[i16]> {
    vec![v; COLUMN_FOOTPRINT].into_boxed_slice()
}

fn id(n: u16) -> BlockId {
    BlockId(n)
}

#[test]
fn bedrock_starts_sixty_below_each_columns_surface() {
    let (stone, bedrock) = (id(1), id(2));
    // Every column's surface at 100 except (lx 0, lz 1) — index 1 — at 90.
    let mut surface = vec![100; COLUMN_FOOTPRINT];
    surface[1] = 90;
    let g = ColumnGround::plain(cols(&surface), 0, stone, bedrock);
    assert_eq!(UNDERGROUND_DEPTH, 60);
    assert_eq!(g.block_at(0, 0, 41).id, stone);
    assert_eq!(g.block_at(0, 0, 40).id, bedrock);
    assert_eq!(g.block_at(0, 1, 30).id, bedrock);
    assert_eq!(g.block_at(0, 1, 31).id, stone);
}

#[test]
fn strata_lookup_is_by_stratigraphic_coordinate() {
    let (a, b, c) = (id(10), id(11), id(12));
    let mut g = ColumnGround::plain(fill(0), 0, a, id(2));
    g.strata = vec![(10, a), (0, b), (-20, c)];
    g.geo = fill(5);
    for (y, want) in [(20, a), (5, a), (4, b), (-15, b), (-16, c), (-40, c)] {
        assert_eq!(g.block_at(1, 1, y).id, want, "y={y}");
    }
}

#[test]
fn column_ground_survives_bincode() {
    let g = ColumnGround::plain(fill(7), -3, id(1), id(2));
    let col = Column::new(0, Block::new(id(1)), Block::new(id(0)), vec![]).with_ground(g.clone());
    let bytes = bincode::serialize(&col).unwrap();
    let back: Column = bincode::deserialize(&bytes).unwrap();
    assert_eq!(back.ground(), Some(&g));
    assert!(g.is_implied(-4) && !g.is_implied(-3));
}

#[test]
fn column_ground_default_is_a_zeroed_footprint() {
    let g = ColumnGround::default();
    assert_eq!(&*g.surface, &[0; COLUMN_FOOTPRINT][..]);
    assert_eq!(&*g.geo, &[0; COLUMN_FOOTPRINT][..]);
}

#[test]
fn column_ground_rejects_a_wrong_length_footprint() {
    let mut g = ColumnGround::plain(fill(3), 0, id(1), id(2));
    g.surface = vec![3; COLUMN_FOOTPRINT + 1].into_boxed_slice();
    let bytes = bincode::serialize(&g).unwrap();
    assert!(bincode::deserialize::<ColumnGround>(&bytes).is_err(), "surface of the wrong length");
    let mut g = ColumnGround::plain(fill(3), 0, id(1), id(2));
    g.geo = vec![0; COLUMN_FOOTPRINT - 1].into_boxed_slice();
    let bytes = bincode::serialize(&g).unwrap();
    assert!(bincode::deserialize::<ColumnGround>(&bytes).is_err(), "geo of the wrong length");
}
