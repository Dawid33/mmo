//! Upgrade plan Task 3.1: the fixed-body mass-properties skip (old fork
//! 34ec12b, pinned there by `crates/game/tests/collider_genesis.rs`
//! `fixed_terrain_bodies_skip_collider_mass_properties`).
//!
//! A fixed body's collider-derived mass is discarded (its effective inverse
//! mass/inertia is zeroed), so the fork never computes it: for terrain
//! `Voxels` it is two full passes over every voxel cell per chunk collider,
//! and the inertia goes through glamx `SymmetricEigen3`, whose std
//! `acos`/`cos` are not bit-identical between native and wasm32 (Review
//! Focus 3). The terrain bodies' `local_mprops` therefore stay at their
//! default through attach and every step, which is what these tests check.
use game::movement::UNITS_PER_METER;
use physics_next::PhysicsWorld;
use rapier3d::prelude::*;

/// One block-scale voxel floor collider (4x1x4 blocks) plus a cuboid, the
/// two collider kinds terrain chunks get.
fn terrain_colliders() -> [ColliderBuilder; 2] {
    let mut cells = Vec::new();
    for x in 0..4 {
        for z in 0..4 {
            cells.push(IVector::new(x, 0, z));
        }
    }
    let half = UNITS_PER_METER * 2.0;
    [
        ColliderBuilder::voxels(Vector::splat(UNITS_PER_METER), &cells),
        ColliderBuilder::cuboid(half, half, half).translation(Vector::new(0.0, -half, 0.0)),
    ]
}

fn world_with(body: RigidBodyBuilder) -> (PhysicsWorld, RigidBodyHandle) {
    let mut w = PhysicsWorld::default();
    let handle = w.bodies.insert(body);
    for collider in terrain_colliders() {
        w.colliders.insert_with_parent(collider, handle, &mut w.bodies);
    }
    (w, handle)
}

/// What upstream would compute for the body: the sum of its colliders' mass
/// properties.
fn collider_mass(w: &PhysicsWorld, body: RigidBodyHandle) -> f32 {
    w.bodies[body].colliders().iter().map(|&co| w.colliders[co].mass_properties().mass()).sum()
}

/// Gate: mmo-fork hunk rapier rigid_body_components.rs:451 fixed bodies skip collider mass properties (dd64debb)
#[test]
fn fixed_terrain_bodies_skip_collider_mass_properties() {
    let (mut w, body) = world_with(RigidBodyBuilder::fixed());
    assert!(collider_mass(&w, body) > 0.0, "the colliders do have mass to skip");

    // At attach (insert_with_parent) ...
    assert_eq!(
        w.bodies[body].mass_properties().local_mprops,
        MassProperties::default(),
        "fixed terrain body computed collider mass properties at attach"
    );
    // ... and at the recompute the first step runs for COLLIDERS changes.
    let mut pipeline = PhysicsPipeline::new();
    for _ in 0..2 {
        w.step(&mut pipeline);
        assert_eq!(
            w.bodies[body].mass_properties().local_mprops,
            MassProperties::default(),
            "fixed terrain body computed collider mass properties in a step"
        );
    }
    let mprops = w.bodies[body].mass_properties();
    assert_eq!(mprops.effective_inv_mass, Vector::ZERO, "fixed stays immovable");
}

/// Gate: mmo-fork hunk rapier user_changes.rs:176 recompute on TYPE change (dd64debb)
/// The skip must not strand a body that stops being fixed: turning it
/// dynamic (the pair filter no longer forbids that with colliders attached,
/// Task 3.5) recomputes the mass on the next step.
#[test]
fn fixed_body_turned_dynamic_gets_its_collider_mass() {
    let (mut w, body) = world_with(RigidBodyBuilder::fixed());
    let mut pipeline = PhysicsPipeline::new();
    w.step(&mut pipeline);
    w.bodies[body].set_body_type(RigidBodyType::Dynamic, true);
    w.step(&mut pipeline);
    let expected = collider_mass(&w, body);
    let mass = w.bodies[body].mass();
    assert!(
        (mass - expected).abs() <= expected * 1e-6,
        "turned-dynamic body has mass {mass}, its colliders {expected}"
    );
}

/// Gate: mmo-fork hunk rapier rigid_body_components.rs:1278 fixed branch resets local_mprops; fix 2cafdbea (turned-fixed body kept its mass, history-dependent hash)
/// The other direction (H5 review M2): a dynamic body turned fixed must
/// drop the mass it had, or its serialized state depends on its history — a
/// born-fixed body and a turned-fixed one with the same colliders would
/// differ in `local_mprops` (and so in snapshots and state hashes).
#[test]
fn dynamic_body_turned_fixed_drops_its_collider_mass() {
    let (mut w, body) = world_with(RigidBodyBuilder::dynamic());
    let mut pipeline = PhysicsPipeline::new();
    w.step(&mut pipeline);
    assert!(w.bodies[body].mass() > 0.0, "dynamic first");
    w.bodies[body].set_body_type(RigidBodyType::Fixed, true);
    w.step(&mut pipeline);
    let (born_fixed, born) = world_with(RigidBodyBuilder::fixed());
    assert_eq!(
        w.bodies[body].mass_properties().local_mprops,
        MassProperties::default(),
        "a turned-fixed body keeps the collider mass it had while dynamic"
    );
    assert_eq!(
        w.bodies[body].mass_properties().local_mprops,
        born_fixed.bodies[born].mass_properties().local_mprops,
        "turned-fixed and born-fixed bodies must agree"
    );
}
