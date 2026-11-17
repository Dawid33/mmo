//! Shared by the interpreter's test binaries: a host that records everything and small assembled programs.
#![allow(dead_code)]

use std::sync::Arc;

use quest_vm::Op;
use quest_vm::asm::{FnBuilder, ProgramBuilder, SlotRef};
use quest_vm::consts::MEM_BYTES_DEFAULT;
use quest_vm::host::{Host, HostFnKind, PromiseId, QueryError, Report};
use quest_vm::program::Program;
use quest_vm::types::TypeDesc;
use quest_vm::vm::{DebugRun, RunEnd, Vm, VmConfig};
use quest_vm::wire::WireValue;

/// The host-API version every test program is built against.
pub const API: u32 = 1;

/// The host's event names every test instance is configured with.
pub const EVENTS: &[&str] = &["Ping", "Pong", "Big", "Quiet"];

type QueryFn = Box<dyn FnMut(u16, &[WireValue]) -> Result<WireValue, QueryError>>;
type CheckFn = Box<dyn FnMut(u16, &[WireValue]) -> Result<(), QueryError>>;

/// Records every call; `query` is answered by `answer` (default `Ok(Unit)`), `check` by `check_answer`
/// (default `Ok`).
#[derive(Default)]
pub struct RecordingHost {
    pub queries: Vec<(u16, Vec<WireValue>)>,
    pub checks: Vec<(u16, Vec<WireValue>)>,
    pub commands: Vec<(u16, Vec<WireValue>)>,
    pub requests: Vec<(PromiseId, u16, Vec<WireValue>)>,
    pub prints: Vec<String>,
    pub reports: Vec<Report>,
    pub answer: Option<QueryFn>,
    pub check_answer: Option<CheckFn>,
}

impl Clone for RecordingHost {
    /// The records; a host answering queries with a closure cannot be cloned.
    fn clone(&self) -> RecordingHost {
        assert!(
            self.answer.is_none() && self.check_answer.is_none(),
            "clone of a host with an answer closure"
        );
        RecordingHost {
            queries: self.queries.clone(),
            checks: self.checks.clone(),
            commands: self.commands.clone(),
            requests: self.requests.clone(),
            prints: self.prints.clone(),
            reports: self.reports.clone(),
            answer: None,
            check_answer: None,
        }
    }
}

impl RecordingHost {
    /// Every call recorded so far, as text (for comparing two hosts).
    pub fn transcript(&self) -> String {
        format!(
            "queries {:?}\nchecks {:?}\ncommands {:?}\nrequests {:?}\nprints {:?}\nreports {:?}",
            self.queries, self.checks, self.commands, self.requests, self.prints, self.reports
        )
    }
}

impl Host for RecordingHost {
    fn query(&mut self, f: u16, args: &[WireValue]) -> Result<WireValue, QueryError> {
        self.queries.push((f, args.to_vec()));
        match &mut self.answer {
            Some(a) => a(f, args),
            None => Ok(WireValue::Unit),
        }
    }
    fn check(&mut self, f: u16, args: &[WireValue]) -> Result<(), QueryError> {
        self.checks.push((f, args.to_vec()));
        match &mut self.check_answer {
            Some(a) => a(f, args),
            None => Ok(()),
        }
    }
    fn command(&mut self, f: u16, args: &[WireValue]) {
        self.commands.push((f, args.to_vec()));
    }
    fn request(&mut self, id: PromiseId, f: u16, args: &[WireValue]) {
        self.requests.push((id, f, args.to_vec()));
    }
    fn print(&mut self, text: &str) {
        self.prints.push(text.to_string());
    }
    fn report(&mut self, report: Report) {
        self.reports.push(report);
    }
}

pub fn cfg() -> VmConfig {
    VmConfig {
        mem_bytes: MEM_BYTES_DEFAULT,
        epoch: 1,
        host_api_version: API,
        events: EVENTS.iter().map(|e| e.to_string()).collect(),
    }
}

pub fn vm(p: &Arc<Program>) -> Vm {
    Vm::new(p.clone(), cfg()).expect("the test program loads")
}

/// Absolute slot of word `word` of fn `f`'s frame (1 = return value, 3.. = parameters).
pub fn frame_slot(p: &Program, f: u16, word: u16) -> u16 {
    p.fns[usize::from(f)].frame_base + word
}

/// Starts fn `f` and runs it with unlimited fuel.
pub fn run_fn(vm: &mut Vm, f: u16) -> DebugRun {
    vm.debug_start(f);
    vm.debug_run(u64::MAX, &mut RecordingHost::default())
}

/// Starts fn 0 and calls `debug_run(budget)` until the run is no longer `OutOfFuel`: the last run with the
/// total fuel charged over all runs, and the number of runs.
pub fn run_in_slices(vm: &mut Vm, budget: u64) -> (DebugRun, u64) {
    vm.debug_start(0);
    let mut host = RecordingHost::default();
    let mut total = 0u64;
    for runs in 1..=1_000_000 {
        let r = vm.debug_run(budget, &mut host);
        total += r.fuel_used;
        if r.end != RunEnd::OutOfFuel {
            let last = DebugRun {
                fuel_used: total,
                end: r.end,
            };
            return (last, runs);
        }
    }
    panic!("the program did not end");
}

/// `main` (fn 0): `ret = x <op> y`, the op on source line `line`. `x`, `y` are raw slot bits.
pub fn binop(op: Op, x: u64, y: u64, line: u32) -> Arc<Program> {
    let mut pb = ProgramBuilder::new(API);
    let (cx, cy) = (pb.const_u64(x), pb.const_u64(y));
    let mut f = pb.func("main", 0);
    let (lx, ly, ret) = (f.local(0), f.local(1), f.ret());
    f.emit_t(Op::LoadConst, lx, cx);
    f.emit_t(Op::LoadConst, ly, cy);
    f.line(line);
    f.emit(op, ret, lx, ly);
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
    f.end();
    Arc::new(pb.finish().expect("binop program"))
}

/// `main` (fn 0): `ret = <op> x`, the op on source line `line`.
pub fn unop(op: Op, x: u64, line: u32) -> Arc<Program> {
    let mut pb = ProgramBuilder::new(API);
    let cx = pb.const_u64(x);
    let mut f = pb.func("main", 0);
    let (lx, ret) = (f.local(0), f.ret());
    f.emit_t(Op::LoadConst, lx, cx);
    f.line(line);
    f.emit(op, ret, lx, 0u16);
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
    f.end();
    Arc::new(pb.finish().expect("unop program"))
}

/// `main` (fn 0) returns the sum of `0..n`; with `via_call`, each addition is a call of fn 1 `add(a, b)`.
pub fn sum_loop(n: i16, via_call: bool) -> Arc<Program> {
    let mut pb = ProgramBuilder::new(API);
    let main = pb.declare("main", 0);
    let add = pb.declare("add", 2);
    {
        let mut f = pb.body(main);
        let (i, sum, lim, one, cond) = (f.local(0), f.local(1), f.local(2), f.local(3), f.local(4));
        let callee = f.of(add);
        f.emit(Op::LoadSmall, i, 0u16, 0u16);
        f.emit(Op::LoadSmall, sum, 0u16, 0u16);
        f.emit(Op::LoadSmall, lim, n as u16, 0u16);
        f.emit(Op::LoadSmall, one, 1u16, 0u16);
        let (top, done) = (f.label(), f.label());
        f.bind(top);
        f.emit(Op::LtI, cond, i, lim);
        f.jump(Op::JmpIfNot, cond, done);
        if via_call {
            f.emit(Op::Copy, callee.param(0), sum, 0u16);
            f.emit(Op::Copy, callee.param(1), i, 0u16);
            f.call(add);
            f.emit(Op::Copy, sum, callee.ret(), 0u16);
        } else {
            f.emit(Op::AddI, sum, sum, i);
        }
        f.emit(Op::AddI, i, i, one);
        f.jump(Op::Jmp, 0u16, top);
        f.bind(done);
        let ret = f.ret();
        f.emit(Op::Copy, ret, sum, 0u16);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    {
        let mut f = pb.body(add);
        let (ret, a, b) = (f.ret(), f.param(0), f.param(1));
        f.emit(Op::AddI, ret, a, b);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    Arc::new(pb.finish().expect("sum_loop program"))
}

/// Host fn ids of [`scenario`]'s promise `get(k: int) -> int` and command `log(n: int)`.
pub const SC_GET: u16 = 20;
pub const SC_LOG: u16 = 7;

/// The payload of both of [`scenario`]'s events.
pub fn sc_event(n: i64) -> WireValue {
    WireValue::Struct {
        name: "Ping".into(),
        fields: vec![("n".into(), WireValue::Int(n))],
    }
}

/// An event-driven program with one global `g: int` (fn 0 initialises it to 0):
/// - `Ping { n }`: spins `spin` iterations (a handler that runs across ticks under little fuel), holds a
///   heap string, `g += await get(n)` (a parked handler and a pending promise; an error leaves `g`), `log(g)`;
/// - `Pong { n }`: `g += 1`, `log(g)`, then faults `DivByZero` when `n == 0` (an aborted segment).
pub fn scenario(spin: i16) -> Arc<Program> {
    let mut pb = ProgramBuilder::new(API);
    let int = pb.ty(TypeDesc::Int);
    let unit = pb.ty(TypeDesc::Unit);
    let ping = pb.ty(TypeDesc::Struct {
        name: "Ping".into(),
        fields: vec![("n".into(), int)],
    });
    pb.ty(TypeDesc::Enum {
        name: quest_vm::types::ERROR_ENUM_NAME.into(),
        variants: ["Lost", "NotFound", "Timeout", "WrongType"]
            .iter()
            .map(|v| (v.to_string(), vec![]))
            .collect(),
    });
    let prom = pb.ty(TypeDesc::Promise(int));
    {
        let mut f = pb.func("init0", 0);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    let g = SlotRef::Global(pb.global("g", int));
    let get = pb.host_site(SC_GET, HostFnKind::Promise, vec![int], prom);
    let log = pb.host_site(SC_LOG, HostFnKind::Command, vec![int], unit);
    let text = pb.string("held across the await");
    let log_g = |f: &mut FnBuilder, tmp: SlotRef| {
        f.emit(Op::Copy, tmp, g, 0u16);
        f.emit(Op::CallHost, log, tmp, tmp);
    };

    let on_ping = pb.declare("on_ping", 1);
    {
        let mut f = pb.body(on_ping);
        let ev = f.param(0);
        let (k, p, val, err, s, tmp) = (
            f.local(0),
            f.local(1),
            f.local(2),
            f.local(3),
            f.local(4),
            f.local(5),
        );
        f.mark_ref(ev);
        f.mark_ref(err);
        f.mark_ref(s);
        f.emit(Op::GetField, k, ev, 0u16);
        let (i, lim, one, cond) = (f.local(6), f.local(7), f.local(8), f.local(9));
        f.emit(Op::LoadSmall, i, 0u16, 0u16);
        f.emit(Op::LoadSmall, lim, spin as u16, 0u16);
        f.emit(Op::LoadSmall, one, 1u16, 0u16);
        let (top, done) = (f.label(), f.label());
        f.bind(top);
        f.emit(Op::LtI, cond, i, lim);
        f.jump(Op::JmpIfNot, cond, done);
        f.emit(Op::AddI, i, i, one);
        f.jump(Op::Jmp, 0u16, top);
        f.bind(done);
        f.emit_t(Op::LoadStr, s, text);
        f.emit(Op::CallHost, get, k, p);
        f.emit(Op::Await, val, p, int);
        f.emit(Op::AddI, g, g, val);
        log_g(&mut f, tmp);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    pb.handler("Ping", ping, on_ping);

    let on_pong = pb.declare("on_pong", 1);
    {
        let mut f = pb.body(on_pong);
        let ev = f.param(0);
        let (k, one, tmp, cond, zero, out) = (
            f.local(0),
            f.local(1),
            f.local(2),
            f.local(3),
            f.local(4),
            f.local(5),
        );
        f.mark_ref(ev);
        f.emit(Op::GetField, k, ev, 0u16);
        f.emit(Op::LoadSmall, one, 1u16, 0u16);
        f.emit(Op::AddI, g, g, one);
        log_g(&mut f, tmp);
        f.emit(Op::LoadSmall, zero, 0u16, 0u16);
        f.emit(Op::EqI, cond, k, zero);
        let skip = f.label();
        f.jump(Op::JmpIfNot, cond, skip);
        f.emit(Op::DivI, out, zero, zero);
        f.bind(skip);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    pb.handler("Pong", ping, on_pong);
    Arc::new(pb.finish().expect("scenario program"))
}
