//! Item piles (`game::item_piles`): dropped items are cell-keyed piles, not
//! entities. A deposit settles onto standable ground (a neighbour's, by an
//! intent to its owner, past the region's edge),
//! merges into nearby piles, and the piles follow their support when blocks
//! are broken or built; Adventure bodies pick them up after a delay, and they
//! expire. Every change undoes bit-exact and reaches the renderer both ways.
use crate::common;

use common::{floor_chunks, r0};
use game::edit::BlockPos;
use game::{
    feet_of, feet_tile, install_block_props, state_hash, Block, BlockId, BlockRegistry, ClientId,
    GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, GameEventKind, Intent, ItemId, ItemStack, PickupView, PileFlight, PileView,
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
    region.data().item_piles().iter().map(|(p, pile)| (game::local_cell(r0(), *p), pile.stacks.clone())).collect()
}

/// Local cell `p` of [`r0`] as the world cell piles are keyed by.
fn w(p: BlockPos) -> game::WorldBlockPos {
    game::WorldBlockPos::from_local(r0(), game::BlockPos::new(p[0], p[1], p[2]))
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
    let centre = &region.data().item_piles()[&w([10, 1, 10])];
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

/// [`region`] at region `rc` instead of [`r0`].
fn region_at(rc: game::RegionCoords) -> Region {
    install();
    let (send, recv) = crossbeam::channel::unbounded();
    std::mem::forget(recv);
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), rc, None);
    let chunks = floor_chunks(0.0..REGION_SIZE, 0.0..REGION_SIZE, BLOCK_VOXELS as u32);
    region.with_data(|r| r.seed_terrain_genesis(chunks));
    region
}

/// A deposit past the region's edge is not clamped into it (Task 6.3): it
/// goes to the cell's owner as a `DepositItems` intent, which puts it down
/// there, settled onto the owner's ground.
#[test]
fn deposit_outside_region_goes_to_its_owner() {
    let mut region = region();
    let n = REGION_BLOCKS as i32;
    deposit(&mut region, [-5, 3, n + 7], stack(DIRT, 1), 0);
    // Inside the floor: it rises out of the solid block.
    deposit(&mut region, [20, 0, 20], stack(DIRT, 1), 0);
    assert_eq!(piles(&region), vec![([20, 1, 20], vec![stack(DIRT, 1)])], "nothing clamped in");
    let sent = region.take_intents();
    let cell = w([-5, 3, n + 7]);
    let owner = game::RegionCoords::new(-1, 1);
    assert_eq!(cell.to_local().0, owner);
    let by = game::EntityId::of_region(r0());
    let [game::IntentMsg::Intent(msg)] = sent.as_slice() else { panic!("one intent: {sent:?}") };
    assert_eq!(msg.target, game::Target::Pile(cell));
    assert_eq!(
        msg.intent,
        Intent::DepositItems { cell, stack: stack(DIRT, 1), expire: PILE_DESPAWN_TICKS, pickup: PICKUP_DELAY_TICKS, by }
    );
    // The owner applies it at the opening of the due tick: settled onto its
    // floor, in its own cell; no reply to a region-sourced deposit granted.
    let mut owner_region = region_at(owner);
    owner_region.handle_event(GameEventKind::Intent(game::IntentMsg::Intent(*msg))).unwrap();
    for _ in 0..2 {
        owner_region.handle_event(GameEventKind::Tick).unwrap();
    }
    let got: Vec<_> = owner_region.data().item_piles().iter().map(|(p, pile)| (*p, pile.stacks.clone())).collect();
    assert_eq!(got, vec![(game::WorldBlockPos::new(cell.x, 1, cell.z), vec![stack(DIRT, 1)])]);
    let replies = owner_region.take_intents();
    assert!(
        replies.iter().all(|m| matches!(m, game::IntentMsg::Reply(r) if matches!(r.reply, game::IntentReply::Granted(_)))),
        "granted: {replies:?}"
    );
}

/// Two takers ask the owner for the last item in the same tick: the lower
/// `EntityId` gets it, the other is refused, and the item exists once.
#[test]
fn takes_of_the_last_item_conflict() {
    let mut region = region();
    let cell = w([10, 1, 10]);
    let now = region.current_tick();
    region.with_data(|r| assert_eq!(r.deposit_items_safe([10, 1, 10], stack(DIRT, 1), now, 0), 0));
    let other = game::RegionCoords::new(-1, 0);
    let (a, b) = (game::EntityId { birth_region: other, serial: 7 }, game::EntityId { birth_region: other, serial: 3 });
    let due = region.current_tick() + 1;
    // The higher id arrives first: arrival order must not decide.
    for by in [a, b] {
        let sent = game::SentIntent {
            target: game::Target::Pile(cell),
            intent: Intent::TakeFromPile { cell, item: DIRT, count: 1, by },
            from: other,
            due,
        };
        region.handle_event(GameEventKind::Intent(game::IntentMsg::Intent(sent))).unwrap();
    }
    for _ in 0..2 {
        region.handle_event(GameEventKind::Tick).unwrap();
    }
    assert!(piles(&region).is_empty(), "the item left the pile");
    let replies: Vec<_> = region
        .take_intents()
        .into_iter()
        .filter_map(|m| match m {
            game::IntentMsg::Reply(r) => Some(r.reply),
            _ => None,
        })
        .collect();
    assert_eq!(
        replies,
        vec![
            game::IntentReply::Granted(Intent::TakeFromPile { cell, item: DIRT, count: 1, by: b }),
            game::IntentReply::Refused(
                Intent::TakeFromPile { cell, item: DIRT, count: 1, by: a },
                game::IntentRefusal::NothingThere
            ),
        ],
        "one grant, one refusal: the item is conserved"
    );
}

/// A deposit a neighbour cannot hold comes back (Task 6.3): a refused
/// `DepositItems` to a body gives its rest back to that body's inventory.
#[test]
fn refused_deposit_goes_back_to_the_inventory() {
    let (mut region, _tile) = region_with_adventurers(&[0]);
    region.with_data(|r| r.clients.get_mut(&0).unwrap().inventory = game::Inventory::new());
    let key = *region.data().player_entites.get(&0).unwrap();
    let by = region.data().entity_id(key).unwrap();
    let cell = game::WorldBlockPos::new(-1, 1, 5);
    let reply = game::SentReply {
        reply: game::IntentReply::Refused(
            Intent::DepositItems { cell, stack: stack(DIRT, 3), expire: 0, pickup: 0, by },
            game::IntentRefusal::PileFull,
        ),
        hint: r0(),
        due: region.current_tick(),
        hops: 0,
    };
    region.handle_event(GameEventKind::Intent(game::IntentMsg::Reply(reply))).unwrap();
    region.handle_event(GameEventKind::Tick).unwrap();
    assert_eq!(inventory_count(&region, 0, DIRT), 3, "the refused rest is back");
}

/// A refused region deposit that waited in the manager's mailbox while its
/// source was parked arrives after the source's clock jumped on waking: the
/// pile it puts down gets a fresh lifetime from now, not the original
/// absolute expiry (long past), or it would vanish on the next tick.
#[test]
fn refused_deposit_after_a_long_park_keeps_a_full_lifetime() {
    let mut region = region();
    let cell = w([10, 1, 10]);
    // Refused while the source was parked: stamped from before the park.
    let refused = Intent::DepositItems {
        cell,
        stack: stack(DIRT, 2),
        expire: PILE_DESPAWN_TICKS,
        pickup: PICKUP_DELAY_TICKS,
        by: game::EntityId::of_region(r0()),
    };
    let woke_at = 3 * PILE_DESPAWN_TICKS;
    region.jump_clock_to(woke_at);
    let reply = game::SentReply {
        reply: game::IntentReply::Refused(refused, game::IntentRefusal::OwnerNotRunning),
        hint: r0(),
        due: 1,
        hops: 0,
    };
    region.handle_event(GameEventKind::Intent(game::IntentMsg::Reply(reply))).unwrap();
    for _ in 0..3 {
        region.handle_event(GameEventKind::Tick).unwrap();
    }
    assert_eq!(piles(&region), vec![([10, 1, 10], vec![stack(DIRT, 2)])], "the refund is put down and kept");
    let expires = region.data().pile_expiry().keys().next().expect("indexed").0;
    assert!(expires >= woke_at + PILE_DESPAWN_TICKS, "a full lifetime from the wake: expires at {expires}");
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
    assert!(!all.contains_key(&w([0, 1, 0])), "the earliest-expiring pile was evicted");
    assert_eq!(all[&w([5, 4, 5])].stacks, vec![stack(STICK, 1)]);
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
    assert!(r.item_piles().contains_key(&w([10, 1, 10])), "the pile fell");
    r.rollback();
    assert_eq!(before, state_hash(&r));
    assert!(r.item_piles().contains_key(&w([10, 2, 10])));
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
    let pile = &region.data().item_piles()[&w(got[0].0)];
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
    let full_piles: Vec<_> = cells.iter().map(|&c| (c, region.data().item_piles()[&w(c)].clone())).collect();
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
        assert_eq!(&after[&w(*c)], p, "{c:?} unchanged");
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

// --- Across a seam (Task 6.3) -------------------------------------------------

use crate::common::rig::{Rig, CLIENT};
use crate::common::seam_translate::{assert_seam_invariant, offset, spawn_tile, Scenario};
use game::{InlineSchedule, InputEvent, Key, Order, WorldBlockPos};

/// Runs `f` on `CLIENT`'s `Client` in the region owning its body.
fn with_client(rig: &mut Rig, f: impl FnOnce(&mut game::Client)) {
    let rc = rig.owners()[0];
    let mut f = Some(f);
    rig.manager.spawner_mut().with_region(rc, |r| {
        r.with_data(|d| (f.take().expect("once"))(d.clients.get_mut(&CLIENT).expect("the client")));
    });
}

/// Deposits `s` (pickable at once) into the pile of world cell `p`, at its
/// owner.
fn deposit_at_owner(rig: &mut Rig, p: WorldBlockPos, s: ItemStack) {
    let (rc, local) = p.to_local();
    rig.manager.spawner_mut().with_region(rc, |r| {
        let now = r.current_tick();
        r.with_data(|d| assert_eq!(d.deposit_items_safe([local.x, local.y, local.z], s, now, 0), 0));
    });
}

/// An Adventure player at the origin tile throws one dirt down-and-across
/// toward −x/−z: it lands two blocks ahead, in the cell one block across the
/// seam / corner. The run stays short of `THROWN_PICKUP_DELAY_TICKS`, so the
/// thrower never takes it back.
struct ThrowAcross;

impl Scenario for ThrowAcross {
    fn setup(&self, rig: &mut Rig, origin: WorldBlockPos) {
        rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some(spawn_tile(origin)) });
        rig.input(InputEvent::FpsCam(true));
        for _ in 0..6 {
            rig.round();
        }
        with_client(rig, |c| {
            c.inventory = game::Inventory::new();
            c.inventory.slots[0] = Some(stack(DIRT, 5));
            c.selected = 0;
        });
        rig.aim_at([origin.x as f64 - 0.5, origin.y as f64 + 0.5, origin.z as f64 - 0.5]);
        rig.key(Key::KeyQ, true);
    }

    fn drive(&self, rig: &mut Rig, round: u32) {
        if round == 1 {
            rig.key(Key::KeyQ, false);
        }
    }

    fn rounds(&self) -> u32 {
        8
    }

    fn crosses(&self) -> bool {
        false
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn throw_across_seam_lands_in_neighbour() {
    let end = assert_seam_invariant(&ThrowAcross, None);
    let want: std::collections::BTreeMap<[i64; 3], Vec<ItemStack>> =
        [([-1, 0, -1], vec![stack(DIRT, 1)])].into_iter().collect();
    assert_eq!(end.piles, want, "one dirt, one block across the seam");
    let (_, inv, _) = end.player().client.clone().expect("a client");
    assert_eq!(inv.slots[0], Some(stack(DIRT, 4)), "one thrown");
    assert_seam_invariant(&ThrowAcross, Some(InlineSchedule::jittery));
}

/// A pile of five dirt rests one block across the seam / corner (−x/−z of
/// the origin) and an Adventure player stands on the origin tile: the pile is
/// in its pickup reach, so the five dirt move into its inventory — once.
struct PickupAcross;

impl Scenario for PickupAcross {
    fn setup(&self, rig: &mut Rig, origin: WorldBlockPos) {
        rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some(spawn_tile(origin)) });
        with_client(rig, |c| c.inventory = game::Inventory::new());
        deposit_at_owner(rig, offset(origin, -1, 0, -1), stack(DIRT, 5));
    }

    fn drive(&self, _rig: &mut Rig, _round: u32) {}

    fn rounds(&self) -> u32 {
        12
    }

    fn crosses(&self) -> bool {
        false
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn pickup_from_neighbour_pile() {
    let end = assert_seam_invariant(&PickupAcross, None);
    assert!(end.piles.is_empty(), "the pile is gone at its owner: {:?}", end.piles);
    let (_, inv, cursor) = end.player().client.clone().expect("a client");
    let dirt: u32 = inv.slots.iter().flatten().chain(cursor.iter()).filter(|s| s.item == DIRT).map(|s| s.count).sum();
    assert_eq!(dirt, 5, "exactly one copy: {inv:?}");
    assert_seam_invariant(&PickupAcross, Some(InlineSchedule::jittery));
}

/// A felled body's `DropItems` across a seam: a dirt column at the spawn
/// region's −x edge toppled about its −x base edge falls into region
/// (−1, 0) — since Task 7.5 the voxel body crosses the seam (its drops in
/// the bundle) and settles there, putting its drops down in its new owner's
/// cells: the four dirt end up in piles there, none here, nothing lost.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn felled_body_drops_across_seam() {
    use game::rapier3d::math::Vector;
    let mut rig = Rig::with_floor(None, crate::common::seam_translate::uniform_floor);
    rig.join_spawn();
    let mid = REGION_BLOCKS as i32 / 2;
    let column: Vec<BlockPos> = (1..=4).map(|y| [0, y, mid]).collect();
    rig.manager.spawner_mut().with_region(game::SPAWN_REGION, |r| {
        r.with_data(|d| {
            for &p in &column {
                assert!(d.set_block_safe(p, Block::new(BlockId(1))), "dirt at {p:?}");
            }
            let bv = BLOCK_VOXELS as f32;
            let pivot = Vector::new(0.0, bv, (mid as f32 + 0.5) * bv);
            let spin = Vector::new(0.0, 0.0, 2.0);
            assert!(d.detach_blocks_pivoting_safe(&column, game::SettlePolicy::DropItems, spin, Some(pivot)).is_some());
        })
    });
    let west = game::RegionCoords::new(-1, 0);
    let dirt_in = |rig: &mut Rig, rc| {
        let mut n = 0;
        rig.manager.spawner_mut().with_region(rc, |r| {
            n = r.data().item_piles().values().flat_map(|p| &p.stacks).filter(|s| s.item == DIRT).map(|s| s.count).sum();
        });
        n
    };
    for _ in 0..120 {
        rig.round();
        if dirt_in(&mut rig, west) == 4 {
            break;
        }
    }
    assert_eq!(dirt_in(&mut rig, west), 4, "the four dirt lie across the seam");
    assert_eq!(dirt_in(&mut rig, game::SPAWN_REGION), 0, "none stayed behind");
    rig.manager.spawner_mut().with_region(west, |r| {
        assert!(r.data().item_piles().keys().all(|p| p.x < 0), "in the cells under the body");
    });
}

/// Review fix (R26): a take's grant carries items the owner already removed.
/// The taker parks (disconnects) after sending the take of a neighbour's
/// pile and before the grant reaches it: the grant finds no body, and its
/// items go into the parked player's record — never lost, never twice.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn grant_to_a_parked_taker_lands_in_its_record() {
    let mut rig = Rig::with_floor(None, crate::common::seam_translate::uniform_floor);
    rig.join_spawn();
    let origin = crate::common::seam_translate::Placement::Seam(crate::common::seam_translate::Axis::X).origin();
    rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some(spawn_tile(origin)) });
    with_client(&mut rig, |c| c.inventory = game::Inventory::new());
    let cell = offset(origin, -1, 0, -1);
    deposit_at_owner(&mut rig, cell, stack(DIRT, 5));
    let sent = |rig: &mut Rig| {
        let mut n = 0;
        rig.manager.spawner_mut().with_region(game::SPAWN_REGION, |r| n = r.data().pile_takes.len());
        n > 0
    };
    for _ in 0..20 {
        rig.round();
        if sent(&mut rig) {
            break;
        }
    }
    assert!(sent(&mut rig), "the take of the neighbour's pile was sent");
    rig.event(game::ServerEvent::ClientDisconnected(CLIENT));
    // The parked body is gone on purpose: not a conservation failure.
    rig.tracked.clear();
    for _ in 0..12 {
        rig.round();
    }
    let record = rig.manager.parked_record(game::PlayerId(CLIENT as u64)).expect("parked").clone();
    let in_record: u32 = record
        .client
        .inventory
        .slots
        .iter()
        .flatten()
        .chain(record.client.cursor.iter())
        .filter(|s| s.item == DIRT)
        .map(|s| s.count)
        .sum();
    let mut in_piles = 0;
    for rc in rig.manager.running_regions() {
        rig.manager.spawner_mut().with_region(rc, |r| {
            in_piles += r.data().item_piles().values().flat_map(|p| &p.stacks).filter(|s| s.item == DIRT).map(|s| s.count).sum::<u32>();
        });
    }
    assert_eq!((in_record, in_piles), (5, 0), "the granted dirt is in the parked record");
    assert_eq!(rig.manager.items_destroyed(), 0);
}
