use macros::rollback;
use rollback::Rollback;

#[rollback]
pub struct Test {
    tick: usize,
    #[recurse]
    inner: Inner,
}

#[rollback]
pub struct Inner {
    inner_tick: usize,
}

#[test]
pub fn transaction_updates() {
    let mut test: Test = Test::default();
    test.new_transaction();
    assert_eq!(test.inner.current(), 1);
}

#[test]
pub fn simple() {
    let mut test: Test = Test::default();
    test.new_transaction();
    test.inner.inner_tick.set(1);
    assert_eq!(test.inner.inner_tick.as_ref(), &1);
    test.rollback();
    assert_eq!(test.inner.inner_tick.as_ref(), &0);
}

#[test]
pub fn two() {
    let mut test: Test = Test::default();
    test.new_transaction();
    test.inner.inner_tick.set(1);
    test.new_transaction();
    test.inner.inner_tick.set(2);
    assert_eq!(test.inner.inner_tick.as_ref(), &2);
    test.rollback();
    assert_eq!(test.inner.inner_tick.as_ref(), &1);
}

#[test]
pub fn two_in_one() {
    let mut test: Test = Test::default();
    test.new_transaction();
    test.inner.inner_tick.set(1);
    test.inner.inner_tick.set(2);
    assert_eq!(test.inner.inner_tick.as_ref(), &2);
    test.rollback();
    assert_eq!(test.inner.inner_tick.as_ref(), &0);
}

// #[test]
// #[ignore]
// pub fn forget_basic() {
//     let mut test: Test = Test::default();
//     assert_eq!(test.oldest_transaction(), 0);
//     assert_eq!(test.current_transaction(), 0);
//     test.forget();
//     assert_eq!(test.oldest_transaction(), 1);
//     test.forget();
//     assert_eq!(test.oldest_transaction(), 1);
// }

// #[test]
// #[ignore]
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
// #[ignore]
// pub fn rollback_forgotten() {
//     let mut test: Test = Test::default();
//     assert_eq!(test.oldest_transaction(), 0);
//     assert_eq!(test.current_transaction(), 0);
//     test.inner.data.inner_tick.set(1);
//     test.forget();
//     test.rollback();
//     assert_eq!(test.oldest_transaction(), 1);
//     assert_eq!(test.current_transaction(), 0);
//     assert_eq!(test.tick.as_ref(), &1);
// }
