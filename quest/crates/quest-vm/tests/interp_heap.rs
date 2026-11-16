//! Heap instructions, builtins and the reaper in the run loop (design D4, D5, D6).

mod common;

use std::hash::Hasher;
use std::sync::Arc;

use common::*;
use quest_vm::asm::{FnBuilder, FnRef, ProgramBuilder, SlotRef};
use quest_vm::fuel::{FUEL_CALL, FUEL_HEAP, FUEL_REAP};
use quest_vm::program::Program;
use quest_vm::types::{TypeDesc, TypeId};
use quest_vm::vm::{RunEnd, Vm, VmConfig};
use quest_vm::wire::WireValue as W;
use quest_vm::{Builtin, FaultKind, Fnv64, Layout, Mem, Op};

const CANONICAL_NAN: u64 = 0x7ff8_0000_0000_0000;

/// The type ids every test program declares (in this order).
struct Tys {
    int: TypeId,
    boolean: TypeId,
    str: TypeId,
    list_int: TypeId,
    list_str: TypeId,
    opt_int: TypeId,
    opt_str: TypeId,
    dict_str_int: TypeId,
}

fn option(t: TypeId) -> TypeDesc {
    TypeDesc::Enum {
        name: "Option".into(),
        variants: vec![
            ("None".into(), vec![]),
            ("Some".into(), vec![("value".into(), t)]),
        ],
    }
}

fn tys(pb: &mut ProgramBuilder) -> Tys {
    let int = pb.ty(TypeDesc::Int);
    let boolean = pb.ty(TypeDesc::Bool);
    let str = pb.ty(TypeDesc::Str);
    Tys {
        int,
        boolean,
        str,
        list_int: pb.ty(TypeDesc::List(int)),
        list_str: pb.ty(TypeDesc::List(str)),
        opt_int: pb.ty(option(int)),
        opt_str: pb.ty(option(str)),
        dict_str_int: pb.ty(TypeDesc::Dict(str, int)),
    }
}

fn some(v: W) -> W {
    W::Variant {
        name: "Some".into(),
        fields: vec![("value".into(), v)],
    }
}

fn none() -> W {
    W::Variant {
        name: "None".into(),
        fields: vec![],
    }
}

fn s(v: &str) -> W {
    W::Str(v.into())
}

fn strs(v: &[&str]) -> W {
    W::List(v.iter().map(|x| s(x)).collect())
}

fn ints(v: &[i64]) -> W {
    W::List(v.iter().map(|x| W::Int(*x)).collect())
}

/// `CallBuiltin`: arguments from `args`, result in `res`.
fn call(f: &mut FnBuilder, b: Builtin, args: SlotRef, res: SlotRef) {
    f.emit(Op::CallBuiltin, b as u16, args, res);
}

fn load_str(pb_str: u32, f: &mut FnBuilder, slot: SlotRef) {
    f.emit_t(Op::LoadStr, slot, pb_str);
}

/// Absolute slot of local `i` of fn 0 (it has no parameters).
fn loc(p: &Program, i: u16) -> u16 {
    frame_slot(p, 0, 3 + i)
}

fn finish(pb: ProgramBuilder) -> Arc<Program> {
    Arc::new(pb.finish().expect("test program"))
}

fn returns(vm: &mut Vm, f: u16) {
    let r = run_fn(vm, f);
    assert_eq!(r.end, RunEnd::Returned);
}

fn fault_of(vm: &mut Vm, f: u16) -> (FaultKind, String, u32) {
    match run_fn(vm, f).end {
        RunEnd::Faulted {
            kind,
            message,
            stack,
        } => (kind, message, stack[0].line),
        other => panic!("expected a fault, got {other:?}"),
    }
}

/// Emits `for _ in 0..n { tmp = <string str_id, or the counter when None>; list.push(tmp) }`; `list` and
/// `tmp` are consecutive, `ctr` the loop's counter, limit, one and condition. `n < 0`: forever.
fn emit_push_loop(
    f: &mut FnBuilder,
    [list, tmp]: [SlotRef; 2],
    ctr: [SlotRef; 4],
    n: i16,
    str_id: Option<u32>,
) {
    let [i, lim, one, cond] = ctr;
    f.emit(Op::LoadSmall, i, 0u16, 0u16);
    f.emit(Op::LoadSmall, lim, n as u16, 0u16);
    f.emit(Op::LoadSmall, one, 1u16, 0u16);
    let (top, done) = (f.label(), f.label());
    f.bind(top);
    if n >= 0 {
        f.emit(Op::LtI, cond, i, lim);
        f.jump(Op::JmpIfNot, cond, done);
    }
    match str_id {
        Some(id) => f.emit_t(Op::LoadStr, tmp, id),
        None => f.emit(Op::Copy, tmp, i, 0u16),
    };
    call(f, Builtin::ListPush, list, cond);
    f.emit(Op::AddI, i, i, one);
    f.jump(Op::Jmp, 0u16, top);
    f.bind(done);
}

fn counters(f: &FnBuilder) -> [SlotRef; 4] {
    [f.local(0), f.local(1), f.local(2), f.local(3)]
}

#[test]
fn struct_field_update_is_cow() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let pt = pb.ty(TypeDesc::Struct {
        name: "P".into(),
        fields: vec![("n".into(), t.int), ("s".into(), t.str)],
    });
    let shape = pb.shape(2, 0b10);
    let (a, b) = (pb.string("a"), pb.string("b"));
    let main = pb.declare("main", 0);
    let drop = pb.declare("drop", 0);
    {
        let mut f = pb.body(main);
        let (o, x, y, q) = (f.local(0), f.local(1), f.local(2), f.local(3));
        f.emit(Op::NewObj, o, shape, 0u16);
        f.emit(Op::LoadSmall, x, 7u16, 0u16);
        f.emit(Op::SetField, o, 0u16, x);
        load_str(a, &mut f, y);
        f.emit(Op::SetField, o, 1u16, y);
        f.emit(Op::Dup, q, o, 0u16);
        f.emit(Op::MakeUnique, q, 0u16, 0u16);
        f.emit(Op::LoadSmall, x, 9u16, 0u16);
        f.emit(Op::SetField, q, 0u16, x);
        load_str(b, &mut f, y);
        f.emit(Op::SetField, q, 1u16, y);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    {
        let mut f = pb.body(drop);
        let m = f.of(main);
        f.emit(Op::Drop, m.local(0), 0u16, 0u16);
        f.emit(Op::Drop, m.local(3), 0u16, 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let p = finish(pb);
    let mut vm = vm(&p);
    let base = vm.heap_used_words();
    returns(&mut vm, 0);
    let p_of = |n, txt: &str| W::Struct {
        name: "P".into(),
        fields: vec![("n".into(), W::Int(n)), ("s".into(), s(txt))],
    };
    assert_eq!(vm.debug_value(loc(&p, 0), pt), Ok(p_of(7, "a")));
    assert_eq!(vm.debug_value(loc(&p, 3), pt), Ok(p_of(9, "b")));
    assert_eq!(vm.slot(loc(&p, 2)), 0, "a heap-kind source is moved in");
    returns(&mut vm, 1);
    assert_eq!(vm.heap_used_words(), base);
}

#[test]
fn nested_take_set_updates_in_place() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let ct = pb.ty(TypeDesc::Struct {
        name: "C".into(),
        fields: vec![("c".into(), t.int)],
    });
    let lc = pb.ty(TypeDesc::List(ct));
    let at = pb.ty(TypeDesc::Struct {
        name: "A".into(),
        fields: vec![("b".into(), lc)],
    });
    let (shape_c, shape_a) = (pb.shape(1, 0), pb.shape(1, 1));
    let main = pb.declare("main", 0);
    let again = pb.declare("again", 0);
    // `a.b[1].c = v` by the take/put chain, on main's slots.
    let chain = |f: &mut FnBuilder, v: u16| {
        let m = f.of(main);
        let (a, t1, t2, i, n) = (m.local(0), m.local(5), m.local(6), m.local(7), m.local(8));
        f.emit(Op::MakeUnique, a, 0u16, 0u16);
        f.emit(Op::TakeField, t1, a, 0u16);
        f.emit(Op::MakeUnique, t1, 0u16, 0u16);
        f.emit(Op::LoadSmall, i, 1u16, 0u16);
        f.emit(Op::TakeIndex, t2, t1, i);
        f.emit(Op::MakeUnique, t2, 0u16, 0u16);
        f.emit(Op::LoadSmall, n, v, 0u16);
        f.emit(Op::SetField, t2, 0u16, n);
        f.emit(Op::SetIndex, t1, i, t2);
        f.emit(Op::SetField, a, 0u16, t1);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
    };
    {
        let mut f = pb.body(main);
        let (a, lst, x, v, copy, unit) = (
            f.local(0),
            f.local(1),
            f.local(2),
            f.local(3),
            f.local(4),
            f.local(9),
        );
        f.emit(Op::NewObj, a, shape_a, 0u16);
        f.emit(Op::NewList, lst, 1u16, 0u16);
        for c in [1u16, 2] {
            f.emit(Op::NewObj, x, shape_c, 0u16);
            f.emit(Op::LoadSmall, v, c, 0u16);
            f.emit(Op::SetField, x, 0u16, v);
            call(&mut f, Builtin::ListPush, lst, unit);
        }
        f.emit(Op::SetField, a, 0u16, lst);
        f.emit(Op::Dup, copy, a, 0u16);
        chain(&mut f, 9);
        f.end();
    }
    {
        let mut f = pb.body(again);
        chain(&mut f, 10);
        f.end();
    }
    let p = finish(pb);
    let mut vm = vm(&p);
    returns(&mut vm, 0);
    let a_of = |cs: &[i64]| W::Struct {
        name: "A".into(),
        fields: vec![(
            "b".into(),
            W::List(
                cs.iter()
                    .map(|c| W::Struct {
                        name: "C".into(),
                        fields: vec![("c".into(), W::Int(*c))],
                    })
                    .collect(),
            ),
        )],
    };
    assert_eq!(vm.debug_value(loc(&p, 0), at), Ok(a_of(&[1, 9])));
    assert_eq!(vm.debug_value(loc(&p, 4), at), Ok(a_of(&[1, 2])));
    // Everything on the path is unique now: the chain allocates nothing.
    let used = vm.heap_used_words();
    returns(&mut vm, 1);
    assert_eq!(vm.heap_used_words(), used);
    assert_eq!(vm.debug_value(loc(&p, 0), at), Ok(a_of(&[1, 10])));
    assert_eq!(vm.debug_value(loc(&p, 4), at), Ok(a_of(&[1, 2])));
}

#[test]
fn list_builtins() {
    use Builtin::*;
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let (sa, sb, sz) = (pb.string("a"), pb.string("b"), pb.string("z"));
    let main = pb.declare("main", 0);
    let drop = pb.declare("drop", 0);
    {
        let mut f = pb.body(main);
        let (l, x, y, unit) = (f.local(20), f.local(21), f.local(22), f.local(40));
        let (sl, sx) = (f.local(30), f.local(31));
        f.emit(Op::NewList, l, 0u16, 0u16);
        for v in [1u16, 2, 3] {
            f.emit(Op::LoadSmall, x, v, 0u16);
            call(&mut f, ListPush, l, unit);
        }
        let r0 = f.local(0);
        f.emit(Op::ListLen, r0, l, 0u16);
        f.emit(Op::LoadSmall, x, 0u16, 0u16);
        f.emit(Op::LoadSmall, y, 0u16, 0u16);
        call(&mut f, ListInsert, l, unit); // [0, 1, 2, 3]
        f.emit(Op::LoadSmall, x, 1u16, 0u16);
        {
            let r = f.local(1);
            call(&mut f, ListRemove, l, r);
        } // 1; [0, 2, 3]
        {
            let r = f.local(2);
            call(&mut f, ListPop, l, r);
        } // Some(3); [0, 2]
        f.emit(Op::LoadSmall, x, 0u16, 0u16);
        f.emit(Op::LoadSmall, y, 1u16, 0u16);
        {
            let r = f.local(3);
            call(&mut f, ListSlice, l, r);
        } // [0]
        f.emit(Op::LoadSmall, x, 2u16, 0u16);
        {
            let r = f.local(4);
            call(&mut f, ListContains, l, r);
        } // true
        f.emit(Op::LoadSmall, x, 5u16, 0u16);
        {
            let r = f.local(5);
            call(&mut f, ListContains, l, r);
        } // false
        f.emit(Op::LoadSmall, x, 1u16, 0u16);
        f.emit(Op::GetIndex, f.local(6), l, x); // 2
        // a list of strings, shared before a push
        f.emit(Op::NewList, sl, 1u16, 0u16);
        load_str(sa, &mut f, sx);
        call(&mut f, ListPush, sl, unit);
        load_str(sb, &mut f, sx);
        call(&mut f, ListPush, sl, unit);
        {
            let r = f.local(7);
            call(&mut f, ListPop, sl, r);
        } // Some("b")
        f.emit(Op::Dup, f.local(8), sl, 0u16); // ["a"], shared
        load_str(sz, &mut f, sx);
        call(&mut f, ListPush, sl, unit); // ["a", "z"]; the copy stays ["a"]
        f.emit(Op::GetIndex, f.local(12), sl, x); // "z"
        {
            let r = f.local(9);
            call(&mut f, ListPop, sl, r);
        } // Some("z")
        {
            let r = f.local(10);
            call(&mut f, ListPop, sl, r);
        } // Some("a")
        {
            let r = f.local(11);
            call(&mut f, ListPop, sl, r);
        } // None
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    {
        let mut f = pb.body(drop);
        let m = f.of(main);
        for i in [20, 2, 3, 7, 8, 30, 9, 10, 11, 12, 31] {
            f.emit(Op::Drop, m.local(i), 0u16, 0u16);
        }
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let p = finish(pb);
    let mut vm = vm(&p);
    let base = vm.heap_used_words();
    returns(&mut vm, 0);
    let v = |i, ty| vm.debug_value(loc(&p, i), ty).expect("a value");
    assert_eq!(v(0, t.int), W::Int(3));
    assert_eq!(v(20, t.list_int), ints(&[0, 2]));
    assert_eq!(v(1, t.int), W::Int(1));
    assert_eq!(v(2, t.opt_int), some(W::Int(3)));
    assert_eq!(v(3, t.list_int), ints(&[0]));
    assert_eq!(v(4, t.boolean), W::Bool(true));
    assert_eq!(v(5, t.boolean), W::Bool(false));
    assert_eq!(v(6, t.int), W::Int(2));
    assert_eq!(v(7, t.opt_str), some(s("b")));
    assert_eq!(v(8, t.list_str), strs(&["a"]));
    assert_eq!(v(12, t.str), s("z"));
    assert_eq!(v(9, t.opt_str), some(s("z")));
    assert_eq!(v(10, t.opt_str), some(s("a")));
    assert_eq!(v(11, t.opt_str), none());
    assert_eq!(v(30, t.list_str), strs(&[]));
    assert_eq!(vm.slot(loc(&p, 31)), 0, "a pushed string is moved in");
    returns(&mut vm, 1);
    assert_eq!(vm.heap_used_words(), base, "every reference given back");
}

#[test]
fn dict_builtins() {
    use Builtin::*;
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let [ka, kb, kc, kz] = ["a", "b", "c", "z"].map(|k| pb.string(k));
    let main = pb.declare("main", 0);
    let drop = pb.declare("drop", 0);
    {
        let mut f = pb.body(main);
        let (d, k, v) = (f.local(20), f.local(21), f.local(22));
        let (kk, vv) = (f.local(23), f.local(24));
        f.emit(Op::NewDict, d, 0b01u16, 0u16);
        for (key, val) in [(ka, 1u16), (kb, 2), (kc, 3)] {
            load_str(key, &mut f, k);
            f.emit(Op::LoadSmall, v, val, 0u16);
            f.emit(Op::SetKey, d, k, v);
        }
        {
            let r = f.local(0);
            call(&mut f, DictLen, d, r);
        } // 3
        load_str(kb, &mut f, k);
        {
            let r = f.local(1);
            call(&mut f, DictGet, d, r);
        } // Some(2)
        load_str(kz, &mut f, k);
        {
            let r = f.local(2);
            call(&mut f, DictGet, d, r);
        } // None
        load_str(ka, &mut f, k);
        {
            let r = f.local(3);
            call(&mut f, DictContains, d, r);
        } // true
        {
            let r = f.local(4);
            call(&mut f, DictRemove, d, r);
        } // Some(1)
        {
            let r = f.local(5);
            call(&mut f, DictRemove, d, r);
        } // None
        {
            let r = f.local(6);
            call(&mut f, DictContains, d, r);
        } // false
        {
            let r = f.local(7);
            call(&mut f, DictKeys, d, r);
        } // ["b", "c"]
        {
            let r = f.local(8);
            call(&mut f, DictValues, d, r);
        } // [2, 3]
        f.emit(Op::Drop, k, 0u16, 0u16);
        f.emit(Op::LoadSmall, k, 0u16, 0u16);
        {
            let r = f.local(9);
            call(&mut f, DictNext, d, r);
        } // 1, the serial of "b"
        f.emit(Op::Copy, k, f.local(9), 0u16);
        {
            let r = f.local(10);
            call(&mut f, DictKeyAt, d, r);
        } // "b"
        {
            let r = f.local(11);
            call(&mut f, DictValAt, d, r);
        } // 2
        f.emit(Op::LoadSmall, k, 3u16, 0u16);
        {
            let r = f.local(12);
            call(&mut f, DictNext, d, r);
        } // -1
        load_str(kc, &mut f, kk);
        f.emit(Op::GetKey, f.local(13), d, kk); // 3
        f.emit(Op::TakeKey, f.local(14), d, kk); // 3
        f.emit(Op::LoadSmall, vv, 30u16, 0u16);
        f.emit(Op::SetKey, d, kk, vv);
        f.emit(Op::GetKey, f.local(15), d, kk); // 30
        f.emit(Op::Dup, f.local(16), d, 0u16); // shared
        f.emit(Op::LoadSmall, vv, 99u16, 0u16);
        f.emit(Op::SetKey, d, kk, vv); // d["c"] = 99; the copy keeps 30
        f.emit(Op::LoadSmall, k, 0u16, 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    {
        let mut f = pb.body(drop);
        let m = f.of(main);
        for i in [20, 1, 4, 7, 8, 10, 16, 23] {
            f.emit(Op::Drop, m.local(i), 0u16, 0u16);
        }
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let p = finish(pb);
    let mut vm = vm(&p);
    let base = vm.heap_used_words();
    returns(&mut vm, 0);
    let v = |i, ty| vm.debug_value(loc(&p, i), ty).expect("a value");
    let dict =
        |pairs: &[(&str, i64)]| W::Dict(pairs.iter().map(|(k, v)| (s(k), W::Int(*v))).collect());
    assert_eq!(v(0, t.int), W::Int(3));
    assert_eq!(v(1, t.opt_int), some(W::Int(2)));
    assert_eq!(v(2, t.opt_int), none());
    assert_eq!(v(3, t.boolean), W::Bool(true));
    assert_eq!(v(4, t.opt_int), some(W::Int(1)));
    assert_eq!(v(5, t.opt_int), none());
    assert_eq!(v(6, t.boolean), W::Bool(false));
    assert_eq!(v(7, t.list_str), strs(&["b", "c"]));
    assert_eq!(v(8, t.list_int), ints(&[2, 3]));
    assert_eq!(v(9, t.int), W::Int(1));
    assert_eq!(v(10, t.str), s("b"));
    assert_eq!(v(11, t.int), W::Int(2));
    assert_eq!(v(12, t.int), W::Int(-1));
    assert_eq!(v(13, t.int), W::Int(3));
    assert_eq!(v(14, t.int), W::Int(3));
    assert_eq!(v(15, t.int), W::Int(30));
    assert_eq!(v(20, t.dict_str_int), dict(&[("b", 2), ("c", 99)]));
    assert_eq!(v(16, t.dict_str_int), dict(&[("b", 2), ("c", 30)]));
    assert_eq!(v(23, t.str), s("c"), "a key slot is borrowed");
    returns(&mut vm, 1);
    assert_eq!(vm.heap_used_words(), base, "every reference given back");
}

#[test]
fn index_and_key_faults() {
    let program = |index: i16| {
        let mut pb = ProgramBuilder::new(API);
        let k = pb.string("k");
        let mut f = pb.func("main", 0);
        let (l, x, i, d, key, out) = (
            f.local(0),
            f.local(1),
            f.local(2),
            f.local(3),
            f.local(4),
            f.local(5),
        );
        f.emit(Op::NewList, l, 0u16, 0u16);
        f.emit(Op::LoadSmall, x, 7u16, 0u16);
        call(&mut f, Builtin::ListPush, l, out);
        f.emit(Op::LoadSmall, i, index as u16, 0u16);
        f.line(10);
        f.emit(Op::GetIndex, out, l, i);
        f.emit(Op::NewDict, d, 1u16, 0u16);
        load_str(k, &mut f, key);
        f.line(20);
        f.emit(Op::GetKey, out, d, key);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
        finish(pb)
    };
    for index in [1, 5, -1] {
        let p = program(index);
        let (kind, _, line) = fault_of(&mut vm(&p), 0);
        assert_eq!(
            (kind, line),
            (FaultKind::IndexOutOfRange, 10),
            "index {index}"
        );
    }
    let p = program(0);
    let (kind, _, line) = fault_of(&mut vm(&p), 0);
    assert_eq!((kind, line), (FaultKind::MissingKey, 20));
}

#[test]
fn str_builtins() {
    use Builtin::*;
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let mut lit = |x: &str| pb.string(x);
    let cases = [
        (StrSplit, "a,b,,c", ","),
        (StrTrim, "  x \n", ""),
        (StrUpper, "aż", ""),
        (StrLower, "AbŻ", ""),
        (StrLen, "zażółć", ""),
        (StrStartsWith, "hello", "he"),
        (StrEndsWith, "hello", "he"),
        (StrContains, "hello", "ll"),
        (ParseInt, "42", ""),
        (ParseInt, "4x", ""),
        (ParseInt, "9223372036854775808", ""),
        (ParseInt, "-9223372036854775808", ""),
        (StrSplit, "abc", ""),
        (ParseInt, "+1", ""),
    ]
    .map(|(b, x, y)| (b, lit(x), lit(y)));
    let (ab, cd, e) = (lit("ab"), lit("cd"), lit("e"));
    let list_str = t.list_str;
    let main = pb.declare("main", 0);
    let drop = pb.declare("drop", 0);
    let concat_at = cases.len() as u16;
    {
        let mut f = pb.body(main);
        let (x, y) = (f.local(20), f.local(21));
        for (i, (b, sx, sy)) in cases.iter().enumerate() {
            load_str(*sx, &mut f, x);
            load_str(*sy, &mut f, y);
            {
                let r = f.local(i as u16);
                call(&mut f, *b, x, r);
            }
        }
        let (r1, r2, r3, r4) = (
            f.local(concat_at),
            f.local(concat_at + 1),
            f.local(concat_at + 2),
            f.local(concat_at + 3),
        );
        load_str(ab, &mut f, x);
        load_str(cd, &mut f, y);
        call(&mut f, StrConcat, x, r1);
        f.emit(Op::Move, x, r1, 0u16);
        load_str(e, &mut f, y);
        call(&mut f, StrConcat, x, r2); // "abcde"
        f.emit(Op::Dup, x, f.local(0), 0u16);
        f.emit(Op::Drop, y, 0u16, 0u16);
        f.emit(Op::LoadSmall, y, list_str, 0u16);
        call(&mut f, Fmt, x, r3);
        f.emit(Op::Drop, x, 0u16, 0u16);
        f.emit(Op::LoadSmall, x, 5u16, 0u16);
        f.emit(Op::LoadSmall, y, t.int, 0u16);
        call(&mut f, Fmt, x, r4);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    {
        let mut f = pb.body(drop);
        let m = f.of(main);
        let heap_slots = [0u16, 1, 2, 3, 8, 9, 10, 11, 12, 13, 15, 16, 17];
        for i in heap_slots {
            f.emit(Op::Drop, m.local(i), 0u16, 0u16);
        }
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let p = finish(pb);
    let mut vm = vm(&p);
    let base = vm.heap_used_words();
    returns(&mut vm, 0);
    let v = |i, ty| vm.debug_value(loc(&p, i), ty).expect("a value");
    assert_eq!(v(0, t.list_str), strs(&["a", "b", "", "c"]));
    assert_eq!(v(1, t.str), s("x"));
    assert_eq!(v(2, t.str), s("Aż"));
    assert_eq!(v(3, t.str), s("abŻ"));
    assert_eq!(v(4, t.int), W::Int(6));
    assert_eq!(v(5, t.boolean), W::Bool(true));
    assert_eq!(v(6, t.boolean), W::Bool(false));
    assert_eq!(v(7, t.boolean), W::Bool(true));
    assert_eq!(v(8, t.opt_int), some(W::Int(42)));
    assert_eq!(v(9, t.opt_int), none());
    assert_eq!(v(10, t.opt_int), none());
    assert_eq!(v(11, t.opt_int), some(W::Int(i64::MIN)));
    assert_eq!(v(12, t.list_str), strs(&["abc"]), "an empty separator");
    assert_eq!(v(13, t.opt_int), none(), "no leading +");
    assert_eq!(v(concat_at + 1, t.str), s("abcde"));
    assert_eq!(v(concat_at + 2, t.str), s(r#"["a", "b", "", "c"]"#));
    assert_eq!(v(concat_at + 3, t.str), s("5"));
    returns(&mut vm, 1);
    assert_eq!(vm.heap_used_words(), base, "every reference given back");
}

#[test]
fn math_builtins() {
    use Builtin::*;
    let f = |x: f64| x.to_bits();
    let i = |x: i64| x as u64;
    let nan = f64::NAN.to_bits() | 1; // a non-canonical NaN
    let cases: [(Builtin, u64, u64, u64); 21] = [
        (Sqrt, f(2.0), 0, 0x3FF6_A09E_667F_3BCD),
        (Sin, f(1.0), 0, f(libm::sin(1.0))),
        (Cos, f(1.0), 0, f(libm::cos(1.0))),
        (Pow, f(2.0), f(0.5), f(libm::pow(2.0, 0.5))),
        (Pow, f(1.1), f(7.3), f(libm::pow(1.1, 7.3))),
        (Floor, f(-0.5), 0, f(-1.0)),
        (Ceil, f(-0.5), 0, f(-0.0)),
        (Round, f(2.5), 0, f(3.0)),
        (Round, f(-2.5), 0, f(-3.0)),
        (AbsF, f(-3.5), 0, f(3.5)),
        (MinF, f(1.0), f(-2.0), f(-2.0)),
        (MaxF, f(1.0), f(-2.0), f(1.0)),
        (MinF, f(0.0), f(-0.0), f(-0.0)),
        (MaxF, f(-0.0), f(0.0), f(0.0)),
        (MinF, nan, f(1.0), CANONICAL_NAN),
        (MaxF, f(1.0), nan, CANONICAL_NAN),
        (Sqrt, f(-1.0), 0, CANONICAL_NAN),
        (AbsF, nan, 0, CANONICAL_NAN),
        (AbsI, i(-5), 0, 5),
        (MinI, i(3), i(-4), i(-4)),
        (MaxI, i(3), i(-4), 3),
    ];
    let mut pb = ProgramBuilder::new(API);
    let consts: Vec<(Builtin, u32, u32)> = cases
        .iter()
        .map(|(b, x, y, _)| (*b, pb.const_u64(*x), pb.const_u64(*y)))
        .collect();
    let mut fb = pb.func("main", 0);
    let (x, y) = (fb.local(30), fb.local(31));
    for (k, (b, cx, cy)) in consts.iter().enumerate() {
        fb.emit_t(Op::LoadConst, x, *cx);
        fb.emit_t(Op::LoadConst, y, *cy);
        {
            let r = fb.local(k as u16);
            call(&mut fb, *b, x, r);
        }
    }
    fb.emit(Op::Ret, 0u16, 0u16, 0u16);
    fb.end();
    let p = finish(pb);
    let mut vm = vm(&p);
    returns(&mut vm, 0);
    for (k, (b, _, _, want)) in cases.iter().enumerate() {
        assert_eq!(vm.slot(loc(&p, k as u16)), *want, "case {k}: {b:?}");
    }

    let mut pb = ProgramBuilder::new(API);
    let c = pb.const_u64(i64::MIN as u64);
    let mut fb = pb.func("main", 0);
    let (x, r) = (fb.local(0), fb.local(1));
    fb.emit_t(Op::LoadConst, x, c);
    fb.line(3);
    call(&mut fb, AbsI, x, r);
    fb.emit(Op::Ret, 0u16, 0u16, 0u16);
    fb.end();
    let p = finish(pb);
    let (kind, _, line) = fault_of(&mut common::vm(&p), 0);
    assert_eq!((kind, line), (FaultKind::IntOverflow, 3));
}

#[test]
fn assert_builtin_faults() {
    let mut pb = ProgramBuilder::new(API);
    let (fine, boom) = (pb.string("fine"), pb.string("boom"));
    let mut f = pb.func("main", 0);
    let (c, m, r) = (f.local(0), f.local(1), f.local(2));
    f.emit(Op::LoadSmall, c, 1u16, 0u16);
    load_str(fine, &mut f, m);
    call(&mut f, Builtin::Assert, c, r);
    f.emit(Op::LoadSmall, c, 0u16, 0u16);
    load_str(boom, &mut f, m);
    f.line(7);
    call(&mut f, Builtin::Assert, c, r);
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
    f.end();
    let p = finish(pb);
    let (kind, message, line) = fault_of(&mut vm(&p), 0);
    assert_eq!(
        (kind, message.as_str(), line),
        (FaultKind::AssertFailed, "boom", 7)
    );
}

/// fn 0 fills a list in main's local 20 with `n` copies of the string `text` (`n < 0`: forever); fn 1
/// runs `then` (given main and the string's id), then returns.
fn fill_then(n: i16, text: &str, then: impl FnOnce(&mut FnBuilder, FnRef, u32)) -> Arc<Program> {
    let mut pb = ProgramBuilder::new(API);
    let sid = pb.string(text);
    let main = pb.declare("main", 0);
    let second = pb.declare("second", 0);
    {
        let mut f = pb.body(main);
        let (l, tmp) = (f.local(20), f.local(21));
        f.emit(Op::NewList, l, 1u16, 0u16);
        let ctr = counters(&f);
        emit_push_loop(&mut f, [l, tmp], ctr, n, Some(sid));
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    {
        let mut f = pb.body(second);
        then(&mut f, main, sid);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    finish(pb)
}

#[test]
fn drop_then_reap_charges_fuel() {
    let p = fill_then(1000, "x", |f, main, _| {
        let l = f.of(main).local(20);
        f.emit(Op::Drop, l, 0u16, 0u16);
    });
    let mut vm = vm(&p);
    let base = vm.heap_used_words();
    returns(&mut vm, 0);
    assert!(vm.heap_used_words() > base);
    vm.debug_start(1);
    let mut host = RecordingHost::default();
    let (mut total, mut last) = (0, vm.heap_used_words());
    loop {
        let r = vm.debug_run(1, &mut host);
        total += r.fuel_used;
        let now = vm.heap_used_words();
        assert!(now <= last, "the heap only shrinks while reaping");
        last = now;
        match r.end {
            RunEnd::OutOfFuel => {}
            RunEnd::Returned => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(vm.heap_used_words(), base);
    // The reaper's steps: the list first (16 children a step, 1000 / 16 -> 63 steps; the last one also
    // frees its block of 1022 payload words), then each string (one step, freeing 2 payload words). Each
    // step costs FUEL_REAP + ceil(words / 8).
    let list_steps = 62 * (FUEL_REAP + 2) + (FUEL_REAP + (8u64 + 1022).div_ceil(8));
    let string_steps = 1000 * (FUEL_REAP + 1);
    assert_eq!(total, FUEL_HEAP + list_steps + string_steps + FUEL_CALL);
}

const SMALL_MEM: u32 = 65_536;

#[test]
fn alloc_waits_for_the_reaper() {
    // 100 strings of 100 bytes: 16-word blocks.
    let text = "y".repeat(100);
    let build_again = |f: &mut FnBuilder, main: FnRef, sid: u32, drop_first: bool| {
        let at = if drop_first { 20 } else { 24 };
        let (l, tmp) = (f.of(main).local(at), f.of(main).local(at + 1));
        if drop_first {
            f.emit(Op::Drop, l, 0u16, 0u16);
        }
        f.emit(Op::NewList, l, 1u16, 0u16);
        let ctr = counters(f);
        emit_push_loop(f, [l, tmp], ctr, 100, Some(sid));
    };
    let small = |p: &Arc<Program>| {
        Vm::new(
            p.clone(),
            VmConfig {
                mem_bytes: SMALL_MEM,
                ..cfg()
            },
        )
        .expect("loads")
    };
    // Control: without the drop, a second list does not fit.
    let p = fill_then(100, &text, |f, main, sid| build_again(f, main, sid, false));
    let mut vm = small(&p);
    returns(&mut vm, 0);
    let (kind, _, _) = fault_of(&mut vm, 1);
    assert_eq!(kind, FaultKind::HeapFull);
    // With it, the dropped list's blocks are reaped before the next instruction and reused.
    let p = fill_then(100, &text, |f, main, sid| build_again(f, main, sid, true));
    let mut vm = small(&p);
    returns(&mut vm, 0);
    let used = vm.heap_used_words();
    returns(&mut vm, 1);
    assert_eq!(vm.heap_used_words(), used);
    let heap_words = {
        let l = Layout::plan(u32::from(p.static_words), SMALL_MEM).expect("plan");
        (l.heap_end - l.heap_base) / 8
    };
    assert!(2 * used > heap_words, "the test needs most of the heap");
}

fn mem_hash(vm: &Vm) -> u64 {
    let mut h = Fnv64::default();
    h.write(vm.mem_bytes());
    h.finish()
}

#[test]
fn heap_full_faults() {
    let p = fill_then(-1, "z", |f, _, _| {
        let r = f.ret();
        f.emit(Op::LoadSmall, r, 5u16, 0u16);
    });
    let mut vm = vm(&p);
    let cp = vm.checkpoint();
    let before = mem_hash(&vm);
    let (kind, _, _) = fault_of(&mut vm, 0);
    assert_eq!(kind, FaultKind::HeapFull);
    assert!(Layout::read_header(&Mem::from_bytes(vm.mem_bytes().to_vec())).is_ok());
    assert!(vm.heap_used_words() > 0, "the heap header still parses");
    returns(&mut vm, 1);
    assert_eq!(vm.slot(frame_slot(&p, 1, 1)), 5, "the VM still runs");
    vm.rollback_to(cp).expect("rollback");
    assert_eq!(mem_hash(&vm), before);
}

#[test]
fn size_dependent_fuel_leaves_a_debt() {
    let mut pb = ProgramBuilder::new(API);
    let main = pb.declare("main", 0);
    let clone = pb.declare("clone", 0);
    {
        let mut f = pb.body(main);
        let (l, tmp, copy) = (f.local(20), f.local(21), f.local(22));
        f.emit(Op::NewList, l, 0u16, 0u16);
        let ctr = counters(&f);
        emit_push_loop(&mut f, [l, tmp], ctr, 800, None);
        f.emit(Op::Dup, copy, l, 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    {
        let mut f = pb.body(clone);
        f.emit(Op::MakeUnique, f.of(main).local(20), 0u16, 0u16);
        for _ in 0..3 {
            f.emit(Op::Nop, 0u16, 0u16, 0u16);
        }
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let p = finish(pb);
    let prepared = || {
        let mut vm = vm(&p);
        returns(&mut vm, 0);
        vm.debug_start(1);
        vm
    };
    let mut whole = prepared();
    let all = whole.debug_run(u64::MAX, &mut RecordingHost::default());
    assert_eq!(all.end, RunEnd::Returned);
    let clone_cost = FUEL_HEAP + 800 / 8;
    assert_eq!(all.fuel_used, clone_cost + 3 + FUEL_CALL);

    let mut vm = prepared();
    let mut host = RecordingHost::default();
    let first = vm.debug_run(5, &mut host);
    assert_eq!(first.end, RunEnd::OutOfFuel);
    assert_eq!(
        first.fuel_used, clone_cost,
        "the clone completes past the budget"
    );
    assert_ne!(vm.slot(loc(&p, 20)), vm.slot(loc(&p, 22)), "cloned");
    // The debt (clone_cost - 5 = 98) delays the next instructions.
    let second = vm.debug_run(50, &mut host);
    assert_eq!(
        (second.end.clone(), second.fuel_used),
        (RunEnd::OutOfFuel, 0)
    );
    let third = vm.debug_run(50, &mut host);
    assert_eq!((third.end.clone(), third.fuel_used), (RunEnd::OutOfFuel, 2));
    let fourth = vm.debug_run(50, &mut host);
    assert_eq!(fourth.end, RunEnd::Returned);
    assert_eq!(
        first.fuel_used + second.fuel_used + third.fuel_used + fourth.fuel_used,
        all.fuel_used
    );
}
