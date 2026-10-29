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

// ---- Orders as sim state -------------------------------------------------

const BEDROCK: BlockId = BlockId(3);
/// Surface block Y of every test column: the band is one fully solid chunk
/// at sub-Y 0 (blocks 0 and 1); everything below is implied.
const SURFACE: i32 = 1;
/// Bedrock starts this level and goes down from there.
const BEDROCK_Y: i32 = SURFACE - UNDERGROUND_DEPTH;

/// One band chunk (sub-Y 0, fully stone) over plain implied ground.
fn stone_column() -> Column {
    let band = SubChunk::Dense(Box::new(Chunk::flat_floor_with(32, STONE)));
    // `below` must differ from the band, or normalising collapses it away.
    Column::new(0, Block::new(BlockId(1)), Block::new(BlockId::AIR), vec![band])
        .with_ground(ColumnGround::plain([SURFACE as i16; 4], 0, STONE, BEDROCK))
}

/// A region over chunk columns `0..=3` x `0..=3` (blocks 0..8), with a
/// render-update channel and client 0.
fn order_region() -> Region {
    let r0 = RegionCoords::new(0, 0);
    let mut columns = Vec::new();
    for x in 0..=3 {
        for z in 0..=3 {
            columns.push((ColumnCoords::new(x, z), stone_column()));
        }
    }
    let built = Region::from_columns(r0, columns);
    let (send, recv) = crossbeam::channel::unbounded();
    std::mem::forget(recv);
    let mut region = Region::new(built.data().clone(), Some(send), r0, None);
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.forget_last_event();
    region
}

fn op(seq: u32) -> OpId {
    OpId { client: 0, seq }
}

fn designate(op: OpId, kind: DesignationKind, area: TileBox) -> Order {
    Order::Designate { op, kind, priority: DEFAULT_PRIORITY, area }
}

/// Applies `order` from client 0 as an event; returns the hash before it.
fn apply(region: &mut Region, order: Order) -> u32 {
    let before = state_hash(region.data());
    region.handle_event(GameEventKind::Order(0, order)).unwrap();
    before
}

/// Applies `order`, checks it rolls back bit-exact, then applies it again for
/// good.
fn apply_checked(region: &mut Region, order: Order) {
    let before = apply(region, order);
    region.rollback_last_event();
    assert_eq!(before, state_hash(region.data()), "{order:?} must roll back bit-exact");
    apply(region, order);
    region.forget_last_event();
}

/// Asserts `order` is rejected with no state change, both called directly
/// and as an event (whose id bump alone changes the hash).
fn assert_rejected(region: &mut Region, order: Order) {
    let before = state_hash(region.data());
    let mut accepted = true;
    region.with_data(|r| accepted = r.apply_order_safe(0, order));
    assert!(!accepted, "{order:?} must be rejected");
    assert_eq!(before, state_hash(region.data()), "{order:?} must change nothing");
    let designations = region.data().designations().clone();
    region.handle_event(GameEventKind::Order(0, order)).unwrap();
    region.forget_last_event();
    assert_eq!(&designations, region.data().designations());
}

fn block_at(region: &Region, p: TilePos) -> BlockId {
    TileSource::block(region.data(), p)
}

#[test]
fn designate_dig_marks_only_valid_tiles_and_rolls_back() {
    let mut region = order_region();
    let area = TileBox::new([2, SURFACE, 2], [5, SURFACE + 1, 5]);
    apply_checked(&mut region, designate(op(1), DesignationKind::Dig, area));
    let d = region.data().designations();
    assert_eq!(d.len(), 16, "the air layer above adds nothing");
    assert!(d.keys().all(|p| p[1] == SURFACE));
    assert!(d.values().all(|d| d.op == op(1) && d.priority == DEFAULT_PRIORITY));
}

#[test]
fn order_over_cap_is_rejected_whole() {
    let mut region = order_region();
    // 64 x 64 columns at most, so the y extent makes it one tile over.
    let side = REGION_BLOCKS.min(64);
    let levels = (MAX_ORDER_TILES / (side as u64 * side as u64)) as i32;
    let area = TileBox::new([0, SURFACE, 0], [side - 1, SURFACE + levels - 1, side - 1]);
    assert_eq!(area.tile_count(), MAX_ORDER_TILES);
    let mut over = area;
    over.min[1] -= 1;
    assert!(over.tile_count() > MAX_ORDER_TILES);
    assert_rejected(&mut region, designate(op(1), DesignationKind::Dig, over));
    assert!(region.data().designations().is_empty());
}

#[test]
fn area_is_clipped_to_region() {
    let mut region = order_region();
    let area = TileBox::new([-4, SURFACE, -4], [3, SURFACE, 3]);
    apply_checked(&mut region, designate(op(1), DesignationKind::Dig, area));
    let d = region.data().designations();
    assert_eq!(d.len(), 16);
    assert!(d.keys().all(|p| (0..4).contains(&p[0]) && (0..4).contains(&p[2])));
}

#[test]
fn inverted_area_is_rejected() {
    let mut region = order_region();
    let good = TileBox::new([2, SURFACE, 2], [5, SURFACE, 5]);
    for axis in 0..3 {
        let mut bad = good;
        std::mem::swap(&mut bad.min[axis], &mut bad.max[axis]);
        if axis == 1 {
            bad.min[1] = SURFACE + 1;
        }
        assert_rejected(&mut region, designate(op(1), DesignationKind::Dig, bad));
        assert_rejected(&mut region, Order::Erase { area: bad, only: None });
    }
    assert!(region.data().designations().is_empty());
}

#[test]
fn full_i32_span_is_rejected_without_panic() {
    let mut region = order_region();
    let full = TileBox { min: [i32::MIN; 3], max: [i32::MAX; 3] };
    assert_rejected(&mut region, designate(op(1), DesignationKind::Dig, full));
    assert_rejected(&mut region, Order::Erase { area: full, only: None });
    let inverted = TileBox { min: [i32::MAX; 3], max: [i32::MIN; 3] };
    assert_rejected(&mut region, designate(op(1), DesignationKind::Dig, inverted));
    assert!(region.data().designations().is_empty());
}

#[test]
fn spoofed_op_client_is_rejected() {
    let mut region = order_region();
    let area = TileBox::new([2, SURFACE, 2], [5, SURFACE, 5]);
    let spoofed = OpId { client: 1, seq: 1 };
    assert_rejected(&mut region, designate(spoofed, DesignationKind::Dig, area));
    assert_rejected(&mut region, Order::CancelOp(spoofed));
}

#[test]
fn bad_priority_is_rejected() {
    let mut region = order_region();
    let area = TileBox::new([2, SURFACE, 2], [5, SURFACE, 5]);
    for priority in [0, MAX_PRIORITY + 1] {
        let order = Order::Designate { op: op(1), kind: DesignationKind::Dig, priority, area };
        assert_rejected(&mut region, order);
    }
}

#[test]
fn reapplying_the_same_order_is_identical() {
    let mut region = order_region();
    let order = designate(op(1), DesignationKind::Dig, TileBox::new([2, SURFACE, 2], [5, SURFACE, 5]));
    apply(&mut region, order);
    let h1 = state_hash(region.data());
    region.rollback_last_event();
    apply(&mut region, order);
    assert_eq!(h1, state_hash(region.data()));
}

#[test]
fn cancel_op_spares_tiles_overwritten_by_a_later_op() {
    let mut region = order_region();
    let dig = DesignationKind::Dig;
    apply_checked(&mut region, designate(op(1), dig, TileBox::new([2, SURFACE, 2], [5, SURFACE, 5])));
    apply_checked(&mut region, designate(op(2), dig, TileBox::new([3, SURFACE, 3], [4, SURFACE, 4])));
    apply_checked(&mut region, Order::CancelOp(op(1)));
    let d = region.data().designations();
    assert_eq!(d.len(), 4);
    assert!(d.values().all(|d| d.op == op(2)));
}

#[test]
fn erase_filters_by_kind() {
    let mut region = order_region();
    let build = DesignationKind::Build(game::edit::PLACE_BLOCK);
    // Stone at the surface takes Dig, the air above takes Build.
    let area = TileBox::new([2, SURFACE, 2], [5, SURFACE + 1, 5]);
    apply_checked(&mut region, designate(op(1), DesignationKind::Dig, area));
    apply_checked(&mut region, designate(op(2), build, area));
    assert_eq!(region.data().designations().len(), 32);
    apply_checked(&mut region, Order::Erase { area, only: Some(DesignationKind::Dig) });
    let d = region.data().designations();
    assert_eq!(d.len(), 16);
    assert!(d.values().all(|d| d.kind == build));
}

#[test]
fn dev_complete_builds_a_two_level_wall_then_deconstructs_it() {
    let mut region = order_region();
    let build = DesignationKind::Build(game::edit::PLACE_BLOCK);
    let wall = resolve_drag(DragShape::Wall, [2, SURFACE + 1, 2], [5, SURFACE + 1, 2]);
    assert_eq!(wall.tile_count(), 8);
    apply_checked(&mut region, designate(op(1), build, wall));
    apply_checked(&mut region, Order::DevCompleteAll);
    for p in wall.tiles() {
        assert_eq!(block_at(&region, p), game::edit::PLACE_BLOCK, "{p:?} built");
        assert!(region.data().constructed().contains_key(&p));
    }
    assert!(region.data().designations().is_empty());

    apply_checked(&mut region, designate(op(2), DesignationKind::Deconstruct, wall));
    assert_eq!(region.data().designations().len(), 8);
    apply_checked(&mut region, Order::DevCompleteAll);
    for p in wall.tiles() {
        assert_eq!(block_at(&region, p), BlockId::AIR, "{p:?} taken down");
    }
    assert!(region.data().constructed().is_empty());
    assert!(region.data().designations().is_empty());
}

#[test]
fn dev_complete_drops_stale_designations() {
    let mut region = order_region();
    let area = TileBox::new([2, SURFACE, 2], [3, SURFACE, 3]);
    apply_checked(&mut region, designate(op(1), DesignationKind::Dig, area));
    let stale = [2, SURFACE, 2];
    region.with_data(|r| assert!(r.set_block_safe(stale, Block::new(BlockId::AIR))));
    let snapshot: Vec<_> = (0..8)
        .flat_map(|x| (SURFACE - 2..=SURFACE + 1).flat_map(move |y| (0..8).map(move |z| [x, y, z])))
        .map(|p| (p, block_at(&region, p)))
        .collect();
    apply_checked(&mut region, Order::DevCompleteAll);
    assert!(region.data().designations().is_empty());
    for (p, was) in snapshot {
        let expect = if area.contains(p) { BlockId::AIR } else { was };
        assert_eq!(block_at(&region, p), expect, "{p:?}");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn dig_box_down_to_bedrock_skips_bedrock() {
    let mut region = order_region();
    let area = TileBox::new([2, BEDROCK_Y - 1, 2], [3, SURFACE, 3]);
    apply_checked(&mut region, designate(op(1), DesignationKind::Dig, area));
    let d = region.data().designations();
    assert_eq!(d.len() as u64, area.tile_count() - 2 * 4, "two bedrock levels refused");
    assert!(d.keys().all(|p| p[1] > BEDROCK_Y));
    apply_checked(&mut region, Order::DevCompleteAll);
    for p in area.tiles() {
        let expect = if p[1] > BEDROCK_Y { BlockId::AIR } else { BEDROCK };
        assert_eq!(block_at(&region, p), expect, "{p:?}");
    }
}
