use proptest::prelude::*;
use quest_vm::alloc::*;
use quest_vm::{MemError, Store};

fn setup() -> (Store, Heap) {
    let mut st = Store::new(16, 65_536).unwrap();
    let l = st.layout;
    let h = Heap {
        base: l.heap_base,
        end: l.heap_end,
        script: true,
    };
    h.init(&mut st).unwrap();
    (st, h)
}

fn inbox(st: &Store) -> Heap {
    Heap {
        base: st.layout.inbox_base,
        end: st.layout.inbox_end,
        script: false,
    }
}

fn a(st: &mut Store, h: &Heap, kind: Kind, words: u32) -> u32 {
    h.alloc(st, kind, words).unwrap().unwrap()
}

#[test]
fn first_address_is_pinned() {
    let (mut st, h) = setup();
    let p = a(&mut st, &h, Kind::Str, 1);
    assert_eq!(p, h.base + ALLOC_HEADER_WORDS * 8);
}

#[test]
fn new_object_header_and_zero_payload() {
    let (mut st, h) = setup();
    let p = a(&mut st, &h, Kind::Raw, 3);
    let hd = h.header(&st, p).unwrap();
    assert_eq!(
        (hd.refcount, hd.kind, hd.class, hd.tag, hd.len, hd.aux),
        (1, Kind::Raw, 3, 0, 0, 0)
    );
    for i in 2..8 {
        assert_eq!(st.r(p + i * 8).unwrap(), 0);
    }
}

#[test]
fn smallest_class_is_header_only() {
    let (mut st, h) = setup();
    let p = a(&mut st, &h, Kind::Raw, 0);
    assert_eq!(h.header(&st, p).unwrap().class, 1);
    let q = a(&mut st, &h, Kind::Raw, 0);
    assert_eq!(q, p + 16);
}

#[test]
fn same_class_block_is_reused_lifo() {
    let (mut st, h) = setup();
    let x = a(&mut st, &h, Kind::Str, 3);
    let _b = a(&mut st, &h, Kind::Str, 10);
    h.free(&mut st, x).unwrap();
    assert_eq!(a(&mut st, &h, Kind::Str, 4), x);
    let x = a(&mut st, &h, Kind::Str, 3);
    let y = a(&mut st, &h, Kind::Str, 3);
    h.free(&mut st, x).unwrap();
    h.free(&mut st, y).unwrap();
    assert_eq!(a(&mut st, &h, Kind::Str, 3), y);
    assert_eq!(a(&mut st, &h, Kind::Str, 3), x);
}

#[test]
fn freed_payload_is_zero() {
    let (mut st, h) = setup();
    let p = a(&mut st, &h, Kind::Str, 6);
    for i in 2..8 {
        st.ws(p + i * 8, 0xdead_beef).unwrap();
    }
    h.free(&mut st, p).unwrap();
    for i in 2..8 {
        assert_eq!(st.r(p + i * 8).unwrap(), 0);
    }
    assert_eq!(h.header(&st, p).unwrap().kind, Kind::Free);
    let q = a(&mut st, &h, Kind::Str, 6);
    assert_eq!(q, p);
    assert_eq!(h.header(&st, q).unwrap().len, 0);
}

#[test]
fn double_free_is_mem_error() {
    let (mut st, h) = setup();
    let p = a(&mut st, &h, Kind::Str, 2);
    h.free(&mut st, p).unwrap();
    assert!(h.free(&mut st, p).is_err());
}

#[test]
fn header_packing_pinned() {
    let (mut st, h) = setup();
    let p = a(&mut st, &h, Kind::Obj, 6);
    let hd = ObjHeader {
        refcount: 7,
        kind: Kind::Obj,
        class: 3,
        tag: 0x1234,
        len: 5,
        aux: 9,
    };
    h.set_header(&mut st, p, hd).unwrap();
    assert_eq!(st.r(p).unwrap(), 0x1234_0306_0000_0007);
    assert_eq!(st.r(p + 8).unwrap(), 0x0000_0009_0000_0005);
    assert_eq!(h.header(&st, p).unwrap(), hd);
}

#[test]
fn full_region_is_alloc_error() {
    let (mut st, h) = setup();
    while a_ok(&mut st, &h) {}
    let before = st.mem.bytes().to_vec();
    assert_eq!(h.alloc(&mut st, Kind::Str, 0).unwrap(), Err(AllocError));
    assert_eq!(
        h.alloc(&mut st, Kind::Str, u32::MAX).unwrap(),
        Err(AllocError)
    );
    assert_eq!(st.mem.bytes(), &before[..]);
}

fn a_ok(st: &mut Store, h: &Heap) -> bool {
    h.alloc(st, Kind::Raw, 0).unwrap().is_ok()
}

#[test]
fn bad_pointer_is_mem_error() {
    let (mut st, h) = setup();
    let p = a(&mut st, &h, Kind::Str, 2);
    assert!(h.header(&st, h.end + 64).is_err());
    assert!(h.header(&st, 0).is_err());
    assert!(
        h.header(&st, h.base).is_err(),
        "inside the allocator header"
    );
    assert!(matches!(
        h.header(&st, p + 4),
        Err(MemError::Unaligned { .. })
    ));
    assert!(h.header(&st, u32::MAX - 7).is_err());
    assert!(h.header(&st, p + 8 * 100).is_err(), "past the bump top");
    assert!(h.free(&mut st, p + 3).is_err());
    assert!(
        h.set_header(
            &mut st,
            h.end,
            ObjHeader {
                refcount: 1,
                kind: Kind::Str,
                class: 2,
                tag: 0,
                len: 0,
                aux: 0
            }
        )
        .is_err()
    );
}

#[test]
fn corrupt_header_words_are_errors() {
    let (mut st, h) = setup();
    let p = a(&mut st, &h, Kind::Str, 2);
    st.ws(p, 1 | (200u64 << 32)).unwrap();
    assert!(h.header(&st, p).is_err(), "unknown kind");
    st.ws(p, 1 | (1u64 << 32) | (60u64 << 40)).unwrap();
    assert!(h.header(&st, p).is_err(), "class out of range");
    st.ws(p, 1 | (1u64 << 32)).unwrap();
    assert!(h.header(&st, p).is_err(), "class 0");
}

#[test]
fn used_words_returns_after_frees() {
    let (mut st, h) = setup();
    assert_eq!(h.used_words(&st).unwrap(), 0);
    let p = a(&mut st, &h, Kind::Str, 3);
    let q = a(&mut st, &h, Kind::Str, 0);
    assert_eq!(h.used_words(&st).unwrap(), 8 + 2);
    h.free(&mut st, p).unwrap();
    h.free(&mut st, q).unwrap();
    assert_eq!(h.used_words(&st).unwrap(), 0);
}

#[test]
fn inbox_instance_is_independent() {
    let (mut st, h) = setup();
    let ib = inbox(&st);
    ib.init(&mut st).unwrap();
    let script_before = st.mem.bytes()[h.base as usize..h.end as usize].to_vec();
    let p = ib.alloc(&mut st, Kind::Raw, 4).unwrap().unwrap();
    assert_eq!(p, ib.base + ALLOC_HEADER_WORDS * 8);
    ib.free(&mut st, p).unwrap();
    assert_eq!(
        &st.mem.bytes()[h.base as usize..h.end as usize],
        &script_before[..]
    );
    assert!(
        h.header(&st, p).is_err(),
        "inbox pointer is outside the script heap"
    );
}

#[test]
fn segment_abort_restores_allocator_state() {
    let (mut st, h) = setup();
    let keep = a(&mut st, &h, Kind::Str, 3);
    let before = st.mem.bytes().to_vec();
    st.segment_open();
    let _ = a(&mut st, &h, Kind::Str, 20);
    h.free(&mut st, keep).unwrap();
    st.segment_abort();
    // Everything but the (runtime) segment flag bookkeeping is restored.
    assert_eq!(
        &st.mem.bytes()[h.base as usize..h.end as usize],
        &before[h.base as usize..h.end as usize]
    );
}

#[derive(Clone, Debug)]
enum Op {
    Alloc(u32),
    Free(usize),
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        prop_oneof![
            (0u32..40).prop_map(Op::Alloc),
            (0usize..16).prop_map(Op::Free)
        ],
        0..60,
    )
}

fn run(st: &mut Store, h: &Heap, ops: &[Op]) -> Vec<u32> {
    let mut live: Vec<u32> = Vec::new();
    let mut trace = Vec::new();
    for op in ops {
        match op {
            Op::Alloc(n) => {
                if let Ok(p) = h.alloc(st, Kind::Raw, *n).unwrap() {
                    live.push(p);
                    trace.push(p);
                }
            }
            Op::Free(i) => {
                if !live.is_empty() {
                    let p = live.remove(i % live.len());
                    h.free(st, p).unwrap();
                }
            }
        }
    }
    trace
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn two_runs_same_addresses(ops in ops()) {
        let (mut s1, h1) = setup();
        let (mut s2, h2) = setup();
        prop_assert_eq!(run(&mut s1, &h1, &ops), run(&mut s2, &h2, &ops));
        prop_assert_eq!(s1.mem.bytes(), s2.mem.bytes());
    }

    #[test]
    fn alloc_free_rollback_bit_exact(pre in ops(), ops in ops()) {
        let (mut st, h) = setup();
        run(&mut st, &h, &pre);
        let before = st.mem.bytes().to_vec();
        let cp = st.mem.checkpoint();
        run(&mut st, &h, &ops);
        st.mem.rollback_to(cp).unwrap();
        prop_assert_eq!(st.mem.bytes(), &before[..]);
    }
}

#[test]
fn address_overflow_is_corrupt() {
    let (mut st, _) = setup();
    // A heap whose base lies at the top of the address space: every allocator word is past `u32::MAX`.
    let top = Heap {
        base: u32::MAX - 7,
        end: u32::MAX - 7,
        script: false,
    };
    assert!(matches!(top.used_words(&st), Err(MemError::Corrupt { .. })));
    assert!(matches!(
        top.zombie_head(&st),
        Err(MemError::Corrupt { .. })
    ));
    assert!(matches!(
        top.alloc(&mut st, Kind::Raw, 1),
        Err(MemError::Corrupt { .. })
    ));
    assert!(top.init(&mut st).is_err());
}

#[test]
fn set_header_refuses_kind_free() {
    let (mut st, h) = setup();
    let p = a(&mut st, &h, Kind::Raw, 3);
    let mut hd = h.header(&st, p).unwrap();
    hd.kind = Kind::Free;
    assert_eq!(
        h.set_header(&mut st, p, hd),
        Err(MemError::Corrupt { off: p })
    );
    h.free(&mut st, p).unwrap();
    let mut freed = h.header(&st, p).unwrap();
    freed.kind = Kind::Raw;
    assert_eq!(
        h.set_header(&mut st, p, freed),
        Err(MemError::Corrupt { off: p }),
        "a free block is not revived through its header"
    );
}

#[test]
fn hdr_refuses_a_zombie() {
    use quest_vm::fault::FaultKind;
    use quest_vm::{object, string};
    let (mut st, h) = setup();
    let a = string::new(&mut st, h, "a").unwrap();
    let b = string::new(&mut st, h, "b").unwrap();
    object::release(&mut st, h, a).unwrap();
    object::release(&mut st, h, b).unwrap();
    // `b` heads the zombie list, `a` ends it (its link is 0).
    assert_eq!(string::read(&st, h, a), Err(FaultKind::Corrupt));
    assert_eq!(string::read(&st, h, b), Err(FaultKind::Corrupt));
    assert_eq!(object::release(&mut st, h, b), Err(FaultKind::Corrupt));
}
