use std::collections::{BTreeMap, BTreeSet};

use game::*;

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

// ---- Orders as sim state -------------------------------------------------

/// The level of the ground's top (solid) tile in every test column; the tile
/// above it is open. Everything below the materialised surface layer is
/// implied by the recipe.
const SURFACE: i32 = 4;
/// Bedrock starts this level and goes down from there.
const BEDROCK_Y: i32 = SURFACE - 6;

/// The recipe of every test column: stone to the top of level [`SURFACE`]
/// (no soil), bedrock from level [`BEDROCK_Y`] down.
fn stone_column() -> StudColumnGround {
    let bare = StudProfile { cover: MAT_STONE, cover_plates: 0, soil: MAT_STONE, soil_plates: 0 };
    let plates = TILE_PLATES as i32;
    StudColumnGround::flat((SURFACE + 1) * plates, bare, MAT_STONE, (BEDROCK_Y + 1) * plates)
}

/// A stud region of [`stone_column`]s over `chunks` x `chunks` chunk columns,
/// with a render-update channel and client 0.
fn stone_region(chunks: usize) -> (Region, crossbeam::channel::Receiver<GameDataUpdate>) {
    crate::common::install_real_block_props();
    let r0 = RegionCoords::new(0, 0);
    let ground = (0..chunks).flat_map(|x| (0..chunks).map(move |z| (ColumnCoords::new(x, z), stone_column()))).collect();
    let (send, recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), r0, None);
    region.seed_stud_world(StudRegionSeed { ground, features: Vec::new(), ..Default::default() });
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.forget_last_event();
    (region, recv)
}

/// Extent of the test region in tiles along x and z: every order here stays
/// inside tiles 0..8.
const ORDER_REGION_BLOCKS: usize = 8;

/// A region over the chunk columns covering tiles 0..[`ORDER_REGION_BLOCKS`],
/// with a render-update channel and client 0.
fn order_region() -> Region {
    let (region, recv) = order_region_with_updates();
    std::mem::forget(recv);
    region
}

/// [`order_region`], keeping the render-update receiver.
fn order_region_with_updates() -> (Region, crossbeam::channel::Receiver<GameDataUpdate>) {
    stone_region(ORDER_REGION_BLOCKS.div_ceil(CHUNK_BLOCKS))
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

/// How full tile `p` is and what it is mostly made of.
fn tile_at(region: &Region, p: TilePos) -> (TileFill, Option<u16>) {
    (TileSource::tile_fill(region.data(), p), TileSource::tile_material(region.data(), p))
}

/// What a dev-completed `Build(PLACE_BLOCK)` leaves in a tile.
fn built() -> (TileFill, Option<u16>) {
    (TileFill::Solid, Some(stud_material_of_block(game::edit::PLACE_BLOCK)))
}

const OPEN: (TileFill, Option<u16>) = (TileFill::Open, None);

/// `summarize` counts a box's tiles by what `validate` says of each.
#[test]
fn summary_counts_by_reason() {
    let region = order_region();
    // One column from a bedrock level up to the open tile over the ground.
    let column = TileBox::new([2, BEDROCK_Y, 2], [2, SURFACE + 1, 2]);
    let sum = summarize(region.data(), DesignationKind::Dig, column);
    assert_eq!(sum.valid as i32, SURFACE - BEDROCK_Y);
    assert_eq!(sum.invalid, BTreeMap::from([(Invalid::NothingToDig, 1), (Invalid::Bedrock, 1)]));
}

#[test]
fn designate_dig_marks_only_valid_tiles_and_rolls_back() {
    let mut region = order_region();
    let area = TileBox::new([2, SURFACE, 2], [5, SURFACE + 1, 5]);
    apply_checked(&mut region, designate(op(1), DesignationKind::Dig, area));
    let d = region.data().designations();
    // The ground is one filler slab: the dig is its cells inside the volume, one box
    // (the air layer above adds nothing).
    let cells: Vec<_> = d.keys().map(|b| b.cells()).collect();
    let (s, t) = (TILE_STUDS as i32, TILE_PLATES as i32);
    assert_eq!(cells, vec![([2 * s, SURFACE * t, 2 * s], [6 * s, (SURFACE + 1) * t, 6 * s])]);
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
    assert_eq!(d.len(), 1);
    let s = TILE_STUDS as i32;
    assert!(d.keys().all(|b| b.cells().0[0] == 0 && b.cells().0[2] == 0 && b.cells().1[0] == 4 * s && b.cells().1[2] == 4 * s));
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
fn build_of_air_is_rejected() {
    let mut region = order_region();
    let area = TileBox::new([2, SURFACE + 1, 2], [3, SURFACE + 1, 3]);
    // On stud terrain a build may name any block but air.
    assert_rejected(&mut region, designate(op(1), DesignationKind::Build(BlockId::AIR), area));
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
    let p = StudBox::of_tile(tile.min).unwrap();
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
    // The first box keeps its cells around the second: no cell is designated twice.
    let total: u32 = region.data().designations().keys().map(|b| b.cell_count()).sum();
    assert_eq!(total, 16 * STUD_TILE_CELLS);
    assert_eq!(region.data().designations().values().filter(|d| d.op == op(2)).count(), 1);
    apply_checked(&mut region, Order::CancelOp(op(1)));
    // The later op took its cells out of the first one's box; the rest of that box
    // went with the cancel.
    let d = region.data().designations();
    let (s, t) = (TILE_STUDS as i32, TILE_PLATES as i32);
    let cells: Vec<_> = d.keys().map(|b| b.cells()).collect();
    assert_eq!(cells, vec![([3 * s, SURFACE * t, 3 * s], [5 * s, (SURFACE + 1) * t, 5 * s])]);
    assert!(d.values().all(|d| d.op == op(2)));
}

#[test]
fn erase_filters_by_kind() {
    let mut region = order_region();
    let build = DesignationKind::Build(game::edit::PLACE_BLOCK);
    // Stone at the surface takes Dig, the open tiles above take Build.
    let area = TileBox::new([2, SURFACE, 2], [5, SURFACE + 1, 5]);
    apply_checked(&mut region, designate(op(1), DesignationKind::Dig, area));
    apply_checked(&mut region, designate(op(2), build, area));
    assert_eq!(region.data().designations().len(), 1 + 16, "one box of the slab, a tile box per build");
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
        assert_eq!(tile_at(&region, p), built(), "{p:?} built");
        assert!(region.data().constructed().contains_key(&p));
    }
    assert!(region.data().designations().is_empty());

    apply_checked(&mut region, designate(op(2), DesignationKind::Deconstruct, wall));
    assert_eq!(region.data().designations().len(), 16, "a built tile is two 2 x 2 bricks, each designated");
    apply_checked(&mut region, Order::DevCompleteAll);
    for p in wall.tiles() {
        assert_eq!(tile_at(&region, p), OPEN, "{p:?} taken down");
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
    region.with_data(|r| assert!(crate::common::carve_tile(r, stale)));
    let n = ORDER_REGION_BLOCKS as i32;
    let snapshot: Vec<_> = (0..n)
        .flat_map(|x| (SURFACE - 2..=SURFACE + 1).flat_map(move |y| (0..n).map(move |z| [x, y, z])))
        .map(|p| (p, tile_at(&region, p)))
        .collect();
    apply_checked(&mut region, Order::DevCompleteAll);
    assert!(region.data().designations().is_empty());
    for (p, was) in snapshot {
        let expect = if area.contains(p) { OPEN } else { was };
        assert_eq!(tile_at(&region, p), expect, "{p:?}");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn dig_box_down_to_bedrock_skips_bedrock() {
    let mut region = order_region();
    let area = TileBox::new([2, BEDROCK_Y - 1, 2], [3, SURFACE, 3]);
    apply_checked(&mut region, designate(op(1), DesignationKind::Dig, area));
    let d = region.data().designations();
    assert_eq!(d.len(), 2, "the stone slab's cells in each layer; bedrock refused");
    assert!(d.keys().all(|b| b.cells().0[1] >= (BEDROCK_Y + 1) * TILE_PLATES as i32));
    apply_checked(&mut region, Order::DevCompleteAll);
    for p in area.tiles() {
        let expect = if p[1] > BEDROCK_Y { OPEN } else { (TileFill::Solid, Some(MAT_BEDROCK)) };
        assert_eq!(tile_at(&region, p), expect, "{p:?}");
    }
}

fn pick_solid(cells: &[TilePos]) -> impl Fn(TilePos) -> bool {
    let set: BTreeSet<TilePos> = cells.iter().copied().collect();
    move |p| set.contains(&p)
}

fn straight_down() -> (rapier3d::math::Vector, rapier3d::math::Vector) {
    let s = crate::common::TILE_W;
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
    let s = crate::common::TILE_W;
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
    let s = crate::common::TILE_W;
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
    let s = crate::common::TILE_W;
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

/// `DevCompleteAll` writes in per-chunk batches (one undo entry per chunk —
/// a per-tile loop re-hashed whole fields per tile, seconds on a play-size
/// region, and the client replayed it on every rollback). The batched result
/// must be the per-tile one (tile fill and dominant material), across chunk
/// borders and into implied ground, and roll back bit-exact.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn complete_all_batches_match_per_tile_edits() {
    let build_region = || {
        let (region, recv) = stone_region(2);
        std::mem::forget(recv);
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
            crate::common::carve_tile(r, p);
        }
        let material = stud_material_of_block(game::edit::PLACE_BLOCK);
        for p in build.tiles() {
            let (lo, hi) = stud_tile_box(p).unwrap();
            assert!(r.fill_stud_box_safe(lo, hi, material) > 0);
        }
    });
    let span = TileBox::new([0, SURFACE - 8, 0], [2 * c - 1, SURFACE + 4, 2 * c - 1]);
    for p in span.tiles() {
        assert_eq!(tile_at(&batched, p), tile_at(&per_tile, p), "tile at {p:?}");
    }
    let built: Vec<TilePos> = batched.data().constructed().keys().copied().collect();
    let mut want: Vec<TilePos> = build.tiles().collect();
    want.sort();
    assert_eq!(built, want, "every build is flagged constructed");
    // Deconstructing them all clears the blocks and the flags.
    apply_checked(&mut batched, designate(op(3), DesignationKind::Deconstruct, build));
    apply_checked(&mut batched, Order::DevCompleteAll);
    assert!(batched.data().constructed().is_empty());
    assert!(build.tiles().all(|p| tile_at(&batched, p) == OPEN));
}

