//! Chopping trees (`game::trees`, `DesignationKind::Chop`): which blocks a
//! cut brings down, the tip spin that keeps a felled part inside the region,
//! and the order carried out into a falling voxel body that despawns.
use crate::common;

use common::{floor_chunks, r0};
use game::rapier3d::math::Vector;
use game::voxel_body::SETTLE_TIMEOUT_TICKS;
use game::{
    falling_sets, fell, install_block_props, state_hash, tip_angvel, Block, BlockId, BlockRegistry, DesignationKind,
    GameEventKind, OpId, Order, Region, Rollback, TileBox, TilePos, TileSource, BLOCK_VOXELS, DEFAULT_PRIORITY,
    REGION_BLOCKS, REGION_SIZE, TIP_ANGVEL, TIP_EDGE_MARGIN_BLOCKS,
};

const MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");
const STONE: BlockId = BlockId(2);
const TREE: u32 = 5;
const NEIGHBOUR: u32 = 6;

struct Ids {
    log: BlockId,
    branch: BlockId,
    leaves: BlockId,
}

fn ids() -> Ids {
    let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
    install_block_props(reg.props());
    let id = |n: &str| reg.id_of(n).unwrap();
    Ids { log: id("oak_log"), branch: id("oak_branch"), leaves: id("oak_leaves") }
}

/// Trunk column of the hand-built tree, in the middle of the region.
fn x0() -> i32 {
    REGION_BLOCKS as i32 / 2
}
const Z0: i32 = 8;
/// Top trunk block.
const TRUNK_TOP: i32 = 8;
/// The limb of the hand-built tree's branch.
const BRANCH_LIMB: u8 = 1;

/// A region floored one block deep over its whole area, no players. Its
/// render updates go to a forgotten receiver (rollback needs a sender).
fn region() -> Region {
    let (send, recv) = crossbeam::channel::unbounded();
    std::mem::forget(recv);
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), r0(), None);
    let chunks = floor_chunks(0.0..REGION_SIZE, 0.0..REGION_SIZE, BLOCK_VOXELS as u32);
    region.with_data(|r| r.seed_terrain_genesis(chunks));
    region
}

/// Tree `TREE`: a log trunk at (x0, 1..=8, Z0) under a 3×3 leaf cap at y 9
/// (limb 0), and a branch off the trunk at y 5 running +x to two leaves
/// (limb [`BRANCH_LIMB`]). Tree `NEIGHBOUR` stands 3 blocks −x with its
/// leaves touching the cap.
fn tree_blocks(ids: &Ids) -> Vec<(TilePos, Block)> {
    let x0 = x0();
    let mut out = Vec::new();
    let mut put = |p: TilePos, id: BlockId, tree: u32| {
        let limb = if tree == TREE && p[0] > x0 && p[1] < TRUNK_TOP { BRANCH_LIMB } else { 0 };
        out.push((p, Block::with_tree(id, tree, limb)))
    };
    for y in 1..=TRUNK_TOP {
        put([x0, y, Z0], ids.log, TREE);
    }
    for dx in -1..=1 {
        for dz in -1..=1 {
            put([x0 + dx, TRUNK_TOP + 1, Z0 + dz], ids.leaves, TREE);
        }
    }
    put([x0 + 1, 5, Z0], ids.branch, TREE);
    put([x0 + 2, 6, Z0], ids.branch, TREE);
    put([x0 + 3, 6, Z0], ids.branch, TREE);
    put([x0 + 4, 6, Z0], ids.leaves, TREE);
    put([x0 + 4, 7, Z0], ids.leaves, TREE);
    for y in 1..=TRUNK_TOP {
        put([x0 - 3, y, Z0], ids.log, NEIGHBOUR);
    }
    put([x0 - 2, TRUNK_TOP + 1, Z0], ids.leaves, NEIGHBOUR);
    out
}

fn forested_region(ids: &Ids) -> Region {
    let mut region = region();
    region.with_data(|r| {
        for (p, b) in tree_blocks(ids) {
            assert!(r.set_block_safe(p, b), "place {p:?}");
        }
    });
    region
}

fn tree_of(ids: &Ids, tree: u32) -> Vec<TilePos> {
    let mut v: Vec<_> = tree_blocks(ids).into_iter().filter(|(_, b)| b.tree_id() == tree).map(|(p, _)| p).collect();
    v.sort_unstable();
    v
}

#[test]
fn trunk_cut_drops_everything_above() {
    let ids = ids();
    let region = forested_region(&ids);
    let cut = [x0(), 3, Z0];
    let sets = falling_sets(region.data(), cut);
    assert_eq!(sets.len(), 1, "{sets:?}");
    let above: Vec<_> = tree_of(&ids, TREE).into_iter().filter(|p| p[1] > 3).collect();
    assert_eq!(sets[0], above, "everything above the cut, branch and crown included");
    // The stump (y 1, 2) is not in it.
    assert!(!sets[0].iter().any(|p| p[1] < 3));
}

#[test]
fn trunk_cut_above_a_limb_leaves_the_limb() {
    let ids = ids();
    let region = forested_region(&ids);
    let x0 = x0();
    // The branch's lowest wood is at y 5: a cut at y 6 leaves it on the stump.
    let sets = falling_sets(region.data(), [x0, 6, Z0]);
    assert_eq!(sets.len(), 1);
    assert!(sets[0].iter().all(|p| p[0] <= x0 + 1 && p[1] > 6), "only the trunk above and the cap: {sets:?}");
    // At y 5, the limb's own level, it falls with the trunk.
    let sets = falling_sets(region.data(), [x0, 5, Z0]);
    assert!(sets[0].contains(&[x0 + 4, 7, Z0]) && sets[0].contains(&[x0 + 2, 6, Z0]));
}

#[test]
fn limb_log_cut_takes_one_block_and_fells_the_limb() {
    let ids = ids();
    let mut region = forested_region(&ids);
    let x0 = x0();
    // Make the branch's first block a limb log (as generated limbs are).
    region.with_data(|r| assert!(r.set_block_safe([x0 + 1, 5, Z0], Block::with_tree(ids.log, TREE, BRANCH_LIMB))));
    let f = fell(region.data(), [x0 + 1, 5, Z0]).unwrap();
    assert_eq!(f.cut, vec![[x0 + 1, 5, Z0]], "not the trunk's cross-section");
    assert_eq!(f.falling, vec![vec![[x0 + 2, 6, Z0], [x0 + 3, 6, Z0], [x0 + 4, 6, Z0], [x0 + 4, 7, Z0]]]);
}

#[test]
fn base_cut_drops_the_whole_rest() {
    let ids = ids();
    let region = forested_region(&ids);
    let cut = [x0(), 1, Z0];
    let rest: Vec<_> = tree_of(&ids, TREE).into_iter().filter(|&p| p != cut).collect();
    assert_eq!(falling_sets(region.data(), cut), vec![rest]);
}

#[test]
fn branch_cut_drops_only_that_branch() {
    let ids = ids();
    let region = forested_region(&ids);
    let x0 = x0();
    let sets = falling_sets(region.data(), [x0 + 1, 5, Z0]);
    assert_eq!(sets, vec![vec![[x0 + 2, 6, Z0], [x0 + 3, 6, Z0], [x0 + 4, 6, Z0], [x0 + 4, 7, Z0]]]);
}

#[test]
fn touching_neighbour_crown_is_untouched() {
    let ids = ids();
    let region = forested_region(&ids);
    // The cap leaf at x0 − 1 touches the neighbour's leaf at x0 − 2.
    assert_eq!(TileSource::tree_id(region.data(), [x0() - 2, TRUNK_TOP + 1, Z0]), NEIGHBOUR);
    let sets = falling_sets(region.data(), [x0(), 3, Z0]);
    let neighbour = tree_of(&ids, NEIGHBOUR);
    assert!(sets.iter().flatten().all(|p| !neighbour.contains(p)), "no block of the neighbour falls");
    assert!(sets.iter().flatten().all(|&p| TileSource::tree_id(region.data(), p) == TREE));
    // And chopping the neighbour's trunk fells only the neighbour.
    let sets = falling_sets(region.data(), [x0() - 3, 2, Z0]);
    let above: Vec<_> = neighbour.into_iter().filter(|p| p[1] > 2).collect();
    assert_eq!(sets, vec![above]);
}

#[test]
fn leaves_beside_the_cut_do_not_hold_the_part_up() {
    let ids = ids();
    let mut region = forested_region(&ids);
    let x0 = x0();
    // A leaf strip along the trunk's −z side, y 2..=5: it touches both sides
    // of a cut at y 3.
    let strip: Vec<TilePos> = (2..=5).map(|y| [x0, y, Z0 - 1]).collect();
    region.with_data(|r| {
        for &p in &strip {
            assert!(r.set_block_safe(p, Block::with_tree_id(ids.leaves, TREE)));
        }
    });
    let sets = falling_sets(region.data(), [x0, 3, Z0]);
    assert_eq!(sets.len(), 1, "{sets:?}");
    // y 2 and 3 are as near the stump as the fallen trunk (a tie stands);
    // y 4 and 5 are nearer the fallen trunk.
    assert!(!sets[0].contains(&strip[0]) && !sets[0].contains(&strip[1]));
    assert!(sets[0].contains(&strip[2]) && sets[0].contains(&strip[3]));
    assert!(sets[0].contains(&[x0, TRUNK_TOP, Z0]));
}

/// Tree 9: a 2×2 log trunk at (x0..=x0+1, 1..=6, 20..=21), wood only.
fn wide_trunk(ids: &Ids) -> (Region, Vec<TilePos>) {
    let mut region = region();
    let x0 = x0();
    let mut logs = Vec::new();
    for y in 1..=6 {
        for z in 20..=21 {
            for x in x0..=x0 + 1 {
                logs.push([x, y, z]);
            }
        }
    }
    region.with_data(|r| {
        for &p in &logs {
            assert!(r.set_block_safe(p, Block::with_tree_id(ids.log, 9)));
        }
    });
    logs.sort_unstable();
    (region, logs)
}

#[test]
fn trunk_cut_takes_the_whole_layer() {
    let ids = ids();
    let (region, logs) = wide_trunk(&ids);
    let f = fell(region.data(), [x0(), 3, 20]).unwrap();
    let layer: Vec<_> = logs.iter().copied().filter(|p| p[1] == 3).collect();
    assert_eq!(f.cut, layer, "the 2×2 cross-section");
    let above: Vec<_> = logs.iter().copied().filter(|p| p[1] > 3).collect();
    assert_eq!(f.falling, vec![above]);
}

#[test]
fn anchor_layer_cut_fells_everything() {
    let ids = ids();
    let (region, logs) = wide_trunk(&ids);
    let f = fell(region.data(), [x0() + 1, 1, 21]).unwrap();
    assert_eq!(f.cut.len(), 4);
    let rest: Vec<_> = logs.iter().copied().filter(|p| p[1] > 1).collect();
    assert_eq!(f.falling, vec![rest]);
}

#[test]
fn not_a_tree_has_no_falling_set() {
    let ids = ids();
    let region = forested_region(&ids);
    assert!(falling_sets(region.data(), [x0(), 0, Z0]).is_empty(), "the floor");
    assert!(falling_sets(region.data(), [x0(), 20, Z0]).is_empty(), "air");
}

fn order(kind: DesignationKind, a: TilePos, b: TilePos) -> Order {
    Order::Designate { op: OpId { client: 0, seq: 1 }, kind, priority: DEFAULT_PRIORITY, area: TileBox::new(a, b) }
}

#[test]
fn chop_invalid_on_stone_and_leaves() {
    let ids = ids();
    let mut region = forested_region(&ids);
    let x0 = x0();
    // Floor stone, two trunk logs, a cap leaf and air above it.
    region.with_data(|r| assert!(r.set_block_safe([x0, 0, Z0], Block::new(STONE))));
    let mut ok = false;
    region.with_data(|r| ok = r.apply_order_safe(0, order(DesignationKind::Chop, [x0, 0, Z0], [x0, TRUNK_TOP + 2, Z0])));
    assert!(ok);
    let marked: Vec<_> = region.data().designations().keys().copied().collect();
    let logs: Vec<_> = (1..=TRUNK_TOP).map(|y| [x0, y, Z0]).collect();
    assert_eq!(marked, logs, "only the logs: not the stone floor, the leaf or the air");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn chop_completes_into_a_falling_body() {
    let ids = ids();
    let mut region = forested_region(&ids);
    let x0 = x0();
    let cut = [x0, 3, Z0];
    let falling: Vec<_> = tree_of(&ids, TREE).into_iter().filter(|p| p[1] > 3).collect();
    let stump: Vec<_> = tree_of(&ids, TREE).into_iter().filter(|p| p[1] < 3).collect();
    region.handle_event(GameEventKind::Order(0, order(DesignationKind::Chop, cut, cut))).unwrap();
    region.forget_last_event();

    let before = state_hash(region.data());
    region.handle_event(GameEventKind::Order(0, Order::DevCompleteAll)).unwrap();
    region.rollback_last_event();
    assert_eq!(before, state_hash(region.data()), "the chop rolls back bit-exact");
    region.handle_event(GameEventKind::Order(0, Order::DevCompleteAll)).unwrap();
    region.forget_last_event();

    let r = region.data();
    assert!(r.designations().is_empty());
    let bodies: Vec<_> = r.ecs.voxel_body.iter().filter_map(|(k, v)| v.as_ref().map(|v| (k, v.clone()))).collect();
    assert_eq!(bodies.len(), 1, "one falling part");
    let (body, vb) = &bodies[0];
    assert_eq!(vb.model.blocks.len(), falling.len());
    for &p in falling.iter().chain([&cut]) {
        assert_eq!(TileSource::block(r, p), BlockId::AIR, "{p:?} cleared");
    }
    for &p in &stump {
        assert_eq!(TileSource::block(r, p), ids.log, "stump {p:?} stays");
    }
    for p in tree_of(&ids, NEIGHBOUR) {
        assert_eq!(TileSource::tree_id(r, p), NEIGHBOUR, "neighbour {p:?} stays");
    }
    let spin = r.physics.bodies.get(*r.ecs.rigidbody.get(*body)).unwrap().angvel();
    assert!((spin.length() - TIP_ANGVEL).abs() < 1e-5 && spin.y == 0.0, "tipping: {spin:?}");

    for _ in 0..SETTLE_TIMEOUT_TICKS + 5 {
        region.handle_event(GameEventKind::Tick).unwrap();
        region.forget_last_event();
        if !region.data().ecs.entities.contains_key(*body) {
            break;
        }
    }
    assert!(!region.data().ecs.entities.contains_key(*body), "the felled part despawned");
    assert!(region.data().ecs.voxel_body.iter().all(|(_, v)| v.is_none()));
}

/// The direction the top of a part spun at `w` moves: `w × up`, unit length.
fn tip_direction(w: Vector) -> Vector {
    w.cross(Vector::Y) / TIP_ANGVEL
}

/// A 6-high part straight above `cut`, leaning `lean` blocks in x.
fn part(cut: TilePos, lean: i32) -> Vec<TilePos> {
    (1..=6).map(|dy| [cut[0] + lean, cut[1] + dy, cut[2]]).collect()
}

#[test]
fn tip_follows_the_lean() {
    let mid = REGION_BLOCKS as i32 / 2;
    let cut = [mid, 1, mid];
    let d = tip_direction(tip_angvel(TREE, cut, &part(cut, -1), 0));
    assert!((d - Vector::new(-1.0, 0.0, 0.0)).length() < 1e-5, "{d:?}");
    let d = tip_direction(tip_angvel(TREE, cut, &part(cut, 1), 0));
    assert!((d - Vector::new(1.0, 0.0, 0.0)).length() < 1e-5, "{d:?}");
    // No lean: a rolled direction, the same on every replay.
    let a = tip_angvel(TREE, cut, &part(cut, 0), 7);
    assert_eq!(a, tip_angvel(TREE, cut, &part(cut, 0), 7));
    assert!((a.length() - TIP_ANGVEL).abs() < 1e-5 && a.y == 0.0);
}

#[test]
fn tip_near_edge_points_inward() {
    let mid = REGION_BLOCKS as i32 / 2;
    let edge = TIP_EDGE_MARGIN_BLOCKS - 1;
    // Leaning out over the −x edge: every direction with any −x, and ±z too
    // (the cut itself is inside the margin), would land too close; the
    // nearest-angle direction left is a +x diagonal.
    let cut = [edge, 1, mid];
    let d = tip_direction(tip_angvel(TREE, cut, &part(cut, -1), 0));
    assert!(d.x > 0.5, "tips inward: {d:?}");
    // Leaning +x at the +x edge: tips back −x.
    let cut = [REGION_BLOCKS as i32 - 1 - edge, 1, mid];
    let d = tip_direction(tip_angvel(TREE, cut, &part(cut, 1), 0));
    assert!(d.x < -0.5, "tips inward: {d:?}");
    // Leaning −x with only the −x landing too close: the nearest diagonal.
    let cut = [9, 1, mid];
    let d = tip_direction(tip_angvel(TREE, cut, &part(cut, -1), 0));
    assert!(d.x < -0.5 && d.z.abs() > 0.5, "a −x diagonal: {d:?}");
    // Near the edge but leaning inward and short enough: keeps the lean.
    let cut = [TIP_EDGE_MARGIN_BLOCKS + 1, 1, mid];
    let d = tip_direction(tip_angvel(TREE, cut, &part(cut, 1), 0));
    assert!((d - Vector::new(1.0, 0.0, 0.0)).length() < 1e-5, "{d:?}");
    // Rolled with no lean: whatever the roll, the landing stays inside.
    for tick in 0..32 {
        let cut = [edge, 1, edge];
        let d = tip_direction(tip_angvel(TREE, cut, &part(cut, 0), tick));
        assert!(d.x > 0.5 && d.z > 0.5, "tick {tick}: {d:?}");
    }
}

/// A cactus is one block (`cactus`) for trunk and arms, no leaves: a cut on
/// its column is a trunk cut (`SINGLE_BLOCK_TREES`), so the column above
/// falls with any arm rooted at or above the cut, and an arm rooted below
/// stays on the stump.
#[test]
fn cactus_column_cut_fells_the_top_and_its_arms() {
    let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
    install_block_props(reg.props());
    let cactus = reg.id_of("cactus").unwrap();
    assert!(game::block_props().is_trunk(cactus));
    let mut region = region();
    let x0 = x0();
    // Column (x0, 1..=7, Z0), limb 0; arm 1 out +x at y 2 rising to y 3
    // (under the cut layer); arm 2 out −x at y 5 rising to y 6.
    let mut blocks: Vec<(TilePos, u8)> = (1..=7).map(|y| ([x0, y, Z0], 0)).collect();
    blocks.extend([([x0 + 1, 2, Z0], 1), ([x0 + 2, 2, Z0], 1), ([x0 + 2, 3, Z0], 1)]);
    blocks.extend([([x0 - 1, 5, Z0], 2), ([x0 - 2, 5, Z0], 2), ([x0 - 2, 6, Z0], 2)]);
    region.with_data(|r| {
        for &(p, limb) in &blocks {
            assert!(r.set_block_safe(p, Block::with_tree(cactus, TREE, limb)), "place {p:?}");
        }
    });
    let sets = falling_sets(region.data(), [x0, 4, Z0]);
    assert_eq!(sets.len(), 1, "{sets:?}");
    let mut want: Vec<TilePos> =
        blocks.iter().map(|&(p, _)| p).filter(|p| (p[0] == x0 && p[1] > 4) || p[0] < x0).collect();
    want.sort_unstable();
    assert_eq!(sets[0], want, "the column above and the upper arm; the lower arm stays");
}

// --- Felling as an Adventure player (plan 2026-11-05, Decision 9) ---

/// About 45 degrees down (as in `mining.rs`).
const DOWN_45: f32 = 302.0;

fn input(ev: game::InputEvent) -> GameEventKind {
    GameEventKind::PlayerInput(0, ev)
}

fn forget(region: &mut Region, ev: GameEventKind) {
    region.handle_event(ev).unwrap();
    region.forget_last_event();
}

fn player_body(region: &Region) -> [f32; 3] {
    let data = region.data();
    let e = *data.player_entites.get(&0).unwrap();
    data.physics.bodies.get(*data.ecs.rigidbody.get(e)).unwrap().translation().to_array()
}

fn place_player(region: &mut Region, x: f32, z: f32) {
    let y = player_body(region)[1];
    region.with_data(|r| {
        let e = *r.data.player_entites.get(&0).unwrap();
        r.set_body_pose_safe(e, game::Pose::translation(x, y, z));
    });
}

/// The forested region with player 0 in Adventure, first person, standing
/// just +z of the trunk and looking 45 degrees down at its base log
/// (x0, 1, Z0). Setup is forgotten.
fn adventure_at_the_trunk(ids: &Ids) -> Region {
    let mut region = forested_region(ids);
    forget(&mut region, GameEventKind::CreateClient(0));
    forget(&mut region, input(game::InputEvent::FpsCam(true)));
    region.with_data(|r| {
        let c = r.clients.get_mut(&0).unwrap();
        c.mode = game::PlayMode::Adventure;
        c.inventory = game::Inventory::new();
    });
    for _ in 0..12 {
        forget(&mut region, GameEventKind::Tick);
    }
    let bv = BLOCK_VOXELS as f32;
    place_player(&mut region, (x0() as f32 + 0.5) * bv, (Z0 as f32 + 2.1) * bv);
    forget(&mut region, input(game::InputEvent::MouseMotion { dx: 0.0, dy: DOWN_45 }));
    for _ in 0..3 {
        forget(&mut region, GameEventKind::Tick);
    }
    let hit = region.data().eye_hit(0).expect("the eye ray hits the trunk").block;
    assert_eq!(hit, [x0(), 1, Z0], "aimed at the base log");
    region
}

fn press(pressed: bool) -> GameEventKind {
    input(game::InputEvent::MouseButton { button: game::MouseButton::Left, pressed })
}

/// Holds the left button on the base log until it breaks (asserting the
/// trunk timing) and releases it; with `move_away`, then moves the player
/// well away (outside any event) so it picks nothing up. Returns the events
/// run (none forgotten).
fn chop_base(region: &mut Region, ids: &Ids, move_away: bool) -> usize {
    let needed = game::break_ticks(game::block_props(), Block::with_tree(ids.log, TREE, 0)) as usize;
    let mut events = 0;
    region.handle_event(press(true)).unwrap();
    events += 1;
    for n in 1..=needed {
        region.handle_event(GameEventKind::Tick).unwrap();
        events += 1;
        let gone = TileSource::block(region.data(), [x0(), 1, Z0]) == BlockId::AIR;
        assert_eq!(gone, n == needed, "the trunk breaks on tick {needed}, not {n}");
    }
    region.handle_event(press(false)).unwrap();
    events += 1;
    if !move_away {
        return events;
    }
    let bv = BLOCK_VOXELS as f32;
    region.with_data(|r| {
        let e = *r.data.player_entites.get(&0).unwrap();
        let y = r.data.physics.bodies.get(*r.data.ecs.rigidbody.get(e)).unwrap().translation().y;
        r.set_body_pose_safe(e, game::Pose::translation((x0() as f32 + 0.5) * bv, y, (Z0 as f32 + 40.5) * bv));
    });
    events
}

/// Ticks until no voxel body is left (at most the settle timeout and a
/// margin). Returns the events run.
fn run_to_settle(region: &mut Region) -> usize {
    let mut events = 0;
    for _ in 0..SETTLE_TIMEOUT_TICKS + 5 {
        region.handle_event(GameEventKind::Tick).unwrap();
        events += 1;
        if region.data().ecs.voxel_body.iter().all(|(_, v)| v.is_none()) {
            return events;
        }
    }
    panic!("the felled part never settled");
}

/// Total `item` over every pile, and the pile cells holding it.
fn piled(region: &Region, item: game::ItemId) -> (u32, Vec<TilePos>) {
    let mut total = 0;
    let mut cells = Vec::new();
    for (p, pile) in region.data().item_piles() {
        let n: u32 = pile.stacks.iter().filter(|s| s.item == item).map(|s| u32::from(s.count)).sum();
        if n > 0 {
            total += n;
            cells.push(game::local_cell(*region.data().region, *p));
        }
    }
    (total, cells)
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn player_trunk_cut_yields_every_log() {
    let ids = ids();
    let mut region = adventure_at_the_trunk(&ids);
    chop_base(&mut region, &ids, true);
    assert!(region.data().ecs.voxel_body.iter().any(|(_, v)| v.is_some()), "the tree falls as a body");
    run_to_settle(&mut region);
    let r = region.data();
    for p in tree_of(&ids, TREE) {
        assert_eq!(TileSource::block(r, p), BlockId::AIR, "{p:?} felled");
    }
    let (logs, _) = piled(&region, game::ItemId::block(ids.log));
    assert_eq!(logs, 8, "every trunk log: {:?}", region.data().item_piles());
    let (sticks, _) = piled(&region, game::STICK);
    assert_eq!(sticks, 3, "one stick per branch block");
    let (leaves, _) = piled(&region, game::ItemId::block(ids.leaves));
    assert_eq!(leaves, 0, "leaves drop nothing");
    assert!(region.data().item_piles().len() <= 3, "merged into a few piles: {:?}", region.data().item_piles());
    assert!(region.data().clients[&0].inventory.slots.iter().all(|s| s.is_none()), "nothing picked up");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn logs_lie_where_tree_landed() {
    let ids = ids();
    let mut region = adventure_at_the_trunk(&ids);
    chop_base(&mut region, &ids, true);
    run_to_settle(&mut region);
    let (_, cells) = piled(&region, game::ItemId::block(ids.log));
    let reach = (TRUNK_TOP + 2) as f32;
    let dist = |p: &TilePos| (((p[0] - x0()).pow(2) + (p[2] - Z0).pow(2)) as f32).sqrt();
    for p in &cells {
        assert!(dist(p) <= reach, "pile {p:?} within the tree's reach of the stump");
    }
    assert!(cells.iter().any(|p| dist(p) >= 3.0), "logs lie along the fallen tree, not on the stump: {cells:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn fortress_chop_still_yields_nothing() {
    let ids = ids();
    let mut region = forested_region(&ids);
    let cut = [x0(), 1, Z0];
    forget(&mut region, GameEventKind::Order(0, order(DesignationKind::Chop, cut, cut)));
    forget(&mut region, GameEventKind::Order(0, Order::DevCompleteAll));
    assert!(region.data().ecs.voxel_body.iter().any(|(_, v)| v.is_some()), "the tree falls as a body");
    assert!(region.data().ecs.voxel_body.iter().all(|(_, v)| v.as_ref().is_none_or(|v| v.policy == game::SettlePolicy::Despawn)));
    run_to_settle(&mut region);
    assert!(region.data().item_piles().is_empty(), "a chop order leaves no items");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn player_chop_undo_roundtrip() {
    let ids = ids();
    let mut region = adventure_at_the_trunk(&ids);
    let before = state_hash(region.data());
    let mut events = chop_base(&mut region, &ids, false);
    events += run_to_settle(&mut region);
    assert!(!region.data().item_piles().is_empty(), "the chop dropped logs");
    for _ in 0..events {
        region.rollback_last_event();
    }
    assert_eq!(state_hash(region.data()), before, "felling, settling and the drops roll back bit-exact");
    assert!(region.data().item_piles().is_empty());
    assert_eq!(TileSource::block(region.data(), [x0(), 1, Z0]), ids.log);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn player_mining_a_tree_leaf_breaks_it_alone() {
    let ids = ids();
    let mut region = adventure_at_the_trunk(&ids);
    // A tree-id leaf in front of the trunk's base, in the eye ray's way.
    let leaf = [x0(), 1, Z0 + 1];
    region.with_data(|r| assert!(r.set_block_safe(leaf, Block::with_tree(ids.leaves, TREE, 0))));
    assert_eq!(region.data().eye_hit(0).unwrap().block, leaf, "aimed at the leaf");
    let needed = game::break_ticks(game::block_props(), Block::with_tree(ids.leaves, TREE, 0)) as usize;
    region.handle_event(press(true)).unwrap();
    let mut broke = None;
    for n in 1..=needed + 5 {
        region.handle_event(GameEventKind::Tick).unwrap();
        if TileSource::block(region.data(), leaf) == BlockId::AIR {
            broke = Some(n);
            break;
        }
    }
    assert_eq!(broke, Some(needed), "the leaf breaks on its own timing");
    let r = region.data();
    assert!(r.ecs.voxel_body.iter().all(|(_, v)| v.is_none()), "nothing fells");
    for p in tree_of(&ids, TREE) {
        assert_ne!(TileSource::block(r, p), BlockId::AIR, "{p:?} stands");
    }
    assert!(r.item_piles().is_empty(), "leaves drop nothing");
}
