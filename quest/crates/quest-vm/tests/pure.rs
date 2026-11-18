//! Pure evaluation and mirrors (Task 41, spec §5.11 "Purity" and "Across region boundaries").
mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::{API, RecordingHost, vm};
use quest_vm::asm::{ProgramBuilder, SlotRef};
use quest_vm::host::{HostFnKind, QueryError};
use quest_vm::program::Program;
use quest_vm::pure::{Mirror, PureFault};
use quest_vm::types::{ERROR_ENUM_NAME, TypeDesc};
use quest_vm::vm::{Status, Vm};
use quest_vm::wire::WireValue;
use quest_vm::{FaultKind, Fnv64, LoadError, Op};
use state_hash::StateHash;
use std::hash::Hasher;

/// Host fn ids of the test program's query `ask(n: int) -> int` and command `log(n: int)`.
const ASK: u16 = 3;
const LOG: u16 = 4;

/// The test program: globals `a`, `b` (int), `xs` (list[int]) and `u` (int, read by nothing pure);
/// `Ping { a, b, xs, u }` sets all four; `Pong { n }` spins `n` iterations (a task across ticks).
/// Pure fns, all `(x: int) -> int`: `eval` = `a * x + b + len(xs)`; `ask` = `ask(x) + 1` (a host query);
/// `impure` issues the command `log(x)`; `fault` divides by zero; `spin` never ends; `raise` lets the
/// error `NotFound` escape. `nosig` has no signature.
struct T {
    prog: Arc<Program>,
    eval: u16,
    ask: u16,
    impure: u16,
    fault: u16,
    spin: u16,
    raise: u16,
    nosig: u16,
    // global indices
    a: u16,
    b: u16,
    xs: u16,
    u: u16,
}

fn program() -> T {
    let mut pb = ProgramBuilder::new(API);
    let int = pb.ty(TypeDesc::Int);
    let unit = pb.ty(TypeDesc::Unit);
    let list = pb.ty(TypeDesc::List(int));
    let ping = pb.ty(TypeDesc::Struct {
        name: "Ping".into(),
        fields: vec![
            ("a".into(), int),
            ("b".into(), int),
            ("xs".into(), list),
            ("u".into(), int),
        ],
    });
    let pong = pb.ty(TypeDesc::Struct {
        name: "Pong".into(),
        fields: vec![("n".into(), int)],
    });
    pb.ty(TypeDesc::Enum {
        name: ERROR_ENUM_NAME.into(),
        variants: ["Lost", "NotFound", "Timeout", "WrongType"]
            .iter()
            .map(|v| (v.to_string(), vec![]))
            .collect(),
    });
    {
        let mut f = pb.func("init0", 0);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let init_xs = pb.declare("init_xs", 0);
    {
        let mut f = pb.body(init_xs);
        let ret = f.ret();
        f.mark_ref(ret);
        f.emit(Op::NewList, ret, 0u16, 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let ga = pb.global("a", int);
    let gb = pb.global("b", int);
    let gxs = pb.global("xs", list);
    let gu = pb.global("u", int);
    pb.set_global_init(gxs, init_xs);
    let ask_site = pb.host_site(ASK, HostFnKind::Query, vec![int], int);
    let log_site = pb.host_site(LOG, HostFnKind::Command, vec![int], unit);

    let on_ping = pb.declare("on_ping", 1);
    {
        let mut f = pb.body(on_ping);
        let ev = f.param(0);
        f.mark_ref(ev);
        for (i, g) in [ga, gb, gxs, gu].into_iter().enumerate() {
            f.emit(Op::GetField, SlotRef::Global(g), ev, i as u16);
        }
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    pb.handler("Ping", ping, on_ping);
    let on_pong = pb.declare("on_pong", 1);
    {
        let mut f = pb.body(on_pong);
        let ev = f.param(0);
        f.mark_ref(ev);
        let (n, i, one, cond) = (f.local(0), f.local(1), f.local(2), f.local(3));
        f.emit(Op::GetField, n, ev, 0u16);
        f.emit(Op::LoadSmall, i, 0u16, 0u16);
        f.emit(Op::LoadSmall, one, 1u16, 0u16);
        let (top, done) = (f.label(), f.label());
        f.bind(top);
        f.emit(Op::LtI, cond, i, n);
        f.jump(Op::JmpIfNot, cond, done);
        f.emit(Op::AddI, i, i, one);
        f.emit(Op::AddI, SlotRef::Global(gu), SlotRef::Global(gu), one);
        f.jump(Op::Jmp, 0u16, top);
        f.bind(done);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    pb.handler("Pong", pong, on_pong);

    let pure = |pb: &mut ProgramBuilder, name: &str| {
        let f = pb.declare(name, 1);
        pb.set_sig(f, vec![int], int);
        f
    };
    let eval = pure(&mut pb, "eval");
    {
        let mut f = pb.body(eval);
        let (x, ret, n) = (f.param(0), f.ret(), f.local(0));
        f.emit(Op::MulI, ret, SlotRef::Global(ga), x);
        f.emit(Op::AddI, ret, ret, SlotRef::Global(gb));
        f.emit(Op::ListLen, n, SlotRef::Global(gxs), 0u16);
        f.emit(Op::AddI, ret, ret, n);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let ask = pure(&mut pb, "ask");
    {
        let mut f = pb.body(ask);
        let (x, ret, one, got) = (f.param(0), f.ret(), f.local(0), f.local(1));
        f.emit(Op::CallHost, ask_site, x, got);
        f.emit(Op::LoadSmall, one, 1u16, 0u16);
        f.emit(Op::AddI, ret, got, one);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let impure = pure(&mut pb, "impure");
    {
        let mut f = pb.body(impure);
        let (x, tmp) = (f.param(0), f.local(0));
        f.emit(Op::CallHost, log_site, x, tmp);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let fault = pure(&mut pb, "fault");
    {
        let mut f = pb.body(fault);
        let (x, ret, zero) = (f.param(0), f.ret(), f.local(0));
        f.emit(Op::LoadSmall, zero, 0u16, 0u16);
        f.emit(Op::DivI, ret, x, zero);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let spin = pure(&mut pb, "spin");
    {
        let mut f = pb.body(spin);
        let top = f.label();
        f.bind(top);
        f.emit(Op::Nop, 0u16, 0u16, 0u16);
        f.jump(Op::Jmp, 0u16, top);
        f.end();
    }
    let raise = pure(&mut pb, "raise");
    {
        let mut f = pb.body(raise);
        let err = f.err();
        // `NotFound` (variant 1, no fields) is the immediate `1 << 1 | 1`.
        f.emit(Op::LoadSmall, err, 3u16, 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let nosig = pb.declare("nosig", 1);
    {
        let mut f = pb.body(nosig);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    T {
        prog: Arc::new(pb.finish().expect("the pure test program")),
        eval: eval.0,
        ask: ask.0,
        impure: impure.0,
        fault: fault.0,
        spin: spin.0,
        raise: raise.0,
        nosig: nosig.0,
        a: ga.0,
        b: gb.0,
        xs: gxs.0,
        u: gu.0,
    }
}

fn int(n: i64) -> WireValue {
    WireValue::Int(n)
}

fn ping(a: i64, b: i64, xs: &[i64], u: i64) -> WireValue {
    WireValue::Struct {
        name: "Ping".into(),
        fields: vec![
            ("a".into(), int(a)),
            ("b".into(), int(b)),
            (
                "xs".into(),
                WireValue::List(xs.iter().map(|&x| int(x)).collect()),
            ),
            ("u".into(), int(u)),
        ],
    }
}

/// A live instance of `t` with its initialisers run and `Ping { a, b, xs, u }` handled.
fn live(t: &T, a: i64, b: i64, xs: &[i64], u: i64) -> Vm {
    let mut v = vm(&t.prog);
    set(&mut v, a, b, xs, u);
    v
}

fn set(v: &mut Vm, a: i64, b: i64, xs: &[i64], u: i64) {
    v.deliver("Ping", &ping(a, b, xs, u));
    let r = v.tick(0, u64::MAX, &mut RecordingHost::default());
    assert_eq!(r.status, Status::Idle);
}

/// The state, as `eval_pure` must leave it.
fn state(v: &Vm) -> (u64, Vec<u8>, usize) {
    (v.hash64(), v.mem_bytes().to_vec(), v.log_len())
}

fn mirror_hash(m: &Mirror) -> u64 {
    let mut h = Fnv64::default();
    m.state_hash(&mut h);
    h.finish()
}

#[test]
fn eval_pure_returns_value() {
    let t = program();
    let mut v = live(&t, 3, 4, &[10, 20], 9);
    let before = state(&v);
    let mut host = RecordingHost::default();
    assert_eq!(
        v.eval_pure(t.eval, &[int(5)], 1_000, &mut host),
        Ok(int(21))
    );
    assert_eq!(state(&v), before);
    // A host query is pure.
    host.answer = Some(Box::new(|_, args: &[WireValue]| match args {
        [WireValue::Int(n)] => Ok(int(n * 10)),
        _ => Err(QueryError::Failed("bad args".into())),
    }));
    assert_eq!(v.eval_pure(t.ask, &[int(7)], 1_000, &mut host), Ok(int(71)));
    assert_eq!(host.queries, vec![(ASK, vec![int(7)])]);
    assert_eq!(state(&v), before);
    // The same question, the same answer.
    assert_eq!(
        v.eval_pure(t.eval, &[int(5)], 1_000, &mut host),
        Ok(int(21))
    );
}

#[test]
fn eval_pure_out_of_fuel_is_err_and_state_unchanged() {
    let t = program();
    let mut v = live(&t, 3, 4, &[10], 9);
    // A caller's older checkpoint survives an evaluation.
    let older = v.checkpoint();
    set(&mut v, 1, 2, &[], 3);
    let before = state(&v);
    let mut host = RecordingHost::default();
    for fuel in [0, 1, 7, 500] {
        assert_eq!(
            v.eval_pure(t.spin, &[int(1)], fuel, &mut host),
            Err(PureFault::OutOfFuel)
        );
        assert_eq!(state(&v), before, "fuel {fuel}");
    }
    let mut twin = live(&t, 3, 4, &[10], 9);
    v.rollback_to(older).unwrap();
    assert_eq!(v.hash64(), twin.hash64());

    // Mid-task: a handler paused on fuel goes on as if nothing had been evaluated.
    for x in [&mut v, &mut twin] {
        x.deliver(
            "Pong",
            &WireValue::Struct {
                name: "Pong".into(),
                fields: vec![("n".into(), int(200))],
            },
        );
        assert_eq!(
            x.tick(1, 300, &mut RecordingHost::default()).status,
            Status::Busy
        );
    }
    let paused = state(&v);
    assert_eq!(
        v.eval_pure(t.spin, &[int(1)], 300, &mut host),
        Err(PureFault::OutOfFuel)
    );
    assert_eq!(
        v.eval_pure(t.eval, &[int(2)], 300, &mut host),
        Ok(int(6 + 4 + 1))
    );
    assert_eq!(state(&v), paused);
    for x in [&mut v, &mut twin] {
        assert_eq!(
            x.tick(2, u64::MAX, &mut RecordingHost::default()).status,
            Status::Idle
        );
    }
    assert_eq!(v.hash64(), twin.hash64());
    assert_eq!(v.mem_bytes(), twin.mem_bytes());
}

#[test]
fn eval_pure_fault_is_err() {
    let t = program();
    let mut v = live(&t, 3, 4, &[10], 9);
    let before = state(&v);
    let mut host = RecordingHost::default();
    assert_eq!(
        v.eval_pure(t.fault, &[int(1)], 1_000, &mut host),
        Err(PureFault::Fault(FaultKind::DivByZero))
    );
    assert_eq!(state(&v), before);
    assert_eq!(
        v.eval_pure(t.raise, &[int(1)], 1_000, &mut host),
        Err(PureFault::Raised("NotFound".into()))
    );
    assert_eq!(state(&v), before);
    // An overflow is a fault like any other.
    let mut big = live(&t, i64::MAX, 0, &[], 0);
    assert_eq!(
        big.eval_pure(t.eval, &[int(2)], 1_000, &mut host),
        Err(PureFault::Fault(FaultKind::IntOverflow))
    );
    assert!(
        host.reports.is_empty(),
        "eval_pure reports nothing: {:?}",
        host.reports
    );
}

#[test]
fn eval_pure_impure_is_refused() {
    let t = program();
    let mut v = live(&t, 3, 4, &[10], 9);
    let before = state(&v);
    let mut host = RecordingHost::default();
    assert_eq!(
        v.eval_pure(t.impure, &[int(1)], 1_000, &mut host),
        Err(PureFault::Impure)
    );
    assert!(host.commands.is_empty() && host.checks.is_empty());
    assert_eq!(state(&v), before);
}

#[test]
fn eval_pure_bad_call_is_refused() {
    let t = program();
    let mut v = live(&t, 3, 4, &[10], 9);
    let before = state(&v);
    let mut host = RecordingHost::default();
    for (f, args) in [
        (t.eval, vec![]),
        (t.eval, vec![int(1), int(2)]),
        (t.eval, vec![WireValue::Bool(true)]),
        (t.nosig, vec![int(1)]),
        (9_999, vec![int(1)]),
    ] {
        assert_eq!(
            v.eval_pure(f, &args, 1_000, &mut host),
            Err(PureFault::BadCall),
            "fn {f} {args:?}"
        );
    }
    assert_eq!(state(&v), before);
}

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn small(&mut self) -> i64 {
        (self.next() % 2001) as i64 - 1000
    }
}

#[test]
fn mirror_eval_equals_live_eval() {
    let t = program();
    let set_ab = [t.a, t.b, t.xs];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut v = vm(&t.prog);
    let mut host = RecordingHost::default();
    let mut faults = 0;
    for _ in 0..50 {
        // Now and then a value large enough to overflow.
        let a = if rng.next().is_multiple_of(8) {
            i64::MAX / 3
        } else {
            rng.small()
        };
        let xs: Vec<i64> = (0..rng.next() % 5).map(|_| rng.small()).collect();
        set(&mut v, a, rng.small(), &xs, rng.small());
        let m = v.mirror(&set_ab);
        for _ in 0..3 {
            let x = int(rng.small());
            let live = v.eval_pure(t.eval, std::slice::from_ref(&x), 1_000, &mut host);
            let mirrored = m.eval_pure(&t.prog, t.eval, std::slice::from_ref(&x), 1_000, &mut host);
            assert_eq!(live, mirrored, "a {a} xs {xs:?} x {x:?}");
            faults += usize::from(live.is_err());
        }
    }
    assert!(faults > 0, "the generator never overflowed");
}

#[test]
fn mirror_holds_only_its_set() {
    let t = program();
    let v = live(&t, 3, 4, &[10, 20], 9);
    let m = v.mirror(&[t.xs, t.a, 9_999]);
    let want: BTreeMap<u16, WireValue> = [
        (t.a, int(3)),
        (t.xs, WireValue::List(vec![int(10), int(20)])),
    ]
    .into_iter()
    .collect();
    assert_eq!(m.globals, want);
    assert!(!m.globals.contains_key(&t.u) && !m.globals.contains_key(&t.b));
    // A part's VM gets exactly those globals.
    let mut part = live(&t, 0, 7, &[], 1);
    part.install_mirror(&m).unwrap();
    assert_eq!(part.mirror(&[t.a, t.b, t.xs, t.u]).globals, {
        let mut all = want.clone();
        all.insert(t.b, int(7));
        all.insert(t.u, int(1));
        all
    });
}

#[test]
fn install_mirror_is_undone_by_rollback() {
    let t = program();
    let mut v = live(&t, 3, 4, &[10], 9);
    let cp = v.checkpoint();
    let before = state(&v);
    let m = Mirror {
        globals: [
            (t.a, int(77)),
            (t.xs, WireValue::List(vec![int(5), int(6)])),
        ]
        .into_iter()
        .collect(),
    };
    v.install_mirror(&m).unwrap();
    assert_eq!(v.mirror(&[t.a, t.xs]), m);
    assert_eq!(
        v.eval_pure(t.eval, &[int(1)], 1_000, &mut RecordingHost::default()),
        Ok(int(77 + 4 + 2))
    );
    v.rollback_to(cp).unwrap();
    assert_eq!(v.hash64(), before.0);
    assert_eq!(v.mem_bytes(), &before.1[..]);

    // A mirror that does not fit the program changes nothing.
    for bad in [
        Mirror {
            globals: [(t.a, int(1)), (9_999, int(1))].into_iter().collect(),
        },
        Mirror {
            globals: [(t.a, int(1)), (t.xs, int(1))].into_iter().collect(),
        },
    ] {
        assert!(v.install_mirror(&bad).is_err());
        assert_eq!(state(&v), before);
    }
}

#[test]
fn mirror_hash_ignores_order() {
    let mut fwd = Mirror {
        globals: BTreeMap::new(),
    };
    let mut back = fwd.clone();
    let entries = [
        (0u16, int(1)),
        (3, WireValue::Str("x".into())),
        (7, WireValue::List(vec![int(1), WireValue::Float(f64::NAN)])),
    ];
    for (g, w) in entries.iter().cloned() {
        fwd.globals.insert(g, w);
    }
    for (g, w) in entries.iter().rev().cloned() {
        back.globals.insert(g, w);
    }
    assert_eq!(mirror_hash(&fwd), mirror_hash(&back));
    // A changed value, or the same value under another global, is another hash.
    let mut changed = fwd.clone();
    changed.globals.insert(0, int(2));
    assert_ne!(mirror_hash(&changed), mirror_hash(&fwd));
    let mut moved = fwd.clone();
    let w = moved.globals.remove(&0).unwrap();
    moved.globals.insert(1, w);
    assert_ne!(mirror_hash(&moved), mirror_hash(&fwd));
    // A list's items are not a struct's fields.
    let l = Mirror {
        globals: [(0, WireValue::List(vec![int(1)]))].into_iter().collect(),
    };
    let s = Mirror {
        globals: [(
            0,
            WireValue::Struct {
                name: String::new(),
                fields: vec![(String::new(), int(1))],
            },
        )]
        .into_iter()
        .collect(),
    };
    assert_ne!(mirror_hash(&l), mirror_hash(&s));
}

fn pong(n: i64) -> WireValue {
    WireValue::Struct {
        name: "Pong".into(),
        fields: vec![("n".into(), int(n))],
    }
}

#[test]
fn install_mirror_mid_task_is_busy() {
    let t = program();
    let mut v = live(&t, 3, 4, &[10], 9);
    v.deliver("Pong", &pong(200));
    assert_eq!(
        v.tick(1, 300, &mut RecordingHost::default()).status,
        Status::Busy
    );
    let m = Mirror {
        globals: [(t.a, int(77))].into_iter().collect(),
    };
    let paused = state(&v);
    // Not now (retry later), unlike a mirror that can never fit.
    assert_eq!(v.install_mirror(&m), Err(LoadError::Busy));
    let bad = Mirror {
        globals: [(9_999, int(1))].into_iter().collect(),
    };
    assert!(matches!(v.install_mirror(&bad), Err(LoadError::Invalid(_))));
    assert_eq!(state(&v), paused);
    assert_eq!(
        v.tick(2, u64::MAX, &mut RecordingHost::default()).status,
        Status::Idle
    );
    v.install_mirror(&m).unwrap();
    assert_eq!(v.mirror(&[t.a]), m);
}

#[test]
fn pure_eval_with_zombies_pending_costs_what_the_mirror_does() {
    let t = program();
    let long: Vec<i64> = (0..64).collect();
    // A tick that ran `Ping` (releasing the long list) but ran out of fuel before the reap.
    let mut found = None;
    for fuel in 1..2_000 {
        let mut v = live(&t, 3, 4, &long, 9);
        v.deliver("Ping", &ping(3, 4, &[5], 9));
        let r = v.tick(1, fuel, &mut RecordingHost::default());
        if r.status == Status::Busy
            && v.mirror(&[t.xs]).globals[&t.xs] == WireValue::List(vec![int(5)])
        {
            found = Some(v);
            break;
        }
    }
    let mut v = found.expect("a tick that ends with zombies pending");
    let m = v.mirror(&[t.a, t.b, t.xs]);
    let mut host = RecordingHost::default();
    let least = (0..1_000)
        .find(|&f| {
            m.eval_pure(&t.prog, t.eval, &[int(2)], f, &mut host)
                .is_ok()
        })
        .expect("the mirror answers within 1000 fuel");
    let mirrored = m.eval_pure(&t.prog, t.eval, &[int(2)], least, &mut host);
    assert_eq!(mirrored, Ok(int(6 + 4 + 1)));
    assert_eq!(v.eval_pure(t.eval, &[int(2)], least, &mut host), mirrored);
    assert_eq!(v.status(), Status::Busy, "the zombies are still pending");
}

#[test]
fn mirror_tells_signed_zeros_apart() {
    let one = |x: f64| Mirror {
        globals: [(0, WireValue::List(vec![WireValue::Float(x)]))]
            .into_iter()
            .collect(),
    };
    assert_ne!(mirror_hash(&one(0.0)), mirror_hash(&one(-0.0)));
    assert_ne!(one(0.0), one(-0.0));
    // Any NaN is the one canonical NaN, equal to itself.
    let other_nan = f64::from_bits(0x7ff8_0000_0000_0001);
    assert_eq!(mirror_hash(&one(f64::NAN)), mirror_hash(&one(other_nan)));
    assert_eq!(one(f64::NAN), one(other_nan));
    assert_eq!(one(1.5), one(1.5));
}
