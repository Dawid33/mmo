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

/// Local cell `p` of [`r0`] as the world cell chests are keyed by.
fn wc(p: BlockPos) -> game::WorldBlockPos {
    game::WorldBlockPos::from_local(r0(), game::BlockPos::new(p[0], p[1], p[2]))
}

fn container(region: &Region, p: BlockPos) -> Option<Vec<Option<ItemStack>>> {
    region.data().containers().get(&wc(p)).map(|c| c.slots.clone())
}

/// Fills the chest at local cell `p` (state surgery through the write funnel).
fn stock(r: &mut Rollback, p: BlockPos, slots: Vec<Option<ItemStack>>) {
    r.write_container_safe(wc(p), Some(game::Container { slots }));
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
    // The place is an intent to the cell's owner: it lands at the next tick.
    ticks(&mut region, 1);
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
    assert_eq!(c.open_container, Some(wc(a)), "the chest is open");
    assert!(c.inventory_open, "with the inventory screen");
    assert_eq!(slot(&region, 0), dirt, "nothing was placed");
    assert_eq!(block(&region, above), BlockId::AIR);
    let view = game::container_view(region.data(), 0).expect("a view while open");
    assert_eq!(view.pos, wc(a));
    assert_eq!(view.slots, vec![None; CHEST_SLOTS]);
}

#[test]
fn shift_click_moves_into_chest() {
    let mut region = region();
    let a = chest_under_crosshair(&mut region);
    right_click(&mut region);
    assert_eq!(region.data().clients[&0].open_container, Some(wc(a)));
    let dirt = ItemStack { item: DIRT_ITEM, count: 10 };
    set_slot(&mut region, 0, Some(dirt));
    let shift = |slot| GameEventKind::UiAction(0, UiAction::Click { slot, button: ClickButton::Left, shift: true });
    region.handle_event(shift(SlotRef::Player(0))).unwrap();
    assert_eq!(slot(&region, 0), None, "given up at once");
    // A put intent to the chest's owner (here), sent between ticks: due at
    // the tick after the next one, like any intent an event sends.
    ticks(&mut region, 2);
    assert_eq!(container(&region, a).unwrap()[0], Some(dirt), "into the chest");
    // And back: a container slot shift-clicks into the player — a take,
    // granted when it applies and delivered a tick later.
    region.handle_event(shift(SlotRef::Container(0))).unwrap();
    ticks(&mut region, 3);
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
    assert_eq!(region.data().clients[&0].open_container, Some(wc(a)));
    ticks(&mut region, 2);
    assert_eq!(region.data().clients[&0].open_container, Some(wc(a)), "in reach: stays open");
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
    region.with_data(|r| stock(r, a, contents.clone()));
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
        let p = game::local_cell(crate::common::r0(), *p);
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
        stock(r, a, contents.clone());
        assert!(r.open_container_safe(0, wc(a)), "client 0 opens it");
        assert!(r.open_container_safe(1, wc(a)), "client 1 opens it");
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
    region.with_data(|r| stock(r, a, contents.clone()));
    let parked = SerializedRegion::from_rollback(region.data());
    let restored = parked.to_rollback().expect("the parked region decodes");
    assert_eq!(restored.containers().get(&wc(a)).map(|c| c.slots.clone()), Some(contents));
    assert_eq!(state_hash(&restored), state_hash(region.data()));
}

#[test]
fn chest_undo_roundtrip() {
    let mut region = region();
    let a = target(&region);
    let above = [a[0], a[1] + 1, a[2]];
    set_slot(&mut region, 0, Some(ItemStack { item: chest_item(), count: 2 }));
    // Place: sent at this tick, an intent landing at the next one.
    mouse(&mut region, MouseButton::Right, true);
    ticks(&mut region, 1);
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
    assert_eq!(region.data().clients[&0].open_container, Some(wc(above)));
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
    // The put lands two ticks on; undone bit-exact too.
    ticks(&mut region, 1);
    let before = state_hash(region.data());
    ticks(&mut region, 1);
    assert!(container(&region, above).unwrap()[0].is_some(), "the put applied");
    region.rollback_last_event();
    assert_eq!(before, state_hash(region.data()), "a put applied must undo bit-exact");
    ticks(&mut region, 1);
    let stored = container(&region, above).unwrap();
    assert_eq!(stored[0], Some(ItemStack { item: DIRT_ITEM, count: 9 }), "stored");
    assert_eq!(region.data().clients[&0].open_container, Some(wc(above)));
    region.with_data(|r| {
        let before = state_hash(&*r);
        r.new_transaction();
        assert!(r.set_block_safe(above, Block::new(BlockId::AIR)));
        assert!(r.containers().get(&wc(above)).is_none());
        assert_eq!(r.clients[&0].open_container, None);
        assert!(!r.item_piles().is_empty(), "spilled");
        r.rollback();
        assert_eq!(before, state_hash(&*r), "a spill must undo bit-exact");
        assert_eq!(r.containers().get(&wc(above)).map(|c| c.slots.clone()), Some(stored));
        assert_eq!(r.clients[&0].open_container, Some(wc(above)));
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
    region.with_data(|r| {
        let mut slots = vec![None; CHEST_SLOTS];
        slots[4] = Some(dirt);
        stock(r, cells[0], slots);
    });
    region.with_data(|r| {
        assert_eq!(r.clear_blocks_safe(&cells), 2);
    });
    assert!(region.data().containers().is_empty(), "cleared chests leave no container");
    assert_eq!(pile_totals(&region), totals_of(&[Some(dirt)]), "the stored dirt spilled");
}

// --- Across a seam (Task 6.4) -------------------------------------------------

use crate::common::rig::{Rig, CLIENT};
use crate::common::seam_translate::{
    assert_seam_invariant, offset, spawn_tile, uniform_floor, Axis, Placement, Scenario,
};
use game::{InlineSchedule, Order, WorldBlockPos};

fn stack(item: ItemId, count: u32) -> ItemStack {
    ItemStack { item, count }
}

/// Writes a chest holding `slots` into world cell `p`, at its owner.
fn put_chest(rig: &mut Rig, p: WorldBlockPos, slots: Vec<Option<ItemStack>>) {
    let (rc, l) = p.to_local();
    let local = [l.x, l.y, l.z];
    rig.manager.spawner_mut().with_region(rc, |r| {
        r.with_data(|d| {
            assert!(d.set_block_safe(local, Block::new(chest())), "chest at {p:?}");
            d.write_container_safe(p, Some(game::Container { slots }));
        })
    });
}

/// The slots of the chest in world cell `p`, at its owner.
fn chest_at(rig: &mut Rig, p: WorldBlockPos) -> Option<Vec<Option<ItemStack>>> {
    let mut out = None;
    rig.manager.spawner_mut().with_region(p.to_local().0, |r| {
        out = r.data().containers().get(&p).map(|c| c.slots.clone());
    });
    out
}

/// Client `id`'s open chest and whether its inventory screen is open, in the
/// region owning its body (`None`: no region has it).
fn view_of(rig: &mut Rig, id: ClientId) -> Option<(Option<WorldBlockPos>, bool)> {
    let mut out = None;
    for rc in rig.manager.running_regions() {
        rig.manager.spawner_mut().with_region(rc, |r| {
            if let Some(c) = r.data().clients.get(&id).filter(|_| r.data().player_entites.contains_key(&id)) {
                out = Some((c.open_container, c.inventory_open));
            }
        });
    }
    out
}

fn ui(rig: &mut Rig, action: UiAction) {
    let ev = game::GameEvent::new(GameEventKind::UiAction(CLIENT, action), 0, game::SPAWN_REGION);
    rig.event(game::ServerEvent::ClientPacket(game::ClientPacket::GameEvent(ev), CLIENT));
}

fn click(rig: &mut Rig, slot: SlotRef, shift: bool) {
    ui(rig, UiAction::Click { slot, button: ClickButton::Left, shift });
}

/// An Adventure player on the origin tile right-clicks the chest one block
/// across the seam / corner (−x/−z of it, on the floor): it opens. Then a
/// shift click takes the chest's five dirt into the inventory, and the ten
/// stone are picked onto the cursor and put into chest slot 1.
struct ChestAcross;

impl Scenario for ChestAcross {
    fn setup(&self, rig: &mut Rig, origin: WorldBlockPos) {
        rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some(spawn_tile(origin)) });
        rig.input(InputEvent::FpsCam(true));
        for _ in 0..6 {
            rig.round();
        }
        let rc = rig.owners()[0];
        rig.manager.spawner_mut().with_region(rc, |r| {
            r.with_data(|d| {
                let c = d.clients.get_mut(&CLIENT).unwrap();
                c.inventory = Inventory::new();
                c.inventory.slots[1] = Some(stack(STONE_ITEM, 10));
            })
        });
        let cell = offset(origin, -1, 0, -1);
        let mut slots = vec![None; CHEST_SLOTS];
        slots[0] = Some(stack(DIRT_ITEM, 5));
        put_chest(rig, cell, slots);
        // The chest's halo mirror reaches a neighbour with the owner's next
        // tick: a player sees it before it clicks it.
        for _ in 0..3 {
            rig.round();
        }
        rig.aim_at([cell.x as f64 + 0.5, cell.y as f64 + 0.5, cell.z as f64 + 0.5]);
        rig.input(InputEvent::MouseButton { button: MouseButton::Right, pressed: true });
    }

    fn drive(&self, rig: &mut Rig, round: u32) {
        match round {
            1 => rig.input(InputEvent::MouseButton { button: MouseButton::Right, pressed: false }),
            2 => click(rig, SlotRef::Container(0), true),
            5 => click(rig, SlotRef::Player(1), false),
            6 => click(rig, SlotRef::Container(1), false),
            _ => {}
        }
    }

    fn rounds(&self) -> u32 {
        12
    }

    fn crosses(&self) -> bool {
        false
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn open_chest_across_seam() {
    let end = assert_seam_invariant(&ChestAcross, None);
    let (_, inv, cursor) = end.player().client.clone().expect("a client");
    assert_eq!(inv.slots[0], Some(stack(DIRT_ITEM, 5)), "the dirt was taken: {inv:?}");
    assert_eq!(inv.slots[1], None, "the stone went into the chest");
    assert_eq!(cursor, None);
    let mut want = vec![None; CHEST_SLOTS];
    want[1] = Some(stack(STONE_ITEM, 10));
    let chests: Vec<_> = end.containers.values().cloned().collect();
    assert_eq!(chests, vec![want], "one chest, holding the stone");
    assert_seam_invariant(&ChestAcross, Some(InlineSchedule::jittery));
}

/// A chest two blocks across the −x seam stays open while its viewer walks
/// across the seam within reach, and closes once it walks out of reach.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn open_chest_follows_its_viewer_across_the_seam() {
    let mut rig = Rig::with_floor(None, uniform_floor);
    rig.join_spawn();
    let origin = Placement::Seam(Axis::X).origin();
    rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some(spawn_tile(origin)) });
    for _ in 0..6 {
        rig.round();
    }
    let cell = offset(origin, -2, 0, 2);
    put_chest(&mut rig, cell, vec![None; CHEST_SLOTS]);
    for _ in 0..3 {
        rig.round();
    }
    rig.manager.spawner_mut().with_region(game::SPAWN_REGION, |r| {
        r.with_data(|d| assert!(d.open_container_safe(CLIENT, cell), "opened across the seam"))
    });
    assert_eq!(view_of(&mut rig, CLIENT), Some((Some(cell), true)));
    let west = game::RegionCoords::new(-1, 0);
    let goal = |x: i64| {
        let p = offset(origin, x, 0, 0);
        game::Goal { column: [p.x, p.z], y: None }
    };
    rig.order(Order::MoveTo(goal(-2)));
    rig.walk_until_owned_by(west, "across the seam");
    for _ in 0..30 {
        rig.round();
        assert_eq!(view_of(&mut rig, CLIENT), Some((Some(cell), true)), "in reach: open");
    }
    rig.order(Order::MoveTo(goal(-12)));
    let mut closed = false;
    for _ in 0..crate::common::rig::LEG_ROUNDS {
        rig.round();
        if view_of(&mut rig, CLIENT) == Some((None, false)) {
            closed = true;
            break;
        }
    }
    assert!(closed, "out of reach: closed ({:?})", view_of(&mut rig, CLIENT));
    assert_eq!(chest_at(&mut rig, cell), Some(vec![None; CHEST_SLOTS]), "the chest itself stays");
}

/// Two viewers take from one chest slot in the same tick: the lower
/// `EntityId` gets the stack, the other is refused, and the items exist once.
#[test]
fn two_takers_of_one_slot_one_wins() {
    let mut region = region();
    let a = chest_under_crosshair(&mut region);
    forget(&mut region, GameEventKind::CreateClient(1));
    let wa = game::WorldBlockPos::from_local(r0(), game::BlockPos::new(a[0], a[1], a[2]));
    region.with_data(|r| {
        let mut slots = vec![None; CHEST_SLOTS];
        slots[0] = Some(stack(DIRT_ITEM, 5));
        r.write_container_safe(wa, Some(game::Container { slots }));
        r.clients.get_mut(&1).unwrap().inventory = Inventory::new();
        assert!(r.open_container_safe(0, wa), "client 0 opens it");
        assert!(r.open_container_safe(1, wa), "client 1 opens it");
    });
    for id in [1, 0] {
        let take = UiAction::Click { slot: SlotRef::Container(0), button: ClickButton::Left, shift: false };
        region.handle_event(GameEventKind::UiAction(id, take)).unwrap();
    }
    ticks(&mut region, 3);
    let cursor = |id: ClientId| region.data().clients[&id].cursor;
    let key = |id: ClientId| region.data().entity_id(region.data().player_entites[&id]).unwrap();
    let (winner, loser) = if key(0) < key(1) { (0, 1) } else { (1, 0) };
    assert_eq!(cursor(winner), Some(stack(DIRT_ITEM, 5)), "the lower id took the stack");
    assert_eq!(cursor(loser), None, "the other was refused");
    assert_eq!(container(&region, a).unwrap()[0], None);
    let held: u32 =
        [0, 1].iter().flat_map(|id| region.data().clients[id].inventory.slots.clone()).flatten().map(|s| s.count).sum();
    assert_eq!(held, 0, "nothing else moved");
}

/// A chest in region (−1, 0) by the seam, open in two views — its owner's
/// client 1 standing by it, and `CLIENT` across the seam — is broken by
/// `CLIENT` (an intent to its owner): it spills there once, and both views
/// close.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn break_chest_across_seam_spills_once_and_closes_both_views() {
    let mut rig = Rig::with_floor(None, uniform_floor);
    rig.join_spawn();
    let origin = Placement::Seam(Axis::X).origin();
    rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some(spawn_tile(origin)) });
    for _ in 0..6 {
        rig.round();
    }
    let west = game::RegionCoords::new(-1, 0);
    let cell = offset(origin, -2, 0, 0);
    let contents = mixed_contents();
    put_chest(&mut rig, cell, contents.clone());
    rig.manager.spawner_mut().with_region(west, |r| {
        r.handle_event(GameEventKind::CreateClient(1)).unwrap();
        r.with_data(|d| {
            let e = *d.data.player_entites.get(&1).unwrap();
            let bv = game::BLOCK_VOXELS as f32;
            let (_, l) = offset(cell, -1, 0, 0).to_local();
            let h = *d.data.ecs.rigidbody.get(e);
            let y = d.data.physics.bodies.get(h).unwrap().translation().y;
            d.set_body_pose_safe(e, Pose::translation((l.x as f32 + 0.5) * bv, y, (l.z as f32 + 0.5) * bv));
        });
    });
    for _ in 0..4 {
        rig.round();
    }
    rig.manager.spawner_mut().with_region(west, |r| {
        r.with_data(|d| assert!(d.open_container_safe(1, cell), "client 1 opens it at its owner"))
    });
    rig.manager.spawner_mut().with_region(game::SPAWN_REGION, |r| {
        r.with_data(|d| assert!(d.open_container_safe(CLIENT, cell), "CLIENT opens it across the seam"))
    });
    rig.order(Order::DevBreakBlock(cell));
    let spilled = |rig: &mut Rig| {
        let mut out = BTreeMap::new();
        for rc in rig.manager.running_regions() {
            rig.manager.spawner_mut().with_region(rc, |r| {
                for s in r.data().item_piles().values().flat_map(|p| &p.stacks) {
                    *out.entry(s.item.0).or_insert(0u32) += s.count;
                }
            });
        }
        out
    };
    for _ in 0..8 {
        rig.round();
    }
    assert_eq!(chest_at(&mut rig, cell), None, "the chest is gone");
    assert_eq!(spilled(&mut rig), totals_of(&contents), "spilled exactly once");
    assert_eq!(view_of(&mut rig, CLIENT), Some((None, false)), "the view across the seam closed");
    let mut owner_view = None;
    rig.manager.spawner_mut().with_region(west, |r| {
        owner_view = r.data().clients.get(&1).map(|c| (c.open_container, c.inventory_open));
    });
    assert_eq!(owner_view, Some((None, false)), "the owner's view closed");
    for _ in 0..8 {
        rig.round();
    }
    assert_eq!(spilled(&mut rig), totals_of(&contents), "and never again");
}
