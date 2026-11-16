use proptest::prelude::*;
use quest_vm::alloc::Heap;
use quest_vm::eq::{deep_eq, key_hash};
use quest_vm::fault::FaultKind;
use quest_vm::fmt::fmt_value;
use quest_vm::object::{FrameMaps, incref, obj, reap_step};
use quest_vm::types::{TypeDesc, TypeTable};
use quest_vm::wire::{FromWire, WireError, WireValue, from_wire, to_wire};
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

fn reap_all(st: &mut Store, h: Heap) {
    while reap_step(st, h, &NoFrames).unwrap().is_some() {}
}

fn s(x: &str) -> String {
    x.to_string()
}

fn field(n: &str, v: WireValue) -> (String, WireValue) {
    (s(n), v)
}

// Type ids of `shop()`.
const INT: u16 = 0;
const STR: u16 = 1;
const OPT_INT: u16 = 2;
const ITEM: u16 = 3;
const ITEMS: u16 = 4;
const STAGE: u16 = 5;
const BAG: u16 = 6;
const FLOAT: u16 = 7;
const BOOL: u16 = 8;
const UNIT: u16 = 9;
const PROMISE: u16 = 10;
const INTS: u16 = 11;
const LEDGER: u16 = 12;
const STRS: u16 = 13;

fn shop() -> TypeTable {
    TypeTable(vec![
        TypeDesc::Int,
        TypeDesc::Str,
        TypeDesc::Enum {
            name: s("Option"),
            variants: vec![(s("None"), vec![]), (s("Some"), vec![(s("value"), INT)])],
        },
        TypeDesc::Struct {
            name: s("Item"),
            fields: vec![(s("name"), STR), (s("qty"), OPT_INT)],
        },
        TypeDesc::List(ITEM),
        TypeDesc::Enum {
            name: s("Stage"),
            variants: vec![
                (s("Asked"), vec![]),
                (s("Accepted"), vec![(s("tick"), INT)]),
            ],
        },
        TypeDesc::Dict(STR, INTS),
        TypeDesc::Float,
        TypeDesc::Bool,
        TypeDesc::Unit,
        TypeDesc::Promise(INT),
        TypeDesc::List(INT),
        TypeDesc::Struct {
            name: s("Ledger"),
            fields: vec![(s("owner"), STR), (s("entries"), INTS)],
        },
        TypeDesc::List(STR),
    ])
}

fn some(n: i64) -> WireValue {
    WireValue::Variant {
        name: s("Some"),
        fields: vec![field("value", WireValue::Int(n))],
    }
}

fn none() -> WireValue {
    WireValue::Variant {
        name: s("None"),
        fields: vec![],
    }
}

fn item(name: &str, qty: WireValue) -> WireValue {
    WireValue::Struct {
        name: s("Item"),
        fields: vec![field("name", WireValue::Str(s(name))), field("qty", qty)],
    }
}

fn load(st: &mut Store, h: Heap, t: &TypeTable, ty: u16, w: &WireValue) -> u64 {
    from_wire(st, h, t, ty, w).unwrap().0
}

// ---- encoding --------------------------------------------------------------------------------------

#[test]
fn encode_decode_roundtrip() {
    let every = WireValue::List(vec![
        WireValue::Unit,
        WireValue::Bool(true),
        WireValue::Int(-5),
        WireValue::Float(1.5),
        WireValue::Str(s("héllo")),
        WireValue::Dict(vec![(
            WireValue::Str(s("k")),
            WireValue::List(vec![WireValue::Struct {
                name: s("P"),
                fields: vec![field("v", some(3))],
            }]),
        )]),
    ]);
    let mut bytes = vec![];
    every.encode(&mut bytes);
    assert_eq!(WireValue::decode(&bytes), Ok(every));

    let p = WireValue::Struct {
        name: s("P"),
        fields: vec![field("id", WireValue::Int(7))],
    };
    let mut out = vec![];
    p.encode(&mut out);
    let expect: Vec<u8> = vec![
        7, // Struct
        1, 0, 0, 0, b'P', // name
        1, 0, 0, 0, // one field
        2, 0, 0, 0, b'i', b'd', // field name
        2, 7, 0, 0, 0, 0, 0, 0, 0, // Int 7
    ];
    assert_eq!(out, expect);
}

#[test]
fn nan_is_canonical_on_the_wire() {
    let mut out = vec![];
    WireValue::Float(f64::from_bits(0xfff8_0000_0000_0001)).encode(&mut out);
    let mut expect = vec![3];
    expect.extend_from_slice(&0x7ff8_0000_0000_0000u64.to_le_bytes());
    assert_eq!(out, expect);
}

#[test]
fn decode_rejects_bad_input() {
    let mut full = vec![];
    WireValue::List(vec![WireValue::Int(1)]).encode(&mut full);
    assert_eq!(
        WireValue::decode(&full[..full.len() - 1]),
        Err(WireError::Truncated)
    );
    let mut trailing = full.clone();
    trailing.push(0);
    assert_eq!(WireValue::decode(&trailing), Err(WireError::Trailing));
    assert_eq!(WireValue::decode(&[200]), Err(WireError::BadTag(200)));
    assert_eq!(WireValue::decode(&[]), Err(WireError::Truncated));
    assert_eq!(
        WireValue::decode(&[4, 2, 0, 0, 0, 0xff, 0xfe]),
        Err(WireError::BadUtf8)
    );
    // A count of four billion in ten bytes: Truncated, and no allocation sized by it.
    for tag in [5u8, 6, 7, 8] {
        let mut b = vec![tag];
        if tag >= 7 {
            b.extend_from_slice(&[0, 0, 0, 0]); // empty name
        }
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            WireValue::decode(&b),
            Err(WireError::Truncated),
            "tag {tag}"
        );
    }
    // Nesting has no cap: 33 nested lists decode.
    let nest = |n: usize| {
        let mut b = vec![];
        for _ in 0..n {
            b.extend_from_slice(&[5, 1, 0, 0, 0]);
        }
        b.push(0);
        b
    };
    assert!(WireValue::decode(&nest(33)).is_ok());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = WireValue::decode(&bytes);
    }
}

// ---- heap conversion -------------------------------------------------------------------------------

#[test]
fn from_wire_is_structural() {
    let t = TypeTable(vec![
        TypeDesc::Int,
        TypeDesc::Str,
        TypeDesc::Struct {
            name: s("B"),
            fields: vec![(s("x"), INT), (s("y"), STR)],
        },
        TypeDesc::Enum {
            name: s("E"),
            variants: vec![(s("A"), vec![(s("n"), INT)])],
        },
    ]);
    let (mut st, h) = setup();
    let wrong = |st: &mut Store, ty: u16, w: &WireValue| {
        assert!(
            matches!(from_wire(st, h, &t, ty, w), Err(FromWire::WrongType)),
            "{w:?}"
        );
    };
    let st_of = |fields: Vec<(String, WireValue)>| WireValue::Struct {
        name: s("A"),
        fields,
    };
    let x = || field("x", WireValue::Int(1));
    let y = || field("y", WireValue::Str(s("why")));

    // Stored as `A { y, x }`, loaded into `B { x, y }`.
    let v = load(&mut st, h, &t, 2, &st_of(vec![y(), x()]));
    assert_eq!(obj::get_field(&st, h, v, 0).unwrap(), 1);
    let ys = obj::get_field(&st, h, v, 1).unwrap();
    assert_eq!(string::read(&st, h, ys).unwrap(), "why");

    wrong(&mut st, 2, &st_of(vec![x()])); // a missing field
    wrong(
        &mut st,
        2,
        &st_of(vec![x(), y(), field("z", WireValue::Int(0))]),
    ); // an extra one
    wrong(&mut st, 2, &st_of(vec![x(), field("y", WireValue::Int(2))])); // another type
    wrong(&mut st, 2, &st_of(vec![x(), x()])); // a repeated name
    wrong(&mut st, 3, &none()); // a variant the enum does not have
    wrong(
        &mut st,
        3,
        &WireValue::Variant {
            name: s("A"),
            fields: vec![],
        },
    );
    wrong(&mut st, INT, &WireValue::Float(1.0));
}

#[test]
fn to_wire_from_wire_roundtrip_through_the_heap() {
    let t = shop();
    let (mut st, h) = setup();
    let items = WireValue::List(vec![
        item("sword", some(2)),
        item("shield", none()),
        item("bow", some(-9)),
    ]);
    let v = load(&mut st, h, &t, ITEMS, &items);
    assert_eq!(to_wire(&st, h, &t, ITEMS, v).unwrap().0, items);

    let bag = WireValue::Dict(vec![
        (
            WireValue::Str(s("b")),
            WireValue::List(vec![WireValue::Int(2)]),
        ),
        (WireValue::Str(s("a")), WireValue::List(vec![])),
    ]);
    let d = load(&mut st, h, &t, BAG, &bag);
    assert_eq!(
        to_wire(&st, h, &t, BAG, d).unwrap().0,
        bag,
        "insertion order"
    );

    for (ty, w) in [
        (UNIT, WireValue::Unit),
        (BOOL, WireValue::Bool(true)),
        (FLOAT, WireValue::Float(-0.0)),
        (
            STAGE,
            WireValue::Variant {
                name: s("Accepted"),
                fields: vec![field("tick", WireValue::Int(5))],
            },
        ),
        (
            STAGE,
            WireValue::Variant {
                name: s("Asked"),
                fields: vec![],
            },
        ),
    ] {
        let v = load(&mut st, h, &t, ty, &w);
        assert_eq!(to_wire(&st, h, &t, ty, v).unwrap().0, w);
    }
    // A field-less variant is an odd immediate: `(tag << 1) | 1`.
    assert_eq!(
        load(
            &mut st,
            h,
            &t,
            STAGE,
            &WireValue::Variant {
                name: s("Asked"),
                fields: vec![]
            }
        ),
        1
    );
    // A NaN loads canonical.
    let nan = load(
        &mut st,
        h,
        &t,
        FLOAT,
        &WireValue::Float(f64::from_bits(0xfff8_0000_0000_0001)),
    );
    assert_eq!(nan, 0x7ff8_0000_0000_0000);

    // The loaded objects are well formed: releasing them and reaping empties the heap.
    let before = h.used_words(&st).unwrap();
    let v = load(&mut st, h, &t, ITEMS, &items);
    quest_vm::object::release(&mut st, h, v).unwrap();
    reap_all(&mut st, h);
    assert_eq!(h.used_words(&st).unwrap(), before);
}

#[test]
fn from_wire_heap_full() {
    let t = shop();
    let (mut st, h) = setup_sized(32_768);
    let before = h.used_words(&st).unwrap();
    // Dozens of items, each with a string, then a dict with many keys: far more than 32 KiB.
    let big = WireValue::List(
        (0..2_000)
            .map(|i| item(&format!("item number {i}"), some(i)))
            .collect(),
    );
    assert!(matches!(
        from_wire(&mut st, h, &t, ITEMS, &big),
        Err(FromWire::Fault(FaultKind::HeapFull))
    ));
    reap_all(&mut st, h);
    assert_eq!(h.used_words(&st).unwrap(), before);

    let bag = WireValue::Dict(
        (0..2_000)
            .map(|i| {
                (
                    WireValue::Str(format!("key {i}")),
                    WireValue::List(vec![WireValue::Int(i)]),
                )
            })
            .collect(),
    );
    assert!(matches!(
        from_wire(&mut st, h, &t, BAG, &bag),
        Err(FromWire::Fault(FaultKind::HeapFull))
    ));
    reap_all(&mut st, h);
    assert_eq!(h.used_words(&st).unwrap(), before);

    // A dict repeating a key is not a dict of this language.
    let (mut st, h) = setup();
    let dup = WireValue::Dict(vec![
        (WireValue::Str(s("a")), WireValue::List(vec![])),
        (WireValue::Str(s("a")), WireValue::List(vec![])),
    ]);
    let before = h.used_words(&st).unwrap();
    assert!(matches!(
        from_wire(&mut st, h, &t, BAG, &dup),
        Err(FromWire::WrongType)
    ));
    reap_all(&mut st, h);
    assert_eq!(h.used_words(&st).unwrap(), before);
}

#[test]
fn walks_are_work_capped() {
    // Type k is `List(k - 1)` over `Int` (type 0). Level 1 is an empty list; each level above holds the
    // level below twice (shared, one object): 2^19 leaves under 20 objects. The walk must stop at its
    // budget instead of enumerating them.
    let mut descs = vec![TypeDesc::Int];
    for k in 1..=20u16 {
        descs.push(TypeDesc::List(k - 1));
    }
    let table = TypeTable(descs);
    let (mut st, h) = setup();
    let mut cur = list::new(&mut st, h, false).unwrap();
    for _ in 2..=20 {
        incref(&mut st, h, cur).unwrap();
        let next = list::new(&mut st, h, true).unwrap();
        let (next, _) = list::push(&mut st, h, next, cur).unwrap();
        let (next, _) = list::push(&mut st, h, next, cur).unwrap();
        cur = next;
    }
    assert_eq!(
        to_wire(&st, h, &table, 20, cur).unwrap_err(),
        FaultKind::ValueTooLarge
    );
    let err = fmt_value(&st, h, &table, 20, cur, true, &mut String::new()).unwrap_err();
    assert_eq!(err, FaultKind::ValueTooLarge);
}

fn show_in(st: &mut Store, h: Heap, t: &TypeTable, ty: u16, w: &WireValue, top: bool) -> String {
    let v = load(st, h, t, ty, w);
    let mut out = String::new();
    fmt_value(st, h, t, ty, v, top, &mut out).unwrap();
    out
}

// ---- formatting ------------------------------------------------------------------------------------

#[test]
fn fmt_pins() {
    let t = shop();
    let (mut st, h) = setup();
    let (mut st2, h2) = setup();
    let mut show = |ty: u16, w: &WireValue, top: bool| show_in(&mut st, h, &t, ty, w, top);
    assert_eq!(show(INT, &WireValue::Int(42), true), "42");
    assert_eq!(show(FLOAT, &WireValue::Float(1.5), true), "1.5");
    assert_eq!(show(FLOAT, &WireValue::Float(2.0), true), "2.0");
    assert_eq!(show(FLOAT, &WireValue::Float(f64::NAN), true), "NaN");
    assert_eq!(show(FLOAT, &WireValue::Float(f64::INFINITY), true), "inf");
    assert_eq!(
        show(FLOAT, &WireValue::Float(f64::NEG_INFINITY), true),
        "-inf"
    );
    assert_eq!(show(BOOL, &WireValue::Bool(true), true), "true");
    assert_eq!(show(BOOL, &WireValue::Bool(false), true), "false");
    assert_eq!(show(UNIT, &WireValue::Unit, true), "()");
    assert_eq!(show(STR, &WireValue::Str(s("ana")), true), "ana");
    assert_eq!(show(STR, &WireValue::Str(s("ana")), false), "\"ana\"");
    assert_eq!(
        show(STR, &WireValue::Str(s("a\"b\\c\nd")), false),
        "\"a\\\"b\\\\c\\nd\""
    );
    assert_eq!(
        show(
            INTS,
            &WireValue::List(vec![WireValue::Int(1), WireValue::Int(2)]),
            true
        ),
        "[1, 2]"
    );
    assert_eq!(show(INTS, &WireValue::List(vec![]), true), "[]");
    assert_eq!(
        show(
            STRS,
            &WireValue::List(vec![WireValue::Str(s("x")), WireValue::Str(s("y"))]),
            true
        ),
        "[\"x\", \"y\"]"
    );
    let one = WireValue::Dict(vec![(WireValue::Str(s("a")), WireValue::Int(1))]);
    let int_dict = TypeTable(vec![TypeDesc::Str, TypeDesc::Int, TypeDesc::Dict(0, 1)]);
    assert_eq!(
        show_in(&mut st2, h2, &int_dict, 2, &one, true),
        "{\"a\": 1}"
    );
    let ledger = WireValue::Struct {
        name: s("Ledger"),
        fields: vec![
            field("owner", WireValue::Str(s("ana"))),
            field("entries", WireValue::List(vec![])),
        ],
    };
    assert_eq!(
        show(LEDGER, &ledger, true),
        "Ledger(owner=\"ana\", entries=[])"
    );
    let accepted = WireValue::Variant {
        name: s("Accepted"),
        fields: vec![field("tick", WireValue::Int(5))],
    };
    assert_eq!(show(STAGE, &accepted, true), "Stage::Accepted(tick=5)");
    let asked = WireValue::Variant {
        name: s("Asked"),
        fields: vec![],
    };
    assert_eq!(show(STAGE, &asked, true), "Stage::Asked");
    assert_eq!(show(OPT_INT, &some(3), true), "Some(3)");
    assert_eq!(show(OPT_INT, &none(), true), "None");
    assert_eq!(
        show(ITEM, &item("bow", some(1)), true),
        "Item(name=\"bow\", qty=Some(1))"
    );
}

#[test]
fn type_table_queries() {
    let t = shop();
    for ty in [STR, ITEMS, ITEM, STAGE, BAG, OPT_INT, LEDGER] {
        assert!(t.is_ref(ty), "{ty}");
    }
    for ty in [INT, FLOAT, BOOL, UNIT, PROMISE] {
        assert!(!t.is_ref(ty), "{ty}");
    }
    assert!(!t.is_ref(999));
    assert!(t.storable(ITEMS) && t.storable(BAG) && t.storable(INT));
    assert!(!t.storable(PROMISE));
    // A type that contains a promise, or a bad id is not storable; a cycle without a promise is.
    let nested = TypeTable(vec![
        TypeDesc::Promise(1),
        TypeDesc::Int,
        TypeDesc::List(0),
        TypeDesc::Struct {
            name: s("S"),
            fields: vec![(s("p"), 2)],
        },
        TypeDesc::List(4),
    ]);
    assert!(!nested.storable(2) && !nested.storable(3));
    assert!(nested.storable(4), "a cycle");
    assert!(!nested.storable(99));
    // Malformed descriptors never panic.
    let (mut st, h) = setup();
    assert!(to_wire(&st, h, &nested, 99, 0).is_err());
    assert!(from_wire(&mut st, h, &nested, 99, &WireValue::Unit).is_err());
    let _ = obj::new(&mut st, h, 0, 0, 0);
}

// ---- recursive data ----------------------------------------------------------------------------------

/// `struct Node { v: int, next: Option[Node] }`: 0 `Int`, 1 `Node`, 2 `Option[Node]`.
fn linked() -> TypeTable {
    TypeTable(vec![
        TypeDesc::Int,
        TypeDesc::Struct {
            name: s("Node"),
            fields: vec![(s("v"), 0), (s("next"), 2)],
        },
        TypeDesc::Enum {
            name: s("Option"),
            variants: vec![(s("None"), vec![]), (s("Some"), vec![(s("value"), 1)])],
        },
    ])
}

/// An `n`-node list on the heap, built tail first (no recursion): the head `Node`.
fn heap_list(st: &mut Store, h: Heap, n: i64) -> u64 {
    let mut next = 1; // `None`
    let mut node = 0;
    for v in (0..n).rev() {
        node = obj::new(st, h, 2, 0b10, 0).unwrap();
        obj::set_field(st, h, node, 0, v as u64).unwrap();
        obj::set_field(st, h, node, 1, next).unwrap();
        next = obj::new(st, h, 1, 0b1, 1).unwrap();
        obj::set_field(st, h, next, 0, node).unwrap();
    }
    node
}

/// The same list as a wire value (no recursion).
fn wire_list(n: i64) -> WireValue {
    let node = |v: i64, next: WireValue| WireValue::Struct {
        name: s("Node"),
        fields: vec![field("v", WireValue::Int(v)), field("next", next)],
    };
    let mut next = none();
    for v in (1..n).rev() {
        next = WireValue::Variant {
            name: s("Some"),
            fields: vec![field("value", node(v, next))],
        };
    }
    node(0, next)
}

#[test]
fn deep_linked_list_walks() {
    let t = linked();
    let (mut st, h) = setup_sized(4 << 20);
    let a = heap_list(&mut st, h, 5_000);
    let b = heap_list(&mut st, h, 5_000);
    assert!(deep_eq(&st, h, a, b, true).unwrap().0);
    assert_eq!(
        key_hash(&st, h, a, true).unwrap().0,
        key_hash(&st, h, b, true).unwrap().0
    );
    let (w, _) = to_wire(&st, h, &t, 1, a).unwrap();
    let mut bytes = vec![];
    w.encode(&mut bytes);
    let back = WireValue::decode(&bytes).unwrap();
    assert!(back == w);
    let (c, _) = from_wire(&mut st, h, &t, 1, &back).unwrap();
    assert!(deep_eq(&st, h, a, c, true).unwrap().0);
    let mut text = String::new();
    fmt_value(&st, h, &t, 1, a, true, &mut text).unwrap();
    assert!(text.starts_with("Node(v=0, next=Some(Node(v=1, next=Some("));
    assert!(text.ends_with(&format!("Node(v=4999, next=None){}", "))".repeat(4_999))));

    // 100 000 nodes as a wire value only: encode, decode, clone, compare, drop.
    let w = wire_list(100_000);
    let mut bytes = vec![];
    w.encode(&mut bytes);
    let back = WireValue::decode(&bytes).unwrap();
    let copy = back.clone();
    assert!(copy == w && back == w);
    assert!(wire_list(99_999) != w);
    drop((w, back, copy));
}
