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
    trees: BTreeMap<TilePos, u32>,
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
    fn tree_id(&self, p: TilePos) -> u32 {
        self.trees.get(&p).copied().unwrap_or(0)
    }
    fn tree_limb(&self, _: TilePos) -> u8 {
        0
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
    let far = TileBox::new([0, 0, 0], [REGION_BLOCKS as i32 + 5, 1, 2]).clip_to_region().unwrap();
    assert_eq!(far.max[0], REGION_BLOCKS as i32 - 1);
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
fn chop_needs_a_solid_tree_block() {
    let reg = BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
    install_block_props(reg.props());
    let (log, leaves) = (reg.id_of("oak_log").unwrap(), reg.id_of("oak_leaves").unwrap());
    let mut s = Src::default();
    s.blocks.insert([0, 0, 0], log);
    s.trees.insert([0, 0, 0], 7);
    s.blocks.insert([1, 0, 0], leaves);
    s.trees.insert([1, 0, 0], 7);
    s.blocks.insert([2, 0, 0], STONE);
    s.blocks.insert([3, 0, 0], log); // a log with no tree id (e.g. built)
    let k = DesignationKind::Chop;
    assert_eq!(validate(&s, k, [0, 0, 0]), Ok(()));
    for x in 1..=4 {
        assert_eq!(validate(&s, k, [x, 0, 0]), Err(Invalid::NotTree), "x = {x}");
    }
    assert!(k.same_tool(DesignationKind::Chop));
    assert!(!k.same_tool(DesignationKind::Dig));
    assert!(!DesignationKind::Dig.same_tool(k));
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
        .with_ground(ColumnGround::plain(vec![SURFACE as i16; game::COLUMN_FOOTPRINT].into_boxed_slice(), 0, STONE, BEDROCK))
}

/// Extent of the test region in blocks along x and z: every order here stays
/// inside blocks 0..8.
const ORDER_REGION_BLOCKS: usize = 8;

/// A region over the chunk columns covering blocks 0..[`ORDER_REGION_BLOCKS`]
/// (4 x 4 chunks at 2-block chunks), with a render-update channel and client 0.
fn order_region() -> Region {
    let (region, recv) = order_region_with_updates();
    std::mem::forget(recv);
    region
}

/// [`order_region`], keeping the render-update receiver.
fn order_region_with_updates() -> (Region, crossbeam::channel::Receiver<GameDataUpdate>) {
    let r0 = RegionCoords::new(0, 0);
    let mut columns = Vec::new();
    let chunks = ORDER_REGION_BLOCKS.div_ceil(CHUNK_BLOCKS);
    for x in 0..chunks {
        for z in 0..chunks {
            columns.push((ColumnCoords::new(x, z), stone_column()));
        }
    }
    let built = Region::from_columns(r0, columns);
    let (send, recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(built.data().clone(), Some(send), r0, None);
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.forget_last_event();
    (region, recv)
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
    let side = (REGION_BLOCKS as i32).min(64);
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
fn order_beyond_level_range_is_rejected_without_panic() {
    let mut region = order_region();
    let build = DesignationKind::Build(game::edit::PLACE_BLOCK);
    for y in [2_000_000_000, MAX_ORDER_LEVEL + 1, MIN_ORDER_LEVEL - 1, -2_000_000_000] {
        let tile = TileBox::new([2, y, 2], [2, y, 2]);
        assert_rejected(&mut region, designate(op(1), build, tile));
        assert_rejected(&mut region, designate(op(1), DesignationKind::Dig, tile));
        assert_rejected(&mut region, Order::Erase { area: tile, only: None });
    }
    // Straddling a bound is rejected whole too.
    let straddle = TileBox::new([2, MAX_ORDER_LEVEL - 1, 2], [2, MAX_ORDER_LEVEL + 1, 2]);
    assert_rejected(&mut region, designate(op(1), build, straddle));
    // The bounds themselves are fine.
    let top = TileBox::new([2, MAX_ORDER_LEVEL, 2], [2, MAX_ORDER_LEVEL, 2]);
    apply_checked(&mut region, designate(op(1), build, top));
    assert_eq!(region.data().designations().len(), 1);
}

#[test]
fn build_with_other_block_id_is_rejected() {
    let mut region = order_region();
    let area = TileBox::new([2, SURFACE + 1, 2], [3, SURFACE + 1, 3]);
    for id in [BEDROCK, BlockId::AIR, BlockId(0xBEEF)] {
        assert_rejected(&mut region, designate(op(1), DesignationKind::Build(id), area));
    }
    assert!(region.data().designations().is_empty());
}

#[test]
fn failed_deconstruct_keeps_constructed_flag() {
    let mut region = order_region();
    let build = DesignationKind::Build(game::edit::PLACE_BLOCK);
    let p = [2, SURFACE + 1, 2];
    let tile = TileBox::new(p, p);
    apply_checked(&mut region, designate(op(1), build, tile));
    apply_checked(&mut region, Order::DevCompleteAll);
    assert!(region.data().constructed().contains_key(&p));
    // The construction turns to bedrock, which refuses every write.
    region.with_data(|r| assert!(r.set_block_safe(p, Block::new(BEDROCK))));
    apply_checked(&mut region, designate(op(2), DesignationKind::Deconstruct, tile));
    apply_checked(&mut region, Order::DevCompleteAll);
    assert_eq!(block_at(&region, p), BEDROCK, "the write was refused");
    assert!(region.data().constructed().contains_key(&p), "so the flag stays");
    assert!(region.data().designations().is_empty());
}

/// The `SetDesignations` / `SetConstructed` payloads queued on `recv`, in
/// order, each tagged `true` for Do and `false` for Undo.
fn order_payloads(
    recv: &crossbeam::channel::Receiver<GameDataUpdate>,
) -> Vec<(bool, GameDataUpdateKind)> {
    recv.try_iter()
        .filter(|u| {
            matches!(u.update_kind, GameDataUpdateKind::SetDesignations(_) | GameDataUpdateKind::SetConstructed(_))
        })
        .map(|u| (matches!(u.do_kind, GameDataTransactionKind::Do), u.update_kind))
        .collect()
}

#[test]
fn render_payloads_carry_new_values_and_undo_restores_prior() {
    let (mut region, recv) = order_region_with_updates();
    let tile = TileBox::new([2, SURFACE, 2], [2, SURFACE, 2]);
    let p = tile.min;
    apply_checked(&mut region, designate(op(1), DesignationKind::Dig, tile));
    let old = region.data().designations()[&p];
    recv.try_iter().count();

    let order = Order::Designate { op: op(2), kind: DesignationKind::Dig, priority: 7, area: tile };
    apply(&mut region, order);
    let new = region.data().designations()[&p];
    region.rollback_last_event();
    let got = order_payloads(&recv);
    assert_eq!(got.len(), 2, "{got:?}");
    match &got[0] {
        (true, GameDataUpdateKind::SetDesignations(c)) => assert_eq!(c, &vec![(p, Some(new))]),
        other => panic!("do payload {other:?}"),
    }
    match &got[1] {
        (false, GameDataUpdateKind::SetDesignations(c)) => assert_eq!(c, &vec![(p, Some(old))]),
        other => panic!("undo payload {other:?}"),
    }

    // A dev-completed build: the constructed flag goes up, and back down.
    let above = [2, SURFACE + 1, 2];
    let build = DesignationKind::Build(game::edit::PLACE_BLOCK);
    apply_checked(&mut region, designate(op(3), build, TileBox::new(above, above)));
    recv.try_iter().count();
    apply(&mut region, Order::DevCompleteAll);
    region.rollback_last_event();
    let got: Vec<_> = order_payloads(&recv)
        .into_iter()
        .filter(|(_, k)| matches!(k, GameDataUpdateKind::SetConstructed(_)))
        .collect();
    assert_eq!(got.len(), 2, "{got:?}");
    match &got[0] {
        (true, GameDataUpdateKind::SetConstructed(c)) => assert_eq!(c, &vec![(above, true)]),
        other => panic!("do payload {other:?}"),
    }
    match &got[1] {
        (false, GameDataUpdateKind::SetConstructed(c)) => assert_eq!(c, &vec![(above, false)]),
        other => panic!("undo payload {other:?}"),
    }
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
    let wall = TileBox::new([2, SURFACE + 1, 2], [5, SURFACE + 2, 2]);
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
    let n = ORDER_REGION_BLOCKS as i32;
    let snapshot: Vec<_> = (0..n)
        .flat_map(|x| (SURFACE - 2..=SURFACE + 1).flat_map(move |y| (0..n).map(move |z| [x, y, z])))
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

fn pick_solid(cells: &[TilePos]) -> impl Fn(TilePos) -> bool {
    let set: BTreeSet<TilePos> = cells.iter().copied().collect();
    move |p| set.contains(&p)
}

fn straight_down() -> (rapier3d::math::Vector, rapier3d::math::Vector) {
    let s = BLOCK_VOXELS as f32;
    (
        rapier3d::math::Vector::new(3.5 * s, 40.0 * s, 3.5 * s),
        rapier3d::math::Vector::new(0.0, -1.0, 0.0),
    )
}

#[test]
fn pick_hits_rock_cap_on_the_level() {
    let (o, d) = straight_down();
    assert_eq!(pick_level_cell(o, d, 10, pick_solid(&[[3, 10, 3]])), Some([3, 10, 3]));
}

#[test]
fn pick_sees_the_floor_below_an_air_cell() {
    let (o, d) = straight_down();
    assert_eq!(pick_level_cell(o, d, 10, pick_solid(&[[3, 9, 3]])), Some([3, 10, 3]));
}

#[test]
fn pick_clamps_open_space_to_the_level() {
    use rapier3d::math::Vector;
    let s = BLOCK_VOXELS as f32;
    let pitch = 55.0_f32.to_radians();
    let d = Vector::new(pitch.cos(), -pitch.sin(), 0.0);
    let o = Vector::new(0.5 * s, 40.0 * s, 3.5 * s);
    let t = (10.0 * s - o.y) / d.y;
    let x = ((o.x + d.x * t) / s).floor() as i32;
    assert_eq!(pick_level_cell(o, d, 10, pick_solid(&[])), Some([x, 10, 3]));
}

#[test]
fn pick_ignores_blocks_above_the_cut() {
    use rapier3d::math::Vector;
    let s = BLOCK_VOXELS as f32;
    // 45 degrees toward +x: enters the level in column 3, but the open-space
    // clamp would say column 4.
    let c = std::f32::consts::FRAC_1_SQRT_2;
    let d = Vector::new(c, -c, 0.0);
    // Crosses the cut (y = 11 blocks) at x = 3.5 blocks.
    let o = Vector::new(3.5 * s - 10.0 * s, 21.0 * s, 3.5 * s);
    assert_eq!(
        pick_level_cell(o, d, 10, pick_solid(&[[3, 11, 3], [3, 10, 3]])),
        Some([3, 10, 3])
    );
}

#[test]
fn visible_pick_hits_blocks_above_the_level() {
    use rapier3d::math::Vector;
    let s = BLOCK_VOXELS as f32;
    let c = std::f32::consts::FRAC_1_SQRT_2;
    let d = Vector::new(c, -c, 0.0);
    // The same ray as `pick_ignores_blocks_above_the_cut`: with nothing cut
    // away it stops on the block above the level.
    let o = Vector::new(3.5 * s - 10.0 * s, 21.0 * s, 3.5 * s);
    let solid = || pick_solid(&[[3, 11, 3], [3, 10, 3]]);
    assert_eq!(pick_visible_cell(o, d, 64.0, false, solid()), Some([3, 11, 3]));
}

#[test]
fn visible_pick_any_depth_and_the_face_cell_for_builds() {
    let (o, d) = straight_down();
    let solid = || pick_solid(&[[3, 2, 3]]);
    assert_eq!(pick_visible_cell(o, d, 64.0, false, solid()), Some([3, 2, 3]), "deep below any level");
    assert_eq!(pick_visible_cell(o, d, 64.0, true, solid()), Some([3, 3, 3]), "the air on its top face");
    assert_eq!(pick_visible_cell(o, d, 20.0, false, solid()), None, "beyond reach");
    assert_eq!(pick_visible_cell(o, d, 64.0, false, pick_solid(&[])), None, "nothing to hit");
}

/// Dug air under a 10-block-thick roof at block column (3, 3): the surface is
/// y 30 everywhere, the room is air y 5..=7, its floor solid at y 4.
fn xray_world() -> (impl Fn(TilePos) -> bool, impl Fn(TilePos) -> bool) {
    let room = |p: TilePos| (p[0], p[2]) == (3, 3) && (5..=7).contains(&p[1]);
    let solid = move |p: TilePos| p[1] < 30 && !room(p);
    let dug = |p: TilePos| is_dug_air(p[1], 30);
    (solid, dug)
}

#[test]
fn dug_air_is_strictly_below_the_surface() {
    assert!(is_dug_air(29, 30));
    assert!(!is_dug_air(30, 30), "air inside the surface block (a half block's top) is not dug");
    assert!(!is_dug_air(31, 30));
}

#[test]
fn xray_sees_the_floor_of_a_room_through_its_roof() {
    let (o, d) = straight_down();
    let (solid, dug) = xray_world();
    assert_eq!(pick_visible_cell(o, d, 64.0, false, &solid), Some([3, 29, 3]), "plain: the roof's top");
    assert_eq!(pick_xray_cell(o, d, 64.0, false, &solid, &dug), Some([3, 4, 3]), "x-ray: the room's floor");
    assert_eq!(pick_xray_cell(o, d, 64.0, true, &solid, &dug), Some([3, 5, 3]), "a build: the air on the floor");
}

#[test]
fn xray_without_a_dug_space_on_the_ray_is_the_plain_pick() {
    let s = BLOCK_VOXELS as f32;
    let (solid, dug) = xray_world();
    let o = rapier3d::math::Vector::new(9.5 * s, 40.0 * s, 9.5 * s);
    let d = rapier3d::math::Vector::new(0.0, -1.0, 0.0);
    assert_eq!(pick_xray_cell(o, d, 64.0, false, &solid, &dug), Some([9, 29, 9]));
    // Air above the surface is not dug: a ray through the open sky hits ground.
    assert_eq!(pick_xray_cell(o, d, 64.0, false, &solid, |_| true), Some([9, 29, 9]));
}

#[test]
fn xray_ignores_a_room_beyond_reach() {
    let (o, d) = straight_down();
    let (solid, dug) = xray_world();
    assert_eq!(pick_xray_cell(o, d, 20.0, false, &solid, &dug), Some([3, 29, 3]), "the room lies 33 blocks down");
}

/// `DevCompleteAll` writes in per-chunk batches (one undo entry per chunk —
/// a per-tile loop re-hashed whole fields per tile, seconds on a play-size
/// region, and the client replayed it on every rollback). The batched result
/// must be the per-tile one, across chunk borders, and roll back bit-exact.
#[test]
fn complete_all_batches_match_per_tile_edits() {
    let build_region = || {
        let r0 = RegionCoords::new(0, 0);
        let mut columns = Vec::new();
        for x in 0..2 {
            for z in 0..2 {
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
    };
    let c = CHUNK_BLOCKS as i32;
    // Straddles the chunk corner at (c, c), down into implied ground.
    let dig = TileBox::new([c - 3, SURFACE - 3, c - 3], [c + 2, SURFACE, c + 2]);
    let build = TileBox::new([c - 2, SURFACE + 1, c - 2], [c + 1, SURFACE + 2, c + 1]);
    let mut batched = build_region();
    apply_checked(&mut batched, designate(op(1), DesignationKind::Dig, dig));
    apply_checked(&mut batched, designate(op(2), DesignationKind::Build(game::edit::PLACE_BLOCK), build));
    let mut per_tile = build_region();
    apply_checked(&mut batched, Order::DevCompleteAll);
    assert!(batched.data().designations().is_empty());
    per_tile.with_data(|r| {
        for p in dig.tiles() {
            r.set_block_safe(p, Block::new(BlockId::AIR));
        }
        for p in build.tiles() {
            r.set_block_safe(p, Block::new(game::edit::PLACE_BLOCK));
        }
    });
    let span = TileBox::new([0, SURFACE - 8, 0], [2 * c - 1, SURFACE + 4, 2 * c - 1]);
    for p in span.tiles() {
        assert_eq!(block_at(&batched, p), block_at(&per_tile, p), "block at {p:?}");
    }
    let built: Vec<TilePos> = batched.data().constructed().keys().copied().collect();
    let mut want: Vec<TilePos> = build.tiles().collect();
    want.sort();
    assert_eq!(built, want, "every build is flagged constructed");
    // Deconstructing them all clears the blocks and the flags.
    apply_checked(&mut batched, designate(op(3), DesignationKind::Deconstruct, build));
    apply_checked(&mut batched, Order::DevCompleteAll);
    assert!(batched.data().constructed().is_empty());
    assert!(build.tiles().all(|p| block_at(&batched, p) == BlockId::AIR));
}
