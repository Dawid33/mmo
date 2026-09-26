//! Upgrade plan Task 1.2: the game's movement-critical parry/rapier fixes,
//! translated to glam and run against PRISTINE crates.io parry 0.31.1 /
//! rapier 0.36.0.
//!
//! - The five `crates/game/tests/shape_cast_grazing.rs` cases and
//!   `gjk_upstream.rs::capsule_touching_cuboid_matches_upstream`, with their
//!   assertions unchanged. The ones upstream fails are `#[ignore]`d with the
//!   reason: that list is Task 3.2/3.3's re-port scope, and each ignore comes
//!   off when its patch lands in the fork.
//! - The centred-capsule contact the nalgebra-era fork gets wrong (Task 5.4
//!   Step 1b adds it to `gjk_upstream.rs` after the switch).
//! - Probes for design decisions D7 (upstream's zero-motion KCC
//!   depenetration, #485) and D6 (`IntegrationParameters::length_unit`).
//!   They pin what upstream does today; run with `-- --nocapture` for the
//!   numbers written up in `docs/superpowers/specs/2026-09-22-rapier-035-spike-results.md`.
use game::movement::{
    CHARACTER_SKIN, GRAVITY, GROUND_SNAP, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, STEP_HEIGHT,
    UNITS_PER_METER, WALK_SPEED,
};
use physics_next::PhysicsWorld;
use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::parry::query::{self, ShapeCastHit, ShapeCastOptions, ShapeCastStatus, cast_shapes};
use rapier3d::parry::shape::{Capsule, Cuboid, Voxels};
use rapier3d::prelude::*;

/// The player capsule and the character controller's skin for it.
const HALF_HEIGHT: f32 = PLAYER_CAPSULE_HALF_HEIGHT;
const RADIUS: f32 = PLAYER_CAPSULE_RADIUS;
const SKIN: f32 = CHARACTER_SKIN;
/// Block edge in world units.
const BLOCK: f32 = UNITS_PER_METER;

fn v(x: f32, y: f32, z: f32) -> Vector {
    Vector::new(x, y, z)
}

fn at(x: f32, y: f32, z: f32) -> Pose {
    Pose::translation(x, y, z)
}

/// The character controller's cast options.
fn skin_cast(max_time_of_impact: f32) -> ShapeCastOptions {
    ShapeCastOptions {
        target_distance: SKIN,
        stop_at_penetration: false,
        max_time_of_impact,
        compute_impact_geometry_on_penetration: true,
    }
}

/// One tick of walking with gravity: mostly along +z, slightly down.
fn walking_step() -> Vector {
    v(0.0, -0.64, 3.4536)
}

fn assert_floor_hit(hit: Option<ShapeCastHit>, context: &str) {
    let hit = hit.unwrap_or_else(|| panic!("{context}: the floor was missed"));
    assert!(hit.time_of_impact < 1.0e-3, "{context}: toi {}", hit.time_of_impact);
    let n = hit.normal1;
    assert!(
        n.x.abs() < 1.0e-4 && n.z.abs() < 1.0e-4 && n.y > 0.9999,
        "{context}: a flat floor's normal is straight up, got ({}, {}, {})",
        n.x,
        n.y,
        n.z
    );
}

/// shape_cast_grazing.rs: starting on the target-distance-rounded surface,
/// the cast missed or returned a stale search direction as the normal.
///
/// Upstream (Task 1.2): no miss, and the normal is within ~2.6e-3 of +Y —
/// but not the test's 1e-4 — and a start 1.5e-3 outside the skin gets the
/// geometric TOI (~2.3e-3) where the fork's `GRAZING_START_MARGIN` reports
/// a resting contact (< 1e-3).
#[test]
#[ignore = "fails on pristine parry 0.31.1 (TOI 2.2e-3 at gap skin+1.5e-3; normal tilt up to 2.6e-3): re-port scope, Task 3.3"]
fn cast_starting_at_the_target_distance_hits_with_the_true_normal() {
    let cuboid = Cuboid::new(v(8.0, 8.0, 8.0));
    let capsule = Capsule::new_y(HALF_HEIGHT, RADIUS);
    for gap in [SKIN + 0.0015, SKIN + 0.0005, SKIN, SKIN - 0.0005, SKIN - 0.006] {
        for z in [-6.0, -2.5, 0.0, 2.01, 5.0] {
            let pos2 = at(0.3, 8.0 + HALF_HEIGHT + RADIUS + gap, z);
            let hit = cast_shapes(&Pose::IDENTITY, Vector::ZERO, &cuboid, &pos2, walking_step(), &capsule, skin_cast(1.0))
                .unwrap();
            assert_floor_hit(hit, &format!("gap {gap} z {z}"));
        }
    }
}

#[test]
fn cast_moving_away_from_within_the_target_distance_is_free() {
    let cuboid = Cuboid::new(v(8.0, 8.0, 8.0));
    let capsule = Capsule::new_y(HALF_HEIGHT, RADIUS);
    let pos2 = at(0.0, 8.0 + HALF_HEIGHT + RADIUS + SKIN, 0.0);
    for vel in [v(0.0, 0.0, 3.0), v(0.0, 1.0, 3.0)] {
        let hit = cast_shapes(&Pose::IDENTITY, Vector::ZERO, &cuboid, &pos2, vel, &capsule, skin_cast(1.0)).unwrap();
        assert!(hit.is_none(), "sliding along or lifting off the floor is no hit: {hit:?}");
    }
}

/// A 3x1x3 floor of `size`-unit voxels, top face at y = `size`.
fn voxel_floor(size: f32) -> Voxels {
    let mut cells = Vec::new();
    for x in 0..3 {
        for z in 0..3 {
            cells.push(IVector::new(x, 0, z));
        }
    }
    Voxels::new(Vector::splat(size), &cells)
}

fn cast_on_floor(floor: &Voxels, size: f32, x: f32, z: f32, vel: Vector) -> Option<ShapeCastHit> {
    let capsule = Capsule::new_y(HALF_HEIGHT, RADIUS);
    let pos2 = at(x, size + HALF_HEIGHT + RADIUS + SKIN + 0.001, z);
    let distance = vel.length();
    cast_shapes(&Pose::IDENTITY, Vector::ZERO, floor, &pos2, vel / distance, &capsule, skin_cast(distance)).unwrap()
}

/// shape_cast_grazing.rs: the voxels cast searched under the shape's bare
/// AABB, moved with the motion instead of grown — a hovering shape crossing a
/// voxel border missed the voxel under it.
#[test]
/// Upstream (Task 1.2): all three casts miss the floor.
#[ignore = "fails on pristine parry 0.31.1 (floor missed: search domain): re-port scope, Task 3.2"]
fn voxels_cast_sees_the_voxel_under_a_hovering_shape() {
    let floor = voxel_floor(BLOCK);
    for z in [BLOCK + 9.443, 2.0 * BLOCK - RADIUS - 0.16, 2.0 * BLOCK - RADIUS - 0.01] {
        assert_floor_hit(cast_on_floor(&floor, BLOCK, 1.5 * BLOCK, z, walking_step()), &format!("z {z}"));
    }
}

/// shape_cast_grazing.rs: every voxel was cast as a free-standing box, so a
/// flat floor's seams reported their edges.
///
/// Upstream (Task 1.2): every start 1e-3 outside the skin gets the geometric
/// TOI (~5.3e-3) instead of a resting contact, and next to a seam (x = 1.01)
/// the normal leans ~4e-3 (0.24°) — far smaller than the fork's old
/// degrees-off bug, still above the 1e-4 bar. The slide is free.
#[test]
#[ignore = "fails on pristine parry 0.31.1 (TOI ~5e-3 from a skin+1e-3 start; normal lean 4e-3 at a seam): re-port scope, Tasks 3.2/3.3"]
fn voxels_cast_reports_no_edges_inside_a_flat_floor() {
    let floor = voxel_floor(1.0);
    let capsule_floor = |x: f32, z: f32| cast_on_floor(&floor, 1.0, x, z, walking_step());
    for x in [1.5, 1.26, 1.01] {
        for z in [1.5, 1.24, 1.76, 1.99] {
            assert_floor_hit(capsule_floor(x, z), &format!("x {x} z {z}"));
        }
    }
    let slide = cast_on_floor(&floor, 1.0, 1.5, 1.24, v(0.0, 0.0, 0.7));
    assert!(
        slide.is_none_or(|hit| hit.status == ShapeCastStatus::Converged && hit.normal1.y > 0.9999),
        "a flat floor must not stop a slide along it: {slide:?}"
    );
}

/// The floor's real rim keeps its edge.
#[test]
fn voxels_cast_keeps_real_edges() {
    let floor = voxel_floor(BLOCK);
    let capsule = Capsule::new_y(HALF_HEIGHT, RADIUS);
    let pos2 = at(1.5 * BLOCK, BLOCK + HALF_HEIGHT + RADIUS - 2.0, 3.0 * BLOCK + 8.0);
    let hit = cast_shapes(&Pose::IDENTITY, Vector::ZERO, &floor, &pos2, v(0.0, 0.0, -1.0), &capsule, skin_cast(8.0))
        .unwrap()
        .expect("the rim must stop the capsule");
    let n = hit.normal1;
    assert!(n.z > 0.3 && n.y > 0.3, "an edge contact leans: ({}, {}, {})", n.x, n.y, n.z);
}

/// gjk_upstream.rs: the off-centre touching capsule, pinned to parry
/// 0.31.1's own output when Plan 1 hand-ported the GJK fixes.
#[test]
fn capsule_touching_cuboid_matches_upstream() {
    let block = Cuboid::new(v(8.0, 8.0, 8.0));
    let capsule = Capsule::new_y(0.7, 0.3);
    let c = query::contact(&at(0.0, -8.0, 0.0), &block, &at(1.0, 1.0, -2.0), &capsule, 0.0).unwrap().unwrap();
    const TOL: f32 = 1.0e-6;
    let close = |got: Vector, want: [f32; 3]| (got - Vector::from_array(want)).length() < TOL;
    assert!((c.dist - 1.2409591e-5).abs() < TOL, "dist {}", c.dist);
    assert!(close(c.normal1, [-0.02856119, 0.9995807, -0.0047601983]), "normal1 {:?}", c.normal1);
    assert!(close(c.point1, [1.0015448, 0.0, -2.000366]), "point1 {:?}", c.point1);
    assert!(close(c.point2, [1.0015442, 1.2397766e-5, -2.000366]), "point2 {:?}", c.point2);
}

/// Task 5.4 Step 1b: centred exactly over the block, the nalgebra-era fork
/// (like parry 0.25.3) stops GJK at distance 0 with a stale 45° direction;
/// 0.31.1 gets the face normal.
#[test]
fn capsule_centred_touching_cuboid_gets_face_normal() {
    let block = Cuboid::new(v(8.0, 8.0, 8.0));
    let capsule = Capsule::new_y(0.7, 0.3);
    let c = query::contact(&at(0.0, -8.0, 0.0), &block, &at(0.0, 1.0, 0.0), &capsule, 0.0).unwrap().unwrap();
    assert!((c.normal1 - Vector::Y).length() < 1.0e-4, "normal1 {:?}", c.normal1);
}

/// The game's controller (`game::movement::character_controller`, walking).
fn game_controller() -> KinematicCharacterController {
    KinematicCharacterController {
        autostep: Some(CharacterAutostep {
            max_height: CharacterLength::Absolute(STEP_HEIGHT * UNITS_PER_METER),
            min_width: CharacterLength::Relative(0.5),
            include_dynamic_bodies: false,
        }),
        snap_to_ground: Some(CharacterLength::Absolute(GROUND_SNAP * UNITS_PER_METER)),
        offset: CharacterLength::Absolute(SKIN),
        ..Default::default()
    }
}

/// A floor of block-scale `Voxels`: `chunks_x` chunk bodies along +x (each
/// `chunk` units wide, one block deep), top face at y = `BLOCK`.
fn floor_world(chunks_x: i32, chunk: f32) -> PhysicsWorld {
    let blocks = (chunk / BLOCK) as i32;
    let mut w = PhysicsWorld { params: IntegrationParameters { dt: game::TICK_RATE as f32 / 1000.0, ..Default::default() }, ..Default::default() };
    let cells: Vec<IVector> = (0..blocks).flat_map(|x| (0..blocks).map(move |z| IVector::new(x, 0, z))).collect();
    for i in 0..chunks_x {
        let body = w.bodies.insert(RigidBodyBuilder::fixed().translation(v(i as f32 * chunk, 0.0, 0.0)));
        w.colliders.insert_with_parent(ColliderBuilder::voxels(Vector::splat(BLOCK), &cells), body, &mut w.bodies);
    }
    w
}

fn add_player(w: &mut PhysicsWorld, pos: Vector) -> (RigidBodyHandle, ColliderHandle) {
    let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(pos));
    let collider = w.colliders.insert_with_parent(ColliderBuilder::capsule_y(HALF_HEIGHT, RADIUS), body, &mut w.bodies);
    (body, collider)
}

fn kcc_move(w: &PhysicsWorld, body: RigidBodyHandle, desired: Vector) -> rapier3d::control::EffectiveCharacterMovement {
    let qp = w.broad_phase.as_query_pipeline(
        w.narrow_phase.query_dispatcher(),
        &w.bodies,
        &w.colliders,
        QueryFilter::default().exclude_rigid_body(body),
    );
    let shape = Capsule::new_y(HALF_HEIGHT, RADIUS);
    game_controller().move_shape(w.params.dt, &qp, &shape, w.bodies[body].position(), desired, |_| {})
}

/// D7: upstream's #485 zero-motion depenetration on a body sunk 0.3 units
/// (the `sunk_body.rs` depth) into ground. It runs only for a zero desired
/// translation, and resolves through `QueryDispatcher::contact`, which parry
/// 0.31.1 does not implement for `Voxels` (`Err(Unsupported)`, skipped): on
/// voxel terrain it does nothing. On a full-chunk cuboid collapse it lifts the
/// body straight up to the skin gap — what `depenetration_lift` does. A
/// walking body is never depenetrated.
#[test]
fn kcc_zero_motion_depenetration_is_cuboid_only() {
    const SINK: f32 = 0.3;
    let chunk = 2.0 * BLOCK;
    let sunk_y = BLOCK + HALF_HEIGHT + RADIUS - SINK;
    let capsule = Capsule::new_y(HALF_HEIGHT, RADIUS);
    let floor = voxel_floor(BLOCK);
    let contact = query::contact(&at(0.5 * BLOCK, sunk_y, 0.5 * BLOCK), &capsule, &Pose::IDENTITY, &floor, 0.0);
    assert!(contact.is_err(), "capsule-Voxels contact is unsupported upstream: {contact:?}");

    for (name, x, z) in [("voxel middle", 0.5 * BLOCK, 0.5 * BLOCK), ("seam", BLOCK, 0.5 * BLOCK), ("chunk corner", chunk, BLOCK)] {
        let mut w = floor_world(2, chunk);
        let (body, _) = add_player(&mut w, v(x, sunk_y, z));
        w.step(&mut PhysicsPipeline::new());
        let still = kcc_move(&w, body, Vector::ZERO);
        let walking = kcc_move(&w, body, v(WALK_SPEED * UNITS_PER_METER * w.params.dt, 0.0, 0.0));
        println!("D7 voxels {name:<13} still: {:?} grounded {}; walking dy {}", still.translation, still.grounded, walking.translation.y);
        assert_eq!(still.translation, Vector::ZERO, "{name}: #485 moved a body sunk into voxels");
        // A seam edge may tilt the slide a few thousandths; no depenetration.
        assert!(walking.translation.y.abs() < 0.1 * SINK, "{name}: walking lifted by {}", walking.translation.y);
    }

    let mut w = PhysicsWorld { params: IntegrationParameters { dt: game::TICK_RATE as f32 / 1000.0, ..Default::default() }, ..Default::default() };
    let ground = w.bodies.insert(RigidBodyBuilder::fixed());
    let half = 0.5 * chunk;
    let cuboid = ColliderBuilder::cuboid(half, half, half).translation(v(half, BLOCK - half, half));
    w.colliders.insert_with_parent(cuboid, ground, &mut w.bodies);
    let (body, _) = add_player(&mut w, v(0.5 * BLOCK, sunk_y, 0.5 * BLOCK));
    w.step(&mut PhysicsPipeline::new());
    let still = kcc_move(&w, body, Vector::ZERO);
    let walking = kcc_move(&w, body, v(WALK_SPEED * UNITS_PER_METER * w.params.dt, 0.0, 0.0));
    println!("D7 cuboid still: {:?} grounded {}; walking dy {}", still.translation, still.grounded, walking.translation.y);
    let t = still.translation;
    assert!(t.x.abs() < 1.0e-4 && t.z.abs() < 1.0e-4, "#485 pushes sideways: {t:?}");
    assert!((t.y - (SINK + SKIN)).abs() < 1.0e-3, "#485 lift {} vs sink + skin {}", t.y, SINK + SKIN);
    assert!(walking.translation.y.abs() < 0.1 * SINK, "walking lifted by {}", walking.translation.y);
}

/// D7, the case `depenetration_lift` declines: a body buried deeper than
/// `STEP_HEIGHT` gets no lift (the game's "buried stays put" policy), so #485
/// is what acts on it — on a full-chunk cuboid, with zero desired motion, it
/// pushes the body out by up to a quarter of the character height per call.
#[test]
fn kcc_zero_motion_moves_a_body_the_lift_leaves_buried() {
    let chunk = 2.0 * BLOCK;
    let sink = STEP_HEIGHT * UNITS_PER_METER + 1.0;
    let mut w = PhysicsWorld { params: IntegrationParameters { dt: game::TICK_RATE as f32 / 1000.0, ..Default::default() }, ..Default::default() };
    let ground = w.bodies.insert(RigidBodyBuilder::fixed());
    let half = 0.5 * chunk;
    w.colliders.insert_with_parent(ColliderBuilder::cuboid(half, half, half).translation(v(half, BLOCK - half, half)), ground, &mut w.bodies);
    let (body, _) = add_player(&mut w, v(0.5 * BLOCK, BLOCK + HALF_HEIGHT + RADIUS - sink, 0.5 * BLOCK));
    w.step(&mut PhysicsPipeline::new());
    let still = kcc_move(&w, body, Vector::ZERO);
    let quarter_height = 0.25 * 2.0 * (HALF_HEIGHT + RADIUS);
    println!("D7 cuboid buried {sink}: still {:?} (cap {quarter_height})", still.translation);
    assert!(still.translation.y > 0.5 * quarter_height, "#485 moved a buried body: {:?}", still.translation);
}

/// Per tick: `(y, grounded, capsule's BVH leaf changed)`.
fn kcc_run(w: &mut PhysicsWorld, body: RigidBodyHandle, collider: ColliderHandle, ticks: usize, walk: f32) -> Vec<(f32, bool, bool)> {
    let mut pipeline = PhysicsPipeline::new();
    w.step(&mut pipeline);
    let leaf = |w: &PhysicsWorld| {
        let qp = w.broad_phase.as_query_pipeline(w.narrow_phase.query_dispatcher(), &w.bodies, &w.colliders, QueryFilter::default());
        qp.bvh.leaf_node(collider.into_raw_parts().0).unwrap().aabb()
    };
    let mut vel_y = 0.0f32;
    (0..ticks)
        .map(|_| {
            let dt = w.params.dt;
            vel_y -= GRAVITY * UNITS_PER_METER * dt;
            let m = kcc_move(w, body, v(walk * dt, vel_y * dt, 0.0));
            if m.grounded {
                vel_y = 0.0;
            }
            let p = w.bodies[body].translation() + m.translation;
            w.bodies[body].set_next_kinematic_translation(p);
            let before = leaf(w);
            w.step(&mut pipeline);
            (w.bodies[body].translation().y, m.grounded, leaf(w) != before)
        })
        .collect()
}

fn walker_on_floor(chunks_x: i32, length_unit: f32) -> (PhysicsWorld, RigidBodyHandle, ColliderHandle) {
    let mut w = floor_world(chunks_x, 2.0 * BLOCK);
    w.params.length_unit = length_unit;
    let (body, collider) = add_player(&mut w, v(0.5 * BLOCK, BLOCK + HALF_HEIGHT + RADIUS + SKIN, 0.5 * BLOCK));
    (w, body, collider)
}

fn max_dy(run: &[(f32, bool, bool)]) -> f32 {
    run.windows(2).map(|p| (p[1].0 - p[0].0).abs()).fold(0.0, f32::max)
}

/// D6: `length_unit` 1 (today) vs `UNITS_PER_METER`. It scales the broad
/// phase's change-detection margin (0.04 × length_unit), contact prediction
/// and recycling, and sleep thresholds — none of which a kinematic-only world
/// driven by the character controller reads. Measured: identical walking
/// trajectories; the walking capsule's BVH leaf changes every tick either
/// way (3.45 units/tick dwarfs both margins); a standing body changes it only
/// on its settling tick either way. (Separately, `BroadPhaseBvh`'s bytes
/// change every step regardless: its serialized `frame_index` counts steps.)
#[test]
fn length_unit_does_not_change_character_movement() {
    const TICKS: usize = 120;
    const STAND_TICKS: usize = 1200;
    let walk = WALK_SPEED * UNITS_PER_METER;
    let mut runs = Vec::new();
    for length_unit in [1.0, UNITS_PER_METER] {
        let (mut w, body, collider) = walker_on_floor(16, length_unit);
        let walked = kcc_run(&mut w, body, collider, TICKS, walk);
        let (mut w, body, collider) = walker_on_floor(2, length_unit);
        let stood = kcc_run(&mut w, body, collider, STAND_TICKS, 0.0);
        let dirty = |run: &[(f32, bool, bool)]| run.iter().filter(|t| t.2).count();
        println!(
            "D6 length_unit {length_unit:>4}: walking leaf updates {}/{TICKS}, max |dy| {:.4}; standing leaf updates {}/{STAND_TICKS}",
            dirty(&walked),
            max_dy(&walked),
            dirty(&stood)
        );
        assert_eq!(dirty(&walked), TICKS);
        assert!(dirty(&stood) <= 1, "a standing body's leaf settles once");
        runs.push(walked);
    }
    assert_eq!(runs[0], runs[1], "length_unit changed the walk");
}

/// Walking straight across a flat block-scale voxel floor. Upstream
/// (Task 1.2) the body dips ~1.16 units just before some chunk seams and
/// pops back up ungrounded (x ≈ 28 and 92 on this floor) — just before the
/// seams between chunk BODIES (32, 96), never at the voxel borders inside a
/// body. What `walking_smoothness.rs` pins in the game; the probe does not
/// isolate which fork fix it needs: the voxels cast (3.2), the cross-collider
/// `cast_shape_on_continuous_surfaces` (3.3, which upstream `move_shape`
/// doesn't use) or the KCC snap gate (3.4).
#[test]
#[ignore = "fails on pristine parry 0.31.1 (seam dips of ~1.2 units at body seams): re-port scope, Tasks 3.2-3.4 together (cause not isolated)"]
fn walk_on_flat_voxel_floor_keeps_its_height() {
    const TICKS: usize = 120;
    let (mut w, body, collider) = walker_on_floor(16, 1.0);
    let run = kcc_run(&mut w, body, collider, TICKS, WALK_SPEED * UNITS_PER_METER);
    let grounded = run.iter().filter(|t| t.1).count();
    assert!(w.bodies[body].translation().x > 8.0 * 2.0 * BLOCK, "the walk crossed at least eight chunk seams");
    assert_eq!(grounded, TICKS, "a walk on flat ground stays grounded");
    assert!(max_dy(&run) < 5.0e-3, "flat walking keeps its height (max |dy| {})", max_dy(&run));
}
