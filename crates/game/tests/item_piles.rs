//! Item piles (`game::item_piles`): dropped items are cell-keyed piles, not
//! entities. A deposit settles onto standable ground inside the region,
//! merges into nearby piles, and the piles follow their support when blocks
//! are broken or built; Adventure bodies pick them up after a delay, and they
//! expire. Every change undoes bit-exact and reaches the renderer both ways.
use crate::common;

use common::{floor_chunks, r0};
use game::edit::BlockPos;
use game::{
    feet_of, feet_tile, install_block_props, state_hash, Block, BlockId, BlockRegistry, ClientId,
    GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, GameEventKind, ItemId, ItemStack, PickupView, PileFlight, PileView,
    PlayMode, Region, Rollback, BLOCK_VOXELS, INVENTORY_SLOTS, MAX_STACK, PICKUP_DELAY_TICKS, PILES_PER_REGION_MAX,
    PILE_DESPAWN_TICKS, REGION_BLOCKS, REGION_SIZE, STICK,
};

const MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");
const DIRT: ItemId = ItemId(1);
const STONE: BlockId = BlockId(2);
const WATER: BlockId = BlockId(5);

fn stack(item: ItemId, count: u32) -> ItemStack {
    ItemStack { item, count }
}

fn install() {
    let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
    install_block_props(reg.props());
}

/// A region floored one block deep (y 0) over its whole area, no players:
/// the rest cell of any open column is y 1.
fn region() -> Region {
    let (region, recv) = region_rx();
    std::mem::forget(recv);
    region
}

/// A floored region and the receiver of its render updates.
fn region_rx() -> (Region, crossbeam::channel::Receiver<GameDataUpdate>) {
    install();
    let (send, recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), r0(), None);
    let chunks = floor_chunks(0.0..REGION_SIZE, 0.0..REGION_SIZE, BLOCK_VOXELS as u32);
    region.with_data(|r| r.seed_terrain_genesis(chunks));
    (region, recv)
}

fn deposit(region: &mut Region, at: BlockPos, s: ItemStack, tick: u64) -> u32 {
    let mut lost = 0;
    region.with_data(|r| lost = r.deposit_items_safe(at, s, tick, PICKUP_DELAY_TICKS));
    lost
}

fn piles(region: &Region) -> Vec<(BlockPos, Vec<ItemStack>)> {
    region.data().item_piles().iter().map(|(p, pile)| (*p, pile.stacks.clone())).collect()
}

fn set_block(region: &mut Region, p: BlockPos, id: BlockId) {
    region.with_data(|r| assert!(r.set_block_safe(p, Block::new(id)), "write at {p:?}"));
}

#[test]
fn deposit_settles_on_floor() {
    let mut region = region();
    assert_eq!(deposit(&mut region, [10, 10, 10], stack(DIRT, 3), 0), 0);
    assert_eq!(piles(&region), vec![([10, 1, 10], vec![stack(DIRT, 3)])]);
}

#[test]
fn deposits_bunch() {
    let mut region = region();
    // Centre first, then its eight neighbours, round and round.
    let mut cells = vec![[10, 1, 10]];
    for dx in -1..=1 {
        for dz in -1..=1 {
            if (dx, dz) != (0, 0) {
                cells.push([10 + dx, 1, 10 + dz]);
            }
        }
    }
    for i in 0..40 {
        deposit(&mut region, cells[i % cells.len()], stack(DIRT, 1), i as u64);
    }
    assert_eq!(piles(&region), vec![([10, 1, 10], vec![stack(DIRT, 40)])]);
}

#[test]
fn merge_respects_max_stack() {
    let mut region = region();
    // 100 sticks, ten at a time.
    for i in 0..10 {
        deposit(&mut region, [10, 1, 10], stack(STICK, 10), i);
    }
    assert_eq!(piles(&region), vec![([10, 1, 10], vec![stack(STICK, MAX_STACK), stack(STICK, 100 - MAX_STACK)])]);
}

#[test]
fn different_items_share_pile_until_full() {
    let mut region = region();
    for k in 0..5u16 {
        deposit(&mut region, [10, 1, 10], stack(ItemId(10 + k), 1), 0);
    }
    let got = piles(&region);
    assert_eq!(got.len(), 2, "{got:?}");
    let centre = &region.data().item_piles()[&[10, 1, 10]];
    assert_eq!(centre.stacks.len(), game::PILE_MAX_STACKS);
    // The nearest cell (Chebyshev 1, lowest key) that rests where it is.
    assert_eq!(got[0], ([9, 1, 9], vec![stack(ItemId(14), 1)]));
}

#[test]
fn pile_falls_when_support_broken() {
    let mut region = region();
    set_block(&mut region, [10, 1, 10], STONE);
    deposit(&mut region, [10, 6, 10], stack(DIRT, 2), 0);
    assert_eq!(piles(&region), vec![([10, 2, 10], vec![stack(DIRT, 2)])]);
    set_block(&mut region, [10, 1, 10], BlockId::AIR);
    assert_eq!(piles(&region), vec![([10, 1, 10], vec![stack(DIRT, 2)])]);
}

#[test]
fn pile_pushed_up_by_placed_block() {
    let mut region = region();
    deposit(&mut region, [10, 1, 10], stack(DIRT, 2), 0);
    set_block(&mut region, [10, 1, 10], STONE);
    assert_eq!(piles(&region), vec![([10, 2, 10], vec![stack(DIRT, 2)])]);
    // A batched fill pushes it up through every new block.
    region.with_data(|r| {
        r.fill_blocks_safe(&[[10, 2, 10], [10, 3, 10]], Block::new(STONE));
    });
    assert_eq!(piles(&region), vec![([10, 4, 10], vec![stack(DIRT, 2)])]);
    // And a batched clear drops it back to the floor.
    region.with_data(|r| {
        r.clear_blocks_safe(&[[10, 1, 10], [10, 2, 10], [10, 3, 10]]);
    });
    assert_eq!(piles(&region), vec![([10, 1, 10], vec![stack(DIRT, 2)])]);
}

#[test]
fn deposit_outside_region_clamps_inside() {
    let mut region = region();
    let n = REGION_BLOCKS as i32;
    deposit(&mut region, [-5, 3, n + 7], stack(DIRT, 1), 0);
    // Inside the floor: it rises out of the solid block.
    deposit(&mut region, [20, 0, 20], stack(DIRT, 1), 0);
    assert_eq!(
        piles(&region),
        vec![([0, 1, n - 1], vec![stack(DIRT, 1)]), ([20, 1, 20], vec![stack(DIRT, 1)])]
    );
}

#[test]
fn deposit_in_water_rests_on_floor() {
    let mut region = region();
    for y in 1..=3 {
        set_block(&mut region, [10, y, 10], WATER);
    }
    deposit(&mut region, [10, 8, 10], stack(DIRT, 1), 0);
    assert_eq!(piles(&region), vec![([10, 1, 10], vec![stack(DIRT, 1)])]);
}

/// [`region`] with Adventure clients `ids`, their bodies at the spawn.
fn region_with_adventurers(ids: &[ClientId]) -> (Region, BlockPos) {
    let mut region = region();
    for &id in ids {
        region.handle_event(GameEventKind::CreateClient(id)).unwrap();
        region.forget_last_event();
        region.with_data(|r| r.clients.get_mut(&id).unwrap().mode = PlayMode::Adventure);
    }
    let e = *region.data().player_entites.get(&ids[0]).unwrap();
    let data = region.data();
    let handle = data.ecs.rigidbody.try_get(e).unwrap();
    let tile = feet_tile(feet_of(data.physics.bodies.get(handle).unwrap().translation()));
    (region, tile)
}

fn inventory_count(region: &Region, id: ClientId, item: ItemId) -> u32 {
    region.data().clients[&id].inventory.slots.iter().flatten().filter(|s| s.item == item).map(|s| s.count).sum()
}

#[test]
fn pickup_after_delay_only() {
    let (mut region, tile) = region_with_adventurers(&[0]);
    deposit(&mut region, tile, stack(STICK, 5), 100);
    region.with_data(|r| r.tick_pickups_safe(100 + PICKUP_DELAY_TICKS - 1));
    assert_eq!(piles(&region).len(), 1, "too early");
    assert_eq!(inventory_count(&region, 0, STICK), 0);
    region.with_data(|r| r.tick_pickups_safe(100 + PICKUP_DELAY_TICKS));
    assert!(piles(&region).is_empty());
    assert_eq!(inventory_count(&region, 0, STICK), 5);
}

#[test]
fn fortress_clients_never_pick_up() {
    let (mut region, tile) = region_with_adventurers(&[0]);
    region.with_data(|r| r.clients.get_mut(&0).unwrap().mode = PlayMode::Fortress);
    deposit(&mut region, tile, stack(STICK, 5), 0);
    region.with_data(|r| r.tick_pickups_safe(1000));
    assert_eq!(piles(&region).len(), 1);
}

#[test]
fn pickup_full_inventory_keeps_remainder() {
    let (mut region, tile) = region_with_adventurers(&[0]);
    region.with_data(|r| {
        let inv = &mut r.clients.get_mut(&0).unwrap().inventory;
        for s in inv.slots.iter_mut() {
            *s = Some(stack(ItemId(40), MAX_STACK));
        }
        inv.slots[INVENTORY_SLOTS - 1] = Some(stack(STICK, MAX_STACK - 4));
    });
    deposit(&mut region, tile, stack(STICK, 10), 0);
    region.with_data(|r| r.tick_pickups_safe(PICKUP_DELAY_TICKS));
    assert_eq!(inventory_count(&region, 0, STICK), MAX_STACK);
    let got = piles(&region);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].1, vec![stack(STICK, 6)], "nothing lost, nothing duplicated");
}

#[test]
fn pickup_undo_restores_pile_and_inventory() {
    let (mut region, tile) = region_with_adventurers(&[0]);
    let now = region.current_tick() as u64;
    region.with_data(|r| {
        r.deposit_items_safe(tile, stack(STICK, 7), now, 0);
    });
    let before = state_hash(region.data());
    // The real tick path: pickups run inside the Tick event.
    region.handle_event(GameEventKind::Tick).unwrap();
    assert!(piles(&region).is_empty(), "the tick picked it up");
    assert_eq!(inventory_count(&region, 0, STICK), 7);
    region.rollback_last_event();
    assert_eq!(before, state_hash(region.data()), "pickup must undo bit-exact");
    assert_eq!(piles(&region).len(), 1);
    assert_eq!(inventory_count(&region, 0, STICK), 0);
}

#[test]
fn two_clients_one_pile_goes_to_lower_client_id_first() {
    let (mut region, tile) = region_with_adventurers(&[3, 1]);
    // Client 1 has room for 4 sticks only; client 3 takes the rest.
    region.with_data(|r| {
        let inv = &mut r.clients.get_mut(&1).unwrap().inventory;
        for s in inv.slots.iter_mut() {
            *s = Some(stack(ItemId(40), MAX_STACK));
        }
        inv.slots[0] = Some(stack(STICK, MAX_STACK - 4));
    });
    deposit(&mut region, tile, stack(STICK, 10), 0);
    region.with_data(|r| r.tick_pickups_safe(PICKUP_DELAY_TICKS));
    assert_eq!(inventory_count(&region, 1, STICK), MAX_STACK);
    assert_eq!(inventory_count(&region, 3, STICK), 6);
    assert!(piles(&region).is_empty());
}

#[test]
fn expiry_removes_pile_at_tick() {
    let mut region = region();
    deposit(&mut region, [10, 1, 10], stack(DIRT, 1), 100);
    region.with_data(|r| r.expire_piles_safe(100 + PILE_DESPAWN_TICKS - 1));
    assert_eq!(piles(&region).len(), 1);
    region.with_data(|r| r.expire_piles_safe(100 + PILE_DESPAWN_TICKS));
    assert!(piles(&region).is_empty());
    assert!(region.data().pile_expiry().is_empty(), "the index goes with the pile");
}

#[test]
fn clock_jump_keeps_pile_loaded_time() {
    let mut region = region();
    // Region tick 0, so the jump's delta is 10_000.
    deposit(&mut region, [10, 1, 10], stack(DIRT, 1), 0);
    let oldest = region.data().oldest();
    region.jump_clock_to(10_000);
    assert_eq!(region.current_tick(), 10_000);
    // R8: the jump ran in its own forgotten transaction: none is pending, and
    // its undo entries (tick write, pile shifts) were dropped, not left in the log.
    assert_eq!(region.data().current(), region.data().oldest(), "the jump leaves no pending transaction");
    assert_eq!(region.data().oldest(), oldest + 1, "the jump was one forgotten transaction");
    region.handle_event(GameEventKind::Tick);
    assert_eq!(piles(&region).len(), 1, "the pile keeps its loaded time across the jump");
    for _ in 0..PILE_DESPAWN_TICKS {
        region.handle_event(GameEventKind::Tick);
    }
    assert!(piles(&region).is_empty());
    assert!(region.data().pile_expiry().is_empty());
}

// ~6 s under the tests' `HashMode::Full`: every map write re-hashes the
// whole pile map, and this fills it to the cap.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cap_evicts_earliest() {
    let mut region = region();
    let n = REGION_BLOCKS as i32;
    assert_eq!((n * n) as usize, PILES_PER_REGION_MAX, "the test fills every column of a 64-block region");
    // A pillar with a gap: column (5, 5) rests two piles, at y 2 and y 4.
    set_block(&mut region, [5, 1, 5], STONE);
    set_block(&mut region, [5, 3, 5], STONE);
    let mut tick = 0;
    region.with_data(|r| {
        for x in 0..n {
            for z in 0..n {
                // No neighbour shares an item, so nothing merges.
                let item = ItemId(10 + (x % 2 + 2 * (z % 2)) as u16);
                r.deposit_items_safe([x, 1, z], stack(item, 1), tick, 0);
                tick += 1;
            }
        }
    });
    assert_eq!(region.data().item_piles().len(), PILES_PER_REGION_MAX);
    let counted = game::item_piles::items_destroyed_on_thread();
    assert_eq!(deposit(&mut region, [5, 4, 5], stack(STICK, 1), tick), 1, "the evicted pile's one item is reported");
    assert_eq!(game::item_piles::items_destroyed_on_thread() - counted, 1, "and counted");
    let all = region.data().item_piles();
    assert_eq!(all.len(), PILES_PER_REGION_MAX);
    assert!(!all.contains_key(&[0, 1, 0]), "the earliest-expiring pile was evicted");
    assert_eq!(all[&[5, 4, 5]].stacks, vec![stack(STICK, 1)]);
    assert_eq!(region.data().pile_expiry().len(), PILES_PER_REGION_MAX);
}

fn pile_payloads(recv: &crossbeam::channel::Receiver<GameDataUpdate>) -> Vec<(bool, Vec<(BlockPos, Option<PileView>)>)> {
    recv.try_iter()
        .filter_map(|u| match u.update_kind {
            GameDataUpdateKind::SetItemPiles(c) => Some((matches!(u.do_kind, GameDataTransactionKind::Do), c)),
            _ => None,
        })
        .collect()
}

#[test]
fn deposit_emits_updates_both_ways() {
    install();
    let (send, recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    r.seed_terrain_genesis(floor_chunks(0.0..REGION_SIZE, 0.0..REGION_SIZE, BLOCK_VOXELS as u32));
    r.new_transaction();
    r.deposit_items_safe([10, 1, 10], stack(DIRT, 2), 0, 0);
    r.forget();
    recv.try_iter().count();

    let before = state_hash(&r);
    r.new_transaction();
    r.deposit_items_safe([10, 1, 10], stack(DIRT, 3), 0, 0);
    r.rollback();
    assert_eq!(before, state_hash(&r));
    let got = pile_payloads(&recv);
    let view = |n| Some(PileView { stacks: vec![stack(DIRT, n)] });
    assert_eq!(got, vec![(true, vec![([10, 1, 10], view(5))]), (false, vec![([10, 1, 10], view(2))])]);
}

#[test]
fn support_break_undo_bit_exact() {
    install();
    let (send, recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    r.seed_terrain_genesis(floor_chunks(0.0..REGION_SIZE, 0.0..REGION_SIZE, BLOCK_VOXELS as u32));
    r.new_transaction();
    assert!(r.set_block_safe([10, 1, 10], Block::new(STONE)));
    r.deposit_items_safe([10, 5, 10], stack(DIRT, 2), 0, 0);
    r.forget();
    recv.try_iter().count();

    let before = state_hash(&r);
    r.new_transaction();
    assert!(r.set_block_safe([10, 1, 10], Block::new(BlockId::AIR)));
    assert!(r.item_piles().contains_key(&[10, 1, 10]), "the pile fell");
    r.rollback();
    assert_eq!(before, state_hash(&r));
    assert!(r.item_piles().contains_key(&[10, 2, 10]));
    let got = pile_payloads(&recv);
    let view = Some(PileView { stacks: vec![stack(DIRT, 2)] });
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got[0].0 && got[0].1.contains(&([10, 1, 10], view.clone())));
    assert!(!got[1].0 && got[1].1.contains(&([10, 2, 10], view)));
}

#[test]
fn closing_inventory_drops_what_does_not_fit_at_the_feet() {
    let (mut region, tile) = region_with_adventurers(&[0]);
    region.with_data(|r| {
        let c = r.clients.get_mut(&0).unwrap();
        for s in c.inventory.slots.iter_mut() {
            *s = Some(stack(ItemId(40), MAX_STACK));
        }
        c.inventory_open = true;
        c.cursor = Some(stack(STICK, 9));
    });
    let before = state_hash(region.data());
    region.handle_event(GameEventKind::UiAction(0, game::UiAction::ToggleInventory)).unwrap();
    let c = &region.data().clients[&0];
    assert!(!c.inventory_open);
    assert_eq!(c.cursor, None);
    let got = piles(&region);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].1, vec![stack(STICK, 9)]);
    assert_eq!([got[0].0[0], got[0].0[2]], [tile[0], tile[2]], "at the feet");
    let pile = &region.data().item_piles()[&got[0].0];
    assert!(pile.pickup_after_tick >= PICKUP_DELAY_TICKS, "not picked straight back up");
    region.rollback_last_event();
    assert_eq!(before, state_hash(region.data()));
}

/// A pile whose support breaks falls onto a full pile, and every cell
/// around its rest cell that could take a stack is full too: its items are
/// destroyed, reported and counted, and nothing else changes.
#[test]
fn resettle_into_full_neighbourhood_counts_loss() {
    let mut region = region();
    let full = vec![stack(DIRT, MAX_STACK); game::PILE_MAX_STACKS];
    let total = MAX_STACK * game::PILE_MAX_STACKS as u32;
    // Rest cell (10, 1, 10) first, then its eight floor neighbours: the only
    // cells within reach that rest where they are (the rest fall or rise).
    let mut cells = vec![[10, 1, 10]];
    for dx in -1..=1 {
        for dz in -1..=1 {
            if (dx, dz) != (0, 0) {
                cells.push([10 + dx, 1, 10 + dz]);
            }
        }
    }
    for &c in &cells {
        assert_eq!(deposit(&mut region, c, stack(DIRT, total), 0), 0);
    }
    // A shelf over the rest cell's pile, with another pile on it.
    set_block(&mut region, [10, 2, 10], STONE);
    deposit(&mut region, [10, 3, 10], stack(STICK, 5), 0);
    assert_eq!(piles(&region).len(), cells.len() + 1);
    let full_piles: Vec<_> = cells.iter().map(|&c| (c, region.data().item_piles()[&c].clone())).collect();
    for (_, p) in &full_piles {
        assert_eq!(p.stacks, full);
    }

    // The shelf is dug out through orders (the batched clear path), as
    // region events, so the region's own counter sees the loss too.
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.forget_last_event();
    let _ = region.take_items_destroyed();
    let counted = game::item_piles::items_destroyed_on_thread();
    let shelf = game::TileBox::new([10, 2, 10], [10, 2, 10]);
    let op = game::OpId { client: 0, seq: 1 };
    let dig = game::Order::Designate { op, kind: game::DesignationKind::Dig, priority: game::DEFAULT_PRIORITY, area: shelf };
    for order in [dig, game::Order::DevCompleteAll] {
        region.handle_event(GameEventKind::Order(0, order)).unwrap();
        region.forget_last_event();
    }
    assert_eq!(game::item_piles::items_destroyed_on_thread() - counted, 5, "the stick pile's 5 items, counted once");
    assert_eq!(region.take_items_destroyed(), 5, "attributed to the region");
    assert_eq!(region.take_items_destroyed(), 0, "and drained");
    let after = region.data().item_piles();
    assert_eq!(after.len(), cells.len(), "the stick pile is gone");
    for (c, p) in &full_piles {
        assert_eq!(&after[c], p, "{c:?} unchanged");
    }
}

/// The client animates pickups (items fly into the picker), so the sim tells
/// the renderer who took what from which cell: one Do-only `ItemsPickedUp`
/// per tick with pickups, naming the picker's body and the stacks taken.
#[test]
fn pickup_tells_the_renderer_who_took_what() {
    let (mut region, recv) = region_rx();
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.forget_last_event();
    region.with_data(|r| r.clients.get_mut(&0).unwrap().mode = PlayMode::Adventure);
    let body = *region.data().player_entites.get(&0).unwrap();
    let data = region.data();
    let handle = data.ecs.rigidbody.try_get(body).unwrap();
    let tile = feet_tile(feet_of(data.physics.bodies.get(handle).unwrap().translation()));
    deposit(&mut region, tile, stack(STICK, 5), 0);
    recv.try_iter().count();
    region.with_data(|r| r.tick_pickups_safe(PICKUP_DELAY_TICKS));
    let picks: Vec<_> = recv
        .try_iter()
        .filter_map(|u| match u.update_kind {
            GameDataUpdateKind::ItemsPickedUp(p) => Some((matches!(u.do_kind, GameDataTransactionKind::Do), p)),
            _ => None,
        })
        .collect();
    assert_eq!(picks.len(), 1, "one pickup update per tick with pickups");
    let (is_do, list) = &picks[0];
    assert!(is_do);
    assert_eq!(
        list,
        &vec![PickupView { cell: tile, entity: body, stacks: vec![stack(STICK, 5)], tick: PICKUP_DELAY_TICKS }]
    );
}

fn landings(recv: &crossbeam::channel::Receiver<GameDataUpdate>) -> Vec<PileFlight> {
    recv.try_iter()
        .filter_map(|u| match u.update_kind {
            GameDataUpdateKind::ItemsLanded(f) if matches!(u.do_kind, GameDataTransactionKind::Do) => Some(f),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Piles have no physics: a pile whose support goes jumps to its new rest
/// cell. The renderer animates that fall from `ItemsLanded` (render-only).
#[test]
fn falling_pile_tells_the_renderer_where_it_fell_from() {
    install();
    let (send, recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    r.seed_terrain_genesis(floor_chunks(0.0..REGION_SIZE, 0.0..REGION_SIZE, BLOCK_VOXELS as u32));
    r.new_transaction();
    assert!(r.set_block_safe([10, 1, 10], Block::new(STONE)));
    r.deposit_items_safe([10, 2, 10], stack(DIRT, 2), 0, 0);
    r.forget();
    recv.try_iter().count();
    r.new_transaction();
    assert!(r.set_block_safe([10, 1, 10], Block::new(BlockId::AIR)));
    let tick = *r.tick as u64;
    assert_eq!(landings(&recv), vec![PileFlight { from: [10, 2, 10], to: [10, 1, 10], stacks: vec![stack(DIRT, 2)], tick }]);
}

/// A deposit from above (a throw, a drop from height) lands where it settles.
#[test]
fn deposit_from_above_tells_the_renderer_its_origin() {
    install();
    let (send, recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    r.seed_terrain_genesis(floor_chunks(0.0..REGION_SIZE, 0.0..REGION_SIZE, BLOCK_VOXELS as u32));
    r.new_transaction();
    r.deposit_items_from_safe([10, 6, 10], [12, 4, 10], stack(DIRT, 3), 7, 0);
    assert_eq!(landings(&recv), vec![PileFlight { from: [10, 6, 10], to: [12, 1, 10], stacks: vec![stack(DIRT, 3)], tick: 7 }]);
    // A deposit that rests where it was made does not animate.
    r.deposit_items_safe([20, 1, 20], stack(DIRT, 1), 7, 0);
    assert!(landings(&recv).is_empty());
}
