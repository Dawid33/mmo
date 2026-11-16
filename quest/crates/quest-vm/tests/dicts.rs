use proptest::prelude::*;
use quest_vm::alloc::Heap;
use quest_vm::consts::REAP_CHILDREN_MAX;
use quest_vm::eq::{deep_eq, key_hash};
use quest_vm::fault::FaultKind;
use quest_vm::object::{FrameMaps, incref, make_unique, obj, reap_step, release};
use quest_vm::{Store, dict, list, string};

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

/// The live entries in iteration order, walked with `next`.
fn entries(st: &Store, h: Heap, d: u64) -> Vec<(u64, u64)> {
    let mut out = vec![];
    let mut pos = 0;
    loop {
        let (p, _) = dict::next(st, h, d, pos).unwrap();
        if p < 0 {
            return out;
        }
        out.push((
            dict::key_at(st, h, d, p).unwrap(),
            dict::val_at(st, h, d, p).unwrap(),
        ));
        pos = p + 1;
    }
}

fn int_dict(st: &mut Store, h: Heap) -> u64 {
    dict::new(st, h, false, false).unwrap()
}

fn put(st: &mut Store, h: Heap, d: u64, k: u64, v: u64) {
    assert_eq!(dict::set(st, h, d, k, v).unwrap().0, d);
}

fn str_(st: &mut Store, h: Heap, s: &str) -> u64 {
    string::new(st, h, s).unwrap()
}

#[test]
fn insertion_order_survives_remove_and_reinsert() {
    let (mut st, h) = setup();
    let d = int_dict(&mut st, h);
    for (k, v) in [(1, 10), (2, 20), (3, 30)] {
        put(&mut st, h, d, k, v);
    }
    assert_eq!(dict::remove(&mut st, h, d, 2).unwrap().0, Some(20));
    put(&mut st, h, d, 4, 40);
    put(&mut st, h, d, 2, 21);
    assert_eq!(entries(&st, h, d), vec![(1, 10), (3, 30), (4, 40), (2, 21)]);
    assert_eq!(dict::len(&st, h, d).unwrap(), 4);
    assert_eq!(dict::remove(&mut st, h, d, 99).unwrap().0, None);
}

#[test]
fn overwrite_keeps_position() {
    let (mut st, h) = setup();
    let d = int_dict(&mut st, h);
    for k in 1..=3 {
        put(&mut st, h, d, k, k * 10);
    }
    put(&mut st, h, d, 2, 99);
    assert_eq!(entries(&st, h, d), vec![(1, 10), (2, 99), (3, 30)]);
    assert_eq!(dict::len(&st, h, d).unwrap(), 3);
}

#[test]
fn string_keys_compare_by_content() {
    let (mut st, h) = setup();
    let d = dict::new(&mut st, h, true, false).unwrap();
    let k1 = str_(&mut st, h, "k");
    let k2 = str_(&mut st, h, "k");
    assert_ne!(k1, k2);
    put(&mut st, h, d, k1, 1);
    assert_eq!(dict::get(&st, h, d, k2).unwrap().0, Some(1));
    assert!(dict::contains(&st, h, d, k2).unwrap().0);
    // Overwrite through the other string: the stored key stays, the passed one is released.
    put(&mut st, h, d, k2, 2);
    assert_eq!(dict::len(&st, h, d).unwrap(), 1);
    assert_eq!(dict::key_at(&st, h, d, 0).unwrap(), k1);
    assert_eq!(dict::get(&st, h, d, k1).unwrap().0, Some(2));
    let other = str_(&mut st, h, "j");
    assert_eq!(dict::get(&st, h, d, other).unwrap().0, None);
    release(&mut st, h, d).unwrap();
    release(&mut st, h, other).unwrap();
    reap_all(&mut st, h);
    assert_eq!(h.used_words(&st).unwrap(), 0);
}

fn pair(st: &mut Store, h: Heap, n: u64, s: &str) -> u64 {
    let o = obj::new(st, h, 2, 0b10, 0).unwrap();
    obj::set_field(st, h, o, 0, n).unwrap();
    let s = str_(st, h, s);
    obj::set_field(st, h, o, 1, s).unwrap();
    o
}

#[test]
fn struct_keys() {
    let (mut st, h) = setup();
    let d = dict::new(&mut st, h, true, false).unwrap();
    let a = pair(&mut st, h, 7, "x");
    put(&mut st, h, d, a, 1);
    let same = pair(&mut st, h, 7, "x");
    let other_str = pair(&mut st, h, 7, "y");
    let other_int = pair(&mut st, h, 8, "x");
    assert_eq!(dict::get(&st, h, d, same).unwrap().0, Some(1));
    assert_eq!(dict::get(&st, h, d, other_str).unwrap().0, None);
    assert_eq!(dict::get(&st, h, d, other_int).unwrap().0, None);
    let (h1, _) = key_hash(&st, h, a, true).unwrap();
    let (h2, _) = key_hash(&st, h, same, true).unwrap();
    assert_eq!(h1, h2);
    // A field-less variant is an odd immediate even in a dict of heap-kind keys.
    put(&mut st, h, d, (3 << 1) | 1, 5);
    assert_eq!(dict::get(&st, h, d, (3 << 1) | 1).unwrap().0, Some(5));
    assert_eq!(dict::get(&st, h, d, (4 << 1) | 1).unwrap().0, None);
}

#[test]
fn iteration_by_position_sees_appends() {
    let (mut st, h) = setup();
    let d = int_dict(&mut st, h);
    put(&mut st, h, d, 1, 1);
    let mut seen = vec![];
    let mut pos = 0;
    loop {
        let (p, _) = dict::next(&st, h, d, pos).unwrap();
        if p < 0 {
            break;
        }
        let k = dict::key_at(&st, h, d, p).unwrap();
        seen.push(k);
        if k == 1 {
            put(&mut st, h, d, 2, 2);
            put(&mut st, h, d, 3, 3);
        }
        pos = p + 1;
    }
    assert_eq!(seen, vec![1, 2, 3]);
    assert_eq!(dict::next(&st, h, d, 3).unwrap().0, -1);
    assert_eq!(dict::next(&st, h, d, 1000).unwrap().0, -1);
}

#[test]
fn missing_key_get_is_none() {
    let (mut st, h) = setup();
    let d = int_dict(&mut st, h);
    assert_eq!(dict::get(&st, h, d, 5).unwrap().0, None);
    assert_eq!(dict::take(&mut st, h, d, 5).unwrap().0, None);
    assert!(!dict::contains(&st, h, d, 5).unwrap().0);
    assert_eq!(dict::next(&st, h, d, 0).unwrap().0, -1);
    assert_eq!(
        dict::key_at(&st, h, d, 0).unwrap_err(),
        FaultKind::IndexOutOfRange
    );
}

#[test]
fn take_leaves_the_key_and_a_set_puts_back_in_place() {
    let (mut st, h) = setup();
    let d = dict::new(&mut st, h, false, true).unwrap();
    let l = list::new(&mut st, h, false).unwrap();
    put(&mut st, h, d, 1, 0);
    put(&mut st, h, d, 2, l);
    assert_eq!(dict::take(&mut st, h, d, 2).unwrap().0, Some(l));
    assert_eq!(entries(&st, h, d), vec![(1, 0), (2, 0)]);
    let (l, _) = list::push(&mut st, h, l, 5).unwrap();
    put(&mut st, h, d, 2, l);
    assert_eq!(entries(&st, h, d), vec![(1, 0), (2, l)]);
    release(&mut st, h, d).unwrap();
    reap_all(&mut st, h);
    assert_eq!(h.used_words(&st).unwrap(), 0);
}

#[test]
fn remove_hands_the_value_over_and_releases_the_key() {
    let (mut st, h) = setup();
    let d = dict::new(&mut st, h, true, true).unwrap();
    let k = str_(&mut st, h, "key");
    let v = str_(&mut st, h, "value");
    put(&mut st, h, d, k, v);
    let probe = str_(&mut st, h, "key");
    let got = dict::remove(&mut st, h, d, probe).unwrap().0.unwrap();
    assert_eq!(got, v);
    release(&mut st, h, got).unwrap();
    release(&mut st, h, probe).unwrap();
    release(&mut st, h, d).unwrap();
    reap_all(&mut st, h);
    assert_eq!(h.used_words(&st).unwrap(), 0);
}

#[test]
fn grow_keeps_order() {
    let (mut st, h) = setup_sized(4 << 20);
    let d = int_dict(&mut st, h);
    for k in 0..1000u64 {
        put(&mut st, h, d, k * 7919 % 10_007, k);
    }
    let e = entries(&st, h, d);
    assert_eq!(e.len(), 1000);
    for (i, (k, v)) in e.iter().enumerate() {
        assert_eq!(*v, i as u64);
        assert_eq!(*k, i as u64 * 7919 % 10_007);
        assert_eq!(dict::get(&st, h, d, *k).unwrap().0, Some(*v));
    }
    // Churn: removes leave tombstones that growth compacts away without losing order.
    for k in (0..1000u64).step_by(2) {
        dict::remove(&mut st, h, d, k * 7919 % 10_007).unwrap();
    }
    for k in 20_000..20_600u64 {
        put(&mut st, h, d, k, k);
    }
    let e = entries(&st, h, d);
    assert_eq!(e.len(), 500 + 600);
    assert_eq!(e[0].1, 1);
    assert_eq!(e[499].1, 999);
    assert_eq!(e[500], (20_000, 20_000));
}

fn scripted(st: &mut Store, h: Heap) -> u64 {
    let d = dict::new(st, h, true, true).unwrap();
    for i in 0..60u64 {
        let k = str_(st, h, &format!("key{}", i % 25));
        let v = str_(st, h, "v");
        put(st, h, d, k, v);
        if i % 3 == 0 {
            let probe = str_(st, h, &format!("key{}", i % 7));
            if let (Some(old), _) = dict::remove(st, h, d, probe).unwrap() {
                release(st, h, old).unwrap();
            }
            release(st, h, probe).unwrap();
        }
    }
    d
}

#[test]
fn layout_is_deterministic() {
    let (mut a, ha) = setup();
    let (mut b, hb) = setup();
    scripted(&mut a, ha);
    scripted(&mut b, hb);
    assert_eq!(a.mem.bytes(), b.mem.bytes());
}

#[test]
fn clone_is_independent_and_shares_children() {
    let (mut st, h) = setup();
    let d = dict::new(&mut st, h, true, true).unwrap();
    let k = str_(&mut st, h, "k");
    let v = str_(&mut st, h, "v");
    put(&mut st, h, d, k, v);
    incref(&mut st, h, d).unwrap();
    let (c, words) = make_unique(&mut st, h, d).unwrap();
    assert_ne!(c, d);
    assert!(words > 0);
    let k2 = str_(&mut st, h, "k2");
    put(&mut st, h, c, k2, 0);
    assert_eq!(dict::len(&st, h, c).unwrap(), 2);
    assert_eq!(dict::len(&st, h, d).unwrap(), 1);
    assert_eq!(dict::key_at(&st, h, c, 0).unwrap(), k);
    release(&mut st, h, c).unwrap();
    reap_all(&mut st, h);
    // The original still owns its children intact.
    assert_eq!(
        string::read(&st, h, dict::val_at(&st, h, d, 0).unwrap()).unwrap(),
        "v"
    );
    release(&mut st, h, d).unwrap();
    reap_all(&mut st, h);
    assert_eq!(h.used_words(&st).unwrap(), 0);
}

#[test]
fn reaping_a_dict_is_bounded_and_frees_everything() {
    let (mut st, h) = setup_sized(1 << 20);
    let d = dict::new(&mut st, h, true, true).unwrap();
    for i in 0..40 {
        let k = str_(&mut st, h, &format!("k{i}"));
        let v = str_(&mut st, h, "v");
        put(&mut st, h, d, k, v);
    }
    // Tombstones are skipped within the same budget.
    for i in 0..10 {
        let probe = str_(&mut st, h, &format!("k{i}"));
        let old = dict::remove(&mut st, h, d, probe).unwrap().0.unwrap();
        release(&mut st, h, old).unwrap();
        release(&mut st, h, probe).unwrap();
    }
    reap_all(&mut st, h);
    release(&mut st, h, d).unwrap();
    // The first step (the dict is the only zombie) spends one budget unit per dead position and two per
    // entry with two heap-kind slots: 16 units of budget report at most 8 entries * (2 releases + 1 position).
    let first = reap_step(&mut st, h, &NoFrames).unwrap().unwrap();
    assert!(
        first <= 3 * REAP_CHILDREN_MAX as u32 / 2,
        "first step reported {first}"
    );
    let mut steps = 0;
    while let Some(w) = reap_step(&mut st, h, &NoFrames).unwrap() {
        steps += 1;
        // A step releases at most REAP_CHILDREN_MAX children; the rest of its work is skipped
        // positions and the zeroed payloads of the blocks it freed.
        assert!(steps < 10_000 && w < 5_000, "step {steps} reported {w}");
    }
    assert!(steps >= 40 * 2 / REAP_CHILDREN_MAX);
    assert_eq!(h.used_words(&st).unwrap(), 0);
}

#[test]
fn deep_eq_cases() {
    let (mut st, h) = setup();
    let ll = |st: &mut Store, xs: &[&[i64]]| {
        let mut outer = list::new(st, h, true).unwrap();
        for xs in xs {
            let mut l = list::new(st, h, false).unwrap();
            for &x in *xs {
                l = list::push(st, h, l, x as u64).unwrap().0;
            }
            outer = list::push(st, h, outer, l).unwrap().0;
        }
        outer
    };
    let a = ll(&mut st, &[&[1, 2], &[3]]);
    let b = ll(&mut st, &[&[1, 2], &[3]]);
    let c = ll(&mut st, &[&[1, 2], &[3, 4]]);
    let d = ll(&mut st, &[&[1, 2]]);
    assert!(deep_eq(&st, h, a, b, true).unwrap().0);
    assert!(deep_eq(&st, h, a, a, true).unwrap().0);
    assert!(!deep_eq(&st, h, a, c, true).unwrap().0);
    assert!(!deep_eq(&st, h, a, d, true).unwrap().0);
    // Different kinds.
    let s = str_(&mut st, h, "x");
    assert!(!deep_eq(&st, h, a, s, true).unwrap().0);
    // A struct differing in one field.
    let p1 = pair(&mut st, h, 1, "a");
    let p2 = pair(&mut st, h, 1, "a");
    let p3 = pair(&mut st, h, 1, "b");
    let p4 = pair(&mut st, h, 2, "a");
    assert!(deep_eq(&st, h, p1, p2, true).unwrap().0);
    assert!(!deep_eq(&st, h, p1, p3, true).unwrap().0);
    assert!(!deep_eq(&st, h, p1, p4, true).unwrap().0);
    // Floats compare by bits.
    let nan = f64::NAN.to_bits();
    let fl = |st: &mut Store, x: f64| {
        let l = list::new(st, h, false).unwrap();
        list::push(st, h, l, x.to_bits()).unwrap().0
    };
    let (n1, n2) = (fl(&mut st, f64::NAN), fl(&mut st, f64::NAN));
    assert!(deep_eq(&st, h, nan, nan, false).unwrap().0);
    assert!(deep_eq(&st, h, n1, n2, true).unwrap().0);
    let (z, nz) = (fl(&mut st, 0.0), fl(&mut st, -0.0));
    assert!(!deep_eq(&st, h, z, nz, true).unwrap().0);
    assert!(
        !deep_eq(&st, h, 0.0f64.to_bits(), (-0.0f64).to_bits(), false)
            .unwrap()
            .0
    );
    // Dicts: same entries in the same order.
    let mk = |st: &mut Store, es: &[(u64, u64)]| {
        let d = dict::new(st, h, false, false).unwrap();
        for &(k, v) in es {
            dict::set(st, h, d, k, v).unwrap();
        }
        d
    };
    let d1 = mk(&mut st, &[(1, 1), (2, 2)]);
    let d2 = mk(&mut st, &[(1, 1), (2, 2)]);
    let d3 = mk(&mut st, &[(1, 1), (2, 3)]);
    let d4 = mk(&mut st, &[(1, 1)]);
    assert!(deep_eq(&st, h, d1, d2, true).unwrap().0);
    assert!(!deep_eq(&st, h, d1, d3, true).unwrap().0);
    assert!(!deep_eq(&st, h, d1, d4, true).unwrap().0);
    // A tombstone does not matter.
    let d5 = mk(&mut st, &[(9, 9), (1, 1), (2, 2)]);
    dict::remove(&mut st, h, d5, 9).unwrap();
    assert!(deep_eq(&st, h, d1, d5, true).unwrap().0);
    // Work is reported: comparing two distinct strings of 3 words costs at least those words.
    let long = "abcdefghijklmnopqrstuvwx";
    let (s1, s2) = (str_(&mut st, h, long), str_(&mut st, h, long));
    assert!(deep_eq(&st, h, s1, s2, true).unwrap().1 >= 3);
}

#[test]
fn deep_eq_depth_is_capped() {
    let (mut st, h) = setup();
    let chain = |st: &mut Store, depth: usize| {
        let mut v = list::new(st, h, false).unwrap();
        for _ in 0..depth {
            let l = list::new(st, h, true).unwrap();
            v = list::push(st, h, l, v).unwrap().0;
        }
        v
    };
    let (a, b) = (chain(&mut st, 30), chain(&mut st, 30));
    assert!(deep_eq(&st, h, a, b, true).unwrap().0);
    let (a, b) = (chain(&mut st, 40), chain(&mut st, 40));
    assert_eq!(deep_eq(&st, h, a, b, true).unwrap_err(), FaultKind::Corrupt);
    assert_eq!(key_hash(&st, h, a, true).unwrap_err(), FaultKind::Corrupt);
}

#[test]
fn list_contains_is_structural() {
    let (mut st, h) = setup();
    let mut l = list::new(&mut st, h, true).unwrap();
    let first = pair_of_ints(&mut st, h);
    l = list::push(&mut st, h, l, first).unwrap().0;
    let needle = pair_of_ints(&mut st, h);
    let (found, work) = list::contains(&st, h, l, needle).unwrap();
    assert!(found);
    assert!(work > 0);
    let miss = pair(&mut st, h, 5, "z");
    assert!(!list::contains(&st, h, l, miss).unwrap().0);
}

fn pair_of_ints(st: &mut Store, h: Heap) -> u64 {
    pair(st, h, 4, "q")
}

#[derive(Clone, Debug)]
enum Op {
    Set(i64, i64),
    Remove(i64),
    Take(i64),
    Dup,
    ReleaseHeld,
    ReapStep,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0i64..40, any::<i64>()).prop_map(|(k, v)| Op::Set(k, v)),
        2 => (0i64..40).prop_map(Op::Remove),
        1 => (0i64..40).prop_map(Op::Take),
        1 => Just(Op::Dup),
        1 => Just(Op::ReleaseHeld),
        1 => Just(Op::ReapStep),
    ]
}

fn model_entries(st: &Store, h: Heap, d: u64) -> Vec<(i64, i64)> {
    entries(st, h, d)
        .into_iter()
        .map(|(k, v)| (k as i64, v as i64))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn dict_matches_ordered_model_and_rolls_back(ops in proptest::collection::vec(op(), 0..120)) {
        let (mut st, h) = setup();
        st.mem.clear_log();
        let cp = st.mem.checkpoint();
        let initial = st.mem.bytes().to_vec();
        let mut cur = int_dict(&mut st, h);
        let mut model: Vec<(i64, i64)> = vec![];
        let mut held: Vec<(u64, Vec<(i64, i64)>)> = vec![];
        for o in ops {
            match o {
                Op::Dup => {
                    incref(&mut st, h, cur).unwrap();
                    held.push((cur, model.clone()));
                    continue;
                }
                Op::ReleaseHeld => {
                    if let Some((p, m)) = held.pop() {
                        prop_assert_eq!(model_entries(&st, h, p), m);
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
                Op::Set(k, v) => {
                    dict::set(&mut st, h, cur, k as u64, v as u64).unwrap();
                    match model.iter_mut().find(|e| e.0 == k) {
                        Some(e) => e.1 = v,
                        None => model.push((k, v)),
                    }
                }
                Op::Remove(k) => {
                    let got = dict::remove(&mut st, h, cur, k as u64).unwrap().0;
                    let want = model.iter().position(|e| e.0 == k).map(|i| model.remove(i).1);
                    prop_assert_eq!(got.map(|x| x as i64), want);
                }
                Op::Take(k) => {
                    let got = dict::take(&mut st, h, cur, k as u64).unwrap().0;
                    let want = model.iter_mut().find(|e| e.0 == k).map(|e| std::mem::take(&mut e.1));
                    prop_assert_eq!(got.map(|x| x as i64), want);
                }
                _ => unreachable!(),
            }
            prop_assert_eq!(model_entries(&st, h, cur), model.clone());
            prop_assert_eq!(dict::len(&st, h, cur).unwrap() as usize, model.len());
            for k in 0..40i64 {
                let got = dict::get(&st, h, cur, k as u64).unwrap().0.map(|x| x as i64);
                prop_assert_eq!(got, model.iter().find(|e| e.0 == k).map(|e| e.1));
            }
        }
        for (p, m) in held.drain(..) {
            prop_assert_eq!(model_entries(&st, h, p), m);
            release(&mut st, h, p).unwrap();
        }
        release(&mut st, h, cur).unwrap();
        reap_all(&mut st, h);
        prop_assert_eq!(h.used_words(&st).unwrap(), 0);
        st.mem.rollback_to(cp).unwrap();
        prop_assert_eq!(st.mem.bytes(), &initial[..]);
    }
}
