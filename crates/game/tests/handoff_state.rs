use game::{IsometryReal, Rollback};
use game::na::{Quaternion, Translation3, Unit};
use game::parry::math::Real;
use std::hash::Hash;

fn crc(rb: &Rollback) -> u32 {
    let mut h = crc32fast::Hasher::new();
    rb.data.hash(&mut h);
    h.finalize()
}

fn pose(x: f32, y: f32, z: f32) -> IsometryReal {
    IsometryReal::from_parts(
        Translation3::new(Real::from(x), Real::from(y), Real::from(z)),
        Unit::<Quaternion<Real>>::identity(),
    )
}

#[test]
fn remove_entity_safe_holds_the_hash_bar() {
    // Dummy render sender: the generated rollback() replays tier-2 undo
    // closures with `client.as_ref().unwrap()`, so a None client panics on
    // rollback. Every sibling rollback test (kinematic/random_ops/
    // rollback_restore/log_model) wires a throwaway channel the same way.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.create_player_safe(7);
    rb.forget(); // bake the create in

    let before = crc(&rb);
    let key = *rb.data.player_entites.get(&7).unwrap();
    rb.new_transaction();
    rb.remove_entity_safe(key, Some(7));
    assert!(!rb.data.ecs.entities.contains_key(key), "entity gone");
    let after_remove = crc(&rb);
    assert_ne!(before, after_remove, "removal must change state");
    rb.rollback();
    assert_eq!(before, crc(&rb), "hash(before) == hash(after undo), bit-exact");
}

#[test]
fn set_body_pose_safe_moves_and_undoes_exactly() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.create_player_safe(3);
    rb.forget();

    let key = *rb.data.player_entites.get(&3).unwrap();
    let before = crc(&rb);
    rb.new_transaction();
    rb.set_body_pose_safe(key, pose(250.0, 26.0, 10.0));
    let handle = rb.data.ecs.rigidbody.try_get(key).unwrap();
    let t = rb.data.physics.bodies.get(handle).unwrap().translation();
    assert_eq!(t.x, Real::from(250.0));
    rb.rollback();
    assert_eq!(before, crc(&rb));
}
