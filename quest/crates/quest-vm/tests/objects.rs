use proptest::prelude::*;
use quest_vm::alloc::{Heap, Kind};
use quest_vm::consts::REAP_CHILDREN_MAX;
use quest_vm::fault::FaultKind;
use quest_vm::object::{self, FrameMaps, incref, is_ptr, make_unique, obj, reap_step, release};
use quest_vm::{Store, list, string};

struct NoFrames;
impl FrameMaps for NoFrames {
    fn ref_slots(&self, _fn_id: u16) -> &[u16] {
        &[]
    }
}

fn setup_sized(mem: u32) -> (Store, Heap) {
    let mut st = Store::new(16, mem).unwrap();
    let l = st.layout;
    let h = Heap {
        base: l.heap_base,
        end: l.heap_end,
        script: true,
    };
    h.init(&mut st).unwrap();
    (st, h)
}

fn setup() -> (Store, Heap) {
    setup_sized(262_144)
}

fn reap_all(st: &mut Store, h: Heap) -> u32 {
    let mut steps = 0;
    while reap_step(st, h, &NoFrames).unwrap().is_some() {
        steps += 1;
    }
    steps
}

fn ints(st: &Store, h: Heap, l: u64) -> Vec<i64> {
    (0..i64::from(list::len(st, h, l).unwrap()))
        .map(|i| list::get(st, h, l, i).unwrap() as i64)
        .collect()
}

fn int_list(st: &mut Store, h: Heap, xs: &[i64]) -> u64 {
    let mut l = list::new(st, h, false).unwrap();
    for &x in xs {
        l = list::push(st, h, l, x as u64).unwrap().0;
    }
    l
}

#[test]
fn cow_copy_is_independent() {
    let (mut st, h) = setup();
    let a = int_list(&mut st, h, &[1, 2]);
    incref(&mut st, h, a).unwrap();
    let b = a;
    let (b2, copied) = make_unique(&mut st, h, b).unwrap();
    assert_ne!(b2, a);
    assert_eq!(copied, 2);
    let b2 = list::push(&mut st, h, b2, 3).unwrap().0;
    assert_eq!(ints(&st, h, a), vec![1, 2]);
    assert_eq!(ints(&st, h, b2), vec![1, 2, 3]);
    assert_eq!(h.header(&st, a as u32).unwrap().refcount, 1);
}

#[test]
fn make_unique_of_unique_or_immediate_copies_nothing() {
    let (mut st, h) = setup();
    let a = int_list(&mut st, h, &[1]);
    assert_eq!(make_unique(&mut st, h, a).unwrap(), (a, 0));
    assert_eq!(make_unique(&mut st, h, 0).unwrap(), (0, 0));
    assert_eq!(make_unique(&mut st, h, 7).unwrap(), (7, 0));
}

#[test]
fn immediates_are_never_counted() {
    let (mut st, h) = setup();
    let before = st.mem.bytes().to_vec();
    for v in [0u64, 3, 1, u64::MAX] {
        assert!(!is_ptr(v));
        incref(&mut st, h, v).unwrap();
        release(&mut st, h, v).unwrap();
    }
    assert_eq!(st.mem.bytes(), &before[..]);
    assert!(is_ptr(64));
}

#[test]
fn release_of_nested_value_frees_everything() {
    let (mut st, h) = setup();
    let base = h.used_words(&st).unwrap();
    let mut outer = list::new(&mut st, h, true).unwrap();
    for i in 0..100 {
        let mut inner = list::new(&mut st, h, true).unwrap();
        for j in 0..3 {
            let s = string::new(&mut st, h, &format!("s{i}-{j}")).unwrap();
            inner = list::push(&mut st, h, inner, s).unwrap().0;
        }
        outer = list::push(&mut st, h, outer, inner).unwrap().0;
    }
    assert!(h.used_words(&st).unwrap() > base);
    release(&mut st, h, outer).unwrap();
    // Nothing is freed by `release` itself beyond linking the zombie.
    assert!(h.used_words(&st).unwrap() > base);
    assert!(reap_all(&mut st, h) > 0);
    assert_eq!(h.used_words(&st).unwrap(), base);
}

#[test]
fn reap_is_bounded() {
    let (mut st, h) = setup();
    let mut l = list::new(&mut st, h, true).unwrap();
    for i in 0..1000 {
        let s = string::new(&mut st, h, &format!("{i}")).unwrap();
        l = list::push(&mut st, h, l, s).unwrap().0;
    }
    release(&mut st, h, l).unwrap();
    let work = reap_step(&mut st, h, &NoFrames).unwrap().unwrap();
    // Sixteen child slots released; the parent is not done, so no payload was zeroed.
    assert_eq!(work, REAP_CHILDREN_MAX as u32);
    // The parent is back at the head with its cursor advanced by exactly the budget...
    let mut at = h.zombie_head(&st).unwrap() as u32;
    let parent = h.header(&st, at).unwrap();
    assert_eq!(parent.len, 1000 - REAP_CHILDREN_MAX as u32);
    // ...and the zombie list is the parent plus exactly the children released.
    let mut zombies = 0;
    while at != 0 {
        zombies += 1;
        at = h.header(&st, at).unwrap().refcount;
    }
    assert_eq!(zombies, 1 + REAP_CHILDREN_MAX);
    // Running on to the end still frees all of it.
    reap_all(&mut st, h);
    let (st2, h2) = setup();
    assert_eq!(h.used_words(&st).unwrap(), h2.used_words(&st2).unwrap());
}

#[test]
fn reap_with_no_zombies_is_false() {
    let (mut st, h) = setup();
    assert!(reap_step(&mut st, h, &NoFrames).unwrap().is_none());
}

#[test]
fn string_roundtrip_utf8() {
    let (mut st, h) = setup();
    let t = "zażółć gęślą jaźń";
    let s = string::new(&mut st, h, t).unwrap();
    assert_eq!(string::char_len(&st, h, s).unwrap(), 17);
    assert_eq!(string::byte_len(&st, h, s).unwrap(), 26);
    assert_eq!(string::read(&st, h, s).unwrap(), t);
    let e = string::new(&mut st, h, "").unwrap();
    assert_eq!(string::read(&st, h, e).unwrap(), "");
    let c = string::concat(&mut st, h, &[s, e, s]).unwrap().0;
    assert_eq!(string::read(&st, h, c).unwrap(), format!("{t}{t}"));
    assert_eq!(string::read(&st, h, s).unwrap(), t);
}

#[test]
fn string_with_wrong_kind_is_corrupt() {
    let (mut st, h) = setup();
    let l = int_list(&mut st, h, &[1]);
    assert_eq!(string::read(&st, h, l), Err(FaultKind::Corrupt));
    assert_eq!(string::byte_len(&st, h, 0), Err(FaultKind::Corrupt));
}

#[test]
fn list_faults() {
    let (mut st, h) = setup();
    let l = int_list(&mut st, h, &[1, 2, 3]);
    assert_eq!(list::get(&st, h, l, 3), Err(FaultKind::IndexOutOfRange));
    assert_eq!(list::get(&st, h, l, -1), Err(FaultKind::IndexOutOfRange));
    assert_eq!(
        list::get(&st, h, l, i64::MIN),
        Err(FaultKind::IndexOutOfRange)
    );
    assert_eq!(
        list::remove(&mut st, h, l, 3),
        Err(FaultKind::IndexOutOfRange)
    );
    assert_eq!(
        list::insert(&mut st, h, l, 4, 9),
        Err(FaultKind::IndexOutOfRange)
    );
    assert_eq!(
        list::set(&mut st, h, l, 3, 9),
        Err(FaultKind::IndexOutOfRange)
    );
    let e = list::new(&mut st, h, false).unwrap();
    assert_eq!(list::pop(&mut st, h, e).unwrap(), None);
}

#[test]
fn list_mutation_needs_a_unique_list() {
    let (mut st, h) = setup();
    let l = int_list(&mut st, h, &[1]);
    incref(&mut st, h, l).unwrap();
    assert_eq!(list::push(&mut st, h, l, 2), Err(FaultKind::Corrupt));
    assert_eq!(list::set(&mut st, h, l, 0, 2), Err(FaultKind::Corrupt));
    assert_eq!(list::pop(&mut st, h, l), Err(FaultKind::Corrupt));
    assert_eq!(ints(&st, h, l), vec![1]);
}

#[test]
fn list_grows_across_classes_and_keeps_order() {
    let (mut st, h) = setup();
    let xs: Vec<i64> = (0..200).collect();
    let l = int_list(&mut st, h, &xs);
    assert_eq!(ints(&st, h, l), xs);
    let l = list::insert(&mut st, h, l, 0, (-1i64) as u64).unwrap().0;
    assert_eq!(list::remove(&mut st, h, l, 1).unwrap().0, 0);
    assert_eq!(list::get(&st, h, l, 0).unwrap() as i64, -1);
    assert!(list::contains(&st, h, l, 199).unwrap());
    assert!(!list::contains(&st, h, l, 200).unwrap());
}

#[test]
fn slice_clamps() {
    let (mut st, h) = setup();
    let l = int_list(&mut st, h, &[1, 2, 3, 4]);
    let s = list::slice(&mut st, h, l, 1, 10).unwrap().0;
    assert_eq!(ints(&st, h, s), vec![2, 3, 4]);
    let s = list::slice(&mut st, h, l, 3, 1).unwrap().0;
    assert_eq!(ints(&st, h, s), Vec::<i64>::new());
    let s = list::slice(&mut st, h, l, 9, 12).unwrap().0;
    assert_eq!(ints(&st, h, s), Vec::<i64>::new());
    assert_eq!(
        list::slice(&mut st, h, l, -1, 2),
        Err(FaultKind::IndexOutOfRange)
    );
    assert_eq!(
        list::slice(&mut st, h, l, 0, -2),
        Err(FaultKind::IndexOutOfRange)
    );
}

#[test]
fn slice_of_refs_shares_elements() {
    let (mut st, h) = setup();
    let base = h.used_words(&st).unwrap();
    let s = string::new(&mut st, h, "x").unwrap();
    let l = list::new(&mut st, h, true).unwrap();
    let l = list::push(&mut st, h, l, s).unwrap().0;
    let sl = list::slice(&mut st, h, l, 0, 1).unwrap().0;
    assert_eq!(h.header(&st, s as u32).unwrap().refcount, 2);
    release(&mut st, h, l).unwrap();
    release(&mut st, h, sl).unwrap();
    reap_all(&mut st, h);
    assert_eq!(h.used_words(&st).unwrap(), base);
}

#[test]
fn list_set_take_pop_ownership() {
    let (mut st, h) = setup();
    let base = h.used_words(&st).unwrap();
    let a = string::new(&mut st, h, "a").unwrap();
    let b = string::new(&mut st, h, "b").unwrap();
    let l = list::new(&mut st, h, true).unwrap();
    let l = list::push(&mut st, h, l, a).unwrap().0;
    // take moves the element out, leaving an empty slot.
    let got = list::take(&mut st, h, l, 0).unwrap();
    assert_eq!(got, a);
    assert_eq!(list::get(&st, h, l, 0).unwrap(), 0);
    // set stores ownership of b and releases what was there (nothing).
    list::set(&mut st, h, l, 0, b).unwrap();
    // set over a held element releases it.
    list::set(&mut st, h, l, 0, got).unwrap();
    assert_eq!(h.header(&st, b as u32).unwrap().refcount, 0);
    reap_all(&mut st, h);
    let p = list::pop(&mut st, h, l).unwrap().unwrap();
    assert_eq!(p, a);
    release(&mut st, h, p).unwrap();
    release(&mut st, h, l).unwrap();
    reap_all(&mut st, h);
    assert_eq!(h.used_words(&st).unwrap(), base);
}

#[test]
fn struct_fields_and_ref_bits() {
    let (mut st, h) = setup();
    let base = h.used_words(&st).unwrap();
    let o = obj::new(&mut st, h, 2, 0b10, 5).unwrap();
    let s = string::new(&mut st, h, "hello").unwrap();
    obj::set_field(&mut st, h, o, 0, 12345).unwrap();
    obj::set_field(&mut st, h, o, 1, s).unwrap();
    assert_eq!(obj::get_field(&st, h, o, 0).unwrap(), 12345);
    assert_eq!(obj::get_field(&st, h, o, 1).unwrap(), s);
    assert_eq!(obj::tag(&st, h, o).unwrap(), 5);
    assert_eq!(obj::tag(&st, h, (9 << 1) | 1).unwrap(), 9);
    assert_eq!(obj::get_field(&st, h, o, 2), Err(FaultKind::Corrupt));
    assert_eq!(h.header(&st, o as u32).unwrap().kind, Kind::Obj);
    // Field 0 holds 12345-ish bits that look like nothing; release touches the string only.
    // Field 0 is not a ref field: it may hold anything, even a pointer-shaped value.
    let t = string::new(&mut st, h, "bystander").unwrap();
    obj::set_field(&mut st, h, o, 0, t).unwrap();
    release(&mut st, h, o).unwrap();
    reap_all(&mut st, h);
    // Only the marked field's string was released; `t` was never counted by the object.
    assert_eq!(h.header(&st, t as u32).unwrap().refcount, 1);
    assert_eq!(string::read(&st, h, t).unwrap(), "bystander");
    release(&mut st, h, t).unwrap();
    reap_all(&mut st, h);
    assert_eq!(h.used_words(&st).unwrap(), base);
}

#[test]
fn corrupt_field_count_cannot_panic() {
    let (mut st, h) = setup();
    let p = h.alloc(&mut st, Kind::Obj, 60).unwrap().unwrap();
    let mut hd = h.header(&st, p).unwrap();
    hd.len = 40;
    h.set_header(&mut st, p, hd).unwrap();
    let o = u64::from(p);
    assert_eq!(
        obj::set_field(&mut st, h, o, 40, 1),
        Err(FaultKind::Corrupt)
    );
    assert_eq!(obj::get_field(&st, h, o, 0), Err(FaultKind::Corrupt));
    assert_eq!(make_unique(&mut st, h, o), Ok((o, 0)));
    // Ref bits beyond the field count are refused as well.
    hd.len = 2;
    hd.aux = 0b100;
    h.set_header(&mut st, p, hd).unwrap();
    assert_eq!(obj::get_field(&st, h, o, 0), Err(FaultKind::Corrupt));
    release(&mut st, h, o).unwrap();
    assert_eq!(reap_step(&mut st, h, &NoFrames), Err(FaultKind::Corrupt));
}

#[test]
fn reported_work_is_the_real_work() {
    let (mut st, h) = setup();
    // A fresh list has 4 payload words: a block of 6 words total, class 3 holds 6 elements.
    let mut l = list::new(&mut st, h, false).unwrap();
    let cap = 6;
    for i in 0..cap {
        let (n, w) = list::push(&mut st, h, l, i).unwrap();
        assert_eq!(w, 0);
        l = n;
    }
    // The seventh element grows class 3 -> class 4: six elements moved, the old six-word payload zeroed.
    let (l2, w) = list::push(&mut st, h, l, 6).unwrap();
    assert_eq!(w, 6 + 6);
    assert_ne!(l2, l);
    // Insert at the front of seven elements shifts seven words, no growth.
    let (l2, w) = list::insert(&mut st, h, l2, 0, 99).unwrap();
    assert_eq!(w, 7);
    // Remove at index 1 of eight shifts six words.
    assert_eq!(list::remove(&mut st, h, l2, 1).unwrap().1, 6);
    let (_, w) = list::slice(&mut st, h, l2, 2, 5).unwrap();
    assert_eq!(w, 3);
    let s = string::new(&mut st, h, "0123456789").unwrap();
    assert_eq!(string::concat(&mut st, h, &[s, s]).unwrap().1, 3);
    // Reaping a list of two strings: two child slots, then the freed block's payload (class 3 = 6 words)
    // on the step that finishes it.
    let mut l = list::new(&mut st, h, true).unwrap();
    for _ in 0..2 {
        let s = string::new(&mut st, h, "x").unwrap();
        l = list::push(&mut st, h, l, s).unwrap().0;
    }
    release(&mut st, h, l).unwrap();
    assert_eq!(reap_step(&mut st, h, &NoFrames).unwrap(), Some(2 + 6));
}

#[test]
fn struct_clone_increfs_only_ref_fields() {
    let (mut st, h) = setup();
    let o = obj::new(&mut st, h, 2, 0b10, 1).unwrap();
    let s = string::new(&mut st, h, "hi").unwrap();
    // An int field whose bits look like a pointer must never be counted.
    obj::set_field(&mut st, h, o, 0, s & !1).unwrap();
    obj::set_field(&mut st, h, o, 1, s).unwrap();
    incref(&mut st, h, o).unwrap();
    let (o2, copied) = make_unique(&mut st, h, o).unwrap();
    assert_ne!(o2, o);
    assert_eq!(copied, 2);
    assert_eq!(h.header(&st, s as u32).unwrap().refcount, 2);
    let t = obj::take_field(&mut st, h, o2, 1).unwrap();
    assert_eq!(t, s);
    assert_eq!(obj::get_field(&st, h, o2, 1).unwrap(), 0);
}

#[test]
fn object_new_rejects_bad_shape() {
    let (mut st, h) = setup();
    assert_eq!(obj::new(&mut st, h, 2, 0b100, 0), Err(FaultKind::Corrupt));
    assert_eq!(obj::new(&mut st, h, 33, 0, 0), Err(FaultKind::Corrupt));
}

#[test]
fn heap_full_inside_clone_leaves_source_intact() {
    let (mut st, h) = setup_sized(65_536);
    let l = int_list(&mut st, h, &[1, 2, 3]);
    incref(&mut st, h, l).unwrap();
    while h.alloc(&mut st, Kind::Raw, 0).unwrap().is_ok() {}
    assert_eq!(make_unique(&mut st, h, l), Err(FaultKind::HeapFull));
    assert_eq!(ints(&st, h, l), vec![1, 2, 3]);
    assert_eq!(h.header(&st, l as u32).unwrap().refcount, 2);
}

#[test]
fn heap_full_inside_growth_is_a_fault() {
    let (mut st, h) = setup_sized(65_536);
    let mut l = list::new(&mut st, h, false).unwrap();
    let mut fault = None;
    for i in 0..100_000u64 {
        match list::push(&mut st, h, l, i) {
            Ok(n) => l = n.0,
            Err(e) => {
                fault = Some(e);
                break;
            }
        }
    }
    assert_eq!(fault, Some(FaultKind::HeapFull));
    // The list is still whole.
    let n = list::len(&st, h, l).unwrap();
    assert!(n > 0);
    for i in 0..i64::from(n) {
        assert_eq!(list::get(&st, h, l, i).unwrap(), i as u64);
    }
}

#[test]
fn pause_survives_abort_and_rollback() {
    let (mut st, h) = setup();
    let mut l = list::new(&mut st, h, true).unwrap();
    for i in 0..40 {
        let s = string::new(&mut st, h, &format!("{i}")).unwrap();
        l = list::push(&mut st, h, l, s).unwrap().0;
    }
    release(&mut st, h, l).unwrap();
    assert!(reap_step(&mut st, h, &NoFrames).unwrap().is_some());
    st.mem.clear_log();
    let cp = st.mem.checkpoint();
    let before = st.mem.bytes().to_vec();
    // An aborted segment of reaping leaves the half-reaped zombie exactly as it was.
    st.segment_open();
    reap_step(&mut st, h, &NoFrames).unwrap();
    reap_step(&mut st, h, &NoFrames).unwrap();
    st.segment_abort();
    // The shadow keeps its copies after an abort; the script regions are back as they were.
    let (a, b) = (st.layout.static_base as usize, st.layout.heap_end as usize);
    assert_eq!(&before[a..b], &st.mem.bytes()[a..b]);
    reap_step(&mut st, h, &NoFrames).unwrap();
    st.mem.rollback_to(cp).unwrap();
    assert_eq!(&before[..], st.mem.bytes());
    reap_all(&mut st, h);
    let (st2, h2) = setup();
    assert_eq!(h.used_words(&st).unwrap(), h2.used_words(&st2).unwrap());
}

#[test]
fn unknown_zombie_kind_is_corrupt() {
    let (mut st, h) = setup();
    let p = h.alloc(&mut st, Kind::Dict, 2).unwrap().unwrap();
    release(&mut st, h, u64::from(p)).unwrap();
    assert_eq!(reap_step(&mut st, h, &NoFrames), Err(FaultKind::Corrupt));
}

#[test]
fn release_of_a_freed_block_is_corrupt() {
    let (mut st, h) = setup();
    let s = string::new(&mut st, h, "x").unwrap();
    release(&mut st, h, s).unwrap();
    reap_all(&mut st, h);
    assert_eq!(release(&mut st, h, s), Err(FaultKind::Corrupt));
    assert_eq!(incref(&mut st, h, s), Err(FaultKind::Corrupt));
}

#[derive(Clone, Debug)]
enum Op {
    Push(i64),
    Pop,
    Insert(usize, i64),
    Remove(usize),
    Set(usize, i64),
    Dup,
    ReleaseHeld,
    ReapStep,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        any::<i64>().prop_map(Op::Push),
        Just(Op::Pop),
        (any::<usize>(), any::<i64>()).prop_map(|(i, v)| Op::Insert(i, v)),
        any::<usize>().prop_map(Op::Remove),
        (any::<usize>(), any::<i64>()).prop_map(|(i, v)| Op::Set(i, v)),
        Just(Op::Dup),
        Just(Op::ReleaseHeld),
        Just(Op::ReapStep),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn list_matches_vec_model_and_rolls_back(ops in proptest::collection::vec(op(), 0..80)) {
        let (mut st, h) = setup();
        st.mem.clear_log();
        let cp = st.mem.checkpoint();
        let initial = st.mem.bytes().to_vec();
        let mut cur = list::new(&mut st, h, false).unwrap();
        let mut model: Vec<i64> = vec![];
        let mut held: Vec<(u64, Vec<i64>)> = vec![];
        for o in ops {
            match o {
                Op::Dup => {
                    incref(&mut st, h, cur).unwrap();
                    held.push((cur, model.clone()));
                    continue;
                }
                Op::ReleaseHeld => {
                    if let Some((p, m)) = held.pop() {
                        prop_assert_eq!(ints(&st, h, p), m);
                        release(&mut st, h, p).unwrap();
                    }
                    continue;
                }
                Op::ReapStep => {
                    reap_step(&mut st, h, &NoFrames).unwrap();
                    continue;
                }
                _ => {}
            }
            let (u, _) = make_unique(&mut st, h, cur).unwrap();
            cur = u;
            match o {
                Op::Push(v) => {
                    cur = list::push(&mut st, h, cur, v as u64).unwrap().0;
                    model.push(v);
                }
                Op::Pop => {
                    let got = list::pop(&mut st, h, cur).unwrap();
                    prop_assert_eq!(got.map(|x| x as i64), model.pop());
                }
                Op::Insert(i, v) => {
                    let i = i % (model.len() + 1);
                    cur = list::insert(&mut st, h, cur, i as i64, v as u64).unwrap().0;
                    model.insert(i, v);
                }
                Op::Remove(i) => {
                    if !model.is_empty() {
                        let i = i % model.len();
                        prop_assert_eq!(list::remove(&mut st, h, cur, i as i64).unwrap().0 as i64, model.remove(i));
                    }
                }
                Op::Set(i, v) => {
                    if !model.is_empty() {
                        let i = i % model.len();
                        list::set(&mut st, h, cur, i as i64, v as u64).unwrap();
                        model[i] = v;
                    }
                }
                _ => unreachable!(),
            }
            prop_assert_eq!(ints(&st, h, cur), model.clone());
        }
        for (p, m) in held.drain(..) {
            prop_assert_eq!(ints(&st, h, p), m);
            release(&mut st, h, p).unwrap();
        }
        release(&mut st, h, cur).unwrap();
        reap_all(&mut st, h);
        prop_assert_eq!(h.used_words(&st).unwrap(), 0);
        st.mem.rollback_to(cp).unwrap();
        prop_assert_eq!(st.mem.bytes(), &initial[..]);
    }
}

#[test]
fn module_paths_exist() {
    let _: fn(u64) -> bool = object::is_ptr;
}
