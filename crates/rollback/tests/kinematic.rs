//! Regression test for the camera-controller desync: kinematic body
//! mutations wake the body and mark it modified (state that is hashed but
//! was not restored by the old surgical undo closures), and the physics
//! step drains those flags again. Mutation + step per transaction, rolled
//! back across every boundary, must be bit-exact.
use std::hash::Hash;

use nalgebra::{UnitQuaternion, Vector3};
use parry3d::math::Real;
use rapier3d::prelude::PhysicsPipeline;
use rollback::Rollback;

fn state_hash(r: &Rollback) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    r.hash(&mut hasher);
    hasher.finalize()
}

#[test]
fn kinematic_mutation_and_step_roll_back_exactly() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    let mut pipeline = PhysicsPipeline::new();

    r.new_transaction();
    r.create_player_safe(7);
    let mut boundaries = vec![state_hash(&r)];

    for i in 0..3u32 {
        r.new_transaction();
        let e = *r.player_entites.get(&7).unwrap();
        let handle = *r.ecs.rigidbody.get(e);
        // Camera-controller pattern: kinematic mutation under a change()
        // snapshot (wakes the body + marks it modified).
        {
            let bodies = r.physics.bodies.change();
            bodies.get_mut(handle).unwrap().set_next_kinematic_rotation(
                UnitQuaternion::from_axis_angle(
                    &Vector3::y_axis(),
                    Real::from(0.01 * (i + 1) as f32),
                ),
            );
        }
        // Physics-controller pattern: snapshot_raw + a real pipeline step
        // (drains modified flags, advances poses).
        {
            let p = r.physics.snapshot_raw();
            pipeline.step(
                p.gravity,
                p.integration_parameters,
                p.islands,
                p.broad_phase,
                p.narrow_phase,
                p.bodies,
                p.colliders,
                p.implules_joint_set,
                p.multi_body_joint_set,
                p.ccd_solver,
                &(),
                &(),
            );
        }
        boundaries.push(state_hash(&r));
    }

    for expected in boundaries.iter().rev().skip(1) {
        r.rollback();
        assert_eq!(*expected, state_hash(&r), "boundary hash mismatch");
    }
}
