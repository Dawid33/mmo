use rollback::{Rollback, rollback};

#[rollback]
pub struct Test {
    tick: usize,
}

#[test]
pub fn transaction_increments() {
    let mut test: Test = Test::default();
    assert_eq!(test.current(), 0);
    test.new_transaction();
    assert_eq!(test.current(), 1);
    test.new_transaction();
    assert_eq!(test.current(), 2);
}

#[test]
pub fn simple() {
    let mut test: Test = Test::default();
    test.new_transaction();
    test.tick.set(1);
    assert_eq!(test.tick.as_ref(), &1);
    test.rollback();
    assert_eq!(test.tick.as_ref(), &0);
}

#[test]
pub fn two() {
    let mut test: Test = Test::default();
    test.new_transaction();
    test.tick.set(1);
    test.new_transaction();
    test.tick.set(2);
    assert_eq!(test.tick.as_ref(), &2);
    test.rollback();
    assert_eq!(test.tick.as_ref(), &1);
}

#[test]
pub fn two_in_one() {
    let mut test: Test = Test::default();
    test.new_transaction();
    test.tick.set(1);
    test.tick.set(2);
    assert_eq!(test.tick.as_ref(), &2);
    test.rollback();
    assert_eq!(test.tick.as_ref(), &0);
}

// #[test]
// pub fn forget_basic() {
//     let mut test: Test = Test::default();
//     assert_eq!(test.oldest(), 0);
//     assert_eq!(test.current(), 0);
//     test.forget();
//     assert_eq!(test.oldest(), 1);
//     test.forget();
//     assert_eq!(test.current(), 1);
// }

// #[test]
// pub fn forget() {
//     let mut test: Test = Test::default();
//     assert_eq!(test.oldest_transaction(), 0);
//     assert_eq!(test.current_transaction(), 0);
//     test.new_transaction();
//     test.forget();
//     assert_eq!(test.oldest_transaction(), 1);
//     assert_eq!(test.current_transaction(), 1);
// }

// #[test]
// pub fn rollback_forgotten() {
//     let mut test: Test = Test::default();
//     assert_eq!(test.oldest_transaction(), 0);
//     assert_eq!(test.current_transaction(), 0);
//     test.tick.set(1);
//     test.forget();
//     test.rollback();
//     assert_eq!(test.oldest_transaction(), 1);
//     assert_eq!(test.current_transaction(), 0);
//     assert_eq!(test.tick.as_ref(), &1);
// }
