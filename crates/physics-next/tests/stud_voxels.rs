//! Pins the fork's handling of anisotropic `Voxels` cells, `(8, 1.6, 8)` units
//! (a stud chunk's fine grid: one stud wide, one plate tall), before any stud
//! code depends on it: a capsule must rest on, walk level across, and step up
//! a capped brick floor, across one body or two; dynamic shapes must rest on it.
//! Terrain is raw `Voxels`; the controller for the step test is a local copy of
//! `game::movement::character_controller` with `max_height` overridden, because
//! this pins the fork, not the game.
use crate::common::{SPAWN, Walker};
use game::movement::{
    self, AUTOSTEP_MIN_WIDTH_M, CHARACTER_SKIN, GROUND_SNAP, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, TICK_DT,
    UNITS_PER_METER, WALK_SPEED,
};
use physics_next::PhysicsWorld;
use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::prelude::*;
use std::ops::Range;

const FINE: Vector = Vector::new(8.0, 1.6, 8.0);
/// A fine cell's height: half a plate (a plate is 3.2 units).
const CELL: f32 = 1.6;
/// Fine cells of a brick course: three plates = 6 cells = 9.6 units = 0.6 m.
const COURSE: i32 = 6;
/// Per-tick slack on level motion (as `walking_smoothness`).
const LEVEL: f32 = 0.005;
const STEP: f32 = WALK_SPEED * UNITS_PER_METER * TICK_DT;
/// World seam between two bodies (a chunk edge).
const SEAM: f32 = 256.0;

fn world() -> PhysicsWorld {
    PhysicsWorld { params: game::PhysicsParams::default().0, ..Default::default() }
}

/// Fine cell columns covering the half-open unit rectangle.
fn cols(x: &Range<f32>, z: &Range<f32>) -> Vec<(i32, i32)> {
    let r = |r: &Range<f32>| (r.start / 8.0).floor() as i32..(r.end / 8.0).ceil() as i32;
    let zs = r(z);
    r(x).flat_map(|x| zs.clone().map(move |z| (x, z))).collect()
}

/// A column stack of `height` fine cells (cap included) on each of `cols`.
fn stack(cols: &[(i32, i32)], height: i32) -> Vec<IVector> {
    cols.iter().flat_map(|&(x, z)| (0..height).map(move |y| IVector::new(x, y, z))).collect()
}

fn add_voxels(w: &mut PhysicsWorld, keys: &[IVector]) {
    let body = w.bodies.insert(RigidBodyBuilder::fixed());
    w.colliders.insert_with_parent(ColliderBuilder::voxels(FINE, keys), body, &mut w.bodies);
}

/// Floor top: a brick floor of 6 fine cells (three plates) plus the one-cell stud cap.
const TOP: f32 = 7.0 * CELL;

fn walk_range() -> (Range<f32>, Range<f32>) {
    (SPAWN[0] - 64.0..SPAWN[0] + 64.0, SPAWN[1] - 64.0..SPAWN[1] + 120.0 * STEP + 64.0)
}

fn capped_walker(split: bool) -> Walker {
    let (x, z) = walk_range();
    let mut w = world();
    if split {
        assert!(z.contains(&SEAM) && SPAWN[1] < SEAM - 100.0, "seam must lie ahead of the walk");
        add_voxels(&mut w, &stack(&cols(&x, &(z.start..SEAM)), 7));
        add_voxels(&mut w, &stack(&cols(&x, &(SEAM..z.end)), 7));
    } else {
        add_voxels(&mut w, &stack(&cols(&x, &z), 7));
    }
    Walker::over(w, TOP)
}

/// Capsule centre height at rest on a floor of top `top`.
fn rest_y(top: f32) -> f32 {
    top + PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS + CHARACTER_SKIN
}

#[test]
fn capsule_rests_flat_on_capped_floor() {
    let mut w = capped_walker(false);
    for tick in 0..60 {
        w.tick();
        let b = w.body();
        assert!(b.grounded, "lost the ground at tick {tick}: {b:?}");
        assert!((b.y - rest_y(TOP)).abs() < 0.01, "tick {tick}: y {} vs expected {}", b.y, rest_y(TOP));
    }
}

fn assert_level_walk(w: &mut Walker, n: usize) {
    w.keys.back = true;
    w.ticks(1);
    let start = w.body();
    let mut prev = start;
    for tick in 0..n {
        w.tick();
        let now = w.body();
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "lost the ground at {at}");
        assert!((now.y - prev.y).abs() < LEVEL, "bobbed at {at}");
        assert!((now.z - prev.z - STEP).abs() < STEP * 0.01, "speed hitched at {at}");
        assert!((now.x - start.x).abs() < LEVEL, "drifted sideways at {at}");
        prev = now;
    }
    assert!(prev.z - start.z > 40.0 * 8.0 * 0.5, "walk too short: {start:?} -> {prev:?}");
}

#[test]
fn capsule_walks_level_across_cap_cells() {
    assert_level_walk(&mut capped_walker(false), 120);
}

#[test]
fn capsule_walks_level_across_two_bodies() {
    let mut w = capped_walker(true);
    assert_level_walk(&mut w, 120);
    assert!(w.body().z > SEAM, "walk must cross the body seam: {:?}", w.body());
}

#[test]
fn ball_and_cuboid_rest_on_capped_floor() {
    let (x, z) = walk_range();
    let mut w = world();
    w.gravity = Vector::new(0.0, -movement::GRAVITY * UNITS_PER_METER, 0.0);
    add_voxels(&mut w, &stack(&cols(&x, &z), 7));
    let at = |dx: f32| Vector::new(SPAWN[0] + dx, TOP + 20.0, SPAWN[1]);
    let ball = w.bodies.insert(RigidBodyBuilder::dynamic().translation(at(-16.0)));
    w.colliders.insert_with_parent(ColliderBuilder::ball(4.0), ball, &mut w.bodies);
    let cube = w.bodies.insert(RigidBodyBuilder::dynamic().translation(at(16.0)));
    w.colliders.insert_with_parent(ColliderBuilder::cuboid(4.0, 4.0, 4.0), cube, &mut w.bodies);
    let mut pipeline = PhysicsPipeline::new();
    for _ in 0..300 {
        w.step(&mut pipeline);
        for h in [ball, cube] {
            assert!(w.bodies[h].translation().y > TOP - 0.05, "tunnelled: {:?}", w.bodies[h].translation());
        }
    }
    for h in [ball, cube] {
        let b = &w.bodies[h];
        let bottom = b.translation().y - 4.0;
        assert!((bottom - TOP).abs() < 0.05, "bottom {bottom} vs {TOP}");
        assert!(b.linvel().length() < 0.1, "still moving: {:?}", b.linvel());
    }
}

/// The game's controller, with the autostep height set to `step_m` metres.
fn controller(step_m: f32) -> KinematicCharacterController {
    KinematicCharacterController {
        autostep: Some(CharacterAutostep {
            max_height: CharacterLength::Absolute(step_m * UNITS_PER_METER),
            min_width: CharacterLength::Absolute(AUTOSTEP_MIN_WIDTH_M * UNITS_PER_METER),
            include_dynamic_bodies: false,
        }),
        snap_to_ground: Some(CharacterLength::Absolute(GROUND_SNAP * UNITS_PER_METER)),
        offset: CharacterLength::Absolute(CHARACTER_SKIN),
        depenetrate: false,
        ..Default::default()
    }
}

/// Walks +z for 120 ticks toward a step one brick course (3 plates) tall, at
/// 160 units ahead of the spawn; returns the final body z and y.
fn walk_into_step(step_m: f32) -> (f32, f32) {
    let (x, z) = walk_range();
    let wall_z = SPAWN[1] + 160.0;
    let mut w = world();
    add_voxels(&mut w, &stack(&cols(&x, &(z.start..wall_z)), 7));
    add_voxels(&mut w, &stack(&cols(&x, &(wall_z..z.end)), 7 + COURSE));
    let mut walker = Walker::over(w, TOP);
    let kcc = controller(step_m);
    let mut pipeline = PhysicsPipeline::new();
    walker.keys.back = true;
    for tick in 0..120u64 {
        let desired = movement::plan(&mut walker.motion, &walker.keys, 0.0, 1000 + tick);
        let w = &walker.w;
        let body = &w.bodies[walker.body];
        let collider = &w.colliders[body.colliders()[0]];
        let queries = w.broad_phase.as_query_pipeline(
            w.narrow_phase.query_dispatcher(),
            &w.bodies,
            &w.colliders,
            QueryFilter::default().exclude_rigid_body(walker.body),
        );
        let start = Pose::from_translation(body.translation());
        let out = kcc.move_shape(TICK_DT, &queries, collider.shape(), &start, desired, |_| {});
        movement::settle(&mut walker.motion, &desired, &out.translation, out.grounded);
        let t = walker.w.bodies[walker.body].translation();
        walker.w.bodies[walker.body].set_next_kinematic_translation(t + out.translation);
        walker.w.step(&mut pipeline);
    }
    let b = walker.body();
    (b.z, b.y)
}

#[test]
fn capsule_steps_up_one_brick_course() {
    let course = COURSE as f32 * CELL;
    let (z, y) = walk_into_step(0.65);
    assert!(z > SPAWN[1] + 160.0 + PLAYER_CAPSULE_RADIUS, "0.65 m must climb: z {z}");
    assert!((y - rest_y(TOP + course)).abs() < 0.05, "0.65 m: y {y} vs {}", rest_y(TOP + course));
    let (z, y) = walk_into_step(0.55);
    assert!(z < SPAWN[1] + 160.0, "0.55 m must be stopped by the course: z {z} y {y}");
    assert!((y - rest_y(TOP)).abs() < 0.05, "0.55 m: y {y}");
}


/// A corridor one tile wide (2 studs) and two tiles high (12 plates = 24 fine cells
/// above the floor bricks, the floor's stud cap inside it), starting 48 units ahead of
/// the spawn: walls on both sides and a roof over a capped floor.
fn corridor_walker(half_height: f32, radius: f32) -> Walker {
    assert_eq!(SPAWN[0] % 8.0, 0.0, "the corridor is centred on a stud edge");
    let (x, z) = walk_range();
    let start = SPAWN[1] + 48.0;
    let inside = SPAWN[0] - 8.0..SPAWN[0] + 8.0;
    let ahead = start..z.end;
    let mut keys = stack(&cols(&x, &z), 7);
    let roof_from = COURSE + 24;
    for (cx, cz) in cols(&x, &ahead) {
        let in_corridor = (cx as f32 * 8.0) >= inside.start && (cx as f32 * 8.0) < inside.end;
        let ys = if in_corridor { roof_from..roof_from + COURSE } else { 7..roof_from + COURSE };
        keys.extend(ys.map(|y| IVector::new(cx, y, cz)));
    }
    let mut w = world();
    add_voxels(&mut w, &keys);
    Walker::over_capsule(w, TOP, half_height, radius)
}

#[test]
fn stud_body_walks_a_one_by_two_tile_corridor() {
    let s = movement::BodyScale::STUD;
    let mut w = corridor_walker(s.capsule_half_height(), s.capsule_radius());
    assert_level_walk(&mut w, 120);
    assert!(w.body().z > SPAWN[1] + 48.0 + 64.0, "must be well inside the corridor: {:?}", w.body());
}

#[test]
fn a_full_2_4_m_figure_does_not_fit_the_corridor() {
    // The 4/3-scaled voxel capsule (2.4 m tall, 1.07 m wide): what the stud body was.
    let f = movement::STUD_BODY_FACTOR;
    let mut w = corridor_walker(PLAYER_CAPSULE_HALF_HEIGHT * f, PLAYER_CAPSULE_RADIUS * f);
    w.keys.back = true;
    w.ticks(120);
    assert!(w.body().z < SPAWN[1] + 48.0, "must be stopped at the mouth: {:?}", w.body());
}

/// One tick of the GAME's stud body heading `yaw`: `movement::walk_body_scaled` with
/// `BodyScale::STUD` (its capsule, step, snap, tread, edge hold and depenetration
/// lift), then the world step.
fn stud_tick(w: &mut Walker, yaw: f32, tick: u64) {
    let s = movement::BodyScale::STUD;
    let params = s.move_params(movement::MoveParams::PLAYER);
    let world = &w.w;
    let moved = movement::walk_body_scaled(
        &mut w.motion,
        &w.keys,
        yaw,
        tick,
        &world.broad_phase,
        &world.narrow_phase,
        &world.bodies,
        &world.colliders,
        w.body,
        &params,
        s,
    );
    if moved != Vector::ZERO {
        let t = w.w.bodies[w.body].translation();
        w.w.bodies[w.body].set_next_kinematic_translation(t + moved);
    }
    let mut pipeline = PhysicsPipeline::new();
    w.w.step(&mut pipeline);
}

/// Where the stair of [`stud_stair_walker`] begins, units ahead of the spawn.
const STAIR_AHEAD: f32 = 160.0;

/// A stud body on a capped floor, `gap` units before a stair whose `i`-th tread
/// (`tread` units deep) lies `rise_plates * (i + 1)` plates over the floor.
fn stud_stair_walker(rise_plates: i32, tread: f32, gap: f32) -> Walker {
    let s = movement::BodyScale::STUD;
    let (_, z) = walk_range();
    // Wide enough for a diagonal climb.
    let x = SPAWN[0] - 200.0..SPAWN[0] + 200.0;
    let foot = SPAWN[1] + STAIR_AHEAD;
    let mut w = world();
    let mut keys = stack(&cols(&x, &(z.start..foot)), 7);
    for (cx, cz) in cols(&x, &(foot..z.end)) {
        let i = ((cz as f32 * 8.0 - foot) / tread).floor() as i32 + 1;
        keys.extend((0..7 + 2 * rise_plates * i).map(|y| IVector::new(cx, y, cz)));
    }
    add_voxels(&mut w, &keys);
    let mut walker = Walker::over_capsule(w, TOP, s.capsule_half_height(), s.capsule_radius());
    let b = walker.body();
    walker.teleport(Vector::new(b.x, b.y, foot - gap));
    walker
}

/// Centre height of the stud body at rest on a floor of top `top`.
fn stud_rest_y(top: f32) -> f32 {
    let s = movement::BodyScale::STUD;
    top + s.capsule_half_height() + s.capsule_radius() + CHARACTER_SKIN
}

/// The game's stud body (not the voxel capsule of `capsule_steps_up_one_brick_course`)
/// takes a rise of one, two and three plates wherever in a tick it meets the face.
/// A brick (3 plates, 9.6 units) is taller than the capsule's radius (6.4): the step
/// used to be undone whenever the tick had under ~2.3 units of motion left at the face
/// — the lifted body came down on the ledge's edge and slid back off it
/// (`movement::STUD_EDGE_HOLD_ANGLE`). 7 of these 16 phases stalled for good.
fn assert_stud_body_takes_rises(plates: std::ops::RangeInclusive<i32>, phases: usize) {
    let mut failed = vec![];
    for plates in plates {
        for phase in 0..phases {
            let gap = 40.0 + phase as f32 * STEP / phases as f32;
            // One very deep tread: a single rise.
            let mut w = stud_stair_walker(plates, 4096.0, gap);
            w.keys.back = true;
            // The walk to the face, and at most 4 ticks to be over it.
            let ticks = (gap / STEP).ceil() as u64 + 4;
            for tick in 0..ticks {
                stud_tick(&mut w, 0.0, 1000 + tick);
            }
            let (b, face) = (w.body(), SPAWN[1] + STAIR_AHEAD);
            let top = TOP + plates as f32 * 2.0 * CELL;
            if !(b.z > face && b.grounded && (b.y - stud_rest_y(top)).abs() < 0.05) {
                failed.push((plates, phase, b.z - face, b.y - stud_rest_y(top)));
            }
        }
    }
    assert!(failed.is_empty(), "stalled (plates, phase of {phases}, z past the face, y over rest): {failed:?}");
}

#[test]
fn stud_body_steps_up_a_brick_from_any_phase() {
    // Four phases of a brick: phase 0 is one that stalled.
    assert_stud_body_takes_rises(3..=3, 4);
}

/// The whole sweep: one, two and three plates from 16 phases each.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stud_body_steps_up_every_rise_from_every_phase() {
    assert_stud_body_takes_rises(1..=3, 16);
}

/// Straight up, and across at 45 degrees, a stair of five brick-high risers with
/// one-tile (2-stud) treads: the body climbs every one without a jump, never loses
/// more than a tick's ground contact per riser, and keeps most of its walking speed.
fn assert_stud_body_climbs_stair(phases: usize) {
    const RISERS: i32 = 5;
    for (yaw, name) in [(0.0f32, "straight"), (-std::f32::consts::FRAC_PI_4, "diagonal")] {
        for phase in 0..phases {
            let gap = 30.0 + phase as f32 * STEP / phases as f32;
            let mut w = stud_stair_walker(3, 16.0, gap);
            w.keys.back = true;
            let start = w.body();
            let top_z = SPAWN[1] + STAIR_AHEAD + 16.0 * (RISERS - 1) as f32 + 8.0;
            let mut ticks = 0u64;
            let mut airborne = 0;
            while w.body().z < top_z {
                stud_tick(&mut w, yaw, 1000 + ticks);
                ticks += 1;
                airborne += !w.body().grounded as u32;
                assert!(ticks < 200, "{name} phase {phase}: stuck on the stair at {:?}", w.body());
            }
            let b = w.body();
            let forward = (b.z - start.z) / if yaw == 0.0 { 1.0 } else { std::f32::consts::FRAC_1_SQRT_2 };
            assert!(forward / (ticks as f32 * STEP) > 0.8, "{name} phase {phase}: slow climb, {ticks} ticks to {b:?}");
            assert!(airborne <= RISERS as u32, "{name} phase {phase}: {airborne} airborne ticks");
            let top = TOP + (RISERS * 3) as f32 * 2.0 * CELL;
            assert!(b.y > stud_rest_y(top) - 2.0 * CELL - 0.05, "{name} phase {phase}: not on tread {RISERS}: {b:?}");
        }
    }
}

#[test]
fn stud_body_climbs_a_brick_stair() {
    assert_stud_body_climbs_stair(1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stud_body_climbs_a_brick_stair_from_every_phase() {
    assert_stud_body_climbs_stair(8);
}

/// Four plates (0.8 m) is over `STUD_STEP_HEIGHT_M`: the body stops at the face, on
/// the lower floor, from every phase. (The pathfinder's `STUD_STEP_PLATES` is 3.)
#[test]
fn stud_body_does_not_step_up_four_plates() {
    for phase in 0..4 {
        let gap = 40.0 + phase as f32 * STEP / 4.0;
        let mut w = stud_stair_walker(4, 4096.0, gap);
        w.keys.back = true;
        for tick in 0..60 {
            stud_tick(&mut w, 0.0, 1000 + tick);
        }
        let b = w.body();
        assert!(b.z < SPAWN[1] + STAIR_AHEAD, "phase {phase}: climbed 4 plates: {b:?}");
        assert!((b.y - stud_rest_y(TOP)).abs() < 0.25, "phase {phase}: left the floor: {b:?}");
    }
}

/// Holding a body on an edge must not hang it there: walking off a brick-high and a
/// 2-tile-high ledge, the body comes down onto the lower floor and walks on.
#[test]
fn stud_body_walks_off_a_ledge() {
    for plates in [3, 12] {
        let (x, z) = walk_range();
        let edge = SPAWN[1] + 40.0;
        let mut w = world();
        add_voxels(&mut w, &stack(&cols(&x, &(z.start..edge)), 7 + 2 * plates));
        add_voxels(&mut w, &stack(&cols(&x, &(edge..z.end)), 7));
        let s = movement::BodyScale::STUD;
        let top = TOP + plates as f32 * 2.0 * CELL;
        let mut w = Walker::over_capsule(w, top, s.capsule_half_height(), s.capsule_radius());
        w.keys.back = true;
        for tick in 0..60 {
            stud_tick(&mut w, 0.0, 1000 + tick);
        }
        let b = w.body();
        assert!(b.grounded && (b.y - stud_rest_y(TOP)).abs() < 0.25, "{plates} plates: not down on the floor: {b:?}");
        assert!(b.z > edge + 100.0, "{plates} plates: did not walk on: {b:?}");
    }
}
