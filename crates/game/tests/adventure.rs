//! Per-client play mode (`game::adventure`): the body by default, switched by
//! `Order::SetMode`, which puts the adventurer's body on a tile; the switch
//! undoes bit-exact and reaches the UI once. `Order::MoveTo` walks the body
//! along an A* path (`Walker`, planned by `PathPlan`) to a world goal any
//! number of regions away, re-planning once when stuck.
use crate::common;

use common::floor_chunks;
use game::rapier3d::math::Vector;
use game::{
    feet_of, feet_tile, line_step, next_region, PathPlan, RouteMap, WorldBlockPos, PORTAL_DEPTH_BLOCKS, play_mode_update, slope_chisel, state_hash, steer, Block, BlockId, BlockIndex, EntityKey,
    GameEventKind, InputEvent, Key, Goal, Motion, Order, PlayMode, Region, RegionCoords, Rollback, Side, SlopeShape, TilePos,
    TileSource, UiUpdate, Walker, BLOCK_VOXELS, CHUNK_BLOCKS, REGION_BLOCKS, STUCK_TICKS, TICK_RATE, WALK_UP_JUMP_AFTER_STALL_TICKS,
};

const CLIENT: game::ClientId = 7;
/// The tile the adventurer is put on: the air above the one-block floor.
const TILE: TilePos = [10, 1, 12];

/// One region with a flat one-block floor around the spawn and one player
/// settled on it, with a render channel (rollback needs one).
fn region_with_player() -> (Region, EntityKey) {
    region_with_player_on(&[])
}

/// [`region_with_player`] with `cells` built into the terrain before genesis:
/// each a [`WALL`] block, chiselled into the slope when one is given.
fn region_with_player_on(cells: &[(TilePos, Option<SlopeShape>)]) -> (Region, EntityKey) {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), RegionCoords::new(0, 0), None);
    let (x, z) = region.data().spawn_xz();
    let edge = common::CHUNK_EDGE;
    let mut chunks = floor_chunks(x - edge..x + edge, z - edge..z + edge, 16);
    let cb = CHUNK_BLOCKS as i32;
    for &(p, slope) in cells {
        let (_, chunk) = chunks
            .iter_mut()
            .find(|(c, _)| [c.x as i32, c.y as i32, c.z as i32] == [p[0].div_euclid(cb), p[1].div_euclid(cb), p[2].div_euclid(cb)])
            .unwrap_or_else(|| panic!("no floor chunk under {p:?}"));
        let l = p.map(|v| v.rem_euclid(cb) as usize);
        chunk.set_block(l[0], l[1], l[2], Block::new(WALL));
        if let Some(shape) = slope {
            chunk.chisel.insert(BlockIndex::from_xyz(l[0], l[1], l[2]), slope_chisel(WALL, shape));
        }
    }
    region.with_data(|r| r.seed_terrain_genesis(chunks));
    region.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
    // A new client is in its body; these tests start from the overview (no walk, flight).
    region.with_data(|r| r.clients.get_mut(&CLIENT).unwrap().mode = PlayMode::Overview);
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
    region.with_data(|r| r.clients.get_mut(&CLIENT).unwrap().mode = PlayMode::Overview);
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
fn new_client_is_in_a_body() {
    let mut region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
    region.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
    assert_eq!(mode(&region), PlayMode::Adventure);
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
    assert_eq!(mode(&region), PlayMode::Overview);
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
    // The default is the body, so the helper's overview is news and the body is not.
    let mut prev = PlayMode::default();
    assert_eq!(prev, PlayMode::Adventure);
    assert_eq!(
        play_mode_update(&mut prev, region.data(), CLIENT),
        Some(UiUpdate::PlayMode(PlayMode::Overview))
    );
    tick(&mut region);
    assert_eq!(play_mode_update(&mut prev, region.data(), CLIENT), None, "sent only once");
    region.handle_event(GameEventKind::Order(CLIENT, enter_adventure())).unwrap();
    region.forget_last_event();
    assert_eq!(
        play_mode_update(&mut prev, region.data(), CLIENT),
        Some(UiUpdate::PlayMode(PlayMode::Adventure))
    );
    let mut prev = PlayMode::default();
    assert_eq!(play_mode_update(&mut prev, region.data(), CLIENT), None, "Adventure (the default) is not news");
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
    adventure_on(at, &[])
}

/// [`adventure_at`] on terrain with `cells` built in ([`region_with_player_on`]).
fn adventure_on(at: TilePos, cells: &[(TilePos, Option<SlopeShape>)]) -> (Region, EntityKey) {
    let (mut region, e) = region_with_player_on(cells);
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
    Goal::local(RegionCoords::new(0, 0), column, y)
}

/// Tile `p` of region (0, 0) as a world tile.
fn world(p: TilePos) -> WorldBlockPos {
    WorldBlockPos::new(p[0] as i64, p[1], p[2] as i64)
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
fn first_person_ends_the_walk_and_the_keyboard_drives() {
    let (mut region, e) = adventure_at([4, 1, 4]);
    move_to(&mut region, here_goal([20, 20], None));
    tick(&mut region);
    assert!(walker(&region, e).is_some());
    let input = |region: &mut Region, ev: InputEvent| {
        region.handle_event(GameEventKind::PlayerInput(CLIENT, ev)).unwrap();
        region.forget_last_event();
    };
    input(&mut region, InputEvent::FpsCam(true));
    tick(&mut region);
    assert!(walker(&region, e).is_none(), "first person drops the walk");
    let start = body_tile(&region, e);
    input(&mut region, InputEvent::Key { key: Key::KeyW, pressed: true });
    for _ in 0..walk_budget(3.0) {
        tick(&mut region);
    }
    assert_ne!(body_tile(&region, e), start, "W walks the body in first person");
}

#[test]
fn move_to_refused_in_overview() {
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

/// A lone region has no halo: nothing of the next region of the route is
/// known, so a goal beyond it has no way on.
#[test]
fn move_to_far_goal_with_no_way_on_changes_nothing() {
    let (mut region, e) = adventure_at([4, 1, 4]);
    let before = state_hash(region.data());
    let mut accepted = true;
    let goal = Goal::local(RegionCoords::new(2, 0), [4, 4], None);
    region.with_data(|d| accepted = d.move_to_safe(CLIENT, goal, RegionCoords::new(0, 0), None));
    assert!(!accepted);
    assert!(walker(&region, e).is_none());
    assert_eq!(before, state_hash(region.data()));
}

#[test]
fn extreme_untrusted_coordinates_are_refused_without_panic() {
    let (mut region, e) = adventure_at([4, 1, 4]);
    let before = state_hash(region.data());
    for v in [i32::MAX, i32::MIN, -1, REGION_BLOCKS as i32] {
        for goal in [
            here_goal([v, 5], None),
            here_goal([5, v], Some(1)),
            Goal::local(RegionCoords::new(i32::MIN, i32::MAX), [5, 5], None),
            Goal { column: [i64::MIN, i64::MAX], y: None },
            Goal { column: [i64::MAX, 5], y: Some(i32::MIN) },
        ] {
            let mut accepted = true;
            region.with_data(|d| accepted = d.move_to_safe(CLIENT, goal, RegionCoords::new(0, 0), None));
            assert!(!accepted, "{goal:?}");
        }
        let order = Order::SetMode { mode: PlayMode::Adventure, at: Some([v, 1, 5]) };
        let mut ok = true;
        region.with_data(|d| ok = d.apply_order_safe(CLIENT, order));
        assert!(!ok);
        let order = Order::SetMode { mode: PlayMode::Overview, at: Some([5, 1, v]) };
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

/// Walks until the walker is gone (like [`walk_until_arrived`]); the highest
/// `Motion::vel_y` seen. Walking (autostep included) never makes it positive:
/// only a jump does (it leaves the first tick at `JUMP_SPEED` less one tick
/// of gravity).
fn walk_max_vel_y(region: &mut Region, e: EntityKey, max: u32) -> f32 {
    let mut most = f32::MIN;
    for _ in 0..max {
        tick(region);
        most = most.max(motion_of(region, e).vel_y);
        if walker(region, e).is_none() {
            return most;
        }
    }
    panic!("still walking after {max} ticks, body on {:?}", body_tile(region, e));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walker_walks_up_a_slope_without_jumping() {
    // A one-block platform (x 13..24) fronted by a row of slopes at x = 12,
    // low side toward the start.
    let slopes = (6..15).map(|z| ([12, 1, z], Some(SlopeShape::Straight { low: Side::NegX })));
    let platform = (13..24).flat_map(|x| (6..15).map(move |z| ([x, 1, z], None)));
    let cells: Vec<_> = slopes.chain(platform).collect();
    let (mut region, e) = adventure_on([4, 1, 10], &cells);
    move_to(&mut region, here_goal([20, 10], None));
    let w = walker(&region, e).expect("MoveTo must start a walk");
    assert_eq!(w.walk_up.len(), w.path.len());
    assert!(w.walk_up.contains(&true), "the path walks up the slope: {:?}", w.path);
    let vel_y = walk_max_vel_y(&mut region, e, walk_budget(16.0) + 20);
    assert!(vel_y <= 0.0, "the body jumped (vel_y {vel_y})");
    assert_eq!(body_tile(&region, e), [20, 2, 10], "the body must end on the upper level");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walker_jumps_onto_a_slope_from_its_side() {
    // One slope whose low side (and both its diagonals) is walled off 2 high:
    // the only way up is a lateral jump climb.
    let slope = ([12, 1, 10], Some(SlopeShape::Straight { low: Side::NegX }));
    let walls = (9..=11).flat_map(|z| [1, 2].map(|y| ([11, y, z], None)));
    let cells: Vec<_> = std::iter::once(slope).chain(walls).collect();
    let (mut region, e) = adventure_on([12, 1, 4], &cells);
    move_to(&mut region, here_goal([12, 10], Some(2)));
    let w = walker(&region, e).expect("MoveTo must start a walk");
    assert!(!w.walk_up.contains(&true), "no walk-up from the side: {:?}", w.path);
    let vel_y = walk_max_vel_y(&mut region, e, walk_budget(8.0) + 20);
    assert_eq!(body_tile(&region, e), [12, 2, 10], "the body must end on the slope");
    assert!(vel_y > 0.0, "the climb from the side is a jump");
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
fn overview_ends_the_walk() {
    let (mut region, e) = adventure_at([4, 1, 4]);
    move_to(&mut region, here_goal([20, 20], None));
    assert!(walker(&region, e).is_some());
    region
        .handle_event(GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Overview, at: None }))
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
        path: vec![world([4, 1, 4]), world(next)],
        next: 1,
        ..Walker::default()
    };
    let here = RegionCoords::new(0, 0);
    let steer = |w: &Walker, feet, grounded| steer(w, feet, grounded, here);
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
    // No jump onto a slope walked up from its low side.
    let up = Walker { walk_up: vec![false, true], ..walk([5, 2, 4]) };
    assert!(!steer(&up, feet, true).0.jump_held);
}

#[test]
fn steer_jumps_a_stalled_walk_up() {
    let bv = BLOCK_VOXELS as f32;
    let feet = Vector::new(4.5 * bv, bv, 4.5 * bv);
    let here = RegionCoords::new(0, 0);
    let steer = |w: &Walker, feet, grounded| steer(w, feet, grounded, here);
    let up = |next: TilePos, stalled: u32| Walker {
        path: vec![world([4, 1, 4]), world(next)],
        walk_up: vec![false, true],
        next: 1,
        stalled,
        ..Walker::default()
    };
    assert!(WALK_UP_JUMP_AFTER_STALL_TICKS > 0 && WALK_UP_JUMP_AFTER_STALL_TICKS < STUCK_TICKS);
    assert!(!steer(&up([5, 2, 4], 0), feet, true).0.jump_held);
    assert!(!steer(&up([5, 2, 4], WALK_UP_JUMP_AFTER_STALL_TICKS - 1), feet, true).0.jump_held);
    for stalled in [WALK_UP_JUMP_AFTER_STALL_TICKS, STUCK_TICKS] {
        assert!(steer(&up([5, 2, 4], stalled), feet, true).0.jump_held, "stalled {stalled}");
        // Still only on a climb, and only when grounded.
        assert!(!steer(&up([5, 1, 4], stalled), feet, true).0.jump_held);
        assert!(!steer(&up([5, 2, 4], stalled), feet, false).0.jump_held);
    }
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

/// Rounds for the walk of [`move_to_two_regions_away`] (~70 blocks at walk
/// speed is ~330 rounds).
const TWO_REGIONS_ROUNDS: usize = 600;

/// A `MoveTo` goal two regions east, over a floor everywhere: the walk
/// crosses into the next region of the route, walks on toward the goal from
/// there, and ends on the goal's tile.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn move_to_two_regions_away() {
    use common::rig::Rig;
    use common::seam_translate::{spawn_tile, uniform_floor, STAND_LEVEL};
    let n = REGION_BLOCKS as i64;
    let mut rig = Rig::with_floor(None, uniform_floor);
    rig.join_spawn();
    let start = WorldBlockPos::new(n - 3, STAND_LEVEL, 12);
    rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some(spawn_tile(start)) });
    for _ in 0..6 {
        rig.round();
    }
    let far = RegionCoords::new(2, 0);
    let goal = WorldBlockPos::new(2 * n + 3, STAND_LEVEL, 12);
    rig.order(Order::MoveTo(Goal { column: [goal.x, goal.z], y: None }));
    let mut owners = std::collections::BTreeSet::new();
    for _ in 0..TWO_REGIONS_ROUNDS {
        rig.round();
        let now = rig.owners();
        owners.extend(now.iter().copied());
        if now == [far] && rig.walk_state(far).0.is_none() {
            break;
        }
    }
    assert!(owners.contains(&RegionCoords::new(1, 0)), "walked through the region between: {owners:?}");
    assert_eq!(rig.owners(), [far], "the walk ends in the goal's region");
    let (walker, tile) = rig.walk_state(far);
    assert!(walker.is_none(), "still walking: {walker:?}");
    let (_, local) = goal.to_local();
    assert_eq!(tile, [local.x, local.y, local.z], "the walk ends on the goal tile");
}

#[test]
fn next_region_steps_along_the_line() {
    let r = RegionCoords::new;
    assert_eq!(next_region(r(0, 0), r(0, 0), None), r(0, 0));
    assert_eq!(next_region(r(0, 0), r(3, 0), None), r(1, 0));
    assert_eq!(next_region(r(0, 0), r(-2, -2), None), r(-1, -1));
    // Off the diagonal, the step nearest the straight line.
    assert_eq!(next_region(r(0, 0), r(5, 1), None), r(1, 0));
    assert_eq!(next_region(r(0, 0), r(3, 2), None), r(1, 1));
    assert_eq!(next_region(r(0, 0), r(-1, 4), None), r(0, 1));
    // The whole route: one Chebyshev step at a time, never past the box.
    let (mut at, to) = (r(-3, 7), r(4, 2));
    let mut steps = 0;
    while at != to {
        let n = next_region(at, to, None);
        assert!((n.x - at.x).abs() <= 1 && (n.z - at.z).abs() <= 1 && n != at);
        assert!((-3..=4).contains(&n.x) && (2..=7).contains(&n.z), "{n:?} left the box");
        at = n;
        steps += 1;
    }
    assert_eq!(steps, 7);
    // Extremes do not overflow.
    assert_eq!(next_region(r(i32::MIN, i32::MAX), r(i32::MAX, i32::MIN), None), r(i32::MIN + 1, i32::MAX - 1));
}

/// The coarse route over a world map walks around the regions it marks
/// (water, mountain walls), and keeps to the straight line where nothing is
/// in the way or no way around exists.
#[test]
fn next_region_routes_around_impassable_regions() {
    let r = RegionCoords::new;
    let extent = game::MapExtent { origin: r(0, 0), width: 5, height: 5 };
    // A wall across x = 2, open only at z = 4.
    let wall = RouteMap::with_blocked(extent, (0..4).map(|z| r(2, z)));
    let (mut at, to) = (r(0, 1), r(4, 1));
    let mut route = vec![at];
    while at != to {
        at = next_region(at, to, Some(&wall));
        route.push(at);
        assert!(route.len() < 20, "no progress: {route:?}");
    }
    assert!(route.iter().all(|&rc| wall.passable(rc)), "through the wall: {route:?}");
    assert!(route.contains(&r(2, 4)), "around the wall's end: {route:?}");
    // Nothing in the way: as many steps as the straight line.
    let open = RouteMap::open(extent);
    let (mut at, mut steps) = (r(0, 1), 0);
    while at != to {
        at = next_region(at, to, Some(&open));
        steps += 1;
    }
    assert_eq!(steps, 4);
    // Walled off completely: the straight line's step.
    let closed = RouteMap::with_blocked(extent, (0..5).map(|z| r(2, z)));
    assert_eq!(next_region(r(0, 1), to, Some(&closed)), line_step(r(0, 1), to));
}

/// The world map's tile summaries mark what a route walks around: mostly
/// water, or relief past `ROUTE_MAX_RELIEF_PER_REGION_M`.
#[test]
fn route_map_from_world_map_marks_water_and_walls() {
    let r = RegionCoords::new;
    let mut map = crate::history_handover::land_map(4, 1);
    map.tiles[1].sea_fraction = 0.4;
    map.tiles[1].lake_fraction = 0.2;
    map.tiles[2].relief_m = game::ROUTE_MAX_RELIEF_PER_REGION_M * REGION_BLOCKS as f32 + 1.0;
    map.tiles[3].sea_fraction = game::ROUTE_MAX_WATER_SHARE;
    let route = RouteMap::from_world_map(&map);
    assert_eq!((0..4).map(|x| route.passable(r(x, 0))).collect::<Vec<_>>(), [true, false, false, true]);
    assert!(!route.passable(r(4, 0)), "off the map");
    assert_eq!(route.extent(), map.extent());
}

/// A walk to a goal behind an impassable region heads into the region the
/// route goes around by, not the one in the way.
#[test]
fn path_plan_crosses_into_the_routes_next_region() {
    let here = RegionCoords::new(0, 0);
    let flat = Flat { holes: vec![] };
    let n = REGION_BLOCKS as i32;
    let extent = game::MapExtent { origin: here, width: 3, height: 2 };
    let map = RouteMap::with_blocked(extent, [RegionCoords::new(1, 0)]);
    let goal = Goal::local(RegionCoords::new(2, 0), [3, 12], None);
    let plan = PathPlan::plan(&flat, [n - 4, 1, n - 4], &goal, here, Some(&map), game::MAX_PATH_NODES).expect("a stretch");
    assert!(!plan.reaches_goal);
    assert_eq!(plan.path.last().unwrap().to_local().0, RegionCoords::new(1, 1), "{:?}", plan.path.last());
}

#[test]
fn path_plan_reaches_a_goal_past_the_seam_and_heads_for_one_beyond() {
    let n = REGION_BLOCKS as i32;
    let here = RegionCoords::new(0, 0);
    // `Flat` answers everywhere, like a region whose halo is all floor.
    let flat = Flat { holes: vec![] };
    let from = [n - 4, 1, 12];
    let max = game::MAX_PATH_NODES;
    // Within the halo: the path runs over the seam to the goal itself.
    let east = Goal::local(RegionCoords::new(1, 0), [3, 12], None);
    let plan = PathPlan::plan(&flat, from, &east, here, None, max).expect("a path");
    assert!(plan.reaches_goal);
    assert_eq!(plan.path.first(), Some(&world(from)));
    assert_eq!(plan.path.last(), Some(&world([n + 3, 1, 12])));
    // Two regions away: a stretch ending PORTAL_DEPTH_BLOCKS into the next
    // region of the route, straight toward the goal.
    let far = Goal::local(RegionCoords::new(2, 0), [3, 12], None);
    let plan = PathPlan::plan(&flat, from, &far, here, None, max).expect("a stretch");
    assert!(!plan.reaches_goal);
    assert_eq!(plan.path.last(), Some(&world([n + PORTAL_DEPTH_BLOCKS, 1, 12])));
    // Diagonally two regions away: into the corner region.
    let corner = Goal::local(RegionCoords::new(-2, -2), [3, 3], None);
    let plan = PathPlan::plan(&flat, [2, 1, 2], &corner, here, None, max).expect("a stretch");
    let end = *plan.path.last().unwrap();
    assert_eq!(end.to_local().0, RegionCoords::new(-1, -1));
    assert!(end.x < -(PORTAL_DEPTH_BLOCKS as i64) && end.z < -(PORTAL_DEPTH_BLOCKS as i64), "{end:?}");
    // Off a one-region map: the goal moves onto the map's edge column.
    let map = RouteMap::open(game::MapExtent { origin: here, width: 1, height: 1 });
    let plan = PathPlan::plan(&flat, from, &far, here, Some(&map), max).expect("a path");
    assert!(plan.reaches_goal);
    assert_eq!(plan.path.last(), Some(&world([n - 1, 1, 12])));
    assert!(plan.path.iter().all(|p| p.to_local().0 == here), "never off the map");
    // A goal in this region with no path: nothing, even with a way out.
    let holed = Flat { holes: vec![[20, 20]] };
    assert_eq!(PathPlan::plan(&holed, [4, 1, 4], &here_goal([20, 20], None), here, None, max), None);
}

/// A floor everywhere but the quarter-plane `x < 0, z < 0` (the corner
/// region of the halo beyond the origin): water, a cliff — no standing.
struct CornerGone;

impl TileSource for CornerGone {
    fn block(&self, p: TilePos) -> BlockId {
        if p[1] == 0 && !(p[0] < 0 && p[2] < 0) { WALL } else { BlockId::AIR }
    }
    fn is_bedrock(&self, _: TilePos) -> bool {
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

/// A diagonal step of the coarse route whose corner patch cannot be stood
/// on falls back to a region beside it (x first): the walk goes on from
/// there instead of being refused.
#[test]
fn path_plan_diagonal_step_falls_back_to_a_side_region() {
    let here = RegionCoords::new(0, 0);
    let corner = Goal::local(RegionCoords::new(-2, -2), [3, 3], None);
    let plan = PathPlan::plan(&CornerGone, [2, 1, 2], &corner, here, None, game::MAX_PATH_NODES).expect("a stretch");
    assert!(!plan.reaches_goal);
    let end = *plan.path.last().unwrap();
    assert_eq!(end.to_local().0, RegionCoords::new(-1, 0), "into the side region, x first: {end:?}");
    assert!(end.x < -(PORTAL_DEPTH_BLOCKS as i64), "{end:?}");
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
    src.with_data(|d| bundle = Some(d.extract_entity_safe(e)));
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
    assert_eq!(bundle.carried.walker.as_ref().map(|w| w.goal), Some(goal), "the bundle carries the walk");
    let bundle_walker = bundle.carried.walker.clone();

    let mut dst = floored_region(RegionCoords::new(0, 0));
    let before = state_hash(dst.data());
    dst.handle_event(GameEventKind::EntityArrived(bundle)).unwrap();
    let e = *dst.data().player_entites.get(&CLIENT).expect("arrived");
    let w = walker(&dst, e).expect("the walk survives the handoff");
    assert_eq!(w.goal, goal);
    assert!(w.to_goal, "the goal is in the receiving region");
    assert_eq!(Some(&w), bundle_walker.as_ref(), "it walks on along the carried path, no re-plan");
    dst.rollback_last_event();
    assert_eq!(before, state_hash(dst.data()), "arrival + re-plan must roll back bit-exact");
}

#[test]
fn arrived_walker_with_no_way_on_is_dropped() {
    let bundle = walking_bundle(here_goal([20, 20], None));
    // Received two regions away from its goal's region, with no halo: its
    // carried path is not walkable there and nothing leads on.
    let mut dst = floored_region(RegionCoords::new(2, 0));
    dst.handle_event(GameEventKind::EntityArrived(bundle)).unwrap();
    let e = *dst.data().player_entites.get(&CLIENT).expect("arrived");
    assert!(walker(&dst, e).is_none(), "a walk with no way on is dropped on arrival");
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
    // Entering the body at a tile places it only from the overview.
    region.with_data(|r| r.clients.get_mut(&CLIENT).unwrap().mode = PlayMode::Overview);
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
    let goal = Goal::local(RegionCoords::new(1, 0), [5, TILE[2]], None);
    let (mut region, e) = floored_adventurer(Some(one_region_world()));
    common::seed_floor_halo(&mut region, RegionCoords::new(0, 0));
    move_to(&mut region, goal);
    let w = walker(&region, e).expect("MoveTo off the map still walks to the edge");
    assert!(w.to_goal, "the goal moves onto the map");
    assert_eq!(w.path.last(), Some(&world([n - 1, 1, TILE[2]])), "the last tile inside the map");
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
    common::seed_floor_halo(&mut region, RegionCoords::new(0, 0));
    move_to(&mut region, goal);
    let w = walker(&region, e).expect("a walk over the seam");
    assert_eq!(w.path.last(), Some(&world([n + 5, 1, TILE[2]])), "to the goal, through the halo");
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

// --- survival controls -----------------------------------------------------

fn send(region: &mut Region, ev: InputEvent) {
    region.handle_event(GameEventKind::PlayerInput(CLIENT, ev)).unwrap();
    region.forget_last_event();
}

fn tap(region: &mut Region, key: Key) {
    send(region, InputEvent::Key { key, pressed: true });
    send(region, InputEvent::Key { key, pressed: false });
    tick(region);
}

fn double_tap_space(region: &mut Region) {
    tap(region, Key::Space);
    tap(region, Key::Space);
}

fn motion_of(region: &Region, e: EntityKey) -> Motion {
    (*region.data().ecs.motion.try_get(e)).unwrap_or_default()
}

#[test]
fn no_flight_in_adventure() {
    let (mut region, e) = adventure_at([4, 1, 4]);
    send(&mut region, InputEvent::FpsCam(true));
    tick(&mut region);
    double_tap_space(&mut region);
    assert!(!motion_of(&region, e).flying, "an adventurer cannot fly");
    for _ in 0..80 {
        tick(&mut region);
    }
    assert!(motion_of(&region, e).grounded, "the body comes back down");
}

#[test]
fn overview_body_still_flies() {
    let (mut region, e) = region_with_player();
    send(&mut region, InputEvent::FpsCam(true));
    tick(&mut region);
    double_tap_space(&mut region);
    assert!(motion_of(&region, e).flying, "an overview body keeps creative flight");
}

#[test]
fn e_toggles_inventory_in_adventure() {
    let (mut region, _e) = adventure_at([4, 1, 4]);
    send(&mut region, InputEvent::FpsCam(true));
    tick(&mut region);
    let open = |r: &Region| r.data().clients.get(&CLIENT).unwrap().inventory_open;
    let fps = |r: &Region| r.data().clients.get(&CLIENT).unwrap().fps_cam_mode;
    tap(&mut region, Key::KeyE);
    assert!(open(&region));
    assert!(fps(&region), "E no longer toggles the camera mode");
    tap(&mut region, Key::KeyE);
    assert!(!open(&region));
    assert!(fps(&region));
}

#[test]
fn closing_inventory_returns_the_cursor_stack() {
    let (mut region, _e) = adventure_at([4, 1, 4]);
    send(&mut region, InputEvent::FpsCam(true));
    tap(&mut region, Key::KeyE);
    let stack = game::ItemStack { item: game::ItemId::block(BlockId(1)), count: 5 };
    region.with_data(|r| r.clients.get_mut(&CLIENT).unwrap().cursor = Some(stack));
    tap(&mut region, Key::KeyE);
    let c = region.data().clients.get(&CLIENT).unwrap();
    assert_eq!(c.cursor, None);
    let total: u32 = c.inventory.slots.iter().flatten().filter(|s| s.item == stack.item).map(|s| s.count).sum();
    assert!(total >= 5);
}

#[test]
fn digits_and_wheel_select_slot() {
    let (mut region, _e) = adventure_at([4, 1, 4]);
    send(&mut region, InputEvent::FpsCam(true));
    let selected = |r: &Region| r.data().clients.get(&CLIENT).unwrap().selected;
    tap(&mut region, Key::Digit3);
    assert_eq!(selected(&region), 2);
    tap(&mut region, Key::Digit1);
    assert_eq!(selected(&region), 0);
    // Wheel up (positive y) = previous slot, wheel down = next (Minecraft).
    send(&mut region, InputEvent::MouseWheel { x: 0.0, y: 1.0 });
    assert_eq!(selected(&region), 8);
    send(&mut region, InputEvent::MouseWheel { x: 0.0, y: -1.0 });
    assert_eq!(selected(&region), 0);
}

#[test]
fn flying_body_entering_adventure_lands() {
    let (mut region, e) = region_with_player();
    send(&mut region, InputEvent::FpsCam(true));
    tick(&mut region);
    double_tap_space(&mut region);
    assert!(motion_of(&region, e).flying);
    region
        .handle_event(GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Adventure, at: Some(TILE) }))
        .unwrap();
    region.forget_last_event();
    for _ in 0..3 {
        tick(&mut region);
    }
    assert!(!motion_of(&region, e).flying, "survival bodies do not keep flight");
}

#[test]
fn wheel_ignored_while_inventory_open() {
    let (mut region, _e) = adventure_at([4, 1, 4]);
    send(&mut region, InputEvent::FpsCam(true));
    tap(&mut region, Key::KeyE);
    send(&mut region, InputEvent::MouseWheel { x: 0.0, y: 1.0 });
    assert_eq!(region.data().clients.get(&CLIENT).unwrap().selected, 0);
}
