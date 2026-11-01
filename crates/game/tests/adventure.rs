//! Per-client play mode (`game::adventure`): Fortress by default, switched by
//! `Order::SetMode`, which puts the adventurer's body on a tile; the switch
//! undoes bit-exact and reaches the UI once. `Order::MoveTo` walks the body
//! along an A* path (`Walker`), re-planning once when stuck.
use crate::common;

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

/// Chunk layer holding the high floor of [`high_region`] (block 2,000).
const HIGH_CHUNK_Y: i32 = 125;

/// Like `region_with_player`, but the one-block floor lies at block y 2,000
/// (chunk layer 125, ground recipes with surface 2,000 and nothing below the
/// band): far past the pathfinder's fixed scan constants.
fn high_region() -> Region {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), RegionCoords::new(0, 0), None);
    let (x, z) = region.data().spawn_xz();
    let edge = common::CHUNK_EDGE;
    let chunks = floor_chunks(x - edge..x + edge, z - edge..z + edge, 16)
        .into_iter()
        .map(|(c, chunk)| (game::ChunkCoords::new(c.x, HIGH_CHUNK_Y, c.z), chunk))
        .collect::<Vec<_>>();
    let ground = chunks
        .iter()
        .map(|(c, _)| {
            let surface = vec![(HIGH_CHUNK_Y * game::CHUNK_BLOCKS as i32) as i16; game::COLUMN_FOOTPRINT];
            let g = game::ColumnGround::plain(surface.into_boxed_slice(), HIGH_CHUNK_Y, WALL, BlockId(2));
            (game::ColumnCoords::new(c.x, c.z), g)
        })
        .collect::<Vec<_>>();
    region.with_data(|r| {
        r.seed_ground_genesis(ground);
        r.seed_terrain_genesis(chunks);
    });
    region.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
    region
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walker_paths_on_ground_above_block_512() {
    let mut region = high_region();
    let e = *region.data().player_entites.get(&CLIENT).unwrap();
    let floor = HIGH_CHUNK_Y * game::CHUNK_BLOCKS as i32;
    assert_eq!(region.data().column_band([4, 4]), Some((floor, floor + game::CHUNK_BLOCKS as i32 - 1)));
    region
        .handle_event(GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Adventure, at: Some([4, floor + 1, 4]) }))
        .unwrap();
    region.forget_last_event();
    for _ in 0..5 {
        tick(&mut region);
    }
    assert_eq!(body_tile(&region, e), [4, floor + 1, 4], "the body must be put on the high floor");
    move_to(&mut region, here_goal([20, 20], None));
    assert!(walker(&region, e).is_some(), "MoveTo must find a path at block {floor}");
    walk_until_arrived(&mut region, e, walk_budget(40.0));
    assert_eq!(body_tile(&region, e), [20, floor + 1, 20]);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walker_paths_in_dug_space_below_the_band_base() {
    let mut region = high_region();
    let e = *region.data().player_entites.get(&CLIENT).unwrap();
    let floor = HIGH_CHUNK_Y * game::CHUNK_BLOCKS as i32;
    // A tunnel 30 blocks under the surface: chunk layer 123, well below the
    // band base (layer 125), so its chunks materialise out of implied ground.
    let tunnel_y = floor - 30;
    assert!(tunnel_y < HIGH_CHUNK_Y * game::CHUNK_BLOCKS as i32 - 3);
    region.with_data(|d| {
        for x in 4..=20 {
            for y in [tunnel_y, tunnel_y + 1] {
                assert!(d.set_block_safe([x, y, 4], Block::new(BlockId::AIR)), "dig {x},{y}");
            }
        }
    });
    region
        .handle_event(GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Adventure, at: Some([4, tunnel_y, 4]) }))
        .unwrap();
    region.forget_last_event();
    for _ in 0..5 {
        tick(&mut region);
    }
    assert_eq!(body_tile(&region, e), [4, tunnel_y, 4], "the body must be put in the tunnel");
    move_to(&mut region, here_goal([20, 4], Some(tunnel_y)));
    assert!(walker(&region, e).is_some(), "MoveTo must find a path in the tunnel");
    walk_until_arrived(&mut region, e, walk_budget(20.0));
    assert_eq!(body_tile(&region, e), [20, tunnel_y, 4]);
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
    region.with_data(|d| accepted = d.move_to_safe(CLIENT, here_goal([20, 20], None), RegionCoords::new(0, 0), None));
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
    region.with_data(|d| accepted = d.move_to_safe(CLIENT, here_goal([20, 20], None), RegionCoords::new(0, 0), None));
    assert!(!accepted, "the enclosed goal has no path");
    assert!(walker(&region, e).is_none());
    assert_eq!(before, state_hash(region.data()));
}

#[test]
fn move_to_refuses_goal_two_regions_away() {
    let (mut region, e) = adventure_at([4, 1, 4]);
    let mut accepted = true;
    let goal = Goal { region: RegionCoords::new(2, 0), column: [4, 4], y: None };
    region.with_data(|d| accepted = d.move_to_safe(CLIENT, goal, RegionCoords::new(0, 0), None));
    assert!(!accepted);
    assert!(walker(&region, e).is_none());
}

#[test]
fn extreme_untrusted_coordinates_are_refused_without_panic() {
    let (mut region, e) = adventure_at([4, 1, 4]);
    let before = state_hash(region.data());
    for v in [i32::MAX, i32::MIN, -1, REGION_BLOCKS as i32] {
        for goal in [
            here_goal([v, 5], None),
            here_goal([5, v], Some(1)),
            Goal { region: RegionCoords::new(i32::MIN, i32::MAX), column: [5, 5], y: None },
        ] {
            let mut accepted = true;
            region.with_data(|d| accepted = d.move_to_safe(CLIENT, goal, RegionCoords::new(0, 0), None));
            assert!(!accepted, "{goal:?}");
        }
        let order = Order::SetMode { mode: PlayMode::Adventure, at: Some([v, 1, 5]) };
        let mut ok = true;
        region.with_data(|d| ok = d.apply_order_safe(CLIENT, order));
        assert!(!ok);
        let order = Order::SetMode { mode: PlayMode::Fortress, at: Some([5, 1, v]) };
        region.with_data(|d| ok = d.apply_order_safe(CLIENT, order));
        assert!(!ok);
    }
    assert_eq!(before, state_hash(region.data()), "refused orders must change no state");
    assert!(walker(&region, e).is_none());
    // An in-region column with an extreme level is clamped, not a panic.
    region.with_data(|d| {
        d.move_to_safe(CLIENT, here_goal([6, 6], Some(i32::MAX)), RegionCoords::new(0, 0), None);
        d.move_to_safe(CLIENT, here_goal([6, 6], Some(i32::MIN)), RegionCoords::new(0, 0), None);
    });
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

#[test]
fn edge_toward_matches_move_to_plan() {
    // A floor over the whole region, so every edge column is standable.
    let here = RegionCoords::new(0, 0);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), here, None);
    let size = game::REGION_SIZE;
    region.with_data(|r| r.seed_terrain_genesis(floor_chunks(0.0..size, 0.0..size, 16)));
    region.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
    let e = *region.data().player_entites.get(&CLIENT).unwrap();
    region.forget_last_event();
    region
        .handle_event(GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Adventure, at: Some(TILE) }))
        .unwrap();
    region.forget_last_event();
    for _ in 0..5 {
        tick(&mut region);
    }
    let from = game::current_tile(region.data(), feet_of_body(&region, e)).expect("standing");
    for goal in [
        Goal { region: RegionCoords::new(1, 0), column: [5, 30], y: None },
        Goal { region: RegionCoords::new(0, -1), column: [40, 20], y: None },
        Goal { region: RegionCoords::new(-1, 1), column: [60, 3], y: None },
    ] {
        let edge = edge_toward(region.data(), from, &goal, here).expect("an edge tile");
        move_to(&mut region, goal);
        let w = walker(&region, e).expect("MoveTo must start a walk");
        assert_eq!(w.path.first(), Some(&from), "{goal:?}");
        assert_eq!(w.path.last(), Some(&edge), "the sim walks to edge_toward's tile for {goal:?}");
    }
}

fn feet_of_body(region: &Region, e: EntityKey) -> Vector {
    let data = region.data();
    let handle = data.ecs.rigidbody.try_get(e).unwrap();
    feet_of(data.physics.bodies.get(handle).unwrap().translation())
}

// --- The world's outer seam -------------------------------------------------

/// The world is region (0, 0) alone: every side is its outer seam.
fn one_region_world() -> game::MapExtent {
    game::MapExtent { origin: RegionCoords::new(0, 0), width: 1, height: 1 }
}

/// Region (0, 0) floored edge to edge, bounded by `extent`, its player in
/// Adventure on [`TILE`].
fn floored_adventurer(extent: Option<game::MapExtent>) -> (Region, EntityKey) {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), RegionCoords::new(0, 0), None);
    region.set_map_extent(extent);
    let size = game::REGION_SIZE;
    region.with_data(|r| r.seed_terrain_genesis(floor_chunks(0.0..size, 0.0..size, 16)));
    region.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
    region.forget_last_event();
    let e = *region.data().player_entites.get(&CLIENT).unwrap();
    region
        .handle_event(GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Adventure, at: Some(TILE) }))
        .unwrap();
    region.forget_last_event();
    for _ in 0..5 {
        tick(&mut region);
    }
    (region, e)
}

/// Ticks `n` times; the departures the region's boundary scan produced.
fn departures_over(region: &mut Region, n: u32) -> usize {
    (0..n)
        .map(|_| {
            tick(region);
            region.take_transfers().0.len()
        })
        .sum()
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walker_never_leaves_the_map() {
    let n = REGION_BLOCKS as i32;
    let goal = Goal { region: RegionCoords::new(1, 0), column: [5, TILE[2]], y: None };
    let (mut region, e) = floored_adventurer(Some(one_region_world()));
    move_to(&mut region, goal);
    let w = walker(&region, e).expect("MoveTo off the map still walks to the edge");
    assert_eq!(w.exit, None, "no exit across the outer seam");
    assert_eq!(w.path.last(), Some(&[n - 1, 1, TILE[2]]), "the last tile inside the map");
    let mut departed = 0;
    for _ in 0..walk_budget((n - TILE[0]) as f32) {
        tick(&mut region);
        departed += region.take_transfers().0.len();
        if walker(&region, e).is_none() {
            break;
        }
    }
    assert!(walker(&region, e).is_none(), "the walk ends at the edge, body on {:?}", body_tile(&region, e));
    departed += departures_over(&mut region, 20);
    assert_eq!(departed, 0, "nothing hands off out of the world");
    assert_eq!(body_tile(&region, e), [n - 1, 1, TILE[2]], "the walk ends on the last tile inside the map");

    // Unbounded (procedural), the same order walks out across the seam.
    let (mut region, e) = floored_adventurer(None);
    move_to(&mut region, goal);
    assert_eq!(walker(&region, e).and_then(|w| w.exit), Some([1, 0]));
    assert!(departures_over(&mut region, walk_budget((n - TILE[0]) as f32 + 2.0)) > 0, "the walker leaves the region");
}

#[test]
fn body_is_held_at_the_outer_seam() {
    let size = game::REGION_SIZE;
    let push_out = |region: &mut Region, e: EntityKey| {
        region.with_data(|d| {
            let handle = d.ecs.rigidbody.try_get(e).unwrap();
            let mut pose = *d.physics.bodies.get(handle).unwrap().position();
            pose.translation.x = size + 3.0 * game::FLIP_HYSTERESIS;
            d.set_body_pose_safe(e, pose);
        });
    };
    let (mut region, e) = floored_adventurer(Some(one_region_world()));
    push_out(&mut region, e);
    let before = state_hash(region.data());
    region.handle_event(GameEventKind::Tick).unwrap();
    assert!(region.take_transfers().0.is_empty(), "a body past the outer seam must not depart");
    let x = feet_of_body(&region, e).x;
    assert!(x <= size, "the body is put back on the seam, x = {x}");
    assert!(region.data().player_entites.contains_key(&CLIENT));
    region.rollback_last_event();
    assert_eq!(before, state_hash(region.data()), "the hold rolls back bit-exact");

    // Unbounded, the same body hands off east.
    let (mut region, e) = floored_adventurer(None);
    push_out(&mut region, e);
    tick(&mut region);
    let (departures, _) = region.take_transfers();
    assert_eq!(departures.iter().map(|(_, to)| *to).collect::<Vec<_>>(), vec![RegionCoords::new(1, 0)]);
}
