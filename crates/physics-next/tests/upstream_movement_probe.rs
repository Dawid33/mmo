//! Upgrade plan Task 1.2: the game's movement-critical parry/rapier fixes,
//! translated to glam. Written against PRISTINE parry 0.31.1 / rapier 0.36.0,
//! where the cases upstream failed were `#[ignore]`d; they now run against
//! the vendored `*-next` forks, and every ignore came off as its
//! `// mmo-fork:` patch landed (Tasks 3.2-3.4, H6).
//!
//! - The five `crates/game/tests/shape_cast_grazing.rs` cases and
//!   `gjk_upstream.rs::capsule_touching_cuboid_matches_upstream`, with their
//!   assertions unchanged.
//! - The centred-capsule contact the nalgebra-era fork gets wrong (Task 5.4
//!   Step 1b adds it to `gjk_upstream.rs` after the switch).
//! - Probes for design decisions D7 (upstream's zero-motion KCC
//!   depenetration, #485, and the fork's `depenetrate` flag) and D6
//!   (`IntegrationParameters::length_unit`); run with `-- --nocapture` for
//!   the numbers in `docs/superpowers/specs/2026-09-22-rapier-035-spike-results.md`.
use game::movement::{
    AUTOSTEP_MIN_WIDTH_M, CHARACTER_SKIN, GROUND_SNAP, PLAYER_CAPSULE_HALF_HEIGHT,
    PLAYER_CAPSULE_RADIUS, STEP_HEIGHT, UNITS_PER_METER,
};
use physics_next::PhysicsWorld;
use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::parry::query::{ShapeCastHit, ShapeCastOptions, ShapeCastStatus, cast_shapes};
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
    assert!(
        hit.time_of_impact < 1.0e-3,
        "{context}: toi {}",
        hit.time_of_impact
    );
    let n = hit.normal1;
    assert!(
        n.x.abs() < 1.0e-4 && n.z.abs() < 1.0e-4 && n.y > 0.9999,
        "{context}: a flat floor's normal is straight up, got ({}, {}, {})",
        n.x,
        n.y,
        n.z
    );
}

/// Gate: mmo-fork hunk parry shape_cast_support_map_support_map.rs:113 normal from closest points at impact pose; seam bug 4173c4ef (ignored upstream, un-ignored by d54e90b7)
/// shape_cast_grazing.rs: starting on the target-distance-rounded surface,
/// the cast missed or returned a stale search direction as the normal.
///
/// Upstream (Task 1.2): no miss, and the normal is within ~2.6e-3 of +Y —
/// but not the test's 1e-4 — and a start 1.5e-3 outside the skin gets the
/// geometric TOI (~2.3e-3) where the fork's `GRAZING_START_MARGIN` reports
/// a resting contact (< 1e-3).
#[test]
fn cast_starting_at_the_target_distance_hits_with_the_true_normal() {
    let cuboid = Cuboid::new(v(8.0, 8.0, 8.0));
    let capsule = Capsule::new_y(HALF_HEIGHT, RADIUS);
    for gap in [
        SKIN + 0.0015,
        SKIN + 0.0005,
        SKIN,
        SKIN - 0.0005,
        SKIN - 0.006,
    ] {
        for z in [-6.0, -2.5, 0.0, 2.01, 5.0] {
            let pos2 = at(0.3, 8.0 + HALF_HEIGHT + RADIUS + gap, z);
            let hit = cast_shapes(
                &Pose::IDENTITY,
                Vector::ZERO,
                &cuboid,
                &pos2,
                walking_step(),
                &capsule,
                skin_cast(1.0),
            )
            .unwrap();
            assert_floor_hit(hit, &format!("gap {gap} z {z}"));
        }
    }
}

/// Gate: mmo-fork hunk parry shape_cast_support_map_support_map.rs:9/14/30 grazing-start policy (d54e90b7; seam bug 4173c4ef)
#[test]
fn cast_moving_away_from_within_the_target_distance_is_free() {
    let cuboid = Cuboid::new(v(8.0, 8.0, 8.0));
    let capsule = Capsule::new_y(HALF_HEIGHT, RADIUS);
    let pos2 = at(0.0, 8.0 + HALF_HEIGHT + RADIUS + SKIN, 0.0);
    for vel in [v(0.0, 0.0, 3.0), v(0.0, 1.0, 3.0)] {
        let hit = cast_shapes(
            &Pose::IDENTITY,
            Vector::ZERO,
            &cuboid,
            &pos2,
            vel,
            &capsule,
            skin_cast(1.0),
        )
        .unwrap();
        assert!(
            hit.is_none(),
            "sliding along or lifting off the floor is no hit: {hit:?}"
        );
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
    cast_shapes(
        &Pose::IDENTITY,
        Vector::ZERO,
        floor,
        &pos2,
        vel / distance,
        &capsule,
        skin_cast(distance),
    )
    .unwrap()
}

/// Gate: mmo-fork hunk parry shape_cast_voxels_shape.rs:1 search domain seeded from loosened AABB (b6abc1e9; failed on pristine; seam bug 4173c4ef)
/// shape_cast_grazing.rs: the voxels cast searched under the shape's bare
/// AABB, moved with the motion instead of grown — a hovering shape crossing a
/// voxel border missed the voxel under it.
#[test]
/// Upstream (Task 1.2): all three casts miss the floor. Fixed in the fork by
/// Task 3.2 (loosened, grown search domain).
fn voxels_cast_sees_the_voxel_under_a_hovering_shape() {
    let floor = voxel_floor(BLOCK);
    for z in [
        BLOCK + 9.443,
        2.0 * BLOCK - RADIUS - 0.16,
        2.0 * BLOCK - RADIUS - 0.01,
    ] {
        assert_floor_hit(
            cast_on_floor(&floor, BLOCK, 1.5 * BLOCK, z, walking_step()),
            &format!("z {z}"),
        );
    }
}

/// Gate: mmo-fork hunk parry shape_cast_voxels_shape.rs without_internal_edges (b6abc1e9; failed on pristine; seam bug 4173c4ef)
/// shape_cast_grazing.rs: every voxel was cast as a free-standing box, so a
/// flat floor's seams reported their edges.
///
/// Upstream (Task 1.2): every start 1e-3 outside the skin gets the geometric
/// TOI (~5.3e-3) instead of a resting contact, and next to a seam (x = 1.01)
/// the normal leans ~4e-3 (0.24°) — far smaller than the fork's old
/// degrees-off bug, still above the 1e-4 bar. The slide is free. Fixed in
/// the fork by Task 3.2 (resting contact within the grazing margin, covered
/// faces dropped from the normal).
#[test]
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

/// The game's controller (`game::movement::character_controller`, walking).
fn game_controller() -> KinematicCharacterController {
    KinematicCharacterController {
        autostep: Some(CharacterAutostep {
            max_height: CharacterLength::Absolute(STEP_HEIGHT * UNITS_PER_METER),
            min_width: CharacterLength::Absolute(AUTOSTEP_MIN_WIDTH_M * UNITS_PER_METER),
            include_dynamic_bodies: false,
        }),
        snap_to_ground: Some(CharacterLength::Absolute(GROUND_SNAP * UNITS_PER_METER)),
        offset: CharacterLength::Absolute(SKIN),
        ..Default::default()
    }
}

fn add_player(w: &mut PhysicsWorld, pos: Vector) -> (RigidBodyHandle, ColliderHandle) {
    let body = w
        .bodies
        .insert(RigidBodyBuilder::kinematic_position_based().translation(pos));
    let collider = w.colliders.insert_with_parent(
        ColliderBuilder::capsule_y(HALF_HEIGHT, RADIUS),
        body,
        &mut w.bodies,
    );
    (body, collider)
}

fn kcc_move(
    w: &PhysicsWorld,
    body: RigidBodyHandle,
    desired: Vector,
) -> rapier3d::control::EffectiveCharacterMovement {
    kcc_move_with(&game_controller(), w, body, desired)
}

fn kcc_move_with(
    controller: &KinematicCharacterController,
    w: &PhysicsWorld,
    body: RigidBodyHandle,
    desired: Vector,
) -> rapier3d::control::EffectiveCharacterMovement {
    let qp = w.broad_phase.as_query_pipeline(
        w.narrow_phase.query_dispatcher(),
        &w.bodies,
        &w.colliders,
        QueryFilter::default().exclude_rigid_body(body),
    );
    let shape = Capsule::new_y(HALF_HEIGHT, RADIUS);
    controller.move_shape(
        w.params.dt,
        &qp,
        &shape,
        w.bodies[body].position(),
        desired,
        |_| {},
    )
}

/// Gate: mmo-fork (D7) hunk rapier character_controller.rs:206/225/323 `depenetrate` flag: asserts default true and false leaves a buried body (38cd3da5)
/// D7, the case `depenetration_lift` declines: a body buried deeper than
/// `STEP_HEIGHT` gets no lift (the game's "buried stays put" policy). With
/// upstream's default (`depenetrate: true`), #485 acts on it instead — on a
/// full-chunk cuboid, with zero desired motion, it pushes the body out by up
/// to a quarter of the character height per call. The fork's
/// `KinematicCharacterController::depenetrate` flag (D7; the game sets
/// `false`) turns that off: the buried body is not moved.
#[test]
fn kcc_zero_motion_moves_a_body_the_lift_leaves_buried() {
    let chunk = 2.0 * BLOCK;
    let sink = STEP_HEIGHT * UNITS_PER_METER + 1.0;
    let mut w = PhysicsWorld {
        params: IntegrationParameters {
            dt: game::TICK_RATE as f32 / 1000.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let ground = w.bodies.insert(RigidBodyBuilder::fixed());
    let half = 0.5 * chunk;
    w.colliders.insert_with_parent(
        ColliderBuilder::cuboid(half, half, half).translation(v(half, BLOCK - half, half)),
        ground,
        &mut w.bodies,
    );
    let (body, _) = add_player(
        &mut w,
        v(
            0.5 * BLOCK,
            BLOCK + HALF_HEIGHT + RADIUS - sink,
            0.5 * BLOCK,
        ),
    );
    w.step(&mut PhysicsPipeline::new());
    let quarter_height = 0.25 * 2.0 * (HALF_HEIGHT + RADIUS);

    assert!(
        KinematicCharacterController::default().depenetrate,
        "upstream's behaviour stays the default"
    );
    let still = kcc_move(&w, body, Vector::ZERO);
    println!(
        "D7 cuboid buried {sink}: still {:?} (cap {quarter_height})",
        still.translation
    );
    assert!(
        still.translation.y > 0.5 * quarter_height,
        "#485 moved a buried body: {:?}",
        still.translation
    );

    let off = KinematicCharacterController {
        depenetrate: false,
        ..game_controller()
    };
    let still = kcc_move_with(&off, &w, body, Vector::ZERO);
    assert_eq!(
        still.translation,
        Vector::ZERO,
        "depenetrate: false leaves a buried body where it is"
    );
}
