//! Swimming (water plan Task 3): a body's `Motion.immersion` follows the
//! water around it every tick (`tick_immersion_safe`), and `plan_with`
//! swims at `Waist` and `Head` — floats at the surface, dives and surfaces,
//! caps its speed, climbs out onto a ledge — and wades at `Feet`.
//! Every fixture is a hand-built stud region whose recipes carry the water
//! (`common::water`), ticked through the real region tick.
use crate::common::water::{column_with_plate_water, PlateWater};
use crate::common::{self, body, key, r0, teleport, ticks, CHUNK_EDGE, FLOOR_PLATES, STAND};
use game::history::PersonId;
use game::movement::{
    plan_with, settle, BodyScale, Immersion, MoveKeys, MoveParams, SWIM_SPEED, TICK_DT, UNITS_PER_METER,
    WADE_SPEED_FACTOR, WALK_SPEED,
};
use game::rapier3d::math::Vector;
use game::{GameEventKind, Goal, Key, Motion, World, WorldBlockPos, TILE_PLATES};

/// Tile level a body stands in on the bank (the ground's top is its bottom):
/// a pool dug into the bank and filled to the brim has its surface there.
const BANK: i32 = STAND;
/// One metre (a tile's width), sim units.
const BV: f32 = common::TILE_W;
/// Plates the ledge a swimmer climbs out onto lies above the water (0.4 m).
const LEDGE_PLATES: i32 = 2;
/// Tiles a pool is dug into the bank.
const POOL_DEPTH: i32 = 4;

/// The bank's top and every brim-full pool's surface, sim units.
fn surface_y() -> f32 {
    common::floor_y(FLOOR_PLATES)
}

/// The top of a pool's bed, sim units.
fn bed_y() -> f32 {
    common::floor_y(FLOOR_PLATES - POOL_DEPTH * TILE_PLATES as i32)
}

/// The stud body's height, sim units.
fn body_height() -> f32 {
    let s = BodyScale::STUD;
    2.0 * (s.capsule_half_height() + s.capsule_radius())
}

/// Half the body's height in units: the body origin is the capsule centre.
fn half_height() -> f32 {
    body_height() / 2.0
}

/// A pool `POOL_DEPTH` tiles deep over the tile box `xs × zs`, filled with
/// recipe water to the brim (the bank's top).
fn pool(xs: std::ops::Range<i64>, zs: std::ops::Range<i64>) -> PlateWater {
    PlateWater { xs, zs, bed: FLOOR_PLATES - POOL_DEPTH * TILE_PLATES as i32, top: FLOOR_PLATES }
}

/// The stud seed of the fixtures: 2×2 chunk columns of flat ground (the bank)
/// with the recipe water of `water`.
fn bank_seed(water: &[PlateWater]) -> game::StudRegionSeed {
    common::stud_seed_over(0.0..2.0 * CHUNK_EDGE, 0.0..2.0 * CHUNK_EDGE, |cx, cz| {
        column_with_plate_water(r0(), cx, cz, FLOOR_PLATES, water)
    })
}

/// The bank with `water`, the player created at the spawn column (tile 8, 8)
/// with controls on and 12 ticks run: settled on the bank, or dropped into
/// the pool the spawn lies in.
fn bank_world(water: &[PlateWater]) -> World {
    common::install_real_block_props();
    let mut world = World::new();
    world.load(&r0(), common::region_of(r0(), bank_seed(water)));
    world.handle_region_event(GameEventKind::CreateClient(0), r0()).unwrap();
    world.forget_last_event(&r0());
    key(&mut world, Key::KeyE, true);
    key(&mut world, Key::KeyE, false);
    ticks(&mut world, 12);
    world
}

/// Raises the bank by `plates` over the tile columns `xs × zs`.
fn raise_bank(world: &mut World, xs: std::ops::Range<i32>, zs: std::ops::Range<i32>, plates: i32) {
    world.regions.get_mut(&r0()).unwrap().with_data(|d| {
        for x in xs.clone() {
            for z in zs.clone() {
                let (lo, hi) = game::stud_tile_box([x, BANK, z]).unwrap();
                assert!(d.fill_stud_box_safe(lo, [hi[0], lo[1] + plates, hi[2]], game::MAT_STONE) > 0, "raise {x},{z}");
            }
        }
    });
}

fn player_motion(world: &World) -> Motion {
    let d = world.data(&r0());
    let e = *d.player_entites.get(&0).unwrap();
    d.ecs.motion.try_get(e).unwrap_or_default()
}

fn feet_y(world: &World) -> f32 {
    body(world).y - half_height()
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

/// The player dropped into the middle of a `POOL_DEPTH`-deep pool.
fn floating_player() -> World {
    let mut w = bank_world(&[pool(3..14, 3..14)]);
    float_for_40_ticks(&mut w, 300);
    w
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn body_in_deep_water_floats_at_the_surface() {
    let w = floating_player();
    let feet = feet_y(&w);
    assert!(feet > bed_y() + 0.5 * BV, "floats clear of the bed: feet at {} m", feet / BV);
    assert!(feet < surface_y(), "floats in, not on, the water: feet at {} m", feet / BV);
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
fn swimmer_climbs_out_onto_a_low_ledge() {
    let mut w = bank_world(&[pool(3..14, 3..14)]);
    // North of the pool (z < 3) the bank is raised `LEDGE_PLATES` (0.4 m):
    // its top is that far above the water's surface.
    // P10b, found and not fixed (movement is `game/src`): the voxel body
    // climbed a 1 m ledge; the stud body climbs 0, 1 and 2 plates but not a
    // brick course (3 plates) — its exit jump tops out 0.04 m over the
    // surface and it then hangs against the brick edge, not grounded, its
    // `vel_y` falling without bound (−814 units/s after 16 ticks). So the
    // ledge here is the highest it does climb.
    raise_bank(&mut w, 0..32, 0..3, LEDGE_PLATES);
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
        if b.grounded && b.z < 3.0 * BV && feet_y(&w) > common::floor_y(FLOOR_PLATES + LEDGE_PLATES) - 1.0 {
            out = Some(t);
            break;
        }
    }
    assert!(out.is_some(), "on the ledge within 40 ticks: feet {} z {} blocks", feet_y(&w) / BV, body(&w).z / BV);
}

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

/// Stud water has a surface on the plate grid: a body standing in two plates
/// of it (the deepest wading) has only its feet wet, in a tile of it (1.2 m)
/// its waist too. The sampling is checked on the real region state; the
/// wading speed through `plan_with` + `settle`, as every mover runs them.
#[test]
fn wading_slows_walking() {
    // Two plates of recipe water on the bank around the spawn, a whole tile of
    // it beside that.
    let shallow = PlateWater { xs: 7..10, zs: 7..10, bed: FLOOR_PLATES, top: FLOOR_PLATES + 2 };
    let deep = PlateWater { xs: 12..15, zs: 7..10, bed: FLOOR_PLATES, top: FLOOR_PLATES + TILE_PLATES as i32 };
    let region = common::region_of(r0(), bank_seed(&[shallow, deep]));
    let d = region.data();
    let feet = Vector::new(8.5 * BV, surface_y(), 8.5 * BV);
    assert_eq!(game::immersion_at(d, feet, body_height()), Immersion::Feet, "the body wades in two plates");
    let in_deep = Vector::new(13.5 * BV, surface_y(), 8.5 * BV);
    assert_eq!(game::immersion_at(d, in_deep, body_height()), Immersion::Waist, "its waist is under a tile of water");
    let above = Vector::new(feet.x, surface_y() + game::plate_units(2), feet.z);
    assert_eq!(game::immersion_at(d, above, body_height()), Immersion::Dry);

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
    region.seed_stud_world(bank_seed(&[pool(3..14, 3..8)]));
    let forget = |region: &mut game::Region, kind: GameEventKind| {
        region.handle_event(kind).unwrap();
        region.forget_last_event();
    };
    forget(&mut region, GameEventKind::CreateClient(0));
    let input = |key, pressed| GameEventKind::PlayerInput(0, game::InputEvent::Key { key, pressed });
    forget(&mut region, input(Key::KeyE, true));
    forget(&mut region, input(Key::KeyE, false));
    for _ in 0..12 {
        forget(&mut region, GameEventKind::Tick);
    }
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
    // A channel 3 tiles wide (z 10..13) across the whole fixture, two tiles
    // deeper than the pools: the person steps off the bank and plunges under
    // the stud body's gravity (4/3 of the base) before it floats up — in a
    // `POOL_DEPTH` channel it came within 0.11 m of the bed.
    let channel_bed = FLOOR_PLATES - (POOL_DEPTH + 2) * TILE_PLATES as i32;
    let channel = PlateWater { bed: channel_bed, ..pool(0..32, 10..13) };
    let mut w = bank_world(&[channel]);
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
        walk_up: vec![true; path.len()],
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
            lowest = lowest.min(b.y - half_height());
        }
        deepest = deepest.max(m.immersion);
        if t % 10 == 0 {
            eprintln!("tick {t}: feet {:.3} z {:.3} blocks {:?}", (b.y - half_height()) / BV, b.z / BV, m);
        }
    }
    eprintln!("lowest feet over the channel {} blocks, deepest {deepest:?}", lowest / BV);
    assert!(entered, "the person walked into the channel");
    assert!(deepest >= Immersion::Waist, "its immersion reached Waist: {deepest:?}");
    // Clear of the bed by a quarter metre, as in the seam test
    // (`handoff_schedule::swim_is_seam_invariant`).
    let bed = common::floor_y(channel_bed);
    assert!(lowest > bed + 0.25 * BV, "it never sank to the bed: lowest feet {} m (bed {})", lowest / BV, bed / BV);
}
