//! Fixed-fixed broad-phase pair filter guarantees, driven through the full
//! pipeline (same pattern as hash_restore.rs: fork behavior tested from the
//! game crate, which owns the dev harness).
use rapier3d::prelude::*;

/// One plain (non-journaled) pipeline step over a minimal world.
fn step(
    pipeline: &mut PhysicsPipeline,
    broad: &mut BroadPhaseBvh,
    narrow: &mut NarrowPhase,
    islands: &mut IslandManager,
    bodies: &mut RigidBodySet,
    colliders: &mut ColliderSet,
) {
    let gravity = Vector::y() * Real::from(-9.81);
    let mut impulse_joints = ImpulseJointSet::new();
    let mut multibody_joints = MultibodyJointSet::new();
    let mut ccd = CCDSolver::new();
    pipeline.step(
        &gravity,
        &IntegrationParameters::default(),
        islands,
        broad,
        narrow,
        bodies,
        colliders,
        &mut impulse_joints,
        &mut multibody_joints,
        &mut ccd,
        &(),
        &(),
    );
}

/// Two overlapping fixed cuboids + one dynamic ball overlapping both.
/// Returns (fixed collider A, fixed collider B, ball collider).
fn two_fixed_one_ball(
    bodies: &mut RigidBodySet,
    colliders: &mut ColliderSet,
    fixed_collider: impl Fn() -> ColliderBuilder,
) -> (ColliderHandle, ColliderHandle, ColliderHandle) {
    let a = bodies.insert(RigidBodyBuilder::fixed().build());
    let ca = colliders.insert_with_parent(fixed_collider().build(), a, bodies);

    let b = bodies.insert(
        RigidBodyBuilder::fixed()
            .translation(Vector::new(Real::from(1.5), Real::from(0.0), Real::from(0.0)))
            .build(),
    );
    let cb = colliders.insert_with_parent(fixed_collider().build(), b, bodies);

    let ball = bodies.insert(
        RigidBodyBuilder::dynamic()
            .translation(Vector::new(Real::from(0.75), Real::from(1.2), Real::from(0.0)))
            .build(),
    );
    let cball = colliders.insert_with_parent(
        ColliderBuilder::ball(Real::from(0.5)).build(),
        ball,
        bodies,
    );
    (ca, cb, cball)
}

fn cuboid() -> ColliderBuilder {
    ColliderBuilder::cuboid(Real::from(1.0), Real::from(1.0), Real::from(1.0))
}

#[test]
fn fixed_fixed_pairs_are_not_created() {
    let (mut pipeline, mut broad, mut narrow, mut islands) = (
        PhysicsPipeline::new(),
        BroadPhaseBvh::new(),
        NarrowPhase::new(),
        IslandManager::new(),
    );
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());
    let (ca, cb, cball) = two_fixed_one_ball(&mut bodies, &mut colliders, cuboid);

    step(&mut pipeline, &mut broad, &mut narrow, &mut islands, &mut bodies, &mut colliders);

    // The two fixed cuboids overlap but must have NO interaction-graph edge.
    assert!(narrow.contact_pair(ca, cb).is_none(), "fixed-fixed pair must be suppressed");
    // The dynamic ball must still pair with both fixed cuboids.
    assert!(narrow.contact_pair(ca, cball).is_some(), "dynamic-fixed pair must exist");
    assert!(narrow.contact_pair(cb, cball).is_some(), "dynamic-fixed pair must exist");
    // The whole graph holds exactly the two ball edges.
    assert_eq!(narrow.contact_graph().raw_graph().raw_edges().len(), 2);
}

#[test]
fn fixed_fixed_opt_in_still_pairs() {
    let (mut pipeline, mut broad, mut narrow, mut islands) = (
        PhysicsPipeline::new(),
        BroadPhaseBvh::new(),
        NarrowPhase::new(),
        IslandManager::new(),
    );
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());
    // Opt-in on the colliders: the narrow-phase gate is an OR of the two
    // colliders' flags, so the filter must keep the pair if EITHER opts in.
    // Here both opt in; a one-sided variant is asserted below.
    let opt_in = || {
        cuboid().active_collision_types(
            ActiveCollisionTypes::default() | ActiveCollisionTypes::FIXED_FIXED,
        )
    };
    let (ca, cb, _) = two_fixed_one_ball(&mut bodies, &mut colliders, opt_in);

    step(&mut pipeline, &mut broad, &mut narrow, &mut islands, &mut bodies, &mut colliders);

    let pair = narrow.contact_pair(ca, cb).expect("opted-in fixed-fixed pair must exist");
    assert!(
        pair.has_any_active_contact,
        "overlapping opted-in fixed cuboids must produce actual contacts"
    );
}

#[test]
fn fixed_fixed_opt_in_one_sided_still_pairs() {
    let (mut pipeline, mut broad, mut narrow, mut islands) = (
        PhysicsPipeline::new(),
        BroadPhaseBvh::new(),
        NarrowPhase::new(),
        IslandManager::new(),
    );
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());

    let a = bodies.insert(RigidBodyBuilder::fixed().build());
    let ca = colliders.insert_with_parent(
        cuboid()
            .active_collision_types(
                ActiveCollisionTypes::default() | ActiveCollisionTypes::FIXED_FIXED,
            )
            .build(),
        a,
        &mut bodies,
    );
    let b = bodies.insert(
        RigidBodyBuilder::fixed()
            .translation(Vector::new(Real::from(1.5), Real::from(0.0), Real::from(0.0)))
            .build(),
    );
    // Default flags on this side — the pair must still be kept.
    let cb = colliders.insert_with_parent(cuboid().build(), b, &mut bodies);

    step(&mut pipeline, &mut broad, &mut narrow, &mut islands, &mut bodies, &mut colliders);

    assert!(narrow.contact_pair(ca, cb).is_some(), "one-sided opt-in must keep the pair");
}

#[test]
fn fixed_sensor_with_opt_in_still_intersects_fixed() {
    let (mut pipeline, mut broad, mut narrow, mut islands) = (
        PhysicsPipeline::new(),
        BroadPhaseBvh::new(),
        NarrowPhase::new(),
        IslandManager::new(),
    );
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());

    let solid = bodies.insert(RigidBodyBuilder::fixed().build());
    let c_solid = colliders.insert_with_parent(cuboid().build(), solid, &mut bodies);

    let sensor_body = bodies.insert(
        RigidBodyBuilder::fixed()
            .translation(Vector::new(Real::from(0.5), Real::from(0.0), Real::from(0.0)))
            .build(),
    );
    let c_sensor = colliders.insert_with_parent(
        cuboid()
            .sensor(true)
            .active_collision_types(
                ActiveCollisionTypes::default() | ActiveCollisionTypes::FIXED_FIXED,
            )
            .build(),
        sensor_body,
        &mut bodies,
    );

    step(&mut pipeline, &mut broad, &mut narrow, &mut islands, &mut bodies, &mut colliders);

    assert_eq!(
        narrow.intersection_pair(c_sensor, c_solid),
        Some(true),
        "opted-in fixed sensor vs fixed solid must intersect"
    );
}

#[test]
#[should_panic(expected = "fixed-fixed pairs are suppressed")]
fn fixed_to_dynamic_with_colliders_panics() {
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());
    let h = bodies.insert(RigidBodyBuilder::fixed().build());
    let _c = colliders.insert_with_parent(cuboid().build(), h, &mut bodies);
    // The broad phase suppressed this body's fixed-fixed pairs at creation;
    // leaving Fixed would need a re-pairing path that does not exist.
    bodies.get_mut(h).unwrap().set_body_type(RigidBodyType::Dynamic, true);
}

#[test]
fn fixed_to_dynamic_without_colliders_is_allowed() {
    let (mut bodies, _colliders) = (RigidBodySet::new(), ColliderSet::new());
    let h = bodies.insert(RigidBodyBuilder::fixed().build());
    // No colliders -> no pairs could have been suppressed -> legal.
    bodies.get_mut(h).unwrap().set_body_type(RigidBodyType::Dynamic, true);
    assert!(bodies.get(h).unwrap().is_dynamic());
}

#[test]
fn kinematic_to_dynamic_with_colliders_is_allowed() {
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());
    let h = bodies.insert(RigidBodyBuilder::kinematic_position_based().build());
    let _c = colliders.insert_with_parent(cuboid().build(), h, &mut bodies);
    // Non-fixed origins never had pairs suppressed; the fork's own
    // physics_pipeline test (line ~1195) also relies on this transition.
    bodies.get_mut(h).unwrap().set_body_type(RigidBodyType::Dynamic, true);
    assert!(bodies.get(h).unwrap().is_dynamic());
}

#[test]
fn fixed_kinematic_pair_is_not_suppressed() {
    let (mut pipeline, mut broad, mut narrow, mut islands) = (
        PhysicsPipeline::new(),
        BroadPhaseBvh::new(),
        NarrowPhase::new(),
        IslandManager::new(),
    );
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());

    let fixed = bodies.insert(RigidBodyBuilder::fixed().build());
    let c_fixed = colliders.insert_with_parent(cuboid().build(), fixed, &mut bodies);

    let kinematic = bodies.insert(
        RigidBodyBuilder::kinematic_position_based()
            .translation(Vector::new(Real::from(0.5), Real::from(0.0), Real::from(0.0)))
            .build(),
    );
    let c_kinematic = colliders.insert_with_parent(cuboid().build(), kinematic, &mut bodies);

    step(&mut pipeline, &mut broad, &mut narrow, &mut islands, &mut bodies, &mut colliders);

    assert!(
        narrow.contact_pair(c_fixed, c_kinematic).is_some(),
        "fixed-kinematic pair must not be suppressed"
    );
}

#[test]
#[should_panic(expected = "fixed-fixed pairs are suppressed")]
fn set_parent_fixed_to_dynamic_with_collider_panics() {
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());
    let fixed = bodies.insert(RigidBodyBuilder::fixed().build());
    let c = colliders.insert_with_parent(cuboid().build(), fixed, &mut bodies);
    let dynamic = bodies.insert(RigidBodyBuilder::dynamic().build());
    // Re-parenting off a fixed body onto a dynamic one hits the same blind
    // spot as `set_body_type`: the broad phase never re-visits the leaf, so
    // any pairs suppressed while it sat on `fixed` would stay missing.
    colliders.set_parent(c, Some(dynamic), &mut bodies);
}

#[test]
fn set_parent_dynamic_to_fixed_is_allowed() {
    let (mut bodies, mut colliders) = (RigidBodySet::new(), ColliderSet::new());
    let dynamic = bodies.insert(RigidBodyBuilder::dynamic().build());
    let c = colliders.insert_with_parent(cuboid().build(), dynamic, &mut bodies);
    let fixed = bodies.insert(RigidBodyBuilder::fixed().build());
    // Non-fixed -> fixed never needed any pairs suppressed while on `dynamic`,
    // so this direction stays legal.
    colliders.set_parent(c, Some(fixed), &mut bodies);
    assert_eq!(colliders.get(c).unwrap().parent(), Some(fixed));
}
