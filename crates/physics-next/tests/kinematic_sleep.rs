//! `RigidBodyBuilder::can_sleep(false)` must keep a kinematic body awake.
//! Upstream 0.36's `RigidBodyActivation::update_energy` lets a kinematic body
//! sleep once both its velocities are exactly zero, without looking at the
//! thresholds `can_sleep(false)` sets to −1 (`// mmo-fork:` there). The game
//! builds every player and mob `can_sleep(false)`: their poses reach the
//! renderer only from the awake island, every tick. A standing player or a
//! resting slime fell asleep after 0.5 s (10 region ticks once `dt` is
//! `TICK_DT`, 30 at rapier's default 1/60).
use physics_next::PhysicsWorld;
use rapier3d::prelude::*;

fn world_with(body: RigidBodyBuilder) -> (PhysicsWorld, RigidBodyHandle) {
    let mut w = PhysicsWorld { params: game::PhysicsParams::default().0, ..Default::default() };
    let h = w.bodies.insert(body.translation(Vector::new(0.0, 10.0, 0.0)));
    w.colliders.insert_with_parent(ColliderBuilder::cuboid(1.0, 1.0, 1.0), h, &mut w.bodies);
    (w, h)
}

/// Standing still for 2 s of steps.
fn stand(w: &mut PhysicsWorld) {
    let mut pipeline = PhysicsPipeline::new();
    let steps = (2.0 / w.params.dt) as usize;
    for _ in 0..steps {
        w.step(&mut pipeline);
    }
}

#[test]
fn a_kinematic_body_that_cannot_sleep_stays_awake_at_rest() {
    let (mut w, h) = world_with(RigidBodyBuilder::kinematic_position_based().can_sleep(false));
    stand(&mut w);
    assert!(!w.bodies[h].is_sleeping(), "can_sleep(false) is ignored");
    assert!(w.islands.active_bodies().any(|b| b == h), "not in the awake island");
}

/// Upstream's rule still holds for the default: a kinematic platform at rest
/// sleeps.
#[test]
fn a_kinematic_body_that_can_sleep_sleeps_at_rest() {
    let (mut w, h) = world_with(RigidBodyBuilder::kinematic_position_based());
    stand(&mut w);
    assert!(w.bodies[h].is_sleeping());
}
