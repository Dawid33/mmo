//! Survival mining (`game::mining`, plan 2026-11-05 Decisions 7, 8, 11): an
//! Adventure player holding the left button on a block breaks it after
//! `break_ticks` ticks (the hand rule), it drops its item as a pile, a short
//! cooldown follows; the right button places the selected hotbar block
//! (consuming it), and Q / Ctrl+Q throw the selected item two blocks ahead.
//! Fortress clients keep the instant creative edit (`block_edit.rs`).
use crate::common;

use common::r0;
use game::edit::BlockPos;
use game::{
    install_block_props, state_hash, Block, BlockId, BlockRegistry, Chunk, ChunkCoords, GameEventKind, InputEvent,
    ItemId, ItemStack, Key, MouseButton, PlayMode, Pose, Region, Rollback, BREAK_COOLDOWN_TICKS, CHUNK_UNITS,
    STICK, THROWN_PICKUP_DELAY_TICKS,
};

const MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");
const DIRT: BlockId = BlockId(1);
const GRASS: BlockId = BlockId(3);
const OAK_LEAVES: BlockId = BlockId(15);
const DIRT_ITEM: ItemId = ItemId(1);

/// Chunk column holding the player spawn (128, 128).
const SPAWN_CHUNK: usize = 128 / CHUNK_UNITS;

/// About 45 degrees down: 302 px * 0.0026 rad/px at the default sensitivity.
const DOWN_45: f32 = 302.0;

fn input(ev: InputEvent) -> GameEventKind {
    GameEventKind::PlayerInput(0, ev)
}

fn forget(region: &mut Region, ev: GameEventKind) {
    region.handle_event(ev).unwrap();
    region.forget_last_event();
}

/// A two-block dirt floor around the spawn, player 0 settled in the middle
/// of block (8, 2, 8), first person, in Adventure, looking 45 degrees down
/// (`pitch_px` of mouse motion). Setup is forgotten.
fn region(pitch_px: f32) -> Region {
    region_with(pitch_px, |_| Chunk::flat_floor(32))
}

/// [`region`] over a custom floor chunk (the same for every column).
fn region_with(pitch_px: f32, floor: impl Fn(ChunkCoords) -> Chunk) -> Region {
    let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
    install_block_props(reg.props());
    let mut chunks = Vec::new();
    let around = SPAWN_CHUNK.saturating_sub(1)..=SPAWN_CHUNK + 1;
    for x in around.clone() {
        for z in around.clone() {
            let at = ChunkCoords::new(x, 0, z);
            chunks.push((at, floor(at)));
        }
    }
    let (send, recv) = crossbeam::channel::unbounded();
    std::mem::forget(recv);
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), r0(), None);
    region.with_data(|r| r.seed_terrain_genesis(chunks));
    forget(&mut region, GameEventKind::CreateClient(0));
    forget(&mut region, input(InputEvent::FpsCam(true)));
    region.with_data(|r| {
        let c = r.clients.get_mut(&0).unwrap();
        c.mode = PlayMode::Adventure;
        c.inventory = game::Inventory::new();
    });
    for _ in 0..12 {
        forget(&mut region, GameEventKind::Tick);
    }
    let y = body(&region)[1];
    region.with_data(|r| {
        let e = *r.data.player_entites.get(&0).unwrap();
        r.set_body_pose_safe(e, Pose::translation(136.0, y, 136.0));
    });
    forget(&mut region, input(InputEvent::MouseMotion { dx: 0.0, dy: pitch_px }));
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

fn mouse(region: &mut Region, button: MouseButton, pressed: bool) {
    region.handle_event(input(InputEvent::MouseButton { button, pressed })).unwrap();
}

fn key(region: &mut Region, key: Key, pressed: bool) {
    region.handle_event(input(InputEvent::Key { key, pressed })).unwrap();
}

fn ticks(region: &mut Region, n: usize) {
    for _ in 0..n {
        region.handle_event(GameEventKind::Tick).unwrap();
    }
}

fn piles(region: &Region) -> Vec<(BlockPos, Vec<ItemStack>)> {
    region.data().item_piles().iter().map(|(p, pile)| (game::local_cell(r0(), *p), pile.stacks.clone())).collect()
}

fn set_slot(region: &mut Region, slot: usize, stack: Option<ItemStack>) {
    region.with_data(|r| r.clients.get_mut(&0).unwrap().inventory.slots[slot] = stack);
}

fn slot(region: &Region, slot: usize) -> Option<ItemStack> {
    region.data().clients[&0].inventory.slots[slot]
}

/// Ticks until `p` turns to air while the left button is held (at most
/// `limit`), counting the tick that breaks it.
fn ticks_to_break(region: &mut Region, p: BlockPos, limit: usize) -> Option<usize> {
    for n in 1..=limit {
        ticks(region, 1);
        if block(region, p) == BlockId::AIR {
            return Some(n);
        }
    }
    None
}

#[test]
fn dirt_breaks_after_15_ticks() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    assert_eq!(block(&region, a), DIRT);
    mouse(&mut region, MouseButton::Left, true);
    ticks(&mut region, 14);
    assert_eq!(block(&region, a), DIRT, "still solid after 14 ticks");
    let view = game::mining_view(&region.data().clients[&0], region.current_tick() as u64).unwrap();
    assert_eq!(view.target, a);
    assert_eq!(view.progress_per_mille, 14 * 1000 / 15);
    ticks(&mut region, 1);
    assert_eq!(block(&region, a), BlockId::AIR, "broken on the 15th tick");
    // One dirt resting in the hole, on the block below it.
    assert_eq!(piles(&region), vec![(a, vec![ItemStack { item: DIRT_ITEM, count: 1 }])]);
    assert!(region.data().clients[&0].mining.is_none());
}

#[test]
fn a_mined_slope_ends_as_air_and_drops_one_dirt() {
    // Find where the 45 degree eye ray lands on the plain floor, then make
    // exactly that block a slope.
    let a = target(&region(DOWN_45));
    let (coords, (bx, by, bz)) = game::edit::locate(a).unwrap();
    let bi = game::BlockIndex::from_xyz(bx, by, bz);
    let mut region = region_with(DOWN_45, |at| {
        let mut c = Chunk::flat_floor(32);
        if at == coords {
            c.set_block(bx, by, bz, Block::new(DIRT));
            c.chisel.insert(bi, game::slope_chisel(DIRT, game::SlopeShape::Straight { low: game::Side::PosX }));
        }
        c
    });
    let chisel_at = |region: &Region| {
        let snap = region.terrain_snapshot();
        snap.chunks.iter().find(|(c, _)| *c == coords).and_then(|(_, c)| c.chisel.get(&bi).cloned())
    };
    assert_eq!(target(&region), a, "the ray must still land on the same cell");
    assert_eq!(block(&region, a), DIRT);
    let slope = game::slope_chisel(DIRT, game::SlopeShape::Straight { low: game::Side::PosX });
    assert_eq!(chisel_at(&region), Some(slope), "the targeted cell carries the slope chisel");
    mouse(&mut region, MouseButton::Left, true);
    assert!(ticks_to_break(&mut region, a, 40).is_some(), "the slope must break");
    assert_eq!(block(&region, a), BlockId::AIR);
    assert_eq!(chisel_at(&region), None, "a broken block keeps no chisel");
    assert_eq!(piles(&region), vec![(a, vec![ItemStack { item: DIRT_ITEM, count: 1 }])]);
}

#[test]
fn release_cancels_progress() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    mouse(&mut region, MouseButton::Left, true);
    ticks(&mut region, 10);
    mouse(&mut region, MouseButton::Left, false);
    ticks(&mut region, 1);
    assert!(region.data().clients[&0].mining.is_none(), "release cancels");
    mouse(&mut region, MouseButton::Left, true);
    assert_eq!(ticks_to_break(&mut region, a, 40), Some(15), "progress starts over");
}

#[test]
fn retarget_restarts() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    mouse(&mut region, MouseButton::Left, true);
    ticks(&mut region, 10);
    // A quarter turn: another block.
    region.handle_event(input(InputEvent::MouseMotion { dx: 604.0, dy: 0.0 })).unwrap();
    ticks(&mut region, 1);
    let b = target(&region);
    assert_ne!(a, b);
    assert_eq!(block(&region, b), DIRT);
    let m = region.data().clients[&0].mining.clone().unwrap();
    assert_eq!(m.target, b);
    assert_eq!(m.needed_ticks, 15);
    // That tick counted for b: 14 more.
    assert_eq!(ticks_to_break(&mut region, b, 40), Some(14));
    assert_eq!(block(&region, a), DIRT, "the first target was left alone");
}

#[test]
fn cooldown_blocks_next_break_for_6_ticks() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    mouse(&mut region, MouseButton::Left, true);
    assert_eq!(ticks_to_break(&mut region, a, 40), Some(15));
    let b = target(&region);
    assert_ne!(a, b);
    assert_eq!(block(&region, b), DIRT);
    assert_eq!(ticks_to_break(&mut region, b, 60), Some(BREAK_COOLDOWN_TICKS as usize + 15));
}

#[test]
fn leaves_drop_nothing() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    set_block(&mut region, a, OAK_LEAVES);
    assert_eq!(target(&region), a, "leaves stop the eye ray");
    mouse(&mut region, MouseButton::Left, true);
    // Hardness 20: ceil(20 * 30 / 100) = 6.
    assert_eq!(ticks_to_break(&mut region, a, 40), Some(6));
    assert!(piles(&region).is_empty(), "{:?}", piles(&region));
}

#[test]
fn grass_drops_dirt() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    set_block(&mut region, a, GRASS);
    mouse(&mut region, MouseButton::Left, true);
    // Hardness 60: 18 ticks.
    assert_eq!(ticks_to_break(&mut region, a, 40), Some(18));
    assert_eq!(piles(&region), vec![(a, vec![ItemStack { item: DIRT_ITEM, count: 1 }])]);
}

#[test]
fn break_ticks_follow_the_hand_rule() {
    let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
    let props = reg.props();
    let id = |n: &str| props.id_named(n).unwrap();
    assert_eq!(game::break_ticks(&props, Block::new(DIRT)), 15);
    assert_eq!(game::break_ticks(&props, Block::new(id("stone"))), 45);
    assert_eq!(game::break_ticks(&props, Block::new(id("oak_branch"))), 30);
    // A trunk takes FELL_TICKS_FACTOR times as long.
    assert_eq!(game::break_ticks(&props, Block::new(id("oak_log"))), 60 * game::FELL_TICKS_FACTOR);
}

/// Right click with the selected slot, one tick, release.
fn right_click(region: &mut Region) {
    mouse(region, MouseButton::Right, true);
    ticks(region, 1);
    mouse(region, MouseButton::Right, false);
    ticks(region, 1);
}

#[test]
fn place_consumes_selected_and_refuses_non_block() {
    let mut region = region(DOWN_45);
    let hit = region.data().eye_hit(0).unwrap();
    let at = [hit.block[0] + hit.normal[0], hit.block[1] + hit.normal[1], hit.block[2] + hit.normal[2]];
    set_slot(&mut region, 0, Some(ItemStack { item: ItemId(2), count: 3 }));
    right_click(&mut region);
    assert_eq!(block(&region, at), BlockId(2), "stone placed on the hit face");
    assert_eq!(slot(&region, 0), Some(ItemStack { item: ItemId(2), count: 2 }));

    // A stick is not a block: nothing placed, nothing taken.
    let hit = region.data().eye_hit(0).unwrap();
    let at = [hit.block[0] + hit.normal[0], hit.block[1] + hit.normal[1], hit.block[2] + hit.normal[2]];
    set_slot(&mut region, 0, Some(ItemStack { item: STICK, count: 1 }));
    right_click(&mut region);
    assert_eq!(block(&region, at), BlockId::AIR);
    assert_eq!(slot(&region, 0), Some(ItemStack { item: STICK, count: 1 }));
}

#[test]
fn place_with_empty_slot_does_nothing() {
    let mut region = region(DOWN_45);
    let hit = region.data().eye_hit(0).unwrap();
    let at = [hit.block[0] + hit.normal[0], hit.block[1] + hit.normal[1], hit.block[2] + hit.normal[2]];
    // The selected slot (0) is empty; another slot holds dirt.
    set_slot(&mut region, 3, Some(ItemStack { item: DIRT_ITEM, count: 5 }));
    right_click(&mut region);
    assert_eq!(block(&region, at), BlockId::AIR);
    assert_eq!(slot(&region, 3), Some(ItemStack { item: DIRT_ITEM, count: 5 }));
}

#[test]
fn throw_lands_two_blocks_ahead_on_ground() {
    // Looking level, facing -z.
    let mut region = region(0.0);
    set_slot(&mut region, 0, Some(ItemStack { item: DIRT_ITEM, count: 5 }));
    let t = region.current_tick() as u64;
    key(&mut region, Key::KeyQ, true);
    ticks(&mut region, 1);
    key(&mut region, Key::KeyQ, false);
    // Body block (8, 2, 8); two blocks ahead (-z) on the floor (y 2).
    assert_eq!(piles(&region), vec![([8, 2, 6], vec![ItemStack { item: DIRT_ITEM, count: 1 }])]);
    assert_eq!(region.data().item_piles()[&game::WorldBlockPos::from_local(r0(), game::BlockPos::new(8, 2, 6))].pickup_after_tick, t + THROWN_PICKUP_DELAY_TICKS);
    assert_eq!(slot(&region, 0), Some(ItemStack { item: DIRT_ITEM, count: 4 }));

    // Ctrl+Q: the whole stack.
    ticks(&mut region, 1);
    key(&mut region, Key::ControlLeft, true);
    key(&mut region, Key::KeyQ, true);
    ticks(&mut region, 1);
    assert_eq!(piles(&region), vec![([8, 2, 6], vec![ItemStack { item: DIRT_ITEM, count: 5 }])]);
    assert_eq!(slot(&region, 0), None);
}

#[test]
fn mining_undo_roundtrip() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    set_slot(&mut region, 0, Some(ItemStack { item: DIRT_ITEM, count: 5 }));
    let before = state_hash(region.data());
    let mut events = 0;
    let mut run = |region: &mut Region, ev: GameEventKind| {
        region.handle_event(ev).unwrap();
        events += 1;
    };
    run(&mut region, input(InputEvent::MouseButton { button: MouseButton::Left, pressed: true }));
    for _ in 0..20 {
        run(&mut region, GameEventKind::Tick);
    }
    assert_eq!(block(&region, a), BlockId::AIR, "the break happened");
    assert!(!piles(&region).is_empty());
    run(&mut region, input(InputEvent::MouseButton { button: MouseButton::Left, pressed: false }));
    // Places dirt back into the hole (pushing the pile up), then throws.
    run(&mut region, input(InputEvent::MouseButton { button: MouseButton::Right, pressed: true }));
    run(&mut region, GameEventKind::Tick);
    // The place is an intent to the cell's owner: it lands at the next tick.
    run(&mut region, GameEventKind::Tick);
    assert_eq!(block(&region, a), DIRT, "placed back");
    run(&mut region, input(InputEvent::Key { key: Key::KeyQ, pressed: true }));
    run(&mut region, GameEventKind::Tick);
    assert_eq!(slot(&region, 0), Some(ItemStack { item: DIRT_ITEM, count: 3 }));
    for _ in 0..events {
        region.rollback_last_event();
    }
    assert_eq!(state_hash(region.data()), before, "mining, place and throw roll back bit-exact");
    assert!(piles(&region).is_empty());
    assert_eq!(slot(&region, 0), Some(ItemStack { item: DIRT_ITEM, count: 5 }));
}

/// A stale mining state and a cooldown far in the future, as a client
/// brings them from an older region (each region's tick starts at 0 at
/// genesis, so another region's absolute ticks mean nothing here).
fn stale_mining(region: &mut Region) {
    region.with_data(|r| {
        let c = r.clients.get_mut(&0).unwrap();
        c.break_cooldown_until = 1_000_000;
        c.mining = Some(game::Mining { target: [3, 1, 3], started_tick: 999_990, needed_ticks: 15 });
    });
}

#[test]
fn cooldown_does_not_cross_into_a_younger_region() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    stale_mining(&mut region);
    // The body arrives over a seam (here: from region (1, 0)).
    region.with_data(|r| {
        let e = *r.data.player_entites.get(&0).unwrap();
        let mut bundle = r.extract_entity_safe(e);
        bundle.id.birth_region = game::RegionCoords::new(1, 0);
        r.apply_arrival(bundle).expect("the body arrives");
    });
    let c = &region.data().clients[&0];
    assert_eq!((c.break_cooldown_until, c.mining), (0, None), "region-local state reset on arrival");
    assert_eq!(target(&region), a);
    mouse(&mut region, MouseButton::Left, true);
    assert_eq!(ticks_to_break(&mut region, a, 40), Some(15), "mining progresses at once");
}

#[test]
fn cooldown_does_not_survive_a_restore() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    stale_mining(&mut region);
    let record = region.data().player_record(0, r0()).expect("a record");
    assert_eq!((record.client.break_cooldown_until, record.client.mining), (0, None), "the record drops it");
    // Restoring a record that still carries it (an old store) drops it too.
    let mut stale = record.clone();
    stale.client.break_cooldown_until = 1_000_000;
    region.handle_event(GameEventKind::ParkPlayer(0)).unwrap();
    region.handle_event(GameEventKind::RestorePlayer(0, Box::new(stale))).unwrap();
    assert_eq!(region.data().clients[&0].break_cooldown_until, 0);
    region.handle_event(input(InputEvent::FpsCam(true))).unwrap();
    region.handle_event(input(InputEvent::MouseMotion { dx: 0.0, dy: DOWN_45 })).unwrap();
    ticks(&mut region, 3);
    assert_eq!(target(&region), a);
    mouse(&mut region, MouseButton::Left, true);
    assert_eq!(ticks_to_break(&mut region, a, 40), Some(15), "mining progresses at once");
}

// --- Across a seam (Task 6.2) -------------------------------------------------

use std::cell::RefCell;

use crate::common::rig::Rig;
use crate::common::seam_translate::{assert_seam_invariant, offset, spawn_tile, Scenario};
use game::{InlineSchedule, Order, WorldBlockPos};

/// Rounds the button is held at most (dirt: 15 ticks to break).
const MINE_ROUNDS: u32 = 30;

/// The world block `p`'s owner sees, and that owner's tick.
fn owner_block(rig: &mut Rig, p: WorldBlockPos) -> (BlockId, u64) {
    let (rc, local) = p.to_local();
    let mut out = None;
    rig.manager.spawner_mut().with_region(rc, |r| {
        out = Some((
            r.data().block_at([local.x, local.y, local.z]).map_or(BlockId::AIR, |b| b.id),
            r.current_tick(),
        ));
    });
    out.expect("the owner runs")
}

/// An Adventure player two blocks from the seam (+x/+z of the origin)
/// mines the floor block one block across it (−x/−z): the last tick sends
/// the break to the block's owner. Records, per run, how many ticks after
/// the press the block broke at its owner, and whether the drop is a pile
/// there.
#[derive(Default)]
struct MineAcross {
    /// Per run: (ticks from the press to the break, the owner holds the pile).
    seen: RefCell<Vec<(Option<u64>, bool)>>,
    /// The current run: the home tick at the press, the target.
    run: RefCell<Option<(u64, WorldBlockPos)>>,
}

impl Scenario for MineAcross {
    fn setup(&self, rig: &mut Rig, origin: WorldBlockPos) {
        let at = offset(origin, 2, 0, 2);
        rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some(spawn_tile(at)) });
        rig.input(InputEvent::FpsCam(true));
        for _ in 0..6 {
            rig.round();
        }
        let target = offset(origin, -1, -1, -1);
        // The middle of its top face: the ray meets no other solid block.
        rig.aim_at([target.x as f64 + 0.5, target.y as f64 + 1.0, target.z as f64 + 0.5]);
        assert_eq!(rig.eye_hit().map(|h| h.0), Some(target), "the crosshair is on the block across the seam");
        let home = rig.owners()[0];
        let mut now = 0;
        rig.manager.spawner_mut().with_region(home, |r| now = r.current_tick());
        *self.run.borrow_mut() = Some((now, target));
        self.seen.borrow_mut().push((None, false));
        rig.input(InputEvent::MouseButton { button: MouseButton::Left, pressed: true });
    }

    fn drive(&self, rig: &mut Rig, _round: u32) {
        let (pressed_at, target) = self.run.borrow().expect("set up");
        let mut seen = self.seen.borrow_mut();
        let entry = seen.last_mut().expect("this run's entry");
        if entry.0.is_some() {
            return;
        }
        let (id, tick) = owner_block(rig, target);
        if id == BlockId::AIR {
            entry.0 = Some(tick - pressed_at);
            let (rc, _) = target.to_local();
            rig.manager.spawner_mut().with_region(rc, |r| entry.1 = !r.data().item_piles().is_empty());
            // One block: the next one under the crosshair stays.
            rig.input(InputEvent::MouseButton { button: MouseButton::Left, pressed: false });
        }
    }

    fn rounds(&self) -> u32 {
        MINE_ROUNDS
    }

    fn crosses(&self) -> bool {
        false
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn break_block_across_seam() {
    let s = MineAcross::default();
    let end = assert_seam_invariant(&s, None);
    assert!(end.terrain.values().any(|d| d.is_some()), "the block broke: {:?}", end.terrain);
    assert_eq!(
        end.piles.values().cloned().collect::<Vec<_>>(),
        vec![vec![ItemStack { item: DIRT_ITEM, count: 1 }]],
        "one dirt dropped"
    );
    let seen = s.seen.borrow().clone();
    assert_eq!(seen.len(), 4, "one entry per placement");
    let first = seen[0].0.expect("Centre broke it");
    for (i, s) in seen.iter().enumerate() {
        assert_eq!(*s, (Some(first), true), "placement #{i}: (ticks from the press to the break, pile at the owner)");
    }
    // Under jitter the outcome still matches.
    assert_seam_invariant(&MineAcross::default(), Some(InlineSchedule::jittery));
}
