//! Upgrade plan Task 4.1: the arena/set rollback primitives the game's undo
//! closures call (`state.rs`: `alloc_state` + `revert_insert` around every
//! body/collider insert, `is_marked_modified` + `restore_raw` +
//! `pop_modified` around every tracked write), ported from the old fork with
//! the same names and signatures so the call sites move 1:1 at the switch.
//!
//! Ported from `crates/game/tests/hash_restore.rs` (its rapier cases) and the
//! old fork's `collider_set.rs` `insert_with_parent_reverts_exactly_both_tracking_cases`,
//! with two changes: exactness is byte equality of the bincode of the whole
//! set (std `Hash` is gone from the glam-era forks; `StateHash` is Task 4.4),
//! and the "clean parent" state is reached through the public API (a pipeline
//! step clears the modified lists) instead of fork-internal field access.
use physics_next::PhysicsWorld;
use rapier3d::data::arena::Arena;
use rapier3d::prelude::*;
use serde::Serialize;

fn bytes<T: Serialize>(t: &T) -> Vec<u8> {
    bincode::serialize(t).expect("serialize")
}

// --- Arena ---------------------------------------------------------------

/// An arena with a non-trivial free list: slots 0 and 2 free (2 at the head),
/// slot 1 occupied, generation bumped twice.
fn arena_with_history() -> Arena<u32> {
    let mut a = Arena::new();
    let i0 = a.insert(10);
    let _i1 = a.insert(11);
    let i2 = a.insert(12);
    a.remove(i0);
    a.remove(i2);
    a
}

#[test]
fn arena_revert_insert_grow_path_is_exact() {
    let mut a: Arena<u32> = Arena::new();
    let before = bytes(&a);
    let (head, len) = a.alloc_state();
    let i = a.insert(7);
    a.revert_insert(i, head, len);
    assert_eq!(before, bytes(&a), "grow-path revert must restore storage, head and len");
}

#[test]
fn arena_revert_insert_reuse_path_is_exact() {
    let mut a = arena_with_history();
    let before = bytes(&a);
    let (head, len) = a.alloc_state();
    let i = a.insert(99);
    a.revert_insert(i, head, len);
    assert_eq!(before, bytes(&a), "reuse-path revert must restore the free list");
}

#[test]
fn arena_revert_remove_is_exact_and_the_index_resolves() {
    let mut a = arena_with_history();
    let i = a.insert(5);
    let before = bytes(&a);
    let v = a.remove(i).unwrap();
    a.revert_remove(i, v);
    assert_eq!(before, bytes(&a), "revert_remove must restore generation and free list");
    assert_eq!(a.get(i), Some(&5), "the old index must still resolve");
}

#[test]
fn arena_lifo_revert_chain_is_exact() {
    let mut a = arena_with_history();
    let before = bytes(&a);
    let s1 = a.alloc_state();
    let x = a.insert(1);
    let s2 = a.alloc_state();
    let y = a.insert(2);
    let s3 = a.alloc_state();
    let z = a.insert(3); // grows: the free list is drained
    let vy = a.remove(y).unwrap();
    a.revert_remove(y, vy);
    a.revert_insert(z, s3.0, s3.1);
    a.revert_insert(y, s2.0, s2.1);
    a.revert_insert(x, s1.0, s1.1);
    assert_eq!(before, bytes(&a), "LIFO chain over both insert paths must be exact");
}

/// Why the primitives exist: `remove(insert(x))` is not an inverse.
#[test]
fn arena_remove_does_not_undo_an_insert() {
    let mut a = arena_with_history();
    let before = bytes(&a);
    let i = a.insert(1);
    a.remove(i);
    assert_ne!(before, bytes(&a), "generation and free list are serialized state");
}

// --- RigidBodySet --------------------------------------------------------

fn remove_body(w: &mut PhysicsWorld, h: RigidBodyHandle) {
    w.bodies.remove(
        h,
        &mut w.islands,
        &mut w.colliders,
        &mut w.impulse_joints,
        &mut w.multibody_joints,
        &mut w.soft_bodies,
        true,
    );
}

#[test]
fn rigid_body_set_revert_insert_is_exact_on_both_paths() {
    let mut w = PhysicsWorld::default();
    // Grow path.
    let s0 = bytes(&w.bodies);
    let (head, len) = w.bodies.alloc_state();
    let h = w.bodies.insert(RigidBodyBuilder::fixed());
    w.bodies.revert_insert(h, head, len);
    assert_eq!(s0, bytes(&w.bodies), "grow-path revert must be exact");

    // Reuse path: an occupied and a freed slot first.
    let a = w.bodies.insert(RigidBodyBuilder::fixed());
    let _b = w.bodies.insert(RigidBodyBuilder::dynamic());
    remove_body(&mut w, a);
    let s1 = bytes(&w.bodies);
    let (head, len) = w.bodies.alloc_state();
    let c = w.bodies.insert(RigidBodyBuilder::kinematic_position_based());
    w.bodies.revert_insert(c, head, len);
    assert_eq!(s1, bytes(&w.bodies), "reuse-path revert must be exact");
}

#[test]
fn rigid_body_set_remove_does_not_undo_an_insert() {
    let mut w = PhysicsWorld::default();
    let before = bytes(&w.bodies);
    let h = w.bodies.insert(RigidBodyBuilder::fixed());
    remove_body(&mut w, h);
    assert_ne!(before, bytes(&w.bodies), "arena generation/free list are serialized");
}

/// The game's `body_mut_safe` inverse: a tracked write (`get_mut`) on a
/// body not yet on the modified list pushes it; `restore_raw` puts the value
/// (and its MODIFIED flag) back and `pop_modified` the list entry.
#[test]
fn rigid_body_tracked_write_reverts_exactly_both_tracking_cases() {
    let mut w = PhysicsWorld::default();
    let mut pipeline = PhysicsPipeline::new();
    let h = w.bodies.insert(RigidBodyBuilder::kinematic_position_based());
    w.step(&mut pipeline); // clears the modified list and flags
    assert!(!w.bodies.is_marked_modified(h), "setup: the step leaves the body clean");

    for was_modified_case in [false, true] {
        let was_modified = w.bodies.is_marked_modified(h);
        assert_eq!(was_modified, was_modified_case, "case setup");
        let before = bytes(&w.bodies);
        let pre = w.bodies.get(h).unwrap().clone();

        w.bodies.get_mut(h).unwrap().set_next_kinematic_translation(Vector::new(1.0, 2.0, 3.0));
        assert!(w.bodies.is_marked_modified(h), "get_mut marks the body");

        w.bodies.restore_raw(h, pre);
        if !was_modified {
            w.bodies.pop_modified(h);
        }
        assert_eq!(before, bytes(&w.bodies), "value + modified list must restore (was_modified={was_modified})");

        // Second case: leave the body on the modified list.
        w.bodies.get_mut(h);
    }
}

// --- ColliderSet ---------------------------------------------------------

#[test]
fn collider_set_revert_insert_is_exact_on_both_paths() {
    let mut w = PhysicsWorld::default();
    let s0 = bytes(&w.colliders);
    let (head, len) = w.colliders.alloc_state();
    let c = w.colliders.insert(ColliderBuilder::ball(1.0));
    w.colliders.revert_insert(c, head, len);
    assert_eq!(s0, bytes(&w.colliders), "grow-path revert must be exact");

    let doomed = w.colliders.insert(ColliderBuilder::ball(1.0));
    let _kept = w.colliders.insert(ColliderBuilder::cuboid(1.0, 1.0, 1.0));
    w.colliders.remove(doomed, &mut w.islands, &mut w.bodies, &mut w.soft_bodies, false);
    let s1 = bytes(&w.colliders);
    let (head, len) = w.colliders.alloc_state();
    let c = w.colliders.insert(ColliderBuilder::capsule_y(0.5, 0.25));
    w.colliders.revert_insert(c, head, len);
    assert_eq!(s1, bytes(&w.colliders), "reuse-path revert must be exact");
}

/// The game's `attach_collider_safe` inverse, both parent-tracking cases:
/// the parent not yet on the modified list (its push must be popped) and
/// already on it (it must not be).
#[test]
fn collider_insert_with_parent_reverts_exactly_both_tracking_cases() {
    let mut w = PhysicsWorld::default();
    let mut pipeline = PhysicsPipeline::new();
    let parent = w.bodies.insert(RigidBodyBuilder::fixed());
    // Collider history so the insert takes the reuse path.
    let doomed = w.colliders.insert(ColliderBuilder::ball(1.0));
    w.colliders.remove(doomed, &mut w.islands, &mut w.bodies, &mut w.soft_bodies, false);
    w.step(&mut pipeline); // clears modified/removed lists and flags
    assert!(!w.bodies.is_marked_modified(parent), "setup: parent must start clean");

    for was_modified_case in [false, true] {
        let was_modified = w.bodies.is_marked_modified(parent);
        assert_eq!(was_modified, was_modified_case, "case setup");
        let before_bodies = bytes(&w.bodies);
        let before_colliders = bytes(&w.colliders);
        let pre_parent = w.bodies.get(parent).unwrap().clone();
        let (head, len) = w.colliders.alloc_state();

        let c = w.colliders.insert_with_parent(ColliderBuilder::capsule_y(0.5, 0.25), parent, &mut w.bodies);
        assert!(w.bodies.is_marked_modified(parent), "attach must mark the parent");

        w.colliders.revert_insert(c, head, len);
        w.bodies.restore_raw(parent, pre_parent);
        if !was_modified {
            w.bodies.pop_modified(parent);
        }
        assert_eq!(before_bodies, bytes(&w.bodies), "parent + modified list (was_modified={was_modified})");
        assert_eq!(before_colliders, bytes(&w.colliders), "collider slot + alloc + lists (was_modified={was_modified})");

        w.bodies.get_mut(parent);
    }
}

/// The game's `collider_mut_safe` inverse, both tracking cases.
#[test]
fn collider_tracked_write_reverts_exactly_both_tracking_cases() {
    let mut w = PhysicsWorld::default();
    let mut pipeline = PhysicsPipeline::new();
    let c = w.colliders.insert(ColliderBuilder::ball(1.0));
    w.step(&mut pipeline);
    assert!(!w.colliders.is_marked_modified(c), "setup: the step leaves the collider clean");

    for was_modified_case in [false, true] {
        let was_modified = w.colliders.is_marked_modified(c);
        assert_eq!(was_modified, was_modified_case, "case setup");
        let before = bytes(&w.colliders);
        let pre = w.colliders.get(c).unwrap().clone();

        w.colliders.get_mut(c).unwrap().set_translation(Vector::new(4.0, 5.0, 6.0));
        assert!(w.colliders.is_marked_modified(c), "get_mut marks the collider");

        w.colliders.restore_raw(c, pre);
        if !was_modified {
            w.colliders.pop_modified(c);
        }
        assert_eq!(before, bytes(&w.colliders), "value + modified list (was_modified={was_modified})");

        w.colliders.get_mut(c);
    }
}
