//! Upgrade plan Task 3.5: `crates/game/tests/pair_filter.rs` against the
//! vendored rapier 0.36, which has no fork patch for this: upstream filters
//! pairs at creation by same parent, `ActiveCollisionTypes` and collision
//! groups, and re-inserts a collider's broad-phase leaf when a filter input
//! changes (`broad_phase_bvh/update.rs`, PARENT / PARENT_EFFECTIVE_DOMINANCE
//! — set on a parent's TYPE change — / GROUPS).
//!
//! Differences from the old fork's file:
//! - Kinematic↔fixed pairs are filtered too (default `ActiveCollisionTypes`),
//!   so `fixed_kinematic_pair_is_not_suppressed` is inverted.
//! - The old fork forbade leaving `Fixed` with colliders attached
//!   (`set_body_type` / `set_parent` panicked, since it had no re-pairing
//!   path). Upstream re-pairs, so those two panic tests are replaced by
//!   tests that the suppressed pair appears after the change.
//! - `broad_phase_pairs_hash_survives_serde_round_trip` pinned the old fork's
//!   cached pairs digest, which 0.36 does not have (hashing is Task 4.4); what
//!   is kept is the round trip itself.
use physics_next::PhysicsWorld;
use rapier3d::prelude::*;

fn cuboid() -> ColliderBuilder {
    ColliderBuilder::cuboid(1.0, 1.0, 1.0)
}

fn world() -> (PhysicsWorld, PhysicsPipeline) {
    let w = PhysicsWorld { gravity: Vector::new(0.0, -9.81, 0.0), ..Default::default() };
    (w, PhysicsPipeline::new())
}

fn attach(w: &mut PhysicsWorld, body: RigidBodyBuilder, collider: ColliderBuilder) -> (RigidBodyHandle, ColliderHandle) {
    let b = w.bodies.insert(body);
    let c = w.colliders.insert_with_parent(collider, b, &mut w.bodies);
    (b, c)
}

/// Two overlapping fixed cuboids + one dynamic ball overlapping both.
/// Returns (fixed collider A, fixed collider B, ball collider) and the
/// second fixed body.
fn two_fixed_one_ball(
    w: &mut PhysicsWorld,
    fixed_collider: impl Fn() -> ColliderBuilder,
) -> (ColliderHandle, ColliderHandle, ColliderHandle, RigidBodyHandle) {
    let (_, ca) = attach(w, RigidBodyBuilder::fixed(), fixed_collider());
    let (b, cb) = attach(w, RigidBodyBuilder::fixed().translation(Vector::new(1.5, 0.0, 0.0)), fixed_collider());
    let (_, cball) = attach(
        w,
        RigidBodyBuilder::dynamic().translation(Vector::new(0.75, 1.2, 0.0)),
        ColliderBuilder::ball(0.5),
    );
    (ca, cb, cball, b)
}

fn edges(w: &PhysicsWorld) -> usize {
    w.narrow_phase.contact_graph().raw_graph().raw_edges().len()
}

#[test]
fn fixed_fixed_pairs_are_not_created() {
    let (mut w, mut pipeline) = world();
    let (ca, cb, cball, _) = two_fixed_one_ball(&mut w, cuboid);
    w.step(&mut pipeline);

    // The two fixed cuboids overlap but must have NO interaction-graph edge.
    assert!(w.narrow_phase.contact_pair(ca, cb).is_none(), "fixed-fixed pair must be suppressed");
    // The dynamic ball must still pair with both fixed cuboids.
    assert!(w.narrow_phase.contact_pair(ca, cball).is_some(), "dynamic-fixed pair must exist");
    assert!(w.narrow_phase.contact_pair(cb, cball).is_some(), "dynamic-fixed pair must exist");
    // The whole graph holds exactly the two ball edges.
    assert_eq!(edges(&w), 2);
}

#[test]
fn fixed_fixed_opt_in_still_pairs() {
    let (mut w, mut pipeline) = world();
    // Both opt in; a one-sided variant is asserted below.
    let opt_in = || cuboid().active_collision_types(ActiveCollisionTypes::default() | ActiveCollisionTypes::FIXED_FIXED);
    let (ca, cb, _, _) = two_fixed_one_ball(&mut w, opt_in);
    w.step(&mut pipeline);

    let pair = w.narrow_phase.contact_pair(ca, cb).expect("opted-in fixed-fixed pair must exist");
    assert!(pair.has_any_active_contact(), "overlapping opted-in fixed cuboids must produce actual contacts");
}

#[test]
fn fixed_fixed_opt_in_one_sided_still_pairs() {
    let (mut w, mut pipeline) = world();
    let (_, ca) = attach(
        &mut w,
        RigidBodyBuilder::fixed(),
        cuboid().active_collision_types(ActiveCollisionTypes::default() | ActiveCollisionTypes::FIXED_FIXED),
    );
    // Default flags on this side — the pair must still be kept.
    let (_, cb) = attach(&mut w, RigidBodyBuilder::fixed().translation(Vector::new(1.5, 0.0, 0.0)), cuboid());
    w.step(&mut pipeline);

    assert!(w.narrow_phase.contact_pair(ca, cb).is_some(), "one-sided opt-in must keep the pair");
}

#[test]
fn fixed_sensor_with_opt_in_still_intersects_fixed() {
    let (mut w, mut pipeline) = world();
    let (_, c_solid) = attach(&mut w, RigidBodyBuilder::fixed(), cuboid());
    let (_, c_sensor) = attach(
        &mut w,
        RigidBodyBuilder::fixed().translation(Vector::new(0.5, 0.0, 0.0)),
        cuboid()
            .sensor(true)
            .active_collision_types(ActiveCollisionTypes::default() | ActiveCollisionTypes::FIXED_FIXED),
    );
    w.step(&mut pipeline);

    assert_eq!(
        w.narrow_phase.intersection_pair(c_sensor, c_solid),
        Some(true),
        "opted-in fixed sensor vs fixed solid must intersect"
    );
}

/// Replaces the old fork's `fixed_to_dynamic_with_colliders_panics`: a body
/// that leaves `Fixed` gets the pairs the filter suppressed while it was fixed.
#[test]
fn fixed_to_dynamic_with_colliders_re_pairs() {
    let (mut w, mut pipeline) = world();
    let (ca, cb, _, b) = two_fixed_one_ball(&mut w, cuboid);
    w.step(&mut pipeline);
    assert!(w.narrow_phase.contact_pair(ca, cb).is_none());

    w.bodies[b].set_body_type(RigidBodyType::Dynamic, true);
    w.step(&mut pipeline);
    assert!(
        w.narrow_phase.contact_pair(ca, cb).is_some(),
        "the fixed-fixed pair suppressed at creation must appear once one side is dynamic"
    );
}

#[test]
fn fixed_to_dynamic_without_colliders_is_allowed() {
    let mut bodies = RigidBodySet::new();
    let h = bodies.insert(RigidBodyBuilder::fixed());
    bodies[h].set_body_type(RigidBodyType::Dynamic, true);
    assert!(bodies[h].is_dynamic());
}

#[test]
fn kinematic_to_dynamic_with_colliders_is_allowed() {
    let (mut w, _) = world();
    let (h, _) = attach(&mut w, RigidBodyBuilder::kinematic_position_based(), cuboid());
    w.bodies[h].set_body_type(RigidBodyType::Dynamic, true);
    assert!(w.bodies[h].is_dynamic());
}

/// Inverted from the old fork's `fixed_kinematic_pair_is_not_suppressed`:
/// upstream's default `ActiveCollisionTypes` drops kinematic↔fixed at pair
/// creation (the spike's ~1 KB narrow phase), and the pair is re-discovered
/// when the kinematic body turns dynamic.
#[test]
fn fixed_kinematic_pair_is_filtered_at_creation() {
    let (mut w, mut pipeline) = world();
    let (_, c_fixed) = attach(&mut w, RigidBodyBuilder::fixed(), cuboid());
    let (kinematic, c_kinematic) = attach(
        &mut w,
        RigidBodyBuilder::kinematic_position_based().translation(Vector::new(0.5, 0.0, 0.0)),
        cuboid(),
    );
    w.step(&mut pipeline);
    assert!(
        w.narrow_phase.contact_pair(c_fixed, c_kinematic).is_none(),
        "kinematic-fixed pair must be filtered at creation"
    );
    assert_eq!(edges(&w), 0);

    w.bodies[kinematic].set_body_type(RigidBodyType::Dynamic, true);
    w.step(&mut pipeline);
    assert!(
        w.narrow_phase.contact_pair(c_fixed, c_kinematic).is_some(),
        "turning the kinematic body dynamic must re-insert it and create the pair"
    );
}

/// Replaces the old fork's `set_parent_fixed_to_dynamic_with_collider_panics`:
/// re-parenting a collider off a fixed body re-inserts its leaf (PARENT), so
/// the pair suppressed while it sat on the fixed body appears.
#[test]
fn set_parent_fixed_to_dynamic_re_pairs() {
    let (mut w, mut pipeline) = world();
    let (ca, cb, _, _) = two_fixed_one_ball(&mut w, cuboid);
    w.step(&mut pipeline);
    assert!(w.narrow_phase.contact_pair(ca, cb).is_none());

    let dynamic = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(1.5, 0.0, 0.0)));
    w.colliders.set_parent(cb, Some(dynamic), &mut w.bodies);
    w.step(&mut pipeline);
    assert!(
        w.narrow_phase.contact_pair(ca, cb).is_some(),
        "the pair suppressed on the fixed parent must appear on the dynamic one"
    );
}

#[test]
fn set_parent_dynamic_to_fixed_is_allowed() {
    let (mut w, _) = world();
    let (_, c) = attach(&mut w, RigidBodyBuilder::dynamic(), cuboid());
    let fixed = w.bodies.insert(RigidBodyBuilder::fixed());
    w.colliders.set_parent(c, Some(fixed), &mut w.bodies);
    assert_eq!(w.colliders[c].parent(), Some(fixed));
}

/// A parked region serializes its broad and narrow phases; both must come
/// back byte-identical (Review Focus 5). The old fork's pairs-digest check
/// moves to Task 4.4 with the new hashing.
#[test]
fn broad_and_narrow_phase_survive_serde_round_trip() {
    let (mut w, mut pipeline) = world();
    two_fixed_one_ball(&mut w, cuboid);
    w.step(&mut pipeline);
    assert_eq!(edges(&w), 2, "scene must have live pairs");

    let broad = bincode::serialize(&w.broad_phase).expect("serialize broad phase");
    let restored: BroadPhaseBvh = bincode::deserialize(&broad).expect("deserialize broad phase");
    assert_eq!(bincode::serialize(&restored).unwrap(), broad, "broad phase must round-trip in full");

    let narrow = bincode::serialize(&w.narrow_phase).expect("serialize narrow phase");
    let restored: NarrowPhase = bincode::deserialize(&narrow).expect("deserialize narrow phase");
    assert_eq!(bincode::serialize(&restored).unwrap(), narrow, "narrow phase must round-trip in full");
}
