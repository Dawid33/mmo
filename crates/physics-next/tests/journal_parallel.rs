//! H7 review I1: the step journal's first-touch capture is not thread-safe
//! (rayon chunks of the body/collider passes and the staged solver's workers
//! would push into one shared buffer), and the game never enables rapier's
//! `parallel` feature. So `step_journaled` with a journal refuses to run in a
//! `parallel` build instead of silently racing. Run with
//! `just test-one physics-next "--features parallel --test journal_parallel"`.
#![cfg(feature = "parallel")]

use physics_next::PhysicsWorld;
use rapier3d::prelude::*;

#[test]
#[should_panic(expected = "step_journaled does not support the `parallel` feature")]
fn journaled_step_refuses_the_parallel_feature() {
    let mut w = PhysicsWorld { gravity: Vector::new(0.0, -9.81, 0.0), ..Default::default() };
    let b = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(0.0, 1.0, 0.0)));
    w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), b, &mut w.bodies);
    let mut p = PhysicsPipeline::new();
    let _ = w.step_journaled(&mut p);
}

#[test]
fn plain_step_still_runs_under_the_parallel_feature() {
    let mut w = PhysicsWorld { gravity: Vector::new(0.0, -9.81, 0.0), ..Default::default() };
    let b = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(0.0, 1.0, 0.0)));
    w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), b, &mut w.bodies);
    let mut p = PhysicsPipeline::new();
    w.step(&mut p);
    assert!(w.bodies[b].translation().y < 1.0);
}
