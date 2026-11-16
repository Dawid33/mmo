//! Promises: requests at commit, `await` parking, resolution, resume, timeouts and the caps (design D7,
//! spec §8.2).

mod common;

use std::sync::Arc;

use common::*;
use quest_vm::asm::{FnBuilder, FnRef, GlobalRef, ProgramBuilder, SlotRef};
use quest_vm::consts::{PARKED_MAX, PROMISE_TIMEOUT_TICKS, WIRE_DEPTH_MAX};
use quest_vm::host::{AbortCause, HostFnKind, PromiseId, Report};
use quest_vm::layout::{Layout, LoadError, PARKED_REC_WORDS};
use quest_vm::program::Program;
use quest_vm::types::{TypeDesc, TypeId};
use quest_vm::vm::{Resolved, Status, Vm, VmConfig};
use quest_vm::wire::WireValue;
use quest_vm::{Builtin, FaultKind, Op};

/// Host fn ids of the two sites every program has.
const GET: u16 = 20;
const LOG: u16 = 7;

struct Tys {
    int: TypeId,
    str: TypeId,
    ping: TypeId,
    err: TypeId,
    prom: TypeId,
    list_int: TypeId,
}

/// A program under construction: fn 0 is `init0` (every global's default initialiser, leaving 0), site
/// `get` is the promise `get(k: int) -> Promise[int]` (host fn `GET`), site `log` the command
/// `log(n: int)` (host fn `LOG`).
struct B {
    pb: ProgramBuilder,
    t: Tys,
    get: u16,
    log: u16,
}

fn builder() -> B {
    let mut pb = ProgramBuilder::new(API);
    let int = pb.ty(TypeDesc::Int);
    let str = pb.ty(TypeDesc::Str);
    let unit = pb.ty(TypeDesc::Unit);
    let ping = pb.ty(TypeDesc::Struct {
        name: "Ping".into(),
        fields: vec![("n".into(), int)],
    });
    let err = pb.ty(TypeDesc::Enum {
        name: "Error".into(),
        variants: vec![
            ("Changed".into(), vec![("current".into(), int)]),
            ("Lost".into(), vec![]),
            ("NamespaceGone".into(), vec![]),
            ("NotFound".into(), vec![]),
            ("Timeout".into(), vec![]),
            ("WrongType".into(), vec![]),
        ],
    });
    let prom = pb.ty(TypeDesc::Promise(int));
    let list_int = pb.ty(TypeDesc::List(int));
    {
        let mut f = pb.func("init0", 0);
        ret(&mut f);
        f.end();
    }
    let get = pb.host_site(GET, HostFnKind::Promise, vec![int], prom);
    let log = pb.host_site(LOG, HostFnKind::Command, vec![int], unit);
    B {
        pb,
        t: Tys {
            int,
            str,
            ping,
            err,
            prom,
            list_int,
        },
        get,
        log,
    }
}

fn ret(f: &mut FnBuilder) {
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
}

/// `log(k)` through `tmp`.
fn log_k(f: &mut FnBuilder, site: u16, k: i16, tmp: SlotRef) {
    f.emit(Op::LoadSmall, tmp, k as u16, 0u16);
    f.emit(Op::CallHost, site, tmp, tmp);
}

/// `out = 0 / 0` through `zero`: the fault `DivByZero`.
fn div_by_zero(f: &mut FnBuilder, zero: SlotRef, out: SlotRef) {
    f.emit(Op::LoadSmall, zero, 0u16, 0u16);
    f.emit(Op::DivI, out, zero, zero);
}

/// A loop of `n` iterations over locals `base .. base + 4` of the function being built.
fn spin(f: &mut FnBuilder, base: u16, n: i16) {
    let (i, lim, one, cond) = (
        f.local(base),
        f.local(base + 1),
        f.local(base + 2),
        f.local(base + 3),
    );
    f.emit(Op::LoadSmall, i, 0u16, 0u16);
    f.emit(Op::LoadSmall, lim, n as u16, 0u16);
    f.emit(Op::LoadSmall, one, 1u16, 0u16);
    let (top, done) = (f.label(), f.label());
    f.bind(top);
    f.emit(Op::LtI, cond, i, lim);
    f.jump(Op::JmpIfNot, cond, done);
    f.emit(Op::AddI, i, i, one);
    f.jump(Op::Jmp, 0u16, top);
    f.bind(done);
}

/// A handler for `event`: its event parameter is marked; `body` writes the rest (the `Ret` included).
fn handler(b: &mut B, event: &str, name: &str, body: impl FnOnce(&mut FnBuilder, &Tys, u16, u16)) {
    let h = b.pb.declare(name, 1);
    let (get, log) = (b.get, b.log);
    {
        let mut f = b.pb.body(h);
        let ev = f.param(0);
        f.mark_ref(ev);
        body(&mut f, &b.t, get, log);
        f.end();
    }
    let ping = b.t.ping;
    b.pb.handler(event, ping, h);
}

fn finish(b: B) -> Arc<Program> {
    Arc::new(b.pb.finish().expect("program"))
}

fn ping(n: i64) -> WireValue {
    WireValue::Struct {
        name: "Ping".into(),
        fields: vec![("n".into(), WireValue::Int(n))],
    }
}

fn variant(name: &str) -> WireValue {
    WireValue::Variant {
        name: name.into(),
        fields: vec![],
    }
}

/// Ticks at `now` until the instance is no longer busy.
fn settle(vm: &mut Vm, host: &mut RecordingHost, now: u64) {
    for _ in 0..100_000 {
        if vm.tick(now, 1_000, host).status != Status::Busy {
            return;
        }
    }
    panic!("the instance never settled");
}

fn logs(host: &RecordingHost) -> Vec<i64> {
    host.commands
        .iter()
        .map(|(f, args)| match (f, args.as_slice()) {
            (&LOG, [WireValue::Int(n)]) => *n,
            other => panic!("unexpected command {other:?}"),
        })
        .collect()
}

fn id(serial: u64) -> PromiseId {
    // `common::cfg` makes instances of epoch 1
    PromiseId(1 << 32 | serial)
}

fn last_request(host: &RecordingHost) -> PromiseId {
    host.requests.last().expect("a request").0
}

fn slot(p: &Program, g: GlobalRef) -> u16 {
    p.globals[usize::from(g.0)].slot
}

/// The faults reported so far, in order.
fn faults(host: &RecordingHost) -> Vec<FaultKind> {
    host.reports
        .iter()
        .map(|r| match r {
            Report::HandlerAborted {
                cause: AbortCause::Fault { kind, .. },
                ..
            } => *kind,
            other => panic!("unexpected report {other:?}"),
        })
        .collect()
}

/// `Ping` handler: `V = await get(e.n)`, the error moved into `E`, then `log(1)`.
fn await_one(b: &mut B) -> (GlobalRef, GlobalRef) {
    let v = b.pb.global("v", b.t.int);
    let e = b.pb.global("e", b.t.err);
    handler(b, "Ping", "on_ping", |f, t, get, log| {
        let ev = f.param(0);
        let (k, p, val, err, tmp) = (f.local(0), f.local(1), f.local(2), f.local(3), f.local(4));
        f.mark_ref(err);
        f.emit(Op::GetField, k, ev, 0u16);
        f.emit(Op::CallHost, get, k, p);
        f.emit(Op::Await, val, p, t.int);
        f.emit(Op::Copy, SlotRef::Global(v), val, 0u16);
        f.emit(Op::Move, SlotRef::Global(e), err, 0u16);
        log_k(f, log, 1, tmp);
        ret(f);
    });
    (v, e)
}

#[test]
fn request_reaches_the_host_at_commit() {
    let mut b = builder();
    handler(&mut b, "Ping", "on_ping", |f, _, get, _| {
        let ev = f.param(0);
        let (k, p1, p2) = (f.local(0), f.local(1), f.local(2));
        f.emit(Op::GetField, k, ev, 0u16);
        f.emit(Op::CallHost, get, k, p1);
        spin(f, 3, 30);
        f.emit(Op::CallHost, get, k, p2);
        ret(f);
    });
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    vm.deliver("Ping", &ping(5));
    assert_eq!(vm.tick(2, 40, &mut host).status, Status::Busy);
    assert!(host.requests.is_empty(), "a request waits for its commit");
    settle(&mut vm, &mut host, 2);
    assert_eq!(
        host.requests,
        [
            (id(1), GET, vec![WireValue::Int(5)]),
            (id(2), GET, vec![WireValue::Int(5)]),
        ]
    );
    assert!(host.reports.is_empty());
}

#[test]
fn await_parks_and_other_events_run() {
    let mut b = builder();
    let (g, h, v) = (
        b.pb.global("g", b.t.int),
        b.pb.global("h", b.t.int),
        b.pb.global("v", b.t.int),
    );
    handler(&mut b, "Ping", "on_ping", |f, t, get, _| {
        let ev = f.param(0);
        let (k, p, val, err) = (f.local(0), f.local(1), f.local(2), f.local(3));
        f.mark_ref(err);
        f.emit(Op::GetField, k, ev, 0u16);
        f.emit(Op::CallHost, get, k, p);
        f.emit(Op::Await, val, p, t.int);
        f.emit(Op::Copy, SlotRef::Global(h), SlotRef::Global(g), 0u16);
        f.emit(Op::Copy, SlotRef::Global(v), val, 0u16);
        ret(f);
    });
    handler(&mut b, "Pong", "on_pong", |f, _, _, _| {
        f.emit(Op::LoadSmall, SlotRef::Global(g), 42u16, 0u16);
        ret(f);
    });
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    let baseline = vm.heap_used_words();
    vm.deliver("Ping", &ping(1));
    settle(&mut vm, &mut host, 2);
    assert_eq!(host.requests.len(), 1);
    assert!(vm.is_idle(), "a parked handler is not runnable");
    vm.deliver("Pong", &ping(0));
    settle(&mut vm, &mut host, 3);
    assert_eq!(vm.slot(slot(&p, g)), 42);
    assert_eq!(vm.slot(slot(&p, h)), 0);
    assert_eq!(
        vm.resolve(last_request(&host), Ok(WireValue::Int(7))),
        Resolved::Accepted
    );
    settle(&mut vm, &mut host, 4);
    assert_eq!(vm.slot(slot(&p, h)), 42);
    assert_eq!(vm.slot(slot(&p, v)), 7);
    assert!(host.reports.is_empty());
    assert_eq!(vm.heap_used_words(), baseline);
}

#[test]
fn await_of_a_resolved_promise_still_yields() {
    let mut b = builder();
    let (pg, g, h) = (
        b.pb.global("p", b.t.prom),
        b.pb.global("g", b.t.int),
        b.pb.global("h", b.t.int),
    );
    handler(&mut b, "Ping", "on_ping", |f, _, get, _| {
        let ev = f.param(0);
        let k = f.local(0);
        f.emit(Op::GetField, k, ev, 0u16);
        f.emit(Op::CallHost, get, k, SlotRef::Global(pg));
        ret(f);
    });
    handler(&mut b, "Pong", "on_pong", |f, t, _, log| {
        let (val, err) = (f.local(0), f.local(1));
        f.mark_ref(err);
        f.emit(Op::Await, val, SlotRef::Global(pg), t.int);
        f.emit(Op::Copy, SlotRef::Global(h), SlotRef::Global(g), 0u16);
        f.emit(Op::CallHost, log, val, val);
        ret(f);
    });
    handler(&mut b, "Quiet", "on_quiet", |f, _, _, _| {
        f.emit(Op::LoadSmall, SlotRef::Global(g), 5u16, 0u16);
        ret(f);
    });
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    vm.deliver("Ping", &ping(9));
    settle(&mut vm, &mut host, 2);
    assert_eq!(
        vm.resolve(last_request(&host), Ok(WireValue::Int(3))),
        Resolved::Accepted
    );
    vm.deliver("Pong", &ping(0));
    vm.deliver("Quiet", &ping(0));
    settle(&mut vm, &mut host, 3);
    assert_eq!(
        vm.slot(slot(&p, h)),
        5,
        "Quiet ran between the await and the resume"
    );
    assert_eq!(logs(&host), [3]);
    assert!(host.reports.is_empty());
}

#[test]
fn resume_order_follows_arrival() {
    let program = || {
        let mut b = builder();
        handler(&mut b, "Ping", "on_ping", |f, t, get, log| {
            let ev = f.param(0);
            let (k, p, val, err, tmp) =
                (f.local(0), f.local(1), f.local(2), f.local(3), f.local(4));
            f.mark_ref(err);
            f.emit(Op::GetField, k, ev, 0u16);
            f.emit(Op::CallHost, get, k, p);
            f.emit(Op::Await, val, p, t.int);
            log_k(f, log, 1, tmp);
            ret(f);
        });
        handler(&mut b, "Pong", "on_pong", |f, _, _, log| {
            let tmp = f.local(0);
            log_k(f, log, 2, tmp);
            ret(f);
        });
        finish(b)
    };
    for event_first in [true, false] {
        let p = program();
        let mut vm = vm(&p);
        let mut host = RecordingHost::default();
        settle(&mut vm, &mut host, 1);
        vm.deliver("Ping", &ping(1));
        settle(&mut vm, &mut host, 2);
        let pid = last_request(&host);
        if event_first {
            vm.deliver("Pong", &ping(0));
            vm.resolve(pid, Ok(WireValue::Int(0)));
        } else {
            vm.resolve(pid, Ok(WireValue::Int(0)));
            vm.deliver("Pong", &ping(0));
        }
        settle(&mut vm, &mut host, 3);
        let want: &[i64] = if event_first { &[2, 1] } else { &[1, 2] };
        assert_eq!(logs(&host), want, "event first: {event_first}");
    }
}

#[test]
fn fault_after_await_undoes_only_the_last_segment() {
    let mut b = builder();
    let g = b.pb.global("g", b.t.int);
    let keep = b.pb.string("held across the await");
    handler(&mut b, "Ping", "on_ping", |f, t, get, log| {
        let ev = f.param(0);
        let (k, p, val, err, tmp, s, zero, out) = (
            f.local(0),
            f.local(1),
            f.local(2),
            f.local(3),
            f.local(4),
            f.local(5),
            f.local(6),
            f.local(7),
        );
        f.mark_ref(err);
        f.mark_ref(s);
        f.emit_t(Op::LoadStr, s, keep);
        f.emit(Op::LoadSmall, SlotRef::Global(g), 1u16, 0u16);
        log_k(f, log, 10, tmp);
        f.emit(Op::GetField, k, ev, 0u16);
        f.emit(Op::CallHost, get, k, p);
        f.emit(Op::Await, val, p, t.int);
        f.emit(Op::LoadSmall, SlotRef::Global(g), 2u16, 0u16);
        log_k(f, log, 20, tmp);
        div_by_zero(f, zero, out);
        ret(f);
    });
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    let baseline = vm.heap_used_words();
    vm.deliver("Ping", &ping(1));
    settle(&mut vm, &mut host, 2);
    assert_eq!(vm.slot(slot(&p, g)), 1);
    assert_eq!(logs(&host), [10]);
    vm.resolve(last_request(&host), Ok(WireValue::Int(0)));
    settle(&mut vm, &mut host, 3);
    assert_eq!(
        vm.slot(slot(&p, g)),
        1,
        "only the segment after the await is undone"
    );
    assert_eq!(logs(&host), [10], "Y never reached the host");
    assert_eq!(faults(&host), [FaultKind::DivByZero]);
    assert_eq!(
        vm.heap_used_words(),
        baseline,
        "the parked frames are given back"
    );
    assert!(vm.is_idle());
}

#[test]
fn aborted_segment_requests_never_reach_the_host() {
    let mut b = builder();
    handler(&mut b, "Ping", "on_ping", |f, _, get, _| {
        let ev = f.param(0);
        let (k, p, zero, out) = (f.local(0), f.local(1), f.local(2), f.local(3));
        f.emit(Op::GetField, k, ev, 0u16);
        f.emit(Op::CallHost, get, k, p);
        div_by_zero(f, zero, out);
        ret(f);
    });
    handler(&mut b, "Pong", "on_pong", |f, _, get, _| {
        let ev = f.param(0);
        let (k, p) = (f.local(0), f.local(1));
        f.emit(Op::GetField, k, ev, 0u16);
        f.emit(Op::CallHost, get, k, p);
        ret(f);
    });
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    vm.deliver("Ping", &ping(1));
    settle(&mut vm, &mut host, 2);
    assert!(host.requests.is_empty());
    assert_eq!(faults(&host), [FaultKind::DivByZero]);
    vm.deliver("Pong", &ping(2));
    settle(&mut vm, &mut host, 3);
    assert_eq!(
        host.requests,
        [(id(1), GET, vec![WireValue::Int(2)])],
        "the serial of the discarded request is reused"
    );
}

#[test]
fn every_rejection_kind() {
    let mut b = builder();
    let (v, e) = await_one(&mut b);
    let err_ty = b.t.err;
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    let changed = WireValue::Variant {
        name: "Changed".into(),
        fields: vec![("current".into(), WireValue::Int(5))],
    };
    let cases: Vec<(Result<WireValue, WireValue>, Option<WireValue>)> = vec![
        (Err(variant("NotFound")), Some(variant("NotFound"))),
        (Err(changed.clone()), Some(changed)),
        (Err(variant("Lost")), Some(variant("Lost"))),
        (
            Err(variant("NamespaceGone")),
            Some(variant("NamespaceGone")),
        ),
        (Ok(WireValue::Str("x".into())), Some(variant("WrongType"))),
        // an error the program does not know, or not an error at all: `Lost`
        (Err(variant("Bogus")), Some(variant("Lost"))),
        (Err(WireValue::Int(3)), Some(variant("Lost"))),
        (Ok(WireValue::Int(8)), None),
    ];
    for (i, (outcome, want)) in cases.into_iter().enumerate() {
        vm.deliver("Ping", &ping(i as i64));
        settle(&mut vm, &mut host, 2);
        assert_eq!(vm.resolve(last_request(&host), outcome), Resolved::Accepted);
        settle(&mut vm, &mut host, 2);
        assert_eq!(logs(&host).len(), i + 1, "case {i} resumed");
        match want {
            Some(w) => assert_eq!(vm.debug_value(slot(&p, e), err_ty), Ok(w), "case {i}"),
            None => {
                assert_eq!(vm.slot(slot(&p, e)), 0, "case {i}: no error");
                assert_eq!(vm.slot(slot(&p, v)), 8);
            }
        }
    }
    assert!(host.reports.is_empty());
}

#[test]
fn timeout_after_the_deadline() {
    let mut b = builder();
    let (_, e) = await_one(&mut b);
    let err_ty = b.t.err;
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    vm.deliver("Ping", &ping(1));
    let issue = 10;
    settle(&mut vm, &mut host, issue);
    let pid = last_request(&host);
    settle(&mut vm, &mut host, issue + PROMISE_TIMEOUT_TICKS - 1);
    assert!(
        logs(&host).is_empty(),
        "still waiting one tick before the deadline"
    );
    settle(&mut vm, &mut host, issue + PROMISE_TIMEOUT_TICKS);
    assert_eq!(logs(&host), [1]);
    assert_eq!(vm.debug_value(slot(&p, e), err_ty), Ok(variant("Timeout")));
    let before = vm.mem_bytes().to_vec();
    assert_eq!(vm.resolve(pid, Ok(WireValue::Int(1))), Resolved::Unknown);
    assert_eq!(vm.mem_bytes(), &before[..]);
}

#[test]
fn caps() {
    // the parked table
    let mut b = builder();
    await_one(&mut b);
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    for i in 0..PARKED_MAX as i64 {
        vm.deliver("Ping", &ping(i));
    }
    settle(&mut vm, &mut host, 2);
    assert_eq!(host.requests.len(), PARKED_MAX);
    assert!(host.reports.is_empty());
    vm.deliver("Ping", &ping(99));
    settle(&mut vm, &mut host, 3);
    assert_eq!(faults(&host), [FaultKind::TooManyParked]);
    assert_eq!(
        host.requests.len(),
        PARKED_MAX,
        "the refused handler's request is discarded"
    );

    // the promise table: a 17th live promise
    let mut b = builder();
    handler(&mut b, "Ping", "on_ping", |f, _, get, _| {
        let (k, p, i, lim, one, cond) = (
            f.local(0),
            f.local(1),
            f.local(2),
            f.local(3),
            f.local(4),
            f.local(5),
        );
        f.emit(Op::LoadSmall, i, 0u16, 0u16);
        f.emit(Op::LoadSmall, lim, 17u16, 0u16);
        f.emit(Op::LoadSmall, one, 1u16, 0u16);
        let (top, done) = (f.label(), f.label());
        f.bind(top);
        f.emit(Op::LtI, cond, i, lim);
        f.jump(Op::JmpIfNot, cond, done);
        f.emit(Op::Copy, k, i, 0u16);
        f.emit(Op::CallHost, get, k, p);
        f.emit(Op::AddI, i, i, one);
        f.jump(Op::Jmp, 0u16, top);
        f.bind(done);
        ret(f);
    });
    let p = finish(b);
    let mut vm = common::vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    vm.deliver("Ping", &ping(0));
    settle(&mut vm, &mut host, 2);
    assert_eq!(faults(&host), [FaultKind::TooManyPromises]);
    assert!(host.requests.is_empty());
}

#[test]
fn await_twice_faults() {
    let mut b = builder();
    handler(&mut b, "Ping", "on_ping", |f, t, get, _| {
        let ev = f.param(0);
        let (k, p, q, val, err) = (f.local(0), f.local(1), f.local(2), f.local(3), f.local(4));
        f.mark_ref(err);
        f.emit(Op::GetField, k, ev, 0u16);
        f.emit(Op::CallHost, get, k, p);
        f.emit(Op::Copy, q, p, 0u16);
        f.emit(Op::Await, val, p, t.int);
        f.emit(Op::Await, val, q, t.int);
        ret(f);
    });
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    vm.deliver("Ping", &ping(1));
    settle(&mut vm, &mut host, 2);
    vm.resolve(last_request(&host), Ok(WireValue::Int(1)));
    settle(&mut vm, &mut host, 3);
    assert_eq!(faults(&host), [FaultKind::PromiseAlreadyAwaited]);
    assert_eq!(host.requests.len(), 1);
}

/// A `Pong` handler issuing `n` requests it never awaits.
fn issue_many(b: &mut B, n: u16) {
    handler(b, "Pong", "on_pong", |f, _, get, _| {
        let (p, i, lim, one, cond) = (f.local(0), f.local(1), f.local(2), f.local(3), f.local(4));
        f.emit(Op::LoadSmall, i, 0u16, 0u16);
        f.emit(Op::LoadSmall, lim, n, 0u16);
        f.emit(Op::LoadSmall, one, 1u16, 0u16);
        let (top, done) = (f.label(), f.label());
        f.bind(top);
        f.emit(Op::LtI, cond, i, lim);
        f.jump(Op::JmpIfNot, cond, done);
        f.emit(Op::CallHost, get, i, p);
        f.emit(Op::AddI, i, i, one);
        f.jump(Op::Jmp, 0u16, top);
        f.bind(done);
        ret(f);
    });
}

#[test]
fn detached_promise_frees_its_record_on_resolution() {
    let mut b = builder();
    handler(&mut b, "Ping", "on_ping", |f, _, get, _| {
        let ev = f.param(0);
        let (k, p) = (f.local(0), f.local(1));
        f.emit(Op::GetField, k, ev, 0u16);
        f.emit(Op::CallHost, get, k, p);
        f.emit(Op::PromiseDetach, p, 0u16, 0u16);
        ret(f);
    });
    issue_many(&mut b, 16);
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    vm.deliver("Ping", &ping(1));
    settle(&mut vm, &mut host, 2);
    let pid = last_request(&host);
    assert_eq!(vm.resolve(pid, Ok(WireValue::Int(1))), Resolved::Accepted);
    assert_eq!(vm.resolve(pid, Ok(WireValue::Int(1))), Resolved::Unknown);
    // 16 more live promises fit only if the detached one's record was freed
    vm.deliver("Pong", &ping(0));
    settle(&mut vm, &mut host, 3);
    assert!(host.reports.is_empty(), "{:?}", host.reports);
    assert_eq!(host.requests.len(), 17);
}

#[test]
fn await_all() {
    let mut b = builder();
    let v = b.pb.global("v", b.t.list_int);
    let e = b.pb.global("e", b.t.err);
    handler(&mut b, "Ping", "on_ping", |f, t, get, log| {
        let ev = f.param(0);
        let (list, elem, scratch, k, one, val, err, tmp) = (
            f.local(0),
            f.local(1),
            f.local(2),
            f.local(3),
            f.local(4),
            f.local(5),
            f.local(6),
            f.local(7),
        );
        f.mark_ref(list);
        f.mark_ref(val);
        f.mark_ref(err);
        f.emit(Op::GetField, k, ev, 0u16);
        f.emit(Op::LoadSmall, one, 1u16, 0u16);
        f.emit(Op::NewList, list, 0u16, 0u16);
        for _ in 0..3 {
            f.emit(Op::CallHost, get, k, elem);
            f.emit(Op::CallBuiltin, Builtin::ListPush as u16, list, scratch);
            f.emit(Op::AddI, k, k, one);
        }
        f.emit(Op::AwaitAll, val, list, t.list_int);
        f.emit(Op::Move, SlotRef::Global(v), val, 0u16);
        f.emit(Op::Move, SlotRef::Global(e), err, 0u16);
        f.emit(Op::Drop, list, 0u16, 0u16);
        log_k(f, log, 1, tmp);
        ret(f);
    });
    let (list_int, err_ty) = (b.t.list_int, b.t.err);
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);

    // all answers, out of order: one resume, the values in list order
    vm.deliver("Ping", &ping(10));
    settle(&mut vm, &mut host, 2);
    let ids: Vec<PromiseId> = host.requests.iter().map(|r| r.0).collect();
    assert_eq!(ids.len(), 3);
    vm.resolve(ids[2], Ok(WireValue::Int(30)));
    settle(&mut vm, &mut host, 3);
    vm.resolve(ids[0], Ok(WireValue::Int(10)));
    settle(&mut vm, &mut host, 4);
    assert!(
        logs(&host).is_empty(),
        "parked until every promise is resolved"
    );
    vm.resolve(ids[1], Ok(WireValue::Int(20)));
    settle(&mut vm, &mut host, 5);
    assert_eq!(logs(&host), [1]);
    assert_eq!(
        vm.debug_value(slot(&p, v), list_int),
        Ok(WireValue::List(vec![
            WireValue::Int(10),
            WireValue::Int(20),
            WireValue::Int(30)
        ]))
    );
    assert_eq!(vm.slot(slot(&p, e)), 0);

    // failures: the first failed one in list order is the error
    vm.deliver("Ping", &ping(40));
    settle(&mut vm, &mut host, 6);
    let ids: Vec<PromiseId> = host.requests[3..].iter().map(|r| r.0).collect();
    vm.resolve(ids[2], Err(variant("Lost")));
    vm.resolve(ids[1], Err(variant("NotFound")));
    vm.resolve(ids[0], Ok(WireValue::Int(1)));
    settle(&mut vm, &mut host, 7);
    assert_eq!(logs(&host), [1, 1]);
    assert_eq!(vm.debug_value(slot(&p, e), err_ty), Ok(variant("NotFound")));
    assert!(host.reports.is_empty());
}

#[test]
fn resolve_unknown_or_finished_id_is_ignored() {
    let mut b = builder();
    await_one(&mut b);
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    vm.deliver("Ping", &ping(1));
    settle(&mut vm, &mut host, 2);
    let pid = last_request(&host);
    let ignored = |vm: &mut Vm, id: PromiseId| {
        let before = vm.mem_bytes().to_vec();
        assert_eq!(
            vm.resolve(id, Ok(WireValue::Int(1))),
            Resolved::Unknown,
            "{id:?}"
        );
        assert_eq!(vm.mem_bytes(), &before[..], "{id:?} changed the state");
    };
    ignored(&mut vm, id(99));
    ignored(&mut vm, PromiseId(2 << 32 | 1));
    ignored(&mut vm, PromiseId(0));
    assert_eq!(vm.resolve(pid, Ok(WireValue::Int(1))), Resolved::Accepted);
    ignored(&mut vm, pid);
    settle(&mut vm, &mut host, 3);
    ignored(&mut vm, pid);
    assert_eq!(logs(&host), [1]);
}

#[test]
fn oversized_answer_becomes_lost() {
    let mut b = builder();
    let (_, e) = await_one(&mut b);
    let err_ty = b.t.err;
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    vm.deliver("Ping", &ping(1));
    settle(&mut vm, &mut host, 2);
    let huge = WireValue::Str("x".repeat(9_000));
    assert_eq!(
        vm.resolve(last_request(&host), Ok(huge)),
        Resolved::Accepted
    );
    settle(&mut vm, &mut host, 3);
    assert_eq!(vm.debug_value(slot(&p, e), err_ty), Ok(variant("Lost")));
}

#[test]
fn heap_full_while_parking_faults_cleanly() {
    let mut b = builder();
    handler(&mut b, "Ping", "on_ping", |f, t, get, _| {
        let ev = f.param(0);
        let (k, p, val, err, far) = (f.local(0), f.local(1), f.local(2), f.local(3), f.local(700));
        f.mark_ref(err);
        // a frame of 700 words: parking it needs a block larger than the whole heap
        f.emit(Op::LoadSmall, far, 1u16, 0u16);
        f.emit(Op::GetField, k, ev, 0u16);
        f.emit(Op::CallHost, get, k, p);
        f.emit(Op::Await, val, p, t.int);
        ret(f);
    });
    issue_many(&mut b, 1);
    let p = finish(b);
    // the smallest memory the program fits in: a heap of about `HEAP_MIN_BYTES`
    let need = match Layout::plan(u32::from(p.static_words), 0) {
        Err(LoadError::MemTooSmall { need }) => need,
        other => panic!("unexpected plan {other:?}"),
    };
    let mut vm = Vm::new(
        p.clone(),
        VmConfig {
            mem_bytes: need,
            ..cfg()
        },
    )
    .expect("the program loads");
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    let baseline = vm.heap_used_words();
    vm.deliver("Ping", &ping(1));
    settle(&mut vm, &mut host, 2);
    assert_eq!(faults(&host), [FaultKind::HeapFull]);
    assert!(
        host.requests.is_empty(),
        "the segment's request is discarded"
    );
    assert_eq!(vm.heap_used_words(), baseline);
    assert!(vm.is_idle());
    let l = Layout::plan(u32::from(p.static_words), need).expect("layout");
    let parked =
        &vm.mem_bytes()[l.parked_base as usize..][..PARKED_MAX * PARKED_REC_WORDS as usize * 8];
    assert!(parked.iter().all(|&b| b == 0), "no parked record remains");
    vm.deliver("Pong", &ping(0));
    settle(&mut vm, &mut host, 3);
    assert_eq!(host.requests, [(id(1), GET, vec![WireValue::Int(0)])]);
}

#[test]
fn await_in_a_nested_call_restores_every_frame() {
    let mut b = builder();
    let g = b.pb.global("g", b.t.int);
    let s1 = b.pb.global("s1", b.t.str);
    let s2 = b.pb.global("s2", b.t.str);
    let (outer_s, mid_s) = (b.pb.string("outer"), b.pb.string("mid"));
    let mid = b.pb.declare("mid", 1);
    let inner = b.pb.declare("inner", 0);
    {
        // inner(): c = 33; return c + await get(c)
        let mut f = b.pb.body(inner);
        let (c, p, val, err, r) = (f.local(0), f.local(1), f.local(2), f.local(3), f.ret());
        f.mark_ref(err);
        f.emit(Op::LoadSmall, c, 33u16, 0u16);
        f.emit(Op::CallHost, b.get, c, p);
        f.emit(Op::Await, val, p, b.t.int);
        f.emit(Op::AddI, r, c, val);
        ret(&mut f);
        f.end();
    }
    {
        // mid(x): b = x; s = "mid"; r = inner() + b; s2 = s
        let mut f = b.pb.body(mid);
        let (x, bl, s, r) = (f.param(0), f.local(0), f.local(1), f.ret());
        f.mark_ref(s);
        let iret = f.of(inner).ret();
        f.emit(Op::Copy, bl, x, 0u16);
        f.emit_t(Op::LoadStr, s, mid_s);
        f.call(inner);
        f.emit(Op::AddI, r, iret, bl);
        f.emit(Op::Move, SlotRef::Global(s2), s, 0u16);
        ret(&mut f);
        f.end();
    }
    let mid_ref: FnRef = mid;
    handler(&mut b, "Ping", "on_ping", |f, _, _, _| {
        // a = 11; s = "outer"; g = mid(22) + a; s1 = s
        let (a, s) = (f.local(0), f.local(1));
        f.mark_ref(s);
        let callee = f.of(mid_ref);
        f.emit(Op::LoadSmall, a, 11u16, 0u16);
        f.emit_t(Op::LoadStr, s, outer_s);
        f.emit(Op::LoadSmall, callee.param(0), 22u16, 0u16);
        f.call(mid_ref);
        f.emit(Op::AddI, SlotRef::Global(g), callee.ret(), a);
        f.emit(Op::Move, SlotRef::Global(s1), s, 0u16);
        ret(f);
    });
    let str_ty = b.t.str;
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    // two handlers parked in the same three frames
    vm.deliver("Ping", &ping(0));
    vm.deliver("Ping", &ping(0));
    settle(&mut vm, &mut host, 2);
    let ids: Vec<PromiseId> = host.requests.iter().map(|r| r.0).collect();
    assert_eq!(ids.len(), 2);
    vm.resolve(ids[1], Ok(WireValue::Int(200)));
    settle(&mut vm, &mut host, 3);
    assert_eq!(vm.slot(slot(&p, g)), 33 + 200 + 22 + 11);
    vm.resolve(ids[0], Ok(WireValue::Int(100)));
    settle(&mut vm, &mut host, 4);
    assert_eq!(vm.slot(slot(&p, g)), 33 + 100 + 22 + 11);
    assert_eq!(
        vm.debug_value(slot(&p, s1), str_ty),
        Ok(WireValue::Str("outer".into()))
    );
    assert_eq!(
        vm.debug_value(slot(&p, s2), str_ty),
        Ok(WireValue::Str("mid".into()))
    );
    assert!(host.reports.is_empty());
}

/// `depth` lists nested in one another around `Int(0)`, built without recursion.
fn nested(depth: usize) -> WireValue {
    let mut v = WireValue::Int(0);
    for _ in 0..depth {
        v = WireValue::List(vec![v]);
    }
    v
}

#[test]
fn too_deep_answer_becomes_lost() {
    let mut b = builder();
    let (_, e) = await_one(&mut b);
    let err_ty = b.t.err;
    let p = finish(b);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    settle(&mut vm, &mut host, 1);
    for depth in [WIRE_DEPTH_MAX + 1, 1_000_000] {
        vm.deliver("Ping", &ping(1));
        settle(&mut vm, &mut host, 2);
        // `resolve` drops the value it is given without recursion
        assert_eq!(
            vm.resolve(last_request(&host), Ok(nested(depth))),
            Resolved::Accepted
        );
        settle(&mut vm, &mut host, 3);
        assert_eq!(vm.status(), Status::Idle, "depth {depth}");
        assert_eq!(
            vm.debug_value(slot(&p, e), err_ty),
            Ok(variant("Lost")),
            "depth {depth}"
        );
    }
    // an unknown id, and an id already resolved: `Unknown`, the value still dropped without recursion
    assert_eq!(
        vm.resolve(id(999), Ok(nested(1_000_000))),
        Resolved::Unknown
    );
    vm.deliver("Ping", &ping(1));
    settle(&mut vm, &mut host, 4);
    let pid = last_request(&host);
    assert_eq!(vm.resolve(pid, Ok(WireValue::Int(5))), Resolved::Accepted);
    assert_eq!(vm.resolve(pid, Err(nested(1_000_000))), Resolved::Unknown);
    settle(&mut vm, &mut host, 5);
    assert_eq!(logs(&host), [1, 1, 1]);
    assert!(host.reports.is_empty());

    // exactly `WIRE_DEPTH_MAX` deep: accepted and stored as it is, so it decodes at resume (no program
    // can await a type that deep, `Promise` adds a level): the int handler gets `WrongType`
    vm.deliver("Ping", &ping(1));
    settle(&mut vm, &mut host, 6);
    assert_eq!(
        vm.resolve(last_request(&host), Ok(nested(WIRE_DEPTH_MAX))),
        Resolved::Accepted
    );
    settle(&mut vm, &mut host, 7);
    assert_eq!(vm.status(), Status::Idle);
    assert_eq!(
        vm.debug_value(slot(&p, e), err_ty),
        Ok(variant("WrongType"))
    );
    assert!(host.reports.is_empty());
}
