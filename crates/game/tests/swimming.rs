//! Swimming (water plan Task 3): a body's `Motion.immersion` follows the
//! water around it every tick (`tick_immersion_safe`), and `plan_with`
//! swims at `Waist` and `Head` — floats at the surface, dives and surfaces,
//! caps its speed, climbs out onto a one-block ledge — and wades at `Feet`.
//! Every fixture is a hand-built region with recipe water
//! (`common::water::set_recipe_water`), ticked through the real region tick.
use crate::common::water::set_recipe_water;
use crate::common::{self, body, key, r0, teleport, ticks, CHUNK_EDGE};
use game::history::PersonId;
use game::movement::{
    plan_with, settle, Immersion, MoveKeys, MoveParams, PLAYER_HEIGHT, SWIM_SPEED, TICK_DT, UNITS_PER_METER,
    WADE_SPEED_FACTOR, WALK_SPEED,
};
use game::rapier3d::math::Vector;
use game::{Block, BlockId, Chunk, GameEventKind, Goal, Key, Motion, World, WorldBlockPos, BLOCK_VOXELS};

const STONE: BlockId = BlockId(1);
/// Block level of the ground's top (blocks `0..BANK` are solid): a pool dug
/// into it and filled to the brim has its surface here.
const BANK: i32 = 5;
const BV: f32 = BLOCK_VOXELS as f32;
/// Half the body's height in units: the body origin is the capsule centre.
const HALF_HEIGHT: f32 = PLAYER_HEIGHT * UNITS_PER_METER / 2.0;

/// 2×2 chunks of ground `BANK` blocks deep, the player settled on it at the
/// spawn column (block 8, 8).
fn bank_world() -> World {
    common::install_real_block_props();
    let depth = BANK as u32 * BLOCK_VOXELS as u32;
    common::world_of(0.0..2.0 * CHUNK_EDGE, 0.0..2.0 * CHUNK_EDGE, |_, _| Chunk::flat_floor(depth))
}

/// Sets every block of the box `x × y × z` (block ranges) to `block`.
fn fill(world: &mut World, xs: std::ops::Range<i32>, ys: std::ops::Range<i32>, zs: std::ops::Range<i32>, block: BlockId) {
    world.regions.get_mut(&r0()).unwrap().with_data(|d| {
        for x in xs.clone() {
            for y in ys.clone() {
                for z in zs.clone() {
                    assert!(d.set_block_safe([x, y, z], Block::new(block)), "set {x},{y},{z}");
                }
            }
        }
    });
}

/// A pool dug `BANK - 1` (4) blocks deep over the block box `xs × zs`,
/// filled with recipe water to the brim: water at levels `1..BANK`, the
/// bed's top at level 1.
fn dig_pool(world: &mut World, xs: std::ops::Range<i32>, zs: std::ops::Range<i32>) {
    world.regions.get_mut(&r0()).unwrap().with_data(|d| set_recipe_water(d, xs, zs, 0, BANK - 1, BANK - 1));
}

fn player_motion(world: &World) -> Motion {
    let d = world.data(&r0());
    let e = *d.player_entites.get(&0).unwrap();
    d.ecs.motion.try_get(e).unwrap_or_default()
}

fn feet_y(world: &World) -> f32 {
    body(world).y - HALF_HEIGHT
}

/// Ticks until the body has floated at `Waist` with `|vel_y| < 0.1` for 40
/// consecutive ticks; panics after `budget` ticks.
fn float_for_40_ticks(world: &mut World, budget: usize) {
    let mut run = 0;
    for t in 0..budget {
        ticks(world, 1);
        let m = player_motion(world);
        if m.immersion == Immersion::Waist && m.vel_y.abs() < 0.1 {
            run += 1;
            if run >= 40 {
                return;
            }
        } else {
            run = 0;
        }
        if t % 10 == 0 {
            eprintln!("tick {t}: feet {:.3} blocks {:?} vel_y {:.2}", feet_y(world) / BV, m.immersion, m.vel_y);
        }
    }
    let m = player_motion(world);
    panic!(
        "never floated at Waist for 40 ticks in {budget}: feet {:.3} blocks, {:?}, vel_y {}",
        feet_y(world) / BV,
        m.immersion,
        m.vel_y
    );
}

/// The player dropped into the middle of a 4-deep pool.
fn floating_player() -> World {
    let mut w = bank_world();
    dig_pool(&mut w, 3..14, 3..14);
    float_for_40_ticks(&mut w, 300);
    w
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn body_in_deep_water_floats_at_the_surface() {
    let w = floating_player();
    let feet = feet_y(&w) / BV;
    assert!(feet > 1.0 + 0.5, "floats clear of the bed: feet at {feet} blocks");
    assert!(feet < BANK as f32, "floats in, not on, the water: feet at {feet} blocks");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn holding_descend_dives_and_jump_surfaces() {
    let mut w = floating_player();
    key(&mut w, Key::ShiftLeft, true);
    let mut head = false;
    for _ in 0..20 {
        ticks(&mut w, 1);
        head |= player_motion(&w).immersion == Immersion::Head;
    }
    key(&mut w, Key::ShiftLeft, false);
    assert!(head, "descend dives the head under: {:?}", player_motion(&w));
    let dived = feet_y(&w);
    key(&mut w, Key::Space, true);
    let mut surfaced = None;
    for t in 0..40 {
        ticks(&mut w, 1);
        if player_motion(&w).immersion == Immersion::Waist {
            surfaced = Some(t);
            break;
        }
    }
    key(&mut w, Key::Space, false);
    assert!(surfaced.is_some(), "jump surfaces to Waist: {:?}, feet {} -> {}", player_motion(&w), dived / BV, feet_y(&w) / BV);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn swim_speed_is_capped() {
    let mut w = floating_player();
    let b0 = body(&w);
    key(&mut w, Key::KeyW, true);
    for _ in 0..20 {
        ticks(&mut w, 1);
        assert!(player_motion(&w).immersion >= Immersion::Waist, "still swimming");
    }
    let b1 = body(&w);
    let dist = ((b1.x - b0.x).powi(2) + (b1.z - b0.z).powi(2)).sqrt() / UNITS_PER_METER;
    let speed = dist / (20.0 * TICK_DT);
    eprintln!("swim speed {speed} m/s");
    assert!(speed <= SWIM_SPEED + 1e-3, "swim speed {speed} m/s is capped at {SWIM_SPEED}");
    assert!(speed >= 0.9 * SWIM_SPEED, "the body does swim: {speed} m/s");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn swimmer_climbs_out_onto_a_one_block_ledge() {
    let mut w = bank_world();
    dig_pool(&mut w, 3..14, 3..14);
    // North of the pool (z < 3) the bank is raised one block: its top is one
    // block above the water's surface.
    fill(&mut w, 0..32, BANK..BANK + 1, 0..3, STONE);
    float_for_40_ticks(&mut w, 300);
    // Two blocks from the wall, floating where it was.
    let b = body(&w);
    teleport(&mut w, b.x, b.y, 5.5 * BV);
    key(&mut w, Key::KeyW, true);
    key(&mut w, Key::Space, true);
    let mut out = None;
    for t in 0..40 {
        ticks(&mut w, 1);
        let b = body(&w);
        if t % 4 == 0 {
            eprintln!("tick {t}: feet {:.3} z {:.3} blocks {:?}", feet_y(&w) / BV, b.z / BV, player_motion(&w));
        }
        if b.grounded && b.z < 3.0 * BV && feet_y(&w) > (BANK + 1) as f32 * BV - 1.0 {
            out = Some(t);
            break;
        }
    }
    assert!(out.is_some(), "on the ledge within 40 ticks: feet {} z {} blocks", feet_y(&w) / BV, body(&w).z / BV);
}

/// With static water every water cell is full, so a body of the player's
/// height standing in one block of it has its waist under too: `Feet` alone
/// takes a body two blocks tall (or, once water flows, a shallow cell). The
/// sampling is checked on the real region state; the wading speed through
/// `plan_with` + `settle`, as every mover runs them.
/// The climb-out is a surface move: head under, jump against a wall only
/// swims up towards `SWIM_UP_SPEED` (it shot the body up at the exit speed
/// before); at the waist the same keys climb out.
#[test]
fn climb_out_only_at_the_surface() {
    let keys = MoveKeys { jump_held: true, forward: true, ..MoveKeys::default() };
    let max_dv = game::movement::SWIM_VERTICAL_ACCEL * TICK_DT;
    let mut m = Motion { immersion: Immersion::Head, blocked_horizontally: true, ..Motion::default() };
    plan_with(&mut m, &keys, 0.0, 0, &MoveParams::PLAYER);
    let want = game::movement::SWIM_UP_SPEED.min(max_dv) * UNITS_PER_METER;
    assert!((m.vel_y - want).abs() <= 1e-3, "head under: {} vs {want}", m.vel_y);
    for _ in 0..20 {
        plan_with(&mut m, &keys, 0.0, 0, &MoveParams::PLAYER);
    }
    let up = game::movement::SWIM_UP_SPEED * UNITS_PER_METER;
    assert!((m.vel_y - up).abs() <= 1e-3, "head under settles at SWIM_UP_SPEED: {} vs {up}", m.vel_y);

    let mut m = Motion { immersion: Immersion::Waist, blocked_horizontally: true, ..Motion::default() };
    plan_with(&mut m, &keys, 0.0, 0, &MoveParams::PLAYER);
    let exit = game::movement::SWIM_EXIT_SPEED * UNITS_PER_METER;
    assert_eq!(m.vel_y, exit, "at the surface: climbs out");
}

#[test]
fn wading_slows_walking() {
    let mut w = bank_world();
    // One block of recipe water on the bank.
    w.regions.get_mut(&r0()).unwrap().with_data(|d| set_recipe_water(d, 7..10, 7..10, BANK - 1, BANK, BANK - 1));
    let d = w.data(&r0());
    let feet = Vector::new(8.5 * BV, BANK as f32 * BV, 8.5 * BV);
    assert_eq!(game::immersion_at(d, feet, 2.08 * UNITS_PER_METER), Immersion::Feet, "a 2 m body wades");
    assert_eq!(
        game::immersion_at(d, feet, PLAYER_HEIGHT * UNITS_PER_METER),
        Immersion::Waist,
        "a 1.8 m body's waist is in the same full cell"
    );
    let above = Vector::new(feet.x, (BANK + 1) as f32 * BV, feet.z);
    assert_eq!(game::immersion_at(d, above, PLAYER_HEIGHT * UNITS_PER_METER), Immersion::Dry);

    let mut m = Motion { grounded: true, immersion: Immersion::Feet, ..Motion::default() };
    let keys = MoveKeys { forward: true, ..MoveKeys::default() };
    let mut travelled = 0.0;
    for t in 0..20 {
        let desired = plan_with(&mut m, &keys, 0.0, t, &MoveParams::PLAYER);
        let moved = Vector::new(desired.x, 0.0, desired.z);
        settle(&mut m, &desired, &moved, true);
        travelled += (moved.x * moved.x + moved.z * moved.z).sqrt();
    }
    let speed = travelled / UNITS_PER_METER / (20.0 * TICK_DT);
    let want = WALK_SPEED * WADE_SPEED_FACTOR;
    assert!((speed - want).abs() <= 1e-3, "wading speed {speed} vs {want}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn rollback_across_water_surface_restores_hash() {
    common::install_real_block_props();
    // A region with an update channel (undo emits compensating updates):
    // the bank, the player settled on it at block (8, 8), and a pool from
    // a block north of it, so walking north steps off into the water.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = game::Region::new(game::Rollback::new(Some(send.clone())), Some(send), r0(), None);
    let depth = BANK as u32 * BLOCK_VOXELS as u32;
    region.with_data(|r| r.seed_terrain_genesis(common::floor_chunks(0.0..2.0 * CHUNK_EDGE, 0.0..2.0 * CHUNK_EDGE, depth)));
    let forget = |region: &mut game::Region, kind: GameEventKind| {
        region.handle_event(kind).unwrap();
        region.forget_last_event();
    };
    forget(&mut region, GameEventKind::CreateClient(0));
    // A new client is in its body, where E opens the inventory: start from the overview.
    forget(&mut region, GameEventKind::Order(0, game::Order::SetMode { mode: game::PlayMode::Overview, at: None }));
    let input = |key, pressed| GameEventKind::PlayerInput(0, game::InputEvent::Key { key, pressed });
    forget(&mut region, input(Key::KeyE, true));
    forget(&mut region, input(Key::KeyE, false));
    for _ in 0..12 {
        forget(&mut region, GameEventKind::Tick);
    }
    region.with_data(|d| set_recipe_water(d, 3..14, 3..8, 0, BANK - 1, BANK - 1));
    forget(&mut region, input(Key::KeyW, true));

    let before = game::state_hash(region.data());
    let mut seen = Immersion::Dry;
    for _ in 0..30 {
        region.handle_event(GameEventKind::Tick).unwrap();
        let d = region.data();
        let e = *d.player_entites.get(&0).unwrap();
        seen = seen.max(d.ecs.motion.try_get(e).unwrap_or_default().immersion);
    }
    assert!(seen >= Immersion::Waist, "the walk reached the water: {seen:?}");
    for _ in 0..30 {
        region.rollback_last_event();
    }
    assert_eq!(before, game::state_hash(region.data()), "30 ticks across the surface undo bit-exact");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn people_swim_too() {
    let mut w = bank_world();
    // A channel 3 blocks wide (z 10..13), 4 deep, across the whole fixture.
    dig_pool(&mut w, 0..32, 10..13);
    const P: PersonId = PersonId(7);
    let mut e = None;
    w.regions.get_mut(&r0()).unwrap().with_data(|d| e = d.spawn_person_safe(P, (8.5 * BV, 8.5 * BV)));
    let e = e.expect("spawned");
    ticks(&mut w, 10);
    // Deep water is not standable, so no path plans across it: the walk
    // goes straight over the channel, as a path through swimmable water
    // would once pathing swims.
    let path: Vec<WorldBlockPos> = (8..=16).map(|z| WorldBlockPos::new(8, BANK, z)).collect();
    let walker = game::Walker {
        goal: Goal { column: [8, 16], y: Some(BANK) },
        walk_up: vec![false; path.len()],
        path,
        next: 1,
        to_goal: true,
        ..Default::default()
    };
    w.regions.get_mut(&r0()).unwrap().with_data(|d| d.data.ecs.walker.set_safe(e, Some(walker)));
    let mut deepest = Immersion::Dry;
    let mut lowest = f32::MAX;
    let mut entered = false;
    for t in 0..120 {
        ticks(&mut w, 1);
        let d = w.data(&r0());
        let m = d.ecs.motion.try_get(e).unwrap_or_default();
        let b = common::entity_body(d, e);
        let over_channel = (10.0 * BV..13.0 * BV).contains(&b.z);
        entered |= over_channel;
        if over_channel {
            lowest = lowest.min(b.y - HALF_HEIGHT);
        }
        deepest = deepest.max(m.immersion);
        if t % 10 == 0 {
            eprintln!("tick {t}: feet {:.3} z {:.3} blocks {:?}", (b.y - HALF_HEIGHT) / BV, b.z / BV, m);
        }
    }
    eprintln!("lowest feet over the channel {} blocks, deepest {deepest:?}", lowest / BV);
    assert!(entered, "the person walked into the channel");
    assert!(deepest >= Immersion::Waist, "its immersion reached Waist: {deepest:?}");
    // Clear of the bed (top at level 1) by a quarter block, as in the seam
    // test (`handoff_schedule::swim_is_seam_invariant`).
    assert!(lowest > 1.25 * BV, "it never sank to the bed (top at level 1): lowest feet {} blocks", lowest / BV);
}
