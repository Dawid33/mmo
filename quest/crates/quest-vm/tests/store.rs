use proptest::prelude::*;
use quest_vm::consts::HEADER_WORDS;
use quest_vm::layout::*;
use quest_vm::{Checkpoint, Layout, LoadError, Mem, MemError, Store};

fn store() -> Store {
    Store::new(100, 65_536).unwrap()
}

#[test]
fn plan_regions_are_ordered_disjoint_and_fill_memory() {
    for mem_bytes in [65_536u32, 262_144] {
        let l = Layout::plan(100, mem_bytes).unwrap();
        let script_words = (l.heap_end - l.static_base) / 8;
        let regions = [
            (0, HEADER_WORDS * 8),
            (l.static_base, l.heap_base),
            (l.heap_base, l.heap_end),
            (l.shadow_base, l.shadow_base + script_words * 8),
            (l.dirty_base, l.inbox_base),
            (l.inbox_base, l.inbox_end),
            (l.queue_base, l.promises_base),
            (l.promises_base, l.parked_base),
            (l.parked_base, l.cmds_base),
            (l.cmds_base, l.cmds_end),
            (l.trace_base, l.trace_base + 16 * 8),
        ];
        for r in regions.windows(2) {
            assert!(r[0].0 <= r[0].1);
            assert!(r[0].1 <= r[1].0, "{r:?}");
        }
        assert_eq!(l.static_base, HEADER_WORDS * 8);
        assert_eq!(l.heap_base, l.static_base + 100 * 8);
        assert_eq!(l.shadow_base, l.heap_end);
        assert_eq!(l.cmds_end, l.trace_base);
        let end = regions.last().unwrap().1;
        assert!(end <= mem_bytes);
        assert!(mem_bytes - end < 64, "slack {}", mem_bytes - end);
        // The dirty bitmap covers every script word.
        assert!((l.inbox_base - l.dirty_base) * 8 >= script_words);
    }
}

#[test]
fn plan_too_small() {
    let Err(LoadError::MemTooSmall { need }) = Layout::plan(100, 16_384) else {
        panic!("expected MemTooSmall")
    };
    assert!(need > 16_384);
    let l = Layout::plan(100, need).unwrap();
    assert!(l.heap_end - l.heap_base >= 4_096);
    assert!(matches!(
        Layout::plan(100, need - 8),
        Err(LoadError::MemTooSmall { .. })
    ));
}

#[test]
fn header_roundtrip() {
    let mut s = store();
    assert_eq!(Layout::read_header(&s.mem).unwrap(), s.layout);
    s.mem.w64(0, 7).unwrap();
    assert_eq!(Layout::read_header(&s.mem), Err(LoadError::BadMagic));
    let mut s = store();
    s.mem.w64(H_HEAP_END * 8, 8).unwrap();
    assert!(matches!(
        Layout::read_header(&s.mem),
        Err(LoadError::Invalid(_))
    ));
    assert!(matches!(
        Layout::read_header(&Mem::new(8)),
        Err(LoadError::BadMagic)
    ));
    assert!(matches!(
        Layout::read_header(&Mem::new(0)),
        Err(LoadError::Invalid(_))
    ));
}

#[test]
fn abort_restores_script_words_only() {
    let mut s = store();
    let (st, hp, q) = (
        s.layout.slot_off(3),
        s.layout.heap_base + 16,
        s.layout.queue_base,
    );
    s.ws(st, 1).unwrap();
    s.ws(hp, 2).unwrap();
    s.wr(q, 3).unwrap();
    s.segment_open();
    s.ws(st, 10).unwrap();
    s.ws(hp, 20).unwrap();
    s.wr(q, 30).unwrap();
    s.segment_abort();
    assert_eq!((s.r(st), s.r(hp), s.r(q)), (Ok(1), Ok(2), Ok(30)));
    assert!(!s.segment_is_open());
}

#[test]
fn abort_restores_first_value_after_many_rewrites() {
    let mut s = store();
    let a = s.layout.slot_off(0);
    s.ws(a, 5).unwrap();
    s.segment_open();
    for v in 6..20 {
        s.ws(a, v).unwrap();
    }
    s.segment_abort();
    assert_eq!(s.r(a), Ok(5));
}

#[test]
fn commit_makes_writes_final() {
    let mut s = store();
    let a = s.layout.slot_off(0);
    s.segment_open();
    s.ws(a, 9).unwrap();
    s.segment_commit();
    s.segment_open();
    s.ws(a, 11).unwrap();
    s.segment_abort();
    assert_eq!(s.r(a), Ok(9));
}

#[test]
fn write_outside_a_segment_is_not_shadowed() {
    let mut s = store();
    let a = s.layout.slot_off(0);
    s.ws(a, 4).unwrap();
    s.segment_open();
    s.segment_abort();
    assert_eq!(s.r(a), Ok(4));
}

#[test]
fn rollback_across_commit_then_abort() {
    let mut s = store();
    let (a, b) = (s.layout.slot_off(0), s.layout.heap_base);
    s.ws(a, 1).unwrap();
    s.segment_open();
    s.ws(a, 2).unwrap();
    let cp = s.mem.checkpoint();
    let at_open_plus = s.mem.bytes().to_vec();
    s.ws(b, 3).unwrap();
    s.ws(a, 4).unwrap();
    s.segment_commit();
    s.mem.rollback_to(cp).unwrap();
    assert_eq!(s.mem.bytes(), &at_open_plus[..]);
    assert!(s.segment_is_open());
    s.segment_abort();
    assert_eq!((s.r(a), s.r(b)), (Ok(1), Ok(0)));
}

#[test]
fn misplaced_writes_are_errors_not_panics() {
    let mut s = store();
    let l = s.layout;
    assert_eq!(s.ws(0, 1), Err(MemError::OutOfRange { off: 0 }));
    assert_eq!(
        s.ws(l.heap_end, 1),
        Err(MemError::OutOfRange { off: l.heap_end })
    );
    assert_eq!(
        s.ws(l.static_base + 1, 1),
        Err(MemError::Unaligned {
            off: l.static_base + 1
        })
    );
    assert!(s.wr(l.static_base, 1).is_err());
    assert!(s.wr(l.heap_end, 1).is_err());
    assert!(s.wr(l.dirty_base, 1).is_err());
    assert!(s.wr(l.mem_bytes, 1).is_err());
    assert!(s.wr(u32::MAX - 7, 1).is_err());
    assert!(s.wr(l.trace_base, 1).is_ok());
}

#[test]
fn nested_open_keeps_segment_and_stray_commit_abort_are_no_ops() {
    let mut s = store();
    let a = s.layout.slot_off(0);
    s.segment_commit();
    s.segment_abort();
    s.ws(a, 1).unwrap();
    s.segment_open();
    s.ws(a, 2).unwrap();
    s.segment_open();
    s.segment_abort();
    assert_eq!(s.r(a), Ok(1));
}

#[test]
fn abort_restores_last_word_and_both_sides_of_a_bitmap_boundary() {
    let mut s = store();
    let l = s.layout;
    let offs = [
        l.static_base + 63 * 8,
        l.static_base + 64 * 8,
        l.heap_end - 8,
    ];
    for (i, o) in offs.iter().enumerate() {
        s.ws(*o, 100 + i as u64).unwrap();
    }
    s.segment_open();
    for o in offs {
        s.ws(o, 7).unwrap();
    }
    s.segment_abort();
    for (i, o) in offs.iter().enumerate() {
        assert_eq!(s.r(*o), Ok(100 + i as u64));
    }
    for dw in 0..(l.inbox_base - l.dirty_base) / 8 {
        assert_eq!(s.r(l.dirty_base + dw * 8), Ok(0));
    }
}

#[test]
fn stray_dirty_bits_past_the_last_script_word_are_ignored_and_cleared() {
    // A memory whose script words are not a multiple of 64, so the last dirty word has spare bits.
    let mut mem_bytes = 65_536;
    let mut s = loop {
        let s = Store::new(100, mem_bytes).unwrap();
        if !((s.layout.heap_end - s.layout.static_base) / 8).is_multiple_of(64) {
            break s;
        }
        mem_bytes += 8;
    };
    let l = s.layout;
    let script_words = u64::from((l.heap_end - l.static_base) / 8);
    let dirty_off = l.dirty_base + (script_words / 64) as u32 * 8;
    let bit = script_words % 64;
    s.segment_open();
    s.mem.w64(l.queue_base, 0xCD).unwrap();
    s.mem
        .w64(dirty_off, s.r(dirty_off).unwrap() | (1 << bit) | (1 << 63))
        .unwrap();
    let runtime: Vec<u8> = s.mem.bytes()[l.inbox_base as usize..].to_vec();
    s.segment_abort();
    assert_eq!(&s.mem.bytes()[l.inbox_base as usize..], &runtime[..]);
    assert_eq!(s.r(dirty_off), Ok(0));
}

#[derive(Clone, Debug)]
enum Op {
    Ws(u16, u64),
    Wr(u8, u64),
    Open,
    Commit,
    Abort,
    Checkpoint,
    Rollback(u8),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0u16..400, 0u64..4).prop_map(|(i, v)| Op::Ws(i, v)),
        2 => (0u8..16, 0u64..4).prop_map(|(i, v)| Op::Wr(i, v)),
        2 => Just(Op::Open),
        2 => Just(Op::Commit),
        2 => Just(Op::Abort),
        2 => Just(Op::Checkpoint),
        2 => any::<u8>().prop_map(Op::Rollback),
    ]
}

#[derive(Clone)]
struct Model {
    script: Vec<u64>,
    runtime: Vec<u64>,
    open: bool,
    seg: Vec<u64>,
}

proptest! {
    #[test]
    fn segments_and_rollback_match_a_model(ops in proptest::collection::vec(op(), 0..80)) {
        let mut s = Store::new(100, 65_536).unwrap();
        let l = s.layout;
        let n = ((l.heap_end - l.static_base) / 8) as usize;
        let mut m = Model { script: vec![0; n], runtime: vec![0; 16], open: false, seg: vec![] };
        let mut cps: Vec<(Checkpoint, Model)> = vec![];
        let script_off = |i: u16| l.static_base + (u32::from(i) % n as u32) * 8;
        for op in ops {
            match op {
                Op::Ws(i, v) => {
                    s.ws(script_off(i), v).unwrap();
                    m.script[usize::from(i) % n] = v;
                }
                Op::Wr(i, v) => {
                    s.wr(l.queue_base + u32::from(i) * 8, v).unwrap();
                    m.runtime[usize::from(i)] = v;
                }
                Op::Open => {
                    s.segment_open();
                    if !m.open { m.open = true; m.seg = m.script.clone(); }
                }
                Op::Commit => {
                    s.segment_commit();
                    m.open = false;
                }
                Op::Abort => {
                    s.segment_abort();
                    if m.open { m.script = m.seg.clone(); }
                    m.open = false;
                }
                Op::Checkpoint => cps.push((s.mem.checkpoint(), m.clone())),
                Op::Rollback(k) => {
                    if !cps.is_empty() {
                        let k = usize::from(k) % cps.len();
                        s.mem.rollback_to(cps[k].0).unwrap();
                        m = cps[k].1.clone();
                        cps.truncate(k + 1);
                    }
                }
            }
            prop_assert_eq!(s.segment_is_open(), m.open);
            for (i, want) in m.script.iter().enumerate() {
                prop_assert_eq!(s.r(l.static_base + i as u32 * 8).unwrap(), *want);
            }
            for (i, want) in m.runtime.iter().enumerate() {
                prop_assert_eq!(s.r(l.queue_base + i as u32 * 8).unwrap(), *want);
            }
        }
    }
}
