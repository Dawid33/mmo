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

/// Gate: mmo-fork hunk rapier data/arena.rs: exact LIFO inverses Arena::{alloc_state,revert_insert,revert_remove} (394aac5c); chain of inserts/removes reverted byte-exact
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

/// Gate: mmo-fork hunk rapier rigid_body_set.rs 'rollback primitives' (alloc_state/revert_insert, grow+reuse paths) (394aac5c)
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

/// Gate: mmo-fork hunks rapier rigid_body_set.rs restore_raw/is_marked_modified/pop_modified + data/modified_objects.rs 'LIFO undo support' (394aac5c)
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

/// Gate: mmo-fork hunk rapier collider_set.rs restore_raw/pop_modified (the game's collider_mut_safe inverse) (394aac5c)
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
