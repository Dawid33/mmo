//! Gameplay ported onto stud terrain (stud world plan P6b): item piles, drops,
//! chests, tree felling, stud bodies and water — the stud twins of `item_piles`,
//! `containers`, `tree_fall` / `voxel_body` and `water`, each with the rollback bar.
use std::collections::{BTreeMap, BTreeSet};

use crate::common;
use game::movement::BodyScale;
use game::rapier3d::math::Vector;
use game::*;

/// Surface plate of the flat ground: the top of tile level 4, so things rest in level 5.
const TOP: i32 = 30;
const STAND: i32 = TOP / TILE_PLATES as i32;
const BEDROCK: i32 = -12;
const SOIL: StudProfile = StudProfile { cover: MAT_GRASS, cover_plates: 1, soil: MAT_DIRT, soil_plates: 5 };
const CLIENT: ClientId = 7;
/// The hand-made tree's id.
const TREE: u16 = 7;
const N: usize = STUD_CHUNK_STUDS;

fn flat() -> StudColumnGround {
    StudColumnGround::flat(TOP, SOIL, MAT_STONE, BEDROCK)
}

fn flat_ground(cols: usize) -> Vec<(ColumnCoords, StudColumnGround)> {
    (0..cols).flat_map(|x| (0..cols).map(move |z| (ColumnCoords::new(x, z), flat()))).collect()
}

/// A bare stud sim over `seed` (no region around it): for exact rollback bars.
fn world_of(seed: StudRegionSeed) -> Rollback {
    common::install_real_block_props();
    // Render updates go nowhere (the receiver is leaked so sends never fail).
    let (send, recv) = crossbeam::channel::unbounded();
    std::mem::forget(recv);
    let mut rb = Rollback::new(Some(send));
    let extra = seed.feature_chunks();
    rb.seed_stud_ground_genesis(seed.ground, extra);
    rb.set_body_scale_raw(BodyScale::STUD);
    rb
}

fn world(cols: usize) -> Rollback {
    world_of(StudRegionSeed { ground: flat_ground(cols), features: Vec::new() })
}

fn region_of(seed: StudRegionSeed) -> Region {
    common::install_real_block_props();
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Region::new(Rollback::new(Some(send.clone())), Some(send), RegionCoords::new(0, 0), None);
    r.seed_stud_world(seed);
    r
}

/// The rollback bar for `f`: applied and undone the hash is the one before; then it
/// is applied for good and its result returned.
fn bar<T>(rb: &mut Rollback, what: &str, f: impl Fn(&mut Rollback) -> T) -> T {
    let before = state_hash(rb);
    rb.new_transaction();
    let _ = f(rb);
    assert_ne!(state_hash(rb), before, "{what}: changed nothing");
    rb.rollback();
    assert_eq!(state_hash(rb), before, "{what}: hash(before) == hash(after undo)");
    rb.new_transaction();
    let out = f(rb);
    rb.forget();
    out
}

fn w(p: TilePos) -> WorldBlockPos {
    WorldBlockPos::from_local(RegionCoords::new(0, 0), BlockPos::new(p[0], p[1], p[2]))
}

fn piles(rb: &Rollback) -> Vec<(TilePos, Vec<ItemStack>)> {
    rb.item_piles().iter().map(|(p, pile)| (local_cell(RegionCoords::new(0, 0), *p), pile.stacks.clone())).collect()
}

fn item(name: &str) -> ItemId {
    ItemId::block(block_props().id_named(name).unwrap_or_else(|| panic!("no block {name}")))
}

/// Items of `id` over every pile.
fn piled(rb: &Rollback, id: ItemId) -> u32 {
    rb.item_piles().values().flat_map(|p| &p.stacks).filter(|s| s.item == id).map(|s| s.count).sum()
}

fn stack(name: &str, count: u32) -> ItemStack {
    ItemStack { item: item(name), count }
}

/// The stud cell box of tile `p`.
fn tile_box(p: TilePos) -> ([i32; 3], [i32; 3]) {
    stud_tile_box(p).unwrap()
}

// --- Item piles ------------------------------------------------------------------

#[test]
fn piles_rest_on_stud_floors() {
    let mut rb = world(1);
    // From the air: down onto the ground. From inside the ground: up out of it.
    let lost = bar(&mut rb, "deposit", |rb| rb.deposit_items_safe([4, STAND + 6, 4], stack("dirt", 3), 0, 0));
    assert_eq!(lost, 0);
    bar(&mut rb, "deposit in rock", |rb| rb.deposit_items_safe([10, 1, 10], stack("stone", 1), 0, 0));
    assert_eq!(piles(&rb), vec![([4, STAND, 4], vec![stack("dirt", 3)]), ([10, STAND, 10], vec![stack("stone", 1)])]);
    // A low floor (3 plates: a body in the tile stands on it) holds the pile in its
    // own tile; a higher one (4 plates: the floor of the tile above) pushes it up.
    let (lo, _) = tile_box([8, STAND, 8]);
    assert_eq!(rb.fill_stud_box_safe(lo, [lo[0] + 2, lo[1] + 3, lo[2] + 2], MAT_STONE), 12);
    let (lo, _) = tile_box([12, STAND, 12]);
    assert_eq!(rb.fill_stud_box_safe(lo, [lo[0] + 2, lo[1] + 4, lo[2] + 2], MAT_STONE), 16);
    assert!(!rb.pile_support(w([8, STAND, 8])) && rb.pile_rests_in(w([8, STAND, 8])));
    assert!(rb.pile_support(w([12, STAND, 12])) && rb.pile_rests_in(w([12, STAND + 1, 12])));
    assert_eq!(rb.settle_cell(w([8, STAND + 4, 8])), w([8, STAND, 8]));
    assert_eq!(rb.settle_cell(w([12, STAND, 12])), w([12, STAND + 1, 12]));
    // A plate floating in the air holds a pile too (it is the tile's floor).
    let (lo, _) = tile_box([2, STAND + 3, 2]);
    assert_eq!(rb.fill_stud_box_safe(lo, [lo[0] + 2, lo[1] + 1, lo[2] + 2], MAT_STONE), 4);
    assert_eq!(rb.settle_cell(w([2, STAND + 8, 2])), w([2, STAND + 3, 2]));
    assert_eq!(rb.settle_cell(w([2, STAND + 2, 2])), w([2, STAND, 2]), "under the plate: the ground");
}

#[test]
fn piles_follow_digs_and_builds() {
    let mut rb = world(1);
    let (here, below) = ([4, STAND, 4], [4, STAND - 1, 4]);
    rb.deposit_items_safe(here, stack("sand", 2), 0, 0);
    assert_eq!(piles(&rb), vec![(here, vec![stack("sand", 2)])]);
    // The tile under it dug away: the pile falls into the hole (with what the dig
    // yields, were its drops asked for — this is the plain carve).
    let (lo, hi) = tile_box(below);
    let removed = bar(&mut rb, "carve under a pile", |rb| rb.carve_stud_safe(lo, hi));
    assert_eq!(removed.iter().map(|(_, cells)| cells).sum::<u32>(), STUD_TILE_CELLS);
    assert_eq!(piles(&rb), vec![(below, vec![stack("sand", 2)])]);
    // The hole filled again: the pile is pushed back up.
    let filled = bar(&mut rb, "fill a pile's tile", |rb| rb.fill_stud_box_safe(lo, hi, MAT_STONE));
    assert_eq!(filled, STUD_TILE_CELLS);
    assert_eq!(piles(&rb), vec![(here, vec![stack("sand", 2)])]);
    // One brick placed into the pile's tile (a brick is 3 plates: a low floor): it stays.
    let part = stud_carve::box_part([2, 3, 2]).expect("a 2 x 2 brick");
    let brick = StudBrick::part_at([8, 30, 8], part.0, part.1, 4);
    assert!(rb.place_stud_safe(StudChunkCoords { x: 0, y: 0, z: 0 }, brick));
    assert_eq!(piles(&rb), vec![(here, vec![stack("sand", 2)])]);
    // A second brick on it makes the tile solid: up it goes.
    let brick = StudBrick::part_at([8, 33, 8], part.0, part.1, 4);
    bar(&mut rb, "place a brick into a pile's tile", |rb| rb.place_stud_safe(StudChunkCoords { x: 0, y: 0, z: 0 }, brick));
    assert_eq!(piles(&rb), vec![([4, STAND + 1, 4], vec![stack("sand", 2)])]);
    // And removed again: back down onto the first brick.
    bar(&mut rb, "remove the brick", |rb| rb.remove_stud_safe(StudChunkCoords { x: 0, y: 0, z: 0 }, [8, 33, 8]));
    assert_eq!(piles(&rb), vec![(here, vec![stack("sand", 2)])]);
}

fn tick(r: &mut Region) {
    r.handle_event(GameEventKind::Tick).unwrap();
    r.forget_last_event();
}

/// A flat stud region with one Adventure player standing on tile `at`.
fn region_with_adventurer(at: TilePos) -> (Region, EntityKey) {
    let mut r = region_of(StudRegionSeed { ground: flat_ground(REGION_CHUNKS), features: Vec::new() });
    r.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
    r.forget_last_event();
    r.handle_event(GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Adventure, at: Some(at) })).unwrap();
    r.forget_last_event();
    for _ in 0..10 {
        tick(&mut r);
    }
    let e = *r.data().player_entites.get(&CLIENT).unwrap();
    (r, e)
}

fn carried(r: &Region, id: ItemId) -> u32 {
    r.data().clients[&CLIENT].inventory.slots.iter().flatten().filter(|s| s.item == id).map(|s| s.count).sum()
}

#[test]
fn stud_bodies_pick_piles_up_within_a_tile() {
    let at = [20, STAND, 20];
    let (mut r, _) = region_with_adventurer(at);
    let before = carried(&r, item("sand"));
    // Out of reach (the body's box grown by one tile): 3 tiles away, and 4 levels up.
    r.with_data(|d| {
        d.deposit_items_safe([23, STAND, 20], stack("sand", 1), 0, 0);
        let (lo, hi) = stud_tile_box([20, STAND + 5, 20]).unwrap();
        d.fill_stud_box_safe(lo, hi, MAT_STONE);
        d.deposit_items_safe([20, STAND + 6, 20], stack("sand", 1), 0, 0);
    });
    let tick_now = *r.data().tick;
    r.with_data(|d| d.tick_pickups_safe(tick_now + PICKUP_DELAY_TICKS));
    assert_eq!(carried(&r, item("sand")), before, "nothing in reach");
    assert_eq!(r.data().item_piles().len(), 2);
    // Beside it, a tile away diagonally, and on a low floor in the body's own tile row.
    r.with_data(|d| {
        d.deposit_items_safe([21, STAND, 21], stack("sand", 2), 0, 0);
        d.deposit_items_safe([19, STAND, 20], stack("sand", 3), 0, 0);
    });
    let h = state_hash(r.data());
    r.handle_event(GameEventKind::Tick).unwrap();
    r.rollback_last_event();
    assert_eq!(state_hash(r.data()), h, "a tick with pending pickups rolls back");
    for _ in 0..PICKUP_DELAY_TICKS + 2 {
        tick(&mut r);
    }
    assert_eq!(carried(&r, item("sand")), before + 5);
    assert_eq!(r.data().item_piles().len(), 2, "the far piles stay");
}

// --- Drops ------------------------------------------------------------------------

fn order(r: &mut Region, o: Order) {
    r.handle_event(GameEventKind::Order(CLIENT, o)).unwrap();
}

fn designate(seq: u32, kind: DesignationKind, a: TilePos, b: TilePos) -> Order {
    Order::Designate { op: OpId { client: CLIENT, seq }, kind, priority: DEFAULT_PRIORITY, area: TileBox::new(a, b) }
}

#[test]
fn digging_and_deconstructing_drop_into_piles() {
    let mut r = region_of(StudRegionSeed { ground: flat_ground(2), features: Vec::new() });
    let (a, b) = ([4, STAND - 1, 4], [5, STAND - 1, 4]);
    order(&mut r, designate(1, DesignationKind::Dig, a, b));
    r.forget_last_event();
    let h = state_hash(r.data());
    order(&mut r, Order::DevCompleteAll);
    // What a tile of the flat ground yields: its grass and its dirt each drop dirt,
    // one per tile's worth of cells, rounded per material.
    let (lo, hi) = tile_box(a);
    let per_tile: u32 = stud_drops(&world(1).carve_stud_safe(lo, hi)).iter().map(|s| s.count).sum();
    assert_eq!(stud_drops(&[(MAT_GRASS, 4), (MAT_DIRT, 20)]), vec![stack("dirt", 1)], "4 cells are too few for an item");
    assert!((1..=2).contains(&per_tile));
    assert_eq!(piled(r.data(), item("dirt")), 2 * per_tile, "{:?}", piles(r.data()));
    assert_eq!(r.data().item_piles().values().map(|p| p.count()).sum::<u32>(), 2 * per_tile, "nothing but dirt");
    assert!(piles(r.data()).iter().all(|(p, _)| p[1] == STAND - 1 && (4..=5).contains(&p[0])), "{:?}", piles(r.data()));
    r.rollback_last_event();
    assert_eq!(state_hash(r.data()), h, "the dig and its drops undone");
    assert!(r.data().item_piles().is_empty());
    order(&mut r, Order::DevCompleteAll);
    r.forget_last_event();
    // A stone tile built and deconstructed gives its stone back.
    let stone = block_props().id_named("stone").unwrap();
    let c = [9, STAND, 9];
    order(&mut r, designate(2, DesignationKind::Build(stone), c, c));
    order(&mut r, Order::DevCompleteAll);
    assert_eq!(r.data().tile_material(c), Some(MAT_STONE));
    order(&mut r, designate(3, DesignationKind::Deconstruct, c, c));
    let h = state_hash(r.data());
    order(&mut r, Order::DevCompleteAll);
    let yielded = stud_drops(&[(MAT_STONE, STUD_TILE_CELLS)]);
    assert_eq!(yielded.len(), 1, "a stone tile yields something");
    assert_eq!(piled(r.data(), yielded[0].item), yielded[0].count);
    assert!(piles(r.data()).iter().any(|(p, _)| *p == c), "at the deconstructed tile: {:?}", piles(r.data()));
    r.rollback_last_event();
    assert_eq!(state_hash(r.data()), h);
    // The build tool's own carve order drops too: both sides of a seam do the same.
    let (lo, hi) = tile_box([12, STAND - 1, 12]);
    let dirt = piled(r.data(), item("dirt"));
    order(&mut r, Order::CarveStud { lo, hi });
    assert_eq!(piled(r.data(), item("dirt")), dirt + per_tile);
}

// --- Chests -----------------------------------------------------------------------

#[test]
fn a_stud_chest_is_a_tile_of_tagged_bricks_with_a_container() {
    let mut rb = world(1);
    let p = [4, STAND, 4];
    assert!(!rb.place_stud_chest_safe([4, STAND - 1, 4]), "not into the ground");
    assert!(!rb.place_stud_chest_safe([-1, STAND, 4]) && !rb.place_stud_chest_safe([4, STAND, REGION_BLOCKS as i32]));
    assert!(bar(&mut rb, "place a chest", |rb| rb.place_stud_chest_safe(p)));
    assert!(rb.stud_chest_at(p) && !rb.stud_chest_at([5, STAND, 4]) && !rb.stud_chest_at([4, STAND + 1, 4]));
    assert_eq!(rb.tile_fill(p), TileFill::Solid);
    assert_eq!(rb.tile_material(p), Some(STUD_CHEST_MATERIAL));
    assert_eq!(rb.container_seen(w(p)), Some(&Container::chest()));
    assert!(!rb.place_stud_chest_safe(p), "the tile is taken");
    // Plain bricks of the same plastic are no chest.
    let (lo, hi) = tile_box([8, STAND, 8]);
    rb.fill_stud_box_safe(lo, hi, STUD_CHEST_MATERIAL);
    assert!(!rb.stud_chest_at([8, STAND, 8]) && rb.container_seen(w([8, STAND, 8])).is_none());
    // Things in it; a plate shaved off the top leaves it a chest; all of it carved
    // spills the contents and the chest itself where it stood.
    let mut full = Container::chest();
    full.slots[3] = Some(stack("sand", 9));
    full.slots[20] = Some(stack("dirt", 2));
    rb.write_container_safe(w(p), Some(full));
    let (lo, hi) = tile_box(p);
    assert!(!rb.carve_stud_safe([lo[0], hi[1] - 1, lo[2]], hi).is_empty());
    assert!(rb.stud_chest_at(p) && rb.container_seen(w(p)).is_some() && rb.item_piles().is_empty());
    bar(&mut rb, "break a chest", |rb| rb.carve_stud_safe(lo, hi));
    assert!(!rb.stud_chest_at(p) && rb.container_seen(w(p)).is_none());
    assert_eq!(rb.tile_fill(p), TileFill::Open);
    assert_eq!((piled(&rb, item("sand")), piled(&rb, item("dirt")), piled(&rb, item("chest"))), (9, 2, 1));
    assert!(piles(&rb).iter().all(|(c, _)| c[1] == STAND && (c[0] - 4).abs() <= 1 && (c[2] - 4).abs() <= 1), "{:?}", piles(&rb));
}

#[test]
fn chests_are_built_deconstructed_and_opened_by_tile() {
    let p = [22, STAND, 20];
    let (mut r, _) = region_with_adventurer([20, STAND, 20]);
    let chest = chest_block().expect("the manifest has a chest");
    order(&mut r, designate(1, DesignationKind::Build(chest), p, p));
    r.forget_last_event();
    let h = state_hash(r.data());
    order(&mut r, Order::DevCompleteAll);
    assert!(r.data().stud_chest_at(p) && r.data().constructed().contains_key(&p));
    assert_eq!(r.data().containers().get(&w(p)), Some(&Container::chest()));
    r.rollback_last_event();
    assert_eq!(state_hash(r.data()), h, "the chest build undone");
    order(&mut r, Order::DevCompleteAll);
    r.forget_last_event();
    // Two tiles from the player's feet tile: in reach, it opens; a far one does not.
    r.with_data(|d| {
        assert!(d.open_container_safe(CLIENT, w(p)));
        assert!(d.place_stud_chest_safe([40, STAND, 40]));
        assert!(!d.open_container_safe(CLIENT, w([40, STAND, 40])), "out of reach");
    });
    assert_eq!(r.data().clients[&CLIENT].open_container, Some(w(p)));
    tick(&mut r);
    assert_eq!(r.data().clients[&CLIENT].open_container, Some(w(p)), "still in reach a tick later");
    // Deconstructed: it closes for its viewer and the chest lies there as an item.
    order(&mut r, designate(2, DesignationKind::Deconstruct, p, p));
    order(&mut r, Order::DevCompleteAll);
    assert!(!r.data().stud_chest_at(p) && !r.data().containers().contains_key(&w(p)));
    assert_eq!(r.data().clients[&CLIENT].open_container, None);
    assert_eq!(piled(r.data(), item("chest")), 1);
    // A saved region keeps the other chest: its bricks and its container.
    let (portable, _) = r.data().portable_snapshot(&SaveTerms::default());
    let (back, _) = restore_portable(RegionCoords::new(0, 0), StoredRegion::from(portable));
    assert!(back.data().stud_chest_at([40, STAND, 40]));
    assert_eq!(back.data().containers().get(&w([40, STAND, 40])), Some(&Container::chest()));
    assert_eq!(back.data().terrain_kind(), TerrainKind::Stud);
    assert_eq!(piled(back.data(), item("chest")), 1, "the pile on a stud tile is kept too");
}

// --- Trees ------------------------------------------------------------------------

fn boxed(min: [u8; 3], size: [u8; 3], material: u16, data: u16, out: &mut Vec<StudBrick>) {
    fill_box(min, size, material, data, out);
}

/// A tree of id [`TREE`] at tile column (8, 8) of chunk (0, 0): a 2 x 2 stud trunk
/// 30 plates tall on the ground, a 6 x 6 x 6 crown of leaves on top of it, a tuft of
/// leaves against the trunk's foot, and one leaf brick floating apart at head height.
/// Another tree (id 9) stands at tile (14, 8).
fn tree_seed() -> StudRegionSeed {
    let mut bricks = Vec::new();
    boxed([16, 30, 16], [2, 30, 2], MAT_WOOD, TREE, &mut bricks);
    boxed([14, 60, 14], [6, 6, 6], MAT_LEAVES, TREE, &mut bricks);
    boxed([18, 30, 16], [2, 3, 2], MAT_LEAVES, TREE, &mut bricks);
    boxed([22, 42, 16], [2, 3, 2], MAT_LEAVES, TREE, &mut bricks);
    boxed([28, 30, 16], [2, 12, 2], MAT_WOOD, 9, &mut bricks);
    StudRegionSeed { ground: flat_ground(2), features: vec![(StudChunkCoords { x: 0, y: 0, z: 0 }, bricks)] }
}

/// Cells of tree `id` left in the terrain, per material.
fn tree_cells(rb: &Rollback, id: u16) -> BTreeMap<u16, u32> {
    let mut out = BTreeMap::new();
    for c in rb.stud_index.keys() {
        for b in rb.stud_chunk_at(*c).unwrap().bricks().filter(|b| b.data == id) {
            *out.entry(b.material).or_default() += b.size().iter().map(|&v| u32::from(v)).product::<u32>();
        }
    }
    out
}

fn stud_bodies(rb: &Rollback) -> Vec<EntityKey> {
    rb.ecs.stud_body.iter().filter_map(|(k, v)| v.as_ref().map(|_| k)).collect()
}

#[test]
fn chop_is_valid_on_tree_wood_only() {
    let rb = world_of(tree_seed());
    assert_eq!(tree_cells(&rb, TREE), BTreeMap::from([(MAT_WOOD, 120), (MAT_LEAVES, 216 + 12 + 12)]));
    assert_eq!(rb.stud_tree_at([8, STAND, 8]), Some(TREE));
    assert_eq!(rb.stud_tree_at([8, STAND + 4, 8]), Some(TREE), "the trunk's top tile");
    assert_eq!(rb.stud_tree_at([14, STAND, 8]), Some(9));
    for p in [[8, STAND + 5, 8], [9, STAND, 8], [11, STAND + 2, 8], [8, STAND - 1, 8], [3, STAND, 3], [-1, STAND, 8]] {
        assert_eq!(rb.stud_tree_at(p), None, "{p:?}: leaves, ground and air are no tree to chop");
    }
    assert_eq!(validate(&rb, DesignationKind::Chop, [8, STAND + 1, 8]), Ok(()));
    assert_eq!(validate(&rb.tiles(), DesignationKind::Chop, [8, STAND + 1, 8]), Ok(()));
    assert_eq!(validate(&rb, DesignationKind::Chop, [9, STAND, 8]), Err(Invalid::NotTree));
    assert_eq!(validate(&rb, DesignationKind::Chop, [3, STAND - 1, 3]), Err(Invalid::NotTree));
    assert!(rb.stud_fell([9, STAND, 8]).is_none() && rb.stud_fell([3, STAND, 3]).is_none());
}

#[test]
fn felling_takes_the_tree_from_the_cut_up_and_what_it_held() {
    let mut rb = world_of(tree_seed());
    let cut = [8, STAND + 1, 8];
    let plan = rb.stud_fell(cut).unwrap();
    assert_eq!(plan.tree, TREE);
    // The cut: the two trunk bricks of the level (plates 36..42).
    assert_eq!(plan.cut.iter().map(|b| (b.lo, b.hi)).collect::<Vec<_>>(), vec![([16, 36, 16], [18, 39, 18]), ([16, 39, 16], [18, 42, 18])]);
    // Falling: the trunk above, the crown, and the leaf that hung in the air (now
    // that nothing joins it to the stump). Standing: the stump and the tuft against it.
    let falling: BTreeSet<[i32; 3]> = plan.falling.iter().map(|b| b.lo).collect();
    assert!(falling.contains(&[16, 42, 16]) && falling.contains(&[22, 42, 16]));
    assert!(plan.falling.iter().all(|b| b.lo[1] >= 42) && !falling.contains(&[18, 30, 16]));
    let wood_falling: i32 = plan.falling.iter().filter(|b| b.wood).map(|b| (b.hi[1] - b.lo[1]) * 4).sum();
    assert_eq!(wood_falling, 4 * 18);

    let (removed, body) = bar(&mut rb, "fell a tree", |rb| rb.fell_stud_safe(cut, SettlePolicy::DropItems)).unwrap();
    assert_eq!(removed, vec![(MAT_WOOD, 24)]);
    let body = body.expect("the top falls as a body");
    assert_eq!(tree_cells(&rb, TREE), BTreeMap::from([(MAT_WOOD, 24), (MAT_LEAVES, 12)]), "the stump and its tuft stand");
    assert_eq!(tree_cells(&rb, 9), BTreeMap::from([(MAT_WOOD, 48)]), "the other tree is untouched");
    assert_eq!(rb.tile_fill(cut), TileFill::Open);
    assert_eq!(stud_bodies(&rb), vec![body]);
    // The cut's logs lie at the stump; the body carries the rest.
    assert_eq!(piled(&rb, item(STUD_WOOD_DROP_BLOCK)), 1);
    let sb = rb.ecs.stud_body.try_get(body).clone().unwrap();
    assert_eq!(sb.drops, vec![stack(STUD_WOOD_DROP_BLOCK, 3)], "72 wood cells; leaves drop nothing");
    assert_eq!(sb.model.bricks.len(), plan.falling.len());
    assert!(sb.model.bricks.iter().all(|b| b.data == TREE));
    assert_eq!(*rb.ecs.kind.try_get(body), Some(EntityKind::Body));
    assert!(rb.entity_id(body).is_some(), "it can cross a seam");
    // Its body sits at the model's centre of mass, turning about the cut.
    let handle = rb.ecs.rigidbody.try_get(body).unwrap();
    let b = rb.physics.bodies.get(handle).unwrap();
    assert!(b.is_dynamic() && b.angvel().length() > 0.0 && b.linvel().length() > 0.0);
    let mp = stud_model_mass_properties(&sb.model);
    assert!(mp.mass() > 0.6 * WOOD_DENSITY * 6.0, "six trunk bricks and a crown: {} kg", mp.mass());
    assert_eq!(b.user_force().y, -mp.mass() * VOXEL_BODY_GRAVITY * movement::UNITS_PER_METER, "its weight");

    // A chop at the foot fells everything that is left.
    let (removed, rest) = bar(&mut rb, "fell the stump", |rb| rb.fell_stud_safe([8, STAND, 8], SettlePolicy::Despawn)).unwrap();
    assert_eq!(removed, vec![(MAT_WOOD, 24)]);
    assert!(rest.is_some(), "the tuft falls: nothing holds it");
    assert!(tree_cells(&rb, TREE).is_empty());
    assert!(rb.stud_fell([8, STAND, 8]).is_none() && rb.fell_stud_safe([8, STAND, 8], SettlePolicy::Despawn).is_none());
}

#[test]
fn a_chopped_tree_falls_and_leaves_its_logs() {
    let mut r = region_of(tree_seed());
    let cut = [8, STAND + 1, 8];
    order(&mut r, designate(1, DesignationKind::Chop, [8, STAND + 1, 8], [9, STAND + 1, 8]));
    assert_eq!(r.data().designations().len(), 1, "only the trunk tile takes the chop");
    r.forget_last_event();
    let h = state_hash(r.data());
    order(&mut r, Order::DevCompleteAll);
    assert_eq!(stud_bodies(r.data()).len(), 1);
    assert_eq!(r.data().tile_fill(cut), TileFill::Open);
    r.rollback_last_event();
    assert_eq!(state_hash(r.data()), h, "the chop order undone: tree, body and logs");
    assert!(stud_bodies(r.data()).is_empty() && r.data().item_piles().is_empty());
    order(&mut r, Order::DevCompleteAll);
    r.forget_last_event();
    let body = stud_bodies(r.data())[0];
    let start = {
        let d = r.data();
        d.physics.bodies.get(d.ecs.rigidbody.try_get(body).unwrap()).unwrap().translation()
    };
    // It falls, comes to rest (or times out) and turns into its logs.
    let mut lowest = start.y;
    let mut ticks = 0;
    while !stud_bodies(r.data()).is_empty() {
        // Every tick with the body in flight rolls back exactly.
        if ticks % 16 == 0 {
            let h = state_hash(r.data());
            r.handle_event(GameEventKind::Tick).unwrap();
            r.rollback_last_event();
            assert_eq!(state_hash(r.data()), h, "tick {ticks} with a stud body undone");
        }
        tick(&mut r);
        ticks += 1;
        if let Some(h) = r.data().ecs.rigidbody.try_get(body).as_ref() {
            lowest = lowest.min(r.data().physics.bodies.get(*h).map_or(lowest, |b| b.translation().y));
        }
        assert!(ticks <= SETTLE_TIMEOUT_TICKS + 5, "the body never settled");
    }
    assert!(lowest < start.y - 16.0, "it fell: {} -> {lowest}", start.y);
    assert!(lowest > plate_units(TOP) - 16.0, "it did not fall through the ground: {lowest}");
    assert!((ticks as u64) < SETTLE_TIMEOUT_TICKS, "it came to rest ({ticks} ticks), not timed out");
    assert_eq!(piled(r.data(), item(STUD_WOOD_DROP_BLOCK)), 4, "one log from the cut, three from the fallen top");
    assert_eq!(tree_cells(r.data(), TREE), BTreeMap::from([(MAT_WOOD, 24), (MAT_LEAVES, 12)]));
}

// --- Stud bodies ------------------------------------------------------------------

#[test]
fn stud_body_mass_comes_from_its_bricks() {
    let part = stud_carve::box_part([2, 3, 2]).unwrap();
    let brick = |min: [i16; 3], material: u16| StudModelBrick { min, shape: StudShape::Part { part: part.0, rot: part.1 }, material, data: 0 };
    // One 2 x 2 brick of wood: 1 m x 0.6 m x 1 m at 700 kg/m3, centred in its box.
    let one = StudModel { bricks: vec![brick([0, 0, 0], MAT_WOOD)] };
    let mp = stud_model_mass_properties(&one);
    assert!((mp.mass() - 0.6 * WOOD_DENSITY).abs() < 1e-3, "{}", mp.mass());
    assert_eq!(mp.local_com.to_array(), [8.0, 4.8, 8.0]);
    // Stone on top of it pulls the centre up; leaves weigh next to nothing.
    let two = StudModel { bricks: vec![brick([0, 0, 0], MAT_WOOD), brick([0, 3, 0], MAT_STONE)] };
    let mp2 = stud_model_mass_properties(&two);
    assert!((mp2.mass() - 0.6 * (WOOD_DENSITY + STONE_DENSITY)).abs() < 1e-2);
    assert!(mp2.local_com.y > 9.6 && mp2.local_com.x == 8.0);
    assert_eq!(stud_density_of(MAT_LEAVES), LEAF_DENSITY);
    assert_eq!(stud_density_of(4), PLASTIC_DENSITY);
    common::install_real_block_props();
    assert_eq!(stud_model_drops(&two), stud_drops(&[(MAT_STONE, 12), (MAT_WOOD, 12)]), "per material, ascending");
    assert_eq!(stud_model_drops(&two).len(), 2);
    assert_eq!(stud_model_mass_properties(&StudModel::default()).mass(), 0.0);
}

#[test]
fn detached_bricks_become_a_body_and_settle() {
    let (send, recv) = crossbeam::channel::unbounded();
    common::install_real_block_props();
    let mut rb = Rollback::new(Some(send));
    let seed = tree_seed();
    let extra = seed.feature_chunks();
    rb.seed_stud_ground_genesis(seed.ground, extra);
    rb.set_body_scale_raw(BodyScale::STUD);
    let c0 = StudChunkCoords { x: 0, y: 0, z: 0 };
    // The other tree's four bricks, a pick that names nothing, and a bedrock-free miss.
    let wanted: BTreeSet<[u8; 3]> = [[28, 30, 16], [28, 33, 16], [28, 36, 16], [28, 39, 16], [0, 80, 0]].into();
    let picks = BTreeMap::from([(c0, wanted), (StudChunkCoords { x: 1, y: 3, z: 1 }, [[0, 0, 0]].into())]);
    assert!(rb.detach_studs_safe(&BTreeMap::new(), SettlePolicy::Despawn, Vector::ZERO, None).is_none());
    recv.try_iter().count();
    let e = bar(&mut rb, "detach bricks", |rb| rb.detach_studs_safe(&picks, SettlePolicy::Despawn, Vector::ZERO, None)).unwrap();
    assert!(tree_cells(&rb, 9).is_empty(), "the bricks left the terrain");
    let sb = rb.ecs.stud_body.try_get(e).clone().unwrap();
    assert_eq!(sb.model.bricks.iter().map(|b| b.min).collect::<Vec<_>>(), vec![[0, 0, 0], [0, 3, 0], [0, 6, 0], [0, 9, 0]]);
    assert!(sb.drops.is_empty(), "despawn: no drops");
    // Do, undo (the model is re-announced when the removal is undone), do again.
    let adds = recv.try_iter().filter(|u| matches!(u.update_kind, GameDataUpdateKind::AddStudBody(..))).count();
    assert_eq!(adds, 2);
    // Its pose is the model's centre of mass: the middle of the 2 x 12 x 2 column.
    let handle = rb.ecs.rigidbody.try_get(e).unwrap();
    let t = rb.physics.bodies.get(handle).unwrap().translation();
    assert_eq!([t.x, t.z], [29.0 * 8.0, 17.0 * 8.0]);
    assert!((t.y - plate_units(36)).abs() < 1e-3);
    // Settling removes it; its removal undone brings model and body back.
    let before = state_hash(&rb);
    rb.new_transaction();
    rb.tick_stud_bodies_safe(*rb.tick + SETTLE_TIMEOUT_TICKS);
    assert!(stud_bodies(&rb).is_empty());
    rb.rollback();
    assert_eq!(state_hash(&rb), before, "settle undone");
    assert_eq!(stud_bodies(&rb), vec![e]);
}

// --- Water ------------------------------------------------------------------------

/// Sea level of the lake world: the surface plate of its dry ground.
const SEA: i32 = TOP;

/// One chunk column: dry ground at `TOP` for stud x < 8; a lake of water top [`SEA`]
/// over a bed that steps down eastward — 2 plates deep (x 8..12), 3 deep (12..16),
/// then 12 deep.
fn lake() -> StudColumnGround {
    let bed = |x: usize| match x {
        0..=7 => TOP,
        8..=11 => SEA - 2,
        12..=15 => SEA - 3,
        _ => SEA - 12,
    };
    let surface: Vec<i32> = (0..N * N).map(|i| bed(i % N)).collect();
    let water: Vec<i32> = (0..N * N).map(|i| if i % N >= 8 { SEA } else { STUD_NO_WATER }).collect();
    StudColumnGround::new(
        surface.into(),
        water.into(),
        vec![0; N * N].into(),
        vec![SOIL],
        vec![0; N * N].into(),
        vec![(i32::MAX, MAT_STONE)],
        BEDROCK,
    )
    .unwrap()
}

fn lake_world() -> Rollback {
    world_of(StudRegionSeed { ground: vec![(ColumnCoords::new(0, 0), lake())], features: Vec::new() })
}

#[test]
fn stud_water_is_the_recipes_and_graded_by_depth() {
    let rb = lake_world();
    let units = |p: TilePos| rb.water_units_at(w(p));
    // Dry land, and the air over the lake.
    assert_eq!((units([2, STAND, 4]), units([2, STAND - 1, 4]), units([10, STAND, 4])), (0, 0, 0));
    assert_eq!(rb.stud_water_top([2, 4]), None);
    assert_eq!(rb.stud_water_top([9, 4]), Some(SEA));
    // Shallows: the bed is a floor inside the tile, the water stands over it.
    assert_eq!(rb.tile_fill([4, STAND - 1, 4]), TileFill::Floor(4));
    assert_eq!(units([4, STAND - 1, 4]), WATER_FULL * 2 / 6);
    assert_eq!(df_depth(units([4, STAND - 1, 4])), 3);
    assert_eq!(units([6, STAND - 1, 4]), WATER_FULL * 3 / 6);
    assert_eq!(df_depth(units([6, STAND - 1, 4])), 4);
    // The deep: full tiles of water down to the bed, none inside the bed.
    assert_eq!((units([10, STAND - 1, 4]), units([10, STAND - 2, 4])), (WATER_FULL, WATER_FULL));
    assert_eq!(units([10, STAND - 3, 4]), 0, "the lake bed");
    // Pathing: wading depth stands, deeper water does not, and wading costs.
    assert!(standable(&rb, [3, STAND, 4]), "dry");
    assert!(standable(&rb, [4, STAND, 4]), "the floor 2 plates under the surface is waded (it is the tile above's)");
    assert!(!standable(&rb, [6, STAND - 1, 4]), "3 plates of water over the floor");
    assert!(!standable(&rb, [10, STAND - 2, 4]) && !standable(&rb, [10, STAND, 4]), "the deep: no floor to stand on, none in the water");
    assert!(find_path(&rb, [1, STAND, 4], [4, STAND, 4], MAX_PATH_NODES).is_some());
    assert!(find_path(&rb, [1, STAND, 4], [10, STAND - 2, 4], MAX_PATH_NODES).is_none());
    // A body's immersion is read against the surface itself.
    let at = |x: f32, plate: i32| Vector::new(x, plate_units(plate), 40.0);
    let height = BodyScale::STUD.height_m * movement::UNITS_PER_METER;
    assert_eq!(immersion_at(&rb, at(20.0, TOP), height), movement::Immersion::Dry);
    assert_eq!(immersion_at(&rb, at(70.0, SEA - 2), height), movement::Immersion::Feet);
    assert_eq!(immersion_at(&rb, at(150.0, SEA - 7), height), movement::Immersion::Waist);
    assert_eq!(immersion_at(&rb, at(150.0, SEA - 12), height), movement::Immersion::Head);
    assert_eq!(immersion_at(&rb, at(150.0, SEA + 1), height), movement::Immersion::Dry, "over the surface");
    // Buoyancy's share under the surface, in the column under the centre of mass.
    let (lo, hi) = (at(150.0, SEA - 4), at(150.0, SEA + 4));
    assert_eq!(submerged_fraction(&rb, lo, hi, at(150.0, SEA)), 0.5);
    assert_eq!(submerged_fraction(&rb, lo, hi, at(20.0, SEA)), 0.0, "over dry land");
    assert_eq!(submerged_fraction(&rb, at(150.0, SEA - 10), at(150.0, SEA - 5), at(150.0, SEA - 8)), 1.0);
}

#[test]
fn stud_water_is_static_under_edits() {
    let mut rb = lake_world();
    let units = |rb: &Rollback, p: TilePos| rb.water_units_at(w(p));
    // A hole dug on the shore, below the water line and right beside it, stays dry;
    // so does a pit dug into the lake bed (water is the recipe's, over its ground).
    let shore = [3, STAND - 1, 4];
    let (lo, hi) = tile_box(shore);
    assert!(!rb.carve_stud_safe(lo, hi).is_empty());
    assert_eq!(units(&rb, shore), 0);
    assert_eq!(units(&rb, [4, STAND - 1, 4]), WATER_FULL * 2 / 6, "the shallows beside it are as they were");
    let bed = [10, STAND - 3, 4];
    let (lo, hi) = tile_box(bed);
    assert!(!rb.carve_stud_safe(lo, hi).is_empty());
    assert_eq!(units(&rb, bed), 0);
    assert_eq!(units(&rb, [10, STAND - 2, 4]), WATER_FULL);
    // Bricks built into the water displace it; carved away, it is back.
    let wet = [10, STAND - 1, 4];
    let (lo, hi) = tile_box(wet);
    let h = state_hash(&rb);
    rb.new_transaction();
    assert_eq!(rb.fill_stud_box_safe(lo, hi, MAT_STONE), STUD_TILE_CELLS);
    assert_eq!(units(&rb, wet), 0);
    rb.rollback();
    assert_eq!(state_hash(&rb), h);
    assert_eq!(units(&rb, wet), WATER_FULL);
    // A pile dropped into the lake sinks to its bed.
    rb.deposit_items_safe([10, STAND + 2, 6], stack("sand", 1), 0, 0);
    assert_eq!(piles(&rb), vec![([10, STAND - 2, 6], vec![stack("sand", 1)])]);
}
