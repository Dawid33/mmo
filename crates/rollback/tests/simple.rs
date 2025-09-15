#![allow(unused)]
use rollback::rollback;

use crate::mod_test::Test;

pub enum ClientUpdateEvent {}

#[rollback(Test)]
mod mod_test {
    pub struct Test {
        tick: usize,
    }
}

#[test]
pub fn transaction_increments() {
    let mut test = mod_test::Rollback::default();
    assert_eq!(test.current(), 0);
    test.new_transaction();
    assert_eq!(test.current(), 1);
    test.new_transaction();
    assert_eq!(test.current(), 2);
}

#[test]
pub fn as_mut() {
    let mut test = mod_test::Rollback::default();
    test.new_transaction();
    let old = test.tick.as_ref().clone();
    test.tick.undo(move |d| *d = old);
    *test.tick += 1;
    assert_eq!(test.tick.as_ref(), &1);
    test.rollback();
    assert_eq!(test.tick.as_ref(), &0);
}

#[test]
pub fn two() {
    let mut test = mod_test::Rollback::default();
    test.new_transaction();
    let old = test.tick.as_ref().clone();
    test.tick.undo(move |d| *d = old);
    *test.tick += 1;
    test.new_transaction();
    let old = test.tick.as_ref().clone();
    test.tick.undo(move |d| *d = old);
    *test.tick += 1;
    assert_eq!(test.tick.as_ref(), &2);
    test.rollback();
    assert_eq!(test.tick.as_ref(), &1);
}

#[test]
pub fn forget_basic() {
    let mut test = mod_test::Rollback::default();
    assert_eq!(test.oldest(), 0);
    assert_eq!(test.current(), 0);
    test.forget();
    assert_eq!(test.oldest(), 0);
    assert_eq!(test.current(), 0);
}

#[test]
pub fn forget() {
    let mut test = mod_test::Rollback::default();
    assert_eq!(test.oldest(), 0);
    assert_eq!(test.current(), 0);
    test.new_transaction();
    test.forget();
    assert_eq!(test.oldest(), 1);
    assert_eq!(test.current(), 1);
}

#[test]
pub fn rollback_forgotten() {
    let mut test = mod_test::Rollback::default();

    let old = test.tick.as_ref().clone();
    test.tick.undo(move |d| *d = old);
    *test.tick += 1;

    // Create new transaction
    assert_eq!(test.oldest(), 0);
    assert_eq!(test.current(), 0);
    test.new_transaction();
    // Forget the the 1 that was set to tick
    assert_eq!(test.oldest(), 0);
    assert_eq!(test.current(), 1);
    test.forget();
    // roll back the current transaction which has no changes, meaning changes
    // to tick will persist.
    assert_eq!(test.oldest(), 1);
    assert_eq!(test.current(), 1);
    test.rollback();
    assert_eq!(test.oldest(), 0);
    assert_eq!(test.current(), 0);
    assert_eq!(test.tick.as_ref(), &1);
}
