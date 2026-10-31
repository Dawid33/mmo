//! Per-client play mode (`game::adventure`): Fortress by default, switched by
//! `Order::SetMode`, which puts the adventurer's body on a tile; the switch
//! undoes bit-exact and reaches the UI once. `Order::MoveTo` walks the body
//! along an A* path (`Walker`), re-planning once when stuck.
mod common;

use common::floor_chunks;
use game::rapier3d::math::Vector;
use game::{
    edge_toward, feet_of, feet_tile, play_mode_update, state_hash, steer, Block, BlockId, EntityKey, GameEventKind,
    Goal, Motion, Order, PlayMode, Region, RegionCoords, Rollback, TilePos, TileSource, UiUpdate, Walker,
    BLOCK_VOXELS, REGION_BLOCKS, STUCK_TICKS, TICK_RATE,
};

const CLIENT: game::ClientId = 7;
/// The tile the adventurer is put on: the air above the one-block floor.
const TILE: TilePos = [10, 1, 12];

/// One region with a flat one-block floor around the spawn and one player
/// settled on it, with a render channel (rollback needs one).
fn region_with_player() -> (Region, EntityKey) {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), RegionCoords::new(0, 0), None);
    let (x, z) = region.data().spawn_xz();
    let edge = common::CHUNK_EDGE;
    region.with_data(|r| r.seed_terrain_genesis(floor_chunks(x - edge..x + edge, z - edge..z + edge, 16)));
    region.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
    let e = *region.data().player_entites.get(&CLIENT).unwrap();
    for _ in 0..20 {
        tick(&mut region);
    }
    (region, e)
}

fn tick(region: &mut Region) {
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
}

fn mode(region: &Region) -> PlayMode {
    region.data().clients.get(&CLIENT).unwrap().mode
}

fn enter_adventure() -> Order {
    Order::SetMode { mode: PlayMode::Adventure, at: Some(TILE) }
}

#[test]
fn new_client_is_fortress() {
    let (region, _) = region_with_player();
    assert_eq!(mode(&region), PlayMode::Fortress);
}

#[test]
fn set_mode_adventure_places_body_on_tile() {
    let (mut region, e) = region_with_player();
    region.handle_event(GameEventKind::Order(CLIENT, enter_adventure())).unwrap();
    region.forget_last_event();
    tick(&mut region);
    let data = region.data();
    assert_eq!(mode(&region), PlayMode::Adventure);
    let handle = data.ecs.rigidbody.try_get(e).unwrap();
    let t = data.physics.bodies.get(handle).unwrap().translation();
    let bv = BLOCK_VOXELS as f32;
    let (cx, cz) = ((TILE[0] as f32 + 0.5) * bv, (TILE[2] as f32 + 0.5) * bv);
    assert!((t.x - cx).abs() <= 0.5 * bv && (t.z - cz).abs() <= 0.5 * bv, "body at {t:?}, tile centre ({cx}, {cz})");
    assert!(data.ecs.motion.try_get(e).unwrap_or_default().grounded, "body at {t:?} must be grounded");
}

#[test]
fn set_mode_undo_bit_exact() {
    let (mut region, _) = region_with_player();
    let before = state_hash(region.data());
    region.handle_event(GameEventKind::Order(CLIENT, enter_adventure())).unwrap();
    assert_eq!(mode(&region), PlayMode::Adventure);
    region.rollback_last_event();
    assert_eq!(before, state_hash(region.data()), "SetMode must roll back bit-exact");
    assert_eq!(mode(&region), PlayMode::Fortress);
}

#[test]
fn set_mode_adventure_twice_is_a_noop() {
    let (mut region, _) = region_with_player();
    region.handle_event(GameEventKind::Order(CLIENT, enter_adventure())).unwrap();
    region.forget_last_event();
    let before = state_hash(region.data());
    let mut accepted = false;
    region.with_data(|r| accepted = r.set_play_mode_safe(CLIENT, PlayMode::Adventure, Some([20, 1, 20])));
    assert!(accepted);
    assert_eq!(before, state_hash(region.data()));
}

#[test]
fn set_mode_emits_ui_update_once() {
    let (mut region, _) = region_with_player();
    let mut prev = PlayMode::default();
    assert_eq!(play_mode_update(&mut prev, region.data(), CLIENT), None, "Fortress is not news");
    region.handle_event(GameEventKind::Order(CLIENT, enter_adventure())).unwrap();
    region.forget_last_event();
    assert_eq!(
        play_mode_update(&mut prev, region.data(), CLIENT),
        Some(UiUpdate::PlayMode(PlayMode::Adventure))
    );
    tick(&mut region);
    assert_eq!(play_mode_update(&mut prev, region.data(), CLIENT), None, "sent only once");
    region
        .handle_event(GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Fortress, at: None }))
        .unwrap();
    assert_eq!(
        play_mode_update(&mut prev, region.data(), CLIENT),
        Some(UiUpdate::PlayMode(PlayMode::Fortress))
    );
}

#[test]
fn set_mode_teleport_clears_fall_velocity() {
    let (mut region, e) = region_with_player();
    region.with_data(|d| d.data.ecs.motion.set_safe(e, Some(Motion { vel_y: -500.0, flying: true, ..Motion::default() })));
    region.handle_event(GameEventKind::Order(CLIENT, enter_adventure())).unwrap();
    region.forget_last_event();
    let m = region.data().ecs.motion.try_get(e).unwrap();
    assert_eq!((m.vel_y, m.flying), (0.0, false), "a teleport must not carry a fall over");
}

// --- Walker (MoveTo) ---------------------------------------------------------

/// Floor block placed for steps and walls (`edit::PLACE_BLOCK`, solid).
const WALL: BlockId = BlockId(1);

fn adventure_at(at: TilePos) -> (Region, EntityKey) {
    let (mut region, e) = region_with_player();
    region
        .handle_event(GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Adventure, at: Some(at) }))
        .unwrap();
    region.forget_last_event();
    for _ in 0..5 {
        tick(&mut region);
    }
    (region, e)
}

fn here_goal(column: [i32; 2], y: Option<i32>) -> Goal {
    Goal { region: RegionCoords::new(0, 0), column, y }
}

fn move_to(region: &mut Region, goal: Goal) {
    region.handle_event(GameEventKind::Order(CLIENT, Order::MoveTo(goal))).unwrap();
    region.forget_last_event();
}

fn walker(region: &Region, e: EntityKey) -> Option<Walker> {
    region.data().ecs.walker.try_get(e).clone()
}

fn body_tile(region: &Region, e: EntityKey) -> TilePos {
    let data = region.data();
    let handle = data.ecs.rigidbody.try_get(e).unwrap();
    feet_tile(feet_of(data.physics.bodies.get(handle).unwrap().translation()))
}

fn place(region: &mut Region, tiles: impl IntoIterator<Item = TilePos>) {
    let tiles: Vec<_> = tiles.into_iter().collect();
    region.with_data(|d| {
        for p in tiles {
            assert!(d.set_block_safe(p, Block::new(WALL)), "place {p:?}");
        }
    });
}

/// Ticks until the walker is gone (at most `max`); the ticks taken.
fn walk_until_arrived(region: &mut Region, e: EntityKey, max: u32) -> u32 {
    for n in 1..=max {
        tick(region);
        if walker(region, e).is_none() {
            return n;
        }
    }
    panic!("still walking after {max} ticks, body on {:?}", body_tile(region, e));
}

/// Ticks for `metres` at walking speed, plus 50 %.
fn walk_budget(metres: f32) -> u32 {
    (metres / game::movement::WALK_SPEED * 1.5 * 1000.0 / TICK_RATE as f32).ceil() as u32
}

#[test]
fn move_to_walks_body_to_goal() {
    let (mut region, e) = adventure_at([4, 1, 4]);
    move_to(&mut region, here_goal([20, 20], None));
    assert!(walker(&region, e).is_some(), "MoveTo must start a walk");
    walk_until_arrived(&mut region, e, walk_budget(40.0));
    assert_eq!(body_tile(&region, e), [20, 1, 20]);
}

#[test]
fn walker_driven_body_plays_a_gait() {
    let (mut region, e) = adventure_at([4, 1, 4]);
    move_to(&mut region, here_goal([20, 20], None));
    for _ in 0..20 {
        tick(&mut region);
    }
    let a = region.data().ecs.animator.try_get(e).expect("players are rigged");
    let rig = game::anim::rig(a.rig);
    // WALK_SPEED is past ANIM_RUN_MIN_SPEED: the keyboard walk plays `run` too.
    assert!([rig.walk, rig.run].contains(&a.current.clip), "a walking adventurer plays a gait, got clip {}", a.current.clip);
}

#[test]
fn move_to_refused_in_fortress_mode() {
    let (mut region, e) = region_with_player();
    let before = state_hash(region.data());
    let mut accepted = true;
    region.with_data(|d| accepted = d.move_to_safe(CLIENT, here_goal([20, 20], None), RegionCoords::new(0, 0)));
    assert!(!accepted);
    assert!(walker(&region, e).is_none());
    assert_eq!(before, state_hash(region.data()));
}

#[test]
fn move_to_unreachable_changes_nothing() {
    let (mut region, e) = adventure_at([4, 1, 4]);
    // A 2-high ring around (20, 20): no step is that tall.
    let ring = (-1..=1).flat_map(|dx| (-1..=1).map(move |dz| (dx, dz))).filter(|&d| d != (0, 0));
    place(&mut region, ring.flat_map(|(dx, dz)| [1, 2].map(|y| [20 + dx, y, 20 + dz])));
    let before = state_hash(region.data());
    let mut accepted = true;
    region.with_data(|d| accepted = d.move_to_safe(CLIENT, here_goal([20, 20], None), RegionCoords::new(0, 0)));
    assert!(!accepted, "the enclosed goal has no path");
    assert!(walker(&region, e).is_none());
    assert_eq!(before, state_hash(region.data()));
}

#[test]
fn move_to_refuses_goal_two_regions_away() {
    let (mut region, e) = adventure_at([4, 1, 4]);
    let mut accepted = true;
    let goal = Goal { region: RegionCoords::new(2, 0), column: [4, 4], y: None };
    region.with_data(|d| accepted = d.move_to_safe(CLIENT, goal, RegionCoords::new(0, 0)));
    assert!(!accepted);
    assert!(walker(&region, e).is_none());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn move_to_climbs_step() {
    let (mut region, e) = adventure_at([4, 1, 10]);
    // A one-block platform on the route, the goal on top of it.
    place(&mut region, (12..24).flat_map(|x| (6..15).map(move |z| [x, 1, z])));
    move_to(&mut region, here_goal([20, 10], None));
    walk_until_arrived(&mut region, e, walk_budget(16.0) + 20);
    assert_eq!(body_tile(&region, e), [20, 2, 10], "the body must end on the upper level");
}

#[test]
fn move_to_undo_bit_exact() {
    let (mut region, _) = adventure_at([4, 1, 4]);
    let before = state_hash(region.data());
    region.handle_event(GameEventKind::Order(CLIENT, Order::MoveTo(here_goal([20, 20], None)))).unwrap();
    for _ in 0..10 {
        region.handle_event(GameEventKind::Tick).unwrap();
    }
    for _ in 0..11 {
        region.rollback_last_event();
    }
    assert_eq!(before, state_hash(region.data()), "MoveTo + walking must roll back bit-exact");
}

#[test]
fn fortress_mode_ends_the_walk() {
    let (mut region, e) = adventure_at([4, 1, 4]);
    move_to(&mut region, here_goal([20, 20], None));
    assert!(walker(&region, e).is_some());
    region
        .handle_event(GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Fortress, at: None }))
        .unwrap();
    region.forget_last_event();
    assert!(walker(&region, e).is_none());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walker_replans_once_then_gives_up() {
    let (mut region, e) = adventure_at([4, 1, 10]);
    move_to(&mut region, here_goal([20, 10], None));
    assert!(walker(&region, e).is_some());
    // A 2-high wall right ahead, across the whole floor (z 0..32).
    place(&mut region, (0..32).flat_map(|z| [1, 2].map(|y| [5, y, z])));
    let mut replanned = false;
    for _ in 0..2 * STUCK_TICKS + 5 {
        tick(&mut region);
        match walker(&region, e) {
            Some(w) => replanned |= w.replanned,
            None => break,
        }
    }
    assert!(walker(&region, e).is_none(), "a stuck walk must give up");
    assert!(replanned, "it must re-plan once before giving up");
}

#[test]
fn steer_points_at_next_waypoint() {
    let bv = BLOCK_VOXELS as f32;
    let feet = Vector::new(4.5 * bv, bv, 4.5 * bv);
    let walk = |next: TilePos| Walker {
        path: vec![[4, 1, 4], next],
        next: 1,
        ..Walker::default()
    };
    // +x: the yaw look_quat turns forward (-Z) onto +X, and plan walks +x.
    let (keys, yaw) = steer(&walk([5, 1, 4]), feet, true);
    assert!(keys.forward && !keys.sprint && !keys.jump_held && !keys.jump_pressed);
    let fwd = game::look_quat(yaw, 0.0) * Vector::new(0.0, 0.0, -1.0);
    assert!((fwd - Vector::X).length() < 1e-5, "forward {fwd:?}");
    let mut m = Motion { grounded: true, ..Motion::default() };
    let d = game::movement::plan(&mut m, &keys, yaw, 0);
    assert!(d.x > 0.0 && d.z.abs() < 1e-4, "plan moved {d:?}");
    // +z likewise.
    let (_, yaw) = steer(&walk([4, 1, 5]), feet, true);
    let fwd = game::look_quat(yaw, 0.0) * Vector::new(0.0, 0.0, -1.0);
    assert!((fwd - Vector::Z).length() < 1e-5, "forward {fwd:?}");
    // Jump only toward a higher tile, and only when grounded.
    assert!(steer(&walk([5, 2, 4]), feet, true).0.jump_held);
    assert!(!steer(&walk([5, 2, 4]), feet, false).0.jump_held);
    assert!(!steer(&walk([5, 0, 4]), feet, true).0.jump_held);
}

/// A flat one-block floor over every column except `holes`.
struct Flat {
    holes: Vec<[i32; 2]>,
}

impl TileSource for Flat {
    fn block(&self, p: TilePos) -> BlockId {
        if p[1] == 0 && !self.holes.contains(&[p[0], p[2]]) { WALL } else { BlockId::AIR }
    }
    fn is_bedrock(&self, _: TilePos) -> bool {
        false
    }
    fn edge_locked(&self, _: TilePos) -> bool {
        false
    }
    fn is_constructed(&self, _: TilePos) -> bool {
        false
    }
    fn tree_id(&self, _: TilePos) -> u32 {
        0
    }
    fn tree_limb(&self, _: TilePos) -> u8 {
        0
    }
}

#[test]
fn edge_toward_picks_facing_edge() {
    let n = REGION_BLOCKS as i32;
    let here = RegionCoords::new(0, 0);
    let flat = Flat { holes: vec![] };
    let from = [10, 1, 12];
    let east = Goal { region: RegionCoords::new(1, 0), column: [5, 12], y: None };
    assert_eq!(edge_toward(&flat, from, &east, here), Some([n - 1, 1, 12]));
    let west = Goal { region: RegionCoords::new(-1, 0), column: [5, 12], y: None };
    assert_eq!(edge_toward(&flat, from, &west, here), Some([0, 1, 12]));
    let north = Goal { region: RegionCoords::new(0, -1), column: [10, 40], y: None };
    assert_eq!(edge_toward(&flat, from, &north, here), Some([10, 1, 0]));
    // The line's own column has no floor: the two beside it tie, the lower wins.
    let holed = Flat { holes: vec![[n - 1, 12]] };
    assert_eq!(edge_toward(&holed, from, &east, here), Some([n - 1, 1, 11]));
    // Same region, or two regions away: no edge.
    assert_eq!(edge_toward(&flat, from, &Goal { region: here, ..east }, here), None);
    assert_eq!(edge_toward(&flat, from, &Goal { region: RegionCoords::new(2, 0), ..east }, here), None);
}

// --- Seam handoff -----------------------------------------------------------

/// A walking adventurer extracted as its handoff bundle (`EntityKey` freed).
fn walking_bundle(goal: Goal) -> game::EntityBundle {
    let (mut src, e) = adventure_at([4, 1, 4]);
    move_to(&mut src, goal);
    for _ in 0..5 {
        tick(&mut src);
    }
    let mut bundle = None;
    src.with_data(|d| bundle = Some(d.extract_entity_safe(e, RegionCoords::new(0, 0))));
    bundle.unwrap()
}

/// An empty region at `at` with the same floor as `region_with_player`.
fn floored_region(at: RegionCoords) -> Region {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), at, None);
    let (x, z) = region.data().spawn_xz();
    let edge = common::CHUNK_EDGE;
    region.with_data(|r| r.seed_terrain_genesis(floor_chunks(x - edge..x + edge, z - edge..z + edge, 16)));
    region
}

#[test]
fn walker_survives_bundle_roundtrip() {
    let goal = here_goal([20, 20], None);
    let bundle = walking_bundle(goal);
    assert_eq!(bundle.walker.as_ref().map(|w| w.goal), Some(goal), "the bundle carries the walk");

    let mut dst = floored_region(RegionCoords::new(0, 0));
    let before = state_hash(dst.data());
    dst.handle_event(GameEventKind::EntityArrived(bundle)).unwrap();
    let e = *dst.data().player_entites.get(&CLIENT).expect("arrived");
    let w = walker(&dst, e).expect("the walk survives the handoff");
    assert_eq!(w.goal, goal);
    assert_eq!(w.exit, None, "the goal is in the receiving region");
    assert_eq!(w.path.first(), Some(&body_tile(&dst, e)), "re-planned from where the body stands");
    dst.rollback_last_event();
    assert_eq!(before, state_hash(dst.data()), "arrival + re-plan must roll back bit-exact");
}

#[test]
fn arrived_walker_with_out_of_reach_goal_is_dropped() {
    let bundle = walking_bundle(here_goal([20, 20], None));
    // Received two regions away from its goal's region: too far to resume.
    let mut dst = floored_region(RegionCoords::new(2, 0));
    dst.handle_event(GameEventKind::EntityArrived(bundle)).unwrap();
    let e = *dst.data().player_entites.get(&CLIENT).expect("arrived");
    assert!(walker(&dst, e).is_none(), "an out-of-reach walk is dropped on arrival");
}
