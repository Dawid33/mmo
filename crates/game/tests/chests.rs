//! Chests (`game::containers`, plan 2026-11-05 Decision 10): block
//! inventories keyed by cell beside the voxels. Placing a chest creates its
//! container, an Adventure right click opens it (instead of placing),
//! inventory clicks reach its slots, walking away closes it, and breaking it
//! spills its contents as piles — once, closing it for every viewer.
use crate::common;

use common::r0;
use game::edit::BlockPos;
use game::{
    chest_block, install_block_props, ClientId, state_hash, Block, BlockId, BlockRegistry, Chunk, ChunkCoords, ClickButton,
    GameEventKind, InputEvent, Inventory, ItemId, ItemStack, MouseButton, PlayMode, Pose, Region, Rollback,
    SerializedRegion, SlotRef, UiAction, CHEST_SLOTS, CHUNK_UNITS, STARTING_CHESTS, STICK,
};
use std::collections::BTreeMap;

const MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");
const DIRT_ITEM: ItemId = ItemId(1);
const STONE_ITEM: ItemId = ItemId(2);

/// Chunk column holding the player spawn (128, 128).
const SPAWN_CHUNK: usize = 128 / CHUNK_UNITS;
/// About 45 degrees down: 302 px * 0.0026 rad/px at the default sensitivity.
const DOWN_45: f32 = 302.0;

fn input(client: ClientId, ev: InputEvent) -> GameEventKind {
    GameEventKind::PlayerInput(client, ev)
}

fn forget(region: &mut Region, ev: GameEventKind) {
    region.handle_event(ev).unwrap();
    region.forget_last_event();
}

fn chest() -> BlockId {
    chest_block().expect("the manifest has a chest")
}

fn chest_item() -> ItemId {
    ItemId::block(chest())
}

/// A two-block dirt floor around the spawn, player 0 settled in the middle
/// of block (8, 2, 8), first person, in Adventure, looking 45 degrees down,
/// with an empty inventory. Setup is forgotten.
fn region() -> Region {
    let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
    install_block_props(reg.props());
    let mut chunks = Vec::new();
    let around = SPAWN_CHUNK.saturating_sub(1)..=SPAWN_CHUNK + 1;
    for x in around.clone() {
        for z in around.clone() {
            chunks.push((ChunkCoords::new(x, 0, z), Chunk::flat_floor(32)));
        }
    }
    let (send, recv) = crossbeam::channel::unbounded();
    std::mem::forget(recv);
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), r0(), None);
    region.with_data(|r| r.seed_terrain_genesis(chunks));
    forget(&mut region, GameEventKind::CreateClient(0));
    forget(&mut region, input(0, InputEvent::FpsCam(true)));
    region.with_data(|r| {
        let c = r.clients.get_mut(&0).unwrap();
        c.mode = PlayMode::Adventure;
        c.inventory = Inventory::new();
    });
    for _ in 0..12 {
        forget(&mut region, GameEventKind::Tick);
    }
    let y = body(&region)[1];
    region.with_data(|r| {
        let e = *r.data.player_entites.get(&0).unwrap();
        r.set_body_pose_safe(e, Pose::translation(136.0, y, 136.0));
    });
    forget(&mut region, input(0, InputEvent::MouseMotion { dx: 0.0, dy: DOWN_45 }));
    for _ in 0..3 {
        forget(&mut region, GameEventKind::Tick);
    }
    assert!(region.data().clients[&0].fps_cam_mode, "first person must be on");
    region
}

fn body(region: &Region) -> [f32; 3] {
    let data = region.data();
    let e = *data.player_entites.get(&0).unwrap();
    data.physics.bodies.get(*data.ecs.rigidbody.get(e)).unwrap().translation().to_array()
}

fn target(region: &Region) -> BlockPos {
    region.data().eye_hit(0).expect("the eye ray must hit a block").block
}

fn block(region: &Region, p: BlockPos) -> BlockId {
    region.data().block_at(p).map_or(BlockId::AIR, |b| b.id)
}

fn set_block(region: &mut Region, p: BlockPos, id: BlockId) {
    region.with_data(|r| assert!(r.set_block_safe(p, Block::new(id)), "write at {p:?}"));
}

fn set_slot(region: &mut Region, slot: usize, stack: Option<ItemStack>) {
    region.with_data(|r| r.clients.get_mut(&0).unwrap().inventory.slots[slot] = stack);
}

fn slot(region: &Region, slot: usize) -> Option<ItemStack> {
    region.data().clients[&0].inventory.slots[slot]
}

fn container(region: &Region, p: BlockPos) -> Option<Vec<Option<ItemStack>>> {
    region.data().containers().get(&p).map(|c| c.slots.clone())
}

fn mouse(region: &mut Region, button: MouseButton, pressed: bool) {
    region.handle_event(input(0, InputEvent::MouseButton { button, pressed })).unwrap();
}

fn ticks(region: &mut Region, n: usize) {
    for _ in 0..n {
        region.handle_event(GameEventKind::Tick).unwrap();
    }
}

/// A right click this tick (press, tick, release).
fn right_click(region: &mut Region) {
    mouse(region, MouseButton::Right, true);
    ticks(region, 1);
    mouse(region, MouseButton::Right, false);
}

/// Items per item id over every pile.
fn pile_totals(region: &Region) -> BTreeMap<u16, u32> {
    let mut out = BTreeMap::new();
    for pile in region.data().item_piles().values() {
        for s in &pile.stacks {
            *out.entry(s.item.0).or_default() += s.count;
        }
    }
    out
}

/// A chest in the floor block under the crosshair (Fortress-style write),
/// with its container. Returns its cell.
fn chest_under_crosshair(region: &mut Region) -> BlockPos {
    let a = target(region);
    set_block(region, a, chest());
    assert!(container(region, a).is_some(), "a chest written gets a container");
    assert_eq!(target(region), a, "the chest is still under the crosshair");
    a
}

/// 27 slots of three kinds of items in mixed counts.
fn mixed_contents() -> Vec<Option<ItemStack>> {
    (0..CHEST_SLOTS)
        .map(|i| {
            let item = [DIRT_ITEM, STONE_ITEM, STICK][i % 3];
            Some(ItemStack { item, count: 1 + (i as u32 * 7) % 64 })
        })
        .collect()
}

fn totals_of(slots: &[Option<ItemStack>]) -> BTreeMap<u16, u32> {
    let mut out = BTreeMap::new();
    for s in slots.iter().flatten() {
        *out.entry(s.item.0).or_default() += s.count;
    }
    out
}

#[test]
fn starting_kit_has_two_chests() {
    let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
    install_block_props(reg.props());
    let kit = Inventory::starting_kit();
    assert_eq!(kit.slots[0], Some(ItemStack { item: chest_item(), count: STARTING_CHESTS }));
    assert_eq!(STARTING_CHESTS, 2);
}

#[test]
fn placing_chest_creates_container() {
    let mut region = region();
    let a = target(&region);
    let above = [a[0], a[1] + 1, a[2]];
    set_slot(&mut region, 0, Some(ItemStack { item: chest_item(), count: 2 }));
    right_click(&mut region);
    assert_eq!(block(&region, above), chest(), "the chest is placed on the hit face");
    assert_eq!(slot(&region, 0), Some(ItemStack { item: chest_item(), count: 1 }));
    assert_eq!(container(&region, above), Some(vec![None; CHEST_SLOTS]), "an empty container");
    assert_eq!(region.data().containers().len(), 1);
}

#[test]
fn right_click_chest_opens_not_places() {
    let mut region = region();
    let a = chest_under_crosshair(&mut region);
    let above = [a[0], a[1] + 1, a[2]];
    let dirt = Some(ItemStack { item: DIRT_ITEM, count: 5 });
    set_slot(&mut region, 0, dirt);
    right_click(&mut region);
    let c = &region.data().clients[&0];
    assert_eq!(c.open_container, Some(a), "the chest is open");
    assert!(c.inventory_open, "with the inventory screen");
    assert_eq!(slot(&region, 0), dirt, "nothing was placed");
    assert_eq!(block(&region, above), BlockId::AIR);
    let view = game::container_view(region.data(), 0).expect("a view while open");
    assert_eq!(view.pos, a);
    assert_eq!(view.slots, vec![None; CHEST_SLOTS]);
}

#[test]
fn shift_click_moves_into_chest() {
    let mut region = region();
    let a = chest_under_crosshair(&mut region);
    right_click(&mut region);
    assert_eq!(region.data().clients[&0].open_container, Some(a));
    let dirt = ItemStack { item: DIRT_ITEM, count: 10 };
    set_slot(&mut region, 0, Some(dirt));
    let shift = |slot| GameEventKind::UiAction(0, UiAction::Click { slot, button: ClickButton::Left, shift: true });
    region.handle_event(shift(SlotRef::Player(0))).unwrap();
    assert_eq!(slot(&region, 0), None);
    assert_eq!(container(&region, a).unwrap()[0], Some(dirt), "into the chest");
    // And back: a container slot shift-clicks into the player.
    region.handle_event(shift(SlotRef::Container(0))).unwrap();
    assert_eq!(container(&region, a).unwrap()[0], None);
    assert_eq!(slot(&region, 0), Some(dirt));
    // Closing the inventory closes the chest.
    region.handle_event(GameEventKind::UiAction(0, UiAction::ToggleInventory)).unwrap();
    assert_eq!(region.data().clients[&0].open_container, None);
    assert!(game::container_view(region.data(), 0).is_none());
}

#[test]
fn walking_away_closes() {
    let mut region = region();
    let a = chest_under_crosshair(&mut region);
    right_click(&mut region);
    assert_eq!(region.data().clients[&0].open_container, Some(a));
    ticks(&mut region, 2);
    assert_eq!(region.data().clients[&0].open_container, Some(a), "in reach: stays open");
    let y = body(&region)[1];
    // Ten blocks away.
    region.with_data(|r| {
        let e = *r.data.player_entites.get(&0).unwrap();
        r.set_body_pose_safe(e, Pose::translation(136.0 + 160.0, y, 136.0));
    });
    ticks(&mut region, 1);
    let c = &region.data().clients[&0];
    assert_eq!(c.open_container, None, "out of reach: closed");
    assert!(!c.inventory_open, "the screen closes with it");
    assert!(container(&region, a).is_some(), "the chest itself stays");
}

#[test]
fn break_chest_spills_contents_and_chest() {
    let mut region = region();
    let a = chest_under_crosshair(&mut region);
    let contents = mixed_contents();
    region.with_data(|r| {
        r.containers.get_mut(&a).unwrap().slots = contents.clone();
    });
    mouse(&mut region, MouseButton::Left, true);
    let mut broke = false;
    for _ in 0..200 {
        ticks(&mut region, 1);
        if block(&region, a) == BlockId::AIR {
            broke = true;
            break;
        }
    }
    assert!(broke, "the chest breaks");
    assert!(container(&region, a).is_none(), "the container is gone");
    let mut want = totals_of(&contents);
    *want.entry(chest_item().0).or_default() += 1;
    assert_eq!(pile_totals(&region), want, "every item plus the chest, nothing lost");
    for p in region.data().item_piles().keys() {
        let d = (0..3).map(|i| (p[i] - a[i]).abs()).max().unwrap();
        assert!(d <= 1, "pile {p:?} at or around the chest cell {a:?}");
    }
}

#[test]
fn break_open_chest_spills_once_and_closes() {
    let mut region = region();
    let a = chest_under_crosshair(&mut region);
    forget(&mut region, GameEventKind::CreateClient(1));
    let contents = mixed_contents();
    region.with_data(|r| {
        r.containers.get_mut(&a).unwrap().slots = contents.clone();
        assert!(r.open_container_safe(0, a), "client 0 opens it");
        assert!(r.open_container_safe(1, a), "client 1 opens it");
    });
    assert_eq!(
        game::container_view(region.data(), 0),
        game::container_view(region.data(), 1),
        "both see one shared chest"
    );
    // One of them breaks it (an instant edit: no chest item drops).
    set_block(&mut region, a, BlockId::AIR);
    assert!(container(&region, a).is_none());
    assert_eq!(pile_totals(&region), totals_of(&contents), "spilled exactly once");
    for id in [0, 1] {
        let c = &region.data().clients[&id];
        assert_eq!(c.open_container, None, "client {id}'s view closed");
        assert!(!c.inventory_open, "client {id}'s screen closed");
    }
    // A tick later nothing spills again.
    ticks(&mut region, 1);
    assert_eq!(pile_totals(&region), totals_of(&contents));
}

#[test]
fn chest_survives_region_park_restore() {
    let mut region = region();
    let a = chest_under_crosshair(&mut region);
    let contents = mixed_contents();
    region.with_data(|r| {
        r.containers.get_mut(&a).unwrap().slots = contents.clone();
    });
    let parked = SerializedRegion::from_rollback(region.data());
    let restored = parked.to_rollback().expect("the parked region decodes");
    assert_eq!(restored.containers().get(&a).map(|c| c.slots.clone()), Some(contents));
    assert_eq!(state_hash(&restored), state_hash(region.data()));
}

#[test]
fn chest_undo_roundtrip() {
    let mut region = region();
    let a = target(&region);
    let above = [a[0], a[1] + 1, a[2]];
    set_slot(&mut region, 0, Some(ItemStack { item: chest_item(), count: 2 }));
    // Place.
    mouse(&mut region, MouseButton::Right, true);
    let before = state_hash(region.data());
    ticks(&mut region, 1);
    assert!(container(&region, above).is_some());
    region.rollback_last_event();
    assert_eq!(before, state_hash(region.data()), "placing a chest must undo bit-exact");
    assert!(container(&region, above).is_none());
    ticks(&mut region, 1);
    mouse(&mut region, MouseButton::Right, false);
    // Open (aiming at the new chest).
    assert_eq!(target(&region), above);
    mouse(&mut region, MouseButton::Right, true);
    let before = state_hash(region.data());
    ticks(&mut region, 1);
    assert_eq!(region.data().clients[&0].open_container, Some(above));
    region.rollback_last_event();
    assert_eq!(before, state_hash(region.data()), "opening must undo bit-exact");
    ticks(&mut region, 1);
    mouse(&mut region, MouseButton::Right, false);
    // Store, then break with it open: spill + close.
    set_slot(&mut region, 1, Some(ItemStack { item: DIRT_ITEM, count: 9 }));
    let shift = GameEventKind::UiAction(0, UiAction::Click { slot: SlotRef::Player(1), button: ClickButton::Left, shift: true });
    let before = state_hash(region.data());
    region.handle_event(shift.clone()).unwrap();
    region.rollback_last_event();
    assert_eq!(before, state_hash(region.data()), "a chest click must undo bit-exact");
    region.handle_event(shift).unwrap();
    let stored = container(&region, above).unwrap();
    assert_eq!(region.data().clients[&0].open_container, Some(above));
    region.with_data(|r| {
        let before = state_hash(&*r);
        r.new_transaction();
        assert!(r.set_block_safe(above, Block::new(BlockId::AIR)));
        assert!(r.containers().get(&above).is_none());
        assert_eq!(r.clients[&0].open_container, None);
        assert!(!r.item_piles().is_empty(), "spilled");
        r.rollback();
        assert_eq!(before, state_hash(&*r), "a spill must undo bit-exact");
        assert_eq!(r.containers().get(&above).map(|c| c.slots.clone()), Some(stored));
        assert_eq!(r.clients[&0].open_container, Some(above));
        assert!(r.item_piles().is_empty());
    });
}

#[test]
fn batched_fill_and_clear_follow_chests() {
    let mut region = region();
    let a = target(&region);
    let cells = [[a[0] + 2, a[1] + 1, a[2]], [a[0] + 3, a[1] + 1, a[2]]];
    region.with_data(|r| {
        assert_eq!(r.fill_blocks_safe(&cells, Block::new(chest())).len(), 2);
    });
    for c in cells {
        assert_eq!(container(&region, c), Some(vec![None; CHEST_SLOTS]), "a filled chest at {c:?} gets a container");
    }
    let dirt = ItemStack { item: DIRT_ITEM, count: 3 };
    region.with_data(|r| r.containers.get_mut(&cells[0]).unwrap().slots[4] = Some(dirt));
    region.with_data(|r| {
        assert_eq!(r.clear_blocks_safe(&cells), 2);
    });
    assert!(region.data().containers().is_empty(), "cleared chests leave no container");
    assert_eq!(pile_totals(&region), totals_of(&[Some(dirt)]), "the stored dirt spilled");
}
