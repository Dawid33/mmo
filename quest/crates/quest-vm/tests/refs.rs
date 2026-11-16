//! Second-class references: `RefPath` objects re-resolved on every use (design D4, D5; spec §5.3).

mod common;

use std::sync::Arc;

use common::*;
use quest_vm::asm::{FnBuilder, ProgramBuilder, SlotRef};
use quest_vm::fuel::{FUEL_CALL, FUEL_HEAP};
use quest_vm::program::Program;
use quest_vm::types::{TypeDesc, TypeId};
use quest_vm::vm::{RunEnd, Vm};
use quest_vm::wire::WireValue as W;
use quest_vm::{Builtin, FaultKind, Op};

/// `RefMake`'s `c`: the root slot holds a heap kind.
const HEAP_ROOT: u16 = 1;

fn call(f: &mut FnBuilder, b: Builtin, args: SlotRef, res: SlotRef) {
    f.emit(Op::CallBuiltin, b as u16, args, res);
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

fn fault_kind(vm: &mut Vm, f: u16) -> FaultKind {
    match run_fn(vm, f).end {
        RunEnd::Faulted { kind, .. } => kind,
        other => panic!("expected a fault, got {other:?}"),
    }
}

fn ints(v: &[i64]) -> W {
    W::List(v.iter().map(|x| W::Int(*x)).collect())
}

/// `list.push(v)` for each small int, through `tmp` (`tmp` follows `list`), `unit` takes the result.
fn push_ints(f: &mut FnBuilder, list: SlotRef, tmp: SlotRef, unit: SlotRef, vals: &[u16]) {
    for v in vals {
        f.emit(Op::LoadSmall, tmp, *v, 0u16);
        call(f, Builtin::ListPush, list, unit);
    }
}

fn int_list(pb: &mut ProgramBuilder) -> TypeId {
    let int = pb.ty(TypeDesc::Int);
    pb.ty(TypeDesc::List(int))
}

#[test]
fn ref_to_local_list_append() {
    let mut pb = ProgramBuilder::new(API);
    let li = int_list(&mut pb);
    let mut f = pb.func("main", 0);
    let (a, tmp, r, t, unit) = (f.local(0), f.local(1), f.local(2), f.local(3), f.local(5));
    f.emit(Op::NewList, a, 0u16, 0u16);
    push_ints(&mut f, a, tmp, unit, &[1, 2]);
    f.emit(Op::RefMake, r, a, HEAP_ROOT);
    // `r.append(9)`: take the target out, mutate it, store it back.
    f.emit(Op::RefTake, t, r, 0u16);
    let nine = f.local(4);
    f.emit(Op::LoadSmall, nine, 9u16, 0u16);
    call(&mut f, Builtin::ListPush, t, unit);
    f.emit(Op::RefStore, r, t, 0u16);
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
    f.end();
    let p = finish(pb);
    let mut vm = vm(&p);
    returns(&mut vm, 0);
    assert_eq!(vm.debug_value(loc(&p, 0), li), Ok(ints(&[1, 2, 9])));
    assert_eq!(vm.slot(loc(&p, 3)), 0, "the stored list was moved in");
}

/// `Quest { rewards: list[int] }`: its type, and the shape of its object.
fn quest_type(pb: &mut ProgramBuilder) -> (TypeId, u16) {
    let li = int_list(pb);
    let qt = pb.ty(TypeDesc::Struct {
        name: "Quest".into(),
        fields: vec![("rewards".into(), li)],
    });
    (qt, pb.shape(1, 1))
}

fn quest(rewards: &[i64]) -> W {
    W::Struct {
        name: "Quest".into(),
        fields: vec![("rewards".into(), ints(rewards))],
    }
}

/// `quest = Quest { rewards: [10, 20, 30] }` in `quest`, using `lst`, `tmp`, `unit`.
fn build_quest(f: &mut FnBuilder, shape: u16, [quest, lst, tmp, unit]: [SlotRef; 4]) {
    f.emit(Op::NewObj, quest, shape, 0u16);
    f.emit(Op::NewList, lst, 0u16, 0u16);
    push_ints(f, lst, tmp, unit, &[10, 20, 30]);
    f.emit(Op::SetField, quest, 0u16, lst);
}

#[test]
fn ref_into_nested_place() {
    let mut pb = ProgramBuilder::new(API);
    let (qt, shape) = quest_type(&mut pb);
    let mut f = pb.func("main", 0);
    let (q, lst, tmp, unit) = (f.local(0), f.local(1), f.local(2), f.local(3));
    let (r0, r1, r2, i, v, out) = (
        f.local(4),
        f.local(5),
        f.local(6),
        f.local(7),
        f.local(8),
        f.local(9),
    );
    build_quest(&mut f, shape, [q, lst, tmp, unit]);
    // `r2 = &quest.rewards[2]; *r2 = 99`
    f.emit(Op::RefMake, r0, q, HEAP_ROOT);
    f.emit(Op::RefField, r1, r0, 0u16);
    f.emit(Op::LoadSmall, i, 2u16, 0u16);
    f.emit(Op::RefIndex, r2, r1, i);
    f.emit(Op::LoadSmall, v, 99u16, 0u16);
    f.emit(Op::RefStore, r2, v, 0u16);
    f.emit(Op::RefLoad, out, r2, 0u16);
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
    f.end();
    let p = finish(pb);
    let mut vm = vm(&p);
    returns(&mut vm, 0);
    assert_eq!(vm.debug_value(loc(&p, 0), qt), Ok(quest(&[10, 20, 99])));
    assert_eq!(vm.slot(loc(&p, 9)) as i64, 99);
}

#[test]
fn ref_store_whole_value() {
    let mut pb = ProgramBuilder::new(API);
    let str_t = pb.ty(TypeDesc::Str);
    let ls = pb.ty(TypeDesc::List(str_t));
    let (a, b, z) = (pb.string("a"), pb.string("b"), pb.string("z"));
    let mut f = pb.func("main", 0);
    // An int root: `*r = 7`.
    let (x, rx, v) = (f.local(0), f.local(1), f.local(2));
    f.emit(Op::LoadSmall, x, 5u16, 0u16);
    f.emit(Op::RefMake, rx, x, 0u16);
    f.emit(Op::LoadSmall, v, 7u16, 0u16);
    f.emit(Op::RefStore, rx, v, 0u16);
    // A list element: `l = ["a", "b"]; *(&l[1]) = "z"`.
    let (l, tmp, unit, rl, ri, i, s) = (
        f.local(3),
        f.local(4),
        f.local(5),
        f.local(6),
        f.local(7),
        f.local(8),
        f.local(9),
    );
    f.emit(Op::NewList, l, 1u16, 0u16);
    for id in [a, b] {
        f.emit_t(Op::LoadStr, tmp, id);
        call(&mut f, Builtin::ListPush, l, unit);
    }
    f.emit(Op::RefMake, rl, l, HEAP_ROOT);
    f.emit(Op::LoadSmall, i, 1u16, 0u16);
    f.emit(Op::RefIndex, ri, rl, i);
    f.emit_t(Op::LoadStr, s, z);
    f.emit(Op::RefStore, ri, s, 0u16);
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
    f.end();
    let p = finish(pb);
    let mut vm = vm(&p);
    returns(&mut vm, 0);
    assert_eq!(vm.slot(loc(&p, 0)), 7, "the int root was overwritten");
    assert_eq!(vm.slot(loc(&p, 2)), 7, "an immediate source is copied");
    assert_eq!(
        vm.debug_value(loc(&p, 3), ls),
        Ok(W::List(vec![W::Str("a".into()), W::Str("z".into())]))
    );
    assert_eq!(vm.slot(loc(&p, 9)), 0, "a heap-kind source is moved in");
}

#[test]
fn ref_through_shared_value_cows() {
    let mut pb = ProgramBuilder::new(API);
    let (qt, shape) = quest_type(&mut pb);
    let li = int_list(&mut pb);
    let mut f = pb.func("main", 0);
    let (q, lst, tmp, unit) = (f.local(0), f.local(1), f.local(2), f.local(3));
    let (copy, r0, r1, r2, i, v) = (
        f.local(4),
        f.local(5),
        f.local(6),
        f.local(7),
        f.local(8),
        f.local(9),
    );
    let (a, a_tmp, b, ra, t, nine) = (
        f.local(10),
        f.local(11),
        f.local(12),
        f.local(13),
        f.local(14),
        f.local(15),
    );
    // A nested place: `copy = quest; *(&quest.rewards[0]) = 5`.
    build_quest(&mut f, shape, [q, lst, tmp, unit]);
    f.emit(Op::Dup, copy, q, 0u16);
    f.emit(Op::RefMake, r0, q, HEAP_ROOT);
    f.emit(Op::RefField, r1, r0, 0u16);
    f.emit(Op::LoadSmall, i, 0u16, 0u16);
    f.emit(Op::RefIndex, r2, r1, i);
    f.emit(Op::LoadSmall, v, 5u16, 0u16);
    f.emit(Op::RefStore, r2, v, 0u16);
    // A root: `b = a; (&a).append(9)`.
    f.emit(Op::NewList, a, 0u16, 0u16);
    push_ints(&mut f, a, a_tmp, unit, &[1, 2]);
    f.emit(Op::Dup, b, a, 0u16);
    f.emit(Op::RefMake, ra, a, HEAP_ROOT);
    f.emit(Op::RefTake, t, ra, 0u16);
    f.emit(Op::LoadSmall, nine, 9u16, 0u16);
    call(&mut f, Builtin::ListPush, t, unit);
    f.emit(Op::RefStore, ra, t, 0u16);
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
    f.end();
    let p = finish(pb);
    let mut vm = vm(&p);
    returns(&mut vm, 0);
    assert_eq!(vm.debug_value(loc(&p, 0), qt), Ok(quest(&[5, 20, 30])));
    assert_eq!(vm.debug_value(loc(&p, 4), qt), Ok(quest(&[10, 20, 30])));
    assert_eq!(vm.debug_value(loc(&p, 10), li), Ok(ints(&[1, 2, 9])));
    assert_eq!(vm.debug_value(loc(&p, 12), li), Ok(ints(&[1, 2])));
}

#[test]
fn ref_dict_key() {
    let mut pb = ProgramBuilder::new(API);
    let (str_t, int) = (pb.ty(TypeDesc::Str), pb.ty(TypeDesc::Int));
    let dt = pb.ty(TypeDesc::Dict(str_t, int));
    let k = pb.string("k");
    let main = pb.declare("main", 0);
    let remove = pb.declare("remove", 0);
    let load = pb.declare("load", 0);
    let store = pb.declare("store", 0);
    {
        let mut f = pb.body(main);
        let (d, key, v, r, rk, out, out2) = (
            f.local(0),
            f.local(1),
            f.local(2),
            f.local(3),
            f.local(4),
            f.local(5),
            f.local(6),
        );
        // `d = {"k": 1}; rk = &d["k"]; out = *rk; *rk = 5; out2 = *rk`
        f.emit(Op::NewDict, d, 0b01u16, 0u16);
        f.emit_t(Op::LoadStr, key, k);
        f.emit(Op::LoadSmall, v, 1u16, 0u16);
        f.emit(Op::SetKey, d, key, v);
        f.emit(Op::RefMake, r, d, HEAP_ROOT);
        f.emit(Op::RefKey, rk, r, key);
        f.emit(Op::RefLoad, out, rk, 0u16);
        f.emit(Op::LoadSmall, v, 5u16, 0u16);
        f.emit(Op::RefStore, rk, v, 0u16);
        f.emit(Op::RefLoad, out2, rk, 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    {
        // `d.remove("k")`: the dict in main's local 0, the key in local 1.
        let mut f = pb.body(remove);
        let m = f.of(main);
        call(&mut f, Builtin::DictRemove, m.local(0), m.local(7));
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    {
        let mut f = pb.body(load);
        let m = f.of(main);
        f.emit(Op::RefLoad, m.local(5), m.local(4), 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    {
        let mut f = pb.body(store);
        let m = f.of(main);
        f.emit(Op::RefStore, m.local(4), m.local(2), 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let p = finish(pb);
    let mut vm = vm(&p);
    returns(&mut vm, 0);
    assert_eq!(vm.slot(loc(&p, 5)), 1, "loaded through the key");
    assert_eq!(vm.slot(loc(&p, 6)), 5, "stored through the key");
    assert_eq!(
        vm.debug_value(loc(&p, 0), dt),
        Ok(W::Dict(vec![(W::Str("k".into()), W::Int(5))]))
    );
    returns(&mut vm, 1);
    assert_eq!(fault_kind(&mut vm, 2), FaultKind::BadRef);
    assert_eq!(fault_kind(&mut vm, 3), FaultKind::BadRef);
    assert_eq!(
        vm.debug_value(loc(&p, 0), dt),
        Ok(W::Dict(vec![])),
        "a store to a removed key creates nothing"
    );
}

#[test]
fn ref_path_gone_faults() {
    let mut pb = ProgramBuilder::new(API);
    let main = pb.declare("main", 0);
    let shrink = pb.declare("shrink", 0);
    {
        let mut f = pb.body(main);
        let (xs, tmp, unit, r, rx, i, out) = (
            f.local(0),
            f.local(1),
            f.local(2),
            f.local(3),
            f.local(4),
            f.local(5),
            f.local(6),
        );
        f.emit(Op::NewList, xs, 0u16, 0u16);
        push_ints(&mut f, xs, tmp, unit, &[1, 2, 3]);
        f.emit(Op::RefMake, r, xs, HEAP_ROOT);
        f.emit(Op::LoadSmall, i, 2u16, 0u16);
        f.emit(Op::RefIndex, rx, r, i);
        f.emit(Op::RefLoad, out, rx, 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    {
        // `xs.pop()` (to length 2), then `*rx` again.
        let mut f = pb.body(shrink);
        let m = f.of(main);
        call(&mut f, Builtin::ListPop, m.local(0), m.local(7));
        f.emit(Op::RefLoad, m.local(6), m.local(4), 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let p = finish(pb);
    let mut vm = vm(&p);
    returns(&mut vm, 0);
    assert_eq!(vm.slot(loc(&p, 6)), 3);
    assert_eq!(fault_kind(&mut vm, 1), FaultKind::BadRef);
}

#[test]
fn ref_fuel_counts_steps() {
    // `a: A { b: list[C { c: int }] }`; `r = &a.b[1].c` is three steps.
    let mut pb = ProgramBuilder::new(API);
    let (shape_c, shape_a) = (pb.shape(1, 0), pb.shape(1, 1));
    let main = pb.declare("main", 0);
    let load = pb.declare("load", 0);
    {
        let mut f = pb.body(main);
        let (a, lst, x, v, unit) = (f.local(0), f.local(1), f.local(2), f.local(3), f.local(4));
        let (r0, r1, r2, r3, i) = (f.local(5), f.local(6), f.local(7), f.local(8), f.local(9));
        f.emit(Op::NewObj, a, shape_a, 0u16);
        f.emit(Op::NewList, lst, 1u16, 0u16);
        for c in [1u16, 2] {
            f.emit(Op::NewObj, x, shape_c, 0u16);
            f.emit(Op::LoadSmall, v, c, 0u16);
            f.emit(Op::SetField, x, 0u16, v);
            call(&mut f, Builtin::ListPush, lst, unit);
        }
        f.emit(Op::SetField, a, 0u16, lst);
        f.emit(Op::RefMake, r0, a, HEAP_ROOT);
        f.emit(Op::RefField, r1, r0, 0u16);
        f.emit(Op::LoadSmall, i, 1u16, 0u16);
        f.emit(Op::RefIndex, r2, r1, i);
        f.emit(Op::RefField, r3, r2, 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    {
        let mut f = pb.body(load);
        let m = f.of(main);
        f.emit(Op::RefLoad, m.local(10), m.local(8), 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let p = finish(pb);
    let mut vm = vm(&p);
    returns(&mut vm, 0);
    let r = run_fn(&mut vm, 1);
    assert_eq!(r.end, RunEnd::Returned);
    assert_eq!(vm.slot(loc(&p, 10)), 2);
    assert_eq!(r.fuel_used, FUEL_HEAP + 3 + FUEL_CALL);
}
