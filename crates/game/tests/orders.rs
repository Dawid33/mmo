use std::collections::{BTreeMap, BTreeSet};

use game::*;

const STONE: BlockId = BlockId(2);
const WATER: BlockId = BlockId(9);

#[derive(Default)]
struct Src {
    blocks: BTreeMap<TilePos, BlockId>,
    bedrock: BTreeSet<TilePos>,
    edge: BTreeSet<TilePos>,
    built: BTreeSet<TilePos>,
}

impl TileSource for Src {
    fn block(&self, p: TilePos) -> BlockId {
        self.blocks.get(&p).copied().unwrap_or(BlockId::AIR)
    }
    fn is_bedrock(&self, p: TilePos) -> bool {
        self.bedrock.contains(&p)
    }
    fn edge_locked(&self, p: TilePos) -> bool {
        self.edge.contains(&p)
    }
    fn is_constructed(&self, p: TilePos) -> bool {
        self.built.contains(&p)
    }
}

#[test]
fn tiles_iterate_y_then_z_then_x() {
    let t: Vec<_> = TileBox::new([1, 0, 0], [0, 1, 1]).tiles().collect();
    assert_eq!(t.len(), 8);
    assert_eq!(&t[..3], &[[0, 0, 0], [1, 0, 0], [0, 0, 1]]);
}

#[test]
fn tile_count_and_clip() {
    let c = TileBox::new([-5, 0, -5], [3, 2, 3]).clip_to_region().unwrap();
    assert_eq!(c, TileBox { min: [0, 0, 0], max: [3, 2, 3] });
    assert_eq!(c.tile_count(), 4 * 3 * 4);
    assert!(TileBox::new([-9, 0, 0], [-1, 2, 3]).clip_to_region().is_none());
    let far = TileBox::new([0, 0, 0], [REGION_BLOCKS + 5, 1, 2]).clip_to_region().unwrap();
    assert_eq!(far.max[0], REGION_BLOCKS - 1);
}

#[test]
fn dig_rules() {
    let mut s = Src::default();
    let p = |x| [x, 5, 0];
    for x in 1..=4 {
        s.blocks.insert(p(x), STONE);
    }
    s.built.insert(p(2));
    s.bedrock.insert(p(3));
    s.edge.insert(p(4));
    let dig = DesignationKind::Dig;
    assert_eq!(validate(&s, dig, p(0)), Err(Invalid::NothingToDig));
    assert_eq!(validate(&s, dig, p(1)), Ok(()));
    assert_eq!(validate(&s, dig, p(2)), Err(Invalid::Built));
    assert_eq!(validate(&s, dig, p(3)), Err(Invalid::Bedrock));
    assert_eq!(validate(&s, dig, p(4)), Err(Invalid::RegionEdge));
}

#[test]
fn deconstruct_needs_a_construction() {
    let mut s = Src::default();
    s.blocks.insert([0, 0, 0], STONE);
    s.blocks.insert([1, 0, 0], STONE);
    s.built.insert([1, 0, 0]);
    let k = DesignationKind::Deconstruct;
    assert_eq!(validate(&s, k, [0, 0, 0]), Err(Invalid::NotBuilt));
    assert_eq!(validate(&s, k, [1, 0, 0]), Ok(()));
}

#[test]
fn build_needs_air() {
    let mut s = Src::default();
    s.blocks.insert([0, 0, 0], STONE);
    s.blocks.insert([1, 0, 0], WATER);
    let k = DesignationKind::Build(game::edit::PLACE_BLOCK);
    assert_eq!(validate(&s, k, [0, 0, 0]), Err(Invalid::Occupied));
    assert_eq!(validate(&s, k, [1, 0, 0]), Err(Invalid::Occupied));
    assert_eq!(validate(&s, k, [2, 0, 0]), Ok(()));
    assert!(k.same_tool(DesignationKind::Build(STONE)));
    assert!(!k.same_tool(DesignationKind::Dig));
}

#[test]
fn wall_drag_on_one_level_extrudes_dwarf_height() {
    let b = resolve_drag(DragShape::Wall, [2, 10, 2], [5, 10, 3]);
    assert_eq!(b, TileBox { min: [2, 10, 2], max: [5, 11, 3] });
}

#[test]
fn wall_drag_across_levels_keeps_explicit_range() {
    let b = resolve_drag(DragShape::Wall, [2, 10, 2], [5, 13, 3]);
    assert_eq!((b.min[1], b.max[1]), (10, 13));
}

#[test]
fn floor_drag_goes_under_the_level() {
    let b = resolve_drag(DragShape::Floor, [0, 10, 0], [3, 12, 3]);
    assert_eq!((b.min[1], b.max[1]), (9, 9));
}

#[test]
fn summary_counts_by_reason() {
    let mut s = Src::default();
    s.blocks.insert([0, 0, 0], STONE);
    s.blocks.insert([2, 0, 0], STONE);
    s.bedrock.insert([2, 0, 0]);
    let sum = summarize(&s, DesignationKind::Dig, TileBox::new([0, 0, 0], [2, 0, 0]));
    assert_eq!(sum.valid, 1);
    assert_eq!(
        sum.invalid,
        BTreeMap::from([(Invalid::NothingToDig, 1), (Invalid::Bedrock, 1)])
    );
}
