//! Survival mining on stud tiles (`game::mining`, plan 2026-11-05 Decisions 7,
//! 8, 11; stud world plan P10a): an Adventure player holding the left button
//! on a tile breaks it after its break time, it drops its yield as a pile, a
//! short cooldown follows; the right button places the selected hotbar block
//! (consuming it). This file holds what `stud_gameplay::` (break, drop, place,
//! throw, trunk — each tick with the rollback bar) does not: retargeting,
//! exact cooldown counts, the state a body must not carry across a seam or a
//! restore, a whole session rolled back, and a break across a seam in every
//! placement.
use crate::common::{self, r0, STAND};

use game::edit::BlockPos;
use game::{
    state_hash, GameEventKind, InputEvent, ItemId, ItemStack, Key, MouseButton, Order, PlayMode, Region, Rollback,
    TileFill, BREAK_COOLDOWN_TICKS,
};

/// About 45 degrees down: 302 px * 0.0026 rad/px at the default sensitivity.
const DOWN_45: f32 = 302.0;

fn input(ev: InputEvent) -> GameEventKind {
    GameEventKind::PlayerInput(0, ev)
}

fn forget(region: &mut Region, ev: GameEventKind) {
    region.handle_event(ev).unwrap();
    region.forget_last_event();
}

fn dirt() -> ItemId {
    ItemId::block(game::block_props().id_named("dirt").expect("the manifest has dirt"))
}

/// Flat stud ground (grass over dirt over stone) under the whole region,
/// player 0 standing on tile (8, `STAND`, 8), first person, in Adventure,
/// facing −z and looking `pitch_px` of mouse motion down, its inventory
/// empty. Setup is forgotten.
fn region(pitch_px: f32) -> Region {
    common::install_real_block_props();
    let (send, recv) = crossbeam::channel::unbounded();
    std::mem::forget(recv);
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), r0(), None);
    region.seed_stud_world(common::uniform_seed());
    forget(&mut region, GameEventKind::CreateClient(0));
    forget(&mut region, GameEventKind::Order(0, Order::SetMode { mode: PlayMode::Adventure, at: Some([8, STAND, 8]) }));
    forget(&mut region, input(InputEvent::FpsCam(true)));
    region.with_data(|r| r.clients.get_mut(&0).unwrap().inventory = game::Inventory::new());
    for _ in 0..12 {
        forget(&mut region, GameEventKind::Tick);
    }
    forget(&mut region, input(InputEvent::MouseMotion { dx: 0.0, dy: pitch_px }));
    for _ in 0..3 {
        forget(&mut region, GameEventKind::Tick);
    }
    assert!(region.data().clients[&0].fps_cam_mode, "first person must be on");
    region
}

/// The brick under the crosshair: the min of its target box, in region stud cells.
fn target(region: &Region) -> BlockPos {
    region.data().eye_stud_hit(0).expect("the eye ray must hit a brick").lo
}

/// The min stud cell of the brick a right click would place now
/// (`game::brick_placement`).
fn place_at(region: &Region) -> BlockPos {
    let hit = region.data().eye_stud_hit(0).expect("a face under the crosshair");
    let (chunk, brick) = game::brick_placement(&hit.ray, game::MAT_DIRT).expect("a placement");
    let dims = [game::STUD_CHUNK_STUDS as i64, game::STUD_CHUNK_PLATES as i64, game::STUD_CHUNK_STUDS as i64];
    [0, 1, 2].map(|a| (chunk[a] * dims[a]) as i32 + i32::from(brick.min[a]))
}

/// Whether the brick whose box starts at stud cell `p` is gone.
fn open(region: &Region, p: BlockPos) -> bool {
    region.data().stud_material_at(p).is_none()
}

/// Ticks the hand needs for tile `p`.
fn need(region: &Region, p: BlockPos) -> usize {
    region.data().stud_mine_ticks(p).expect("a tile that can be mined") as usize
}

fn mouse(region: &mut Region, button: MouseButton, pressed: bool) {
    region.handle_event(input(InputEvent::MouseButton { button, pressed })).unwrap();
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

/// Ticks until tile `p` is open while the left button is held (at most
/// `limit`), counting the tick that breaks it.
fn ticks_to_break(region: &mut Region, p: BlockPos, limit: usize) -> Option<usize> {
    for n in 1..=limit {
        ticks(region, 1);
        if open(region, p) {
            return Some(n);
        }
    }
    None
}

#[test]
fn release_cancels_progress() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    let n = need(&region, a);
    assert!(n > 4, "a tile of soil takes a while: {n}");
    mouse(&mut region, MouseButton::Left, true);
    ticks(&mut region, n - 2);
    mouse(&mut region, MouseButton::Left, false);
    ticks(&mut region, 1);
    assert!(region.data().clients[&0].mining.is_none(), "release cancels");
    assert!(!open(&region, a));
    mouse(&mut region, MouseButton::Left, true);
    assert_eq!(ticks_to_break(&mut region, a, 3 * n), Some(n), "progress starts over");
}

#[test]
fn retarget_restarts() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    let n = need(&region, a);
    mouse(&mut region, MouseButton::Left, true);
    ticks(&mut region, n - 2);
    // A quarter turn: another tile.
    region.handle_event(input(InputEvent::MouseMotion { dx: 604.0, dy: 0.0 })).unwrap();
    ticks(&mut region, 1);
    let b = target(&region);
    assert_ne!(a, b);
    assert!(!open(&region, b));
    let nb = need(&region, b);
    let m = region.data().clients[&0].mining.clone().unwrap();
    assert_eq!(m.target, b);
    assert_eq!(m.needed_ticks as usize, nb);
    // That tick counted for b: one less.
    assert_eq!(ticks_to_break(&mut region, b, 3 * nb), Some(nb - 1));
    assert!(!open(&region, a), "the first target was left alone");
}

#[test]
fn cooldown_blocks_next_break() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    let n = need(&region, a);
    mouse(&mut region, MouseButton::Left, true);
    assert_eq!(ticks_to_break(&mut region, a, 3 * n), Some(n));
    let b = target(&region);
    assert_ne!(a, b);
    let nb = need(&region, b);
    assert_eq!(ticks_to_break(&mut region, b, 4 * nb), Some(BREAK_COOLDOWN_TICKS as usize + nb));
}

/// Right click with the selected slot, one tick, release.
fn right_click(region: &mut Region) {
    mouse(region, MouseButton::Right, true);
    ticks(region, 1);
    mouse(region, MouseButton::Right, false);
    ticks(region, 1);
}

#[test]
fn place_with_empty_slot_does_nothing() {
    let mut region = region(DOWN_45);
    let at = place_at(&region);
    assert!(open(&region, at));
    // The selected slot (0) is empty; another slot holds dirt.
    set_slot(&mut region, 3, Some(ItemStack { item: dirt(), count: 5 }));
    right_click(&mut region);
    assert!(open(&region, at));
    assert_eq!(slot(&region, 3), Some(ItemStack { item: dirt(), count: 5 }));
}

#[test]
fn mining_undo_roundtrip() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    let n = need(&region, a);
    set_slot(&mut region, 0, Some(ItemStack { item: dirt(), count: 5 }));
    let before = state_hash(region.data());
    let mut events = 0;
    let mut run = |region: &mut Region, ev: GameEventKind| {
        region.handle_event(ev).unwrap();
        events += 1;
    };
    run(&mut region, input(InputEvent::MouseButton { button: MouseButton::Left, pressed: true }));
    for _ in 0..n + 1 {
        run(&mut region, GameEventKind::Tick);
    }
    assert!(open(&region, a), "the break happened");
    assert!(!piles(&region).is_empty());
    run(&mut region, input(InputEvent::MouseButton { button: MouseButton::Left, pressed: false }));
    // Places dirt against the face now under the crosshair, then throws.
    let at = place_at(&region);
    run(&mut region, input(InputEvent::MouseButton { button: MouseButton::Right, pressed: true }));
    run(&mut region, GameEventKind::Tick);
    // The place is an intent to the tile's owner: it lands at the next tick.
    run(&mut region, GameEventKind::Tick);
    assert!(!open(&region, at), "placed");
    assert_eq!(slot(&region, 0), Some(ItemStack { item: dirt(), count: 4 }));
    run(&mut region, input(InputEvent::MouseButton { button: MouseButton::Right, pressed: false }));
    run(&mut region, input(InputEvent::Key { key: Key::KeyQ, pressed: true }));
    run(&mut region, GameEventKind::Tick);
    assert_eq!(slot(&region, 0), Some(ItemStack { item: dirt(), count: 3 }));
    for _ in 0..events {
        region.rollback_last_event();
    }
    assert_eq!(state_hash(region.data()), before, "mining, place and throw roll back bit-exact");
    assert!(piles(&region).is_empty());
    assert_eq!(slot(&region, 0), Some(ItemStack { item: dirt(), count: 5 }));
}

/// A stale mining state and a cooldown far in the future, as a client
/// brings them from an older region (each region's tick starts at 0 at
/// genesis, so another region's absolute ticks mean nothing here).
fn stale_mining(region: &mut Region) {
    region.with_data(|r| {
        let c = r.clients.get_mut(&0).unwrap();
        c.break_cooldown_until = 1_000_000;
        c.mining = Some(game::Mining { target: [3, STAND - 1, 3], started_tick: 999_990, needed_ticks: 15 });
    });
}

#[test]
fn cooldown_does_not_cross_into_a_younger_region() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    let n = need(&region, a);
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
    assert_eq!(ticks_to_break(&mut region, a, 3 * n), Some(n), "mining progresses at once");
}

#[test]
fn cooldown_does_not_survive_a_restore() {
    let mut region = region(DOWN_45);
    let a = target(&region);
    let n = need(&region, a);
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
    assert_eq!(ticks_to_break(&mut region, a, 3 * n), Some(n), "mining progresses at once");
}

// --- Across a seam (Task 6.2) -------------------------------------------------

use std::cell::RefCell;

use crate::common::rig::Rig;
use crate::common::seam_translate::{assert_seam_invariant, offset, spawn_tile, Scenario};
use game::{InlineSchedule, WorldBlockPos};

/// Rounds the button is held at most (a tile of grass over dirt breaks in
/// well under a second).
const MINE_ROUNDS: u32 = 30;

/// Whether world tile `p` lost a brick at its owner, and that owner's tick.
fn owner_open(rig: &mut Rig, p: WorldBlockPos) -> (bool, u64) {
    let (rc, local) = p.to_local();
    let mut out = None;
    rig.manager.spawner_mut().with_region(rc, |r| {
        // A brick of it is gone: the tile is no longer whole.
        out = Some((game::TileSource::tile_fill(r.data(), [local.x, local.y, local.z]) != TileFill::Solid, r.current_tick()));
    });
    out.expect("the owner runs")
}

/// An Adventure player two tiles from the seam (+x/+z of the origin)
/// mines the floor tile one tile across it (−x/−z): the last tick sends
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
        // The middle of its top face: the ray meets no other tile.
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
        let (open, tick) = owner_open(rig, target);
        if open {
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
    // One brick's yield: its share of a tile's worth, rolled — dirt or nothing, and
    // the same in every placement (`assert_seam_invariant`).
    let piled: Vec<ItemStack> = end.piles.values().flatten().cloned().collect();
    assert!(piled.len() <= 1 && piled.iter().all(|s| s.item == dirt() && s.count == 1), "{piled:?}");
    let seen = s.seen.borrow().clone();
    assert_eq!(seen.len(), 4, "one entry per placement");
    let first = seen[0].0.expect("Centre broke it");
    for (i, s) in seen.iter().enumerate() {
        assert_eq!(*s, (Some(first), !piled.is_empty()), "placement #{i}: (ticks from the press to the break, pile at the owner)");
    }
    // Under jitter the outcome still matches.
    assert_seam_invariant(&MineAcross::default(), Some(InlineSchedule::jittery));
}
