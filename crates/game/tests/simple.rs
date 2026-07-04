//! Transaction-counter and licensed-mutation semantics.
//!
//! The old version of this file applied #[rollback] to a standalone struct,
//! which the macro no longer supports (generated code is tied to the crate's
//! GameDataUpdate types). These are the same behaviors exercised against the
//! real GameData Rollback, using the current API.
use std::hash::Hash;

use rapier3d::prelude::RigidBodyBuilder;
use game::Rollback;

fn state_hash(r: &Rollback) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    r.hash(&mut hasher);
    hasher.finalize()
}

fn new_rollback() -> (
    Rollback,
    crossbeam::channel::Receiver<game::GameDataUpdate>,
) {
    let (send, recv) = crossbeam::channel::unbounded();
    (Rollback::new(Some(send)), recv)
}

#[test]
fn transaction_increments() {
    let (mut r, _recv) = new_rollback();
    assert_eq!(r.current(), 0);
    r.new_transaction();
    assert_eq!(r.current(), 1);
    r.new_transaction();
    assert_eq!(r.current(), 2);
}

#[test]
fn update_rolls_back() {
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    r.tick.update(|t| *t += 1);
    assert_eq!(*r.tick, 1);
    r.rollback();
    assert_eq!(*r.tick, 0);
}

#[test]
fn two_transactions_roll_back_stepwise() {
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    r.tick.update(|t| *t += 1);
    r.new_transaction();
    r.tick.update(|t| *t += 1);
    assert_eq!(*r.tick, 2);
    r.rollback();
    assert_eq!(*r.tick, 1);
}

#[test]
fn forget_basic() {
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    assert_eq!(r.oldest(), 0);
    assert_eq!(r.current(), 1);
    r.forget();
    assert_eq!(r.oldest(), 1);
    assert_eq!(r.current(), 1);
}

#[test]
fn forget_advances_oldest_only() {
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    assert_eq!(r.oldest(), 0);
    assert_eq!(r.current(), 1);
    r.new_transaction();
    r.forget();
    assert_eq!(r.oldest(), 1);
    assert_eq!(r.current(), 2);
}

#[test]
fn rollback_after_forget_keeps_forgotten_changes() {
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    r.tick.update(|t| *t += 1);

    assert_eq!(r.oldest(), 0);
    assert_eq!(r.current(), 1);
    r.new_transaction();
    assert_eq!(r.oldest(), 0);
    assert_eq!(r.current(), 2);
    // Forget tx1 (which holds the tick delta) ...
    r.forget();
    assert_eq!(r.oldest(), 1);
    assert_eq!(r.current(), 2);
    // ... then roll back tx2, which has no entries: the forgotten tick
    // change must persist.
    r.rollback();
    assert_eq!(r.oldest(), 1);
    assert_eq!(r.current(), 1);
    assert_eq!(*r.tick, 1);
}

#[test]
fn change_grants_licensed_raw_access_and_rolls_back() {
    // change(): snapshot-first raw &mut to a leaf tier-2 field.
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);
    r.new_transaction();
    let bodies = r.physics.bodies.change();
    bodies.insert(RigidBodyBuilder::fixed().build());
    assert_ne!(h0, state_hash(&r));
    r.rollback();
    assert_eq!(h0, state_hash(&r));
}

#[test]
fn snapshot_raw_grants_multi_field_access_and_rolls_back() {
    // snapshot_raw(): whole-struct snapshot + raw view of every field at
    // once (the physics-step pattern).
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);
    r.new_transaction();
    {
        let p = r.physics.snapshot_raw();
        p.bodies.insert(RigidBodyBuilder::fixed().build());
        p.bodies.insert(RigidBodyBuilder::dynamic().build());
    }
    assert_ne!(h0, state_hash(&r));
    r.rollback();
    assert_eq!(h0, state_hash(&r));
}
