//! The scheduler: tasks, abortable segments, the event queue, buffered host commands (design D7).

mod common;

use std::sync::Arc;

use common::*;
use quest_vm::asm::{FnBuilder, ProgramBuilder, SlotRef};
use quest_vm::consts::WORK_WORDS_MAX;
use quest_vm::host::{AbortCause, DropReason, HostFnKind, Report};
use quest_vm::program::Program;
use quest_vm::types::{TypeDesc, TypeId};
use quest_vm::vm::{Delivery, Status, Vm, VmConfig};
use quest_vm::wire::WireValue;
use quest_vm::{Builtin, FaultKind, Op};

/// Ticks with `fuel` until the instance is no longer busy; the number of ticks.
fn run_idle(vm: &mut Vm, host: &mut RecordingHost, fuel: u64) -> u32 {
    for n in 1..=100_000 {
        let r = vm.tick(u64::from(n), fuel, host);
        if r.status != Status::Busy {
            return n;
        }
    }
    panic!("the instance never went idle");
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

/// `site(text)` for a command or print: the string goes through local `tmp` (a heap kind) and is dropped.
fn say(f: &mut FnBuilder, pb_str: u32, site: u16, tmp: SlotRef) {
    f.emit_t(Op::LoadStr, tmp, pb_str);
    f.emit(Op::CallHost, site, tmp, tmp);
    f.emit(Op::Drop, tmp, 0u16, 0u16);
}

fn print(f: &mut FnBuilder, pb_str: u32, tmp: SlotRef) {
    f.emit_t(Op::LoadStr, tmp, pb_str);
    f.emit(Op::CallBuiltin, Builtin::Print as u16, tmp, tmp);
    f.emit(Op::Drop, tmp, 0u16, 0u16);
}

fn ret(f: &mut FnBuilder) {
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
}

/// The common types: `int`, `str`, `unit` and the event `Ping { n: int }`.
struct Tys {
    int: TypeId,
    str: TypeId,
    unit: TypeId,
    ping: TypeId,
}

fn tys(pb: &mut ProgramBuilder) -> Tys {
    let int = pb.ty(TypeDesc::Int);
    let str = pb.ty(TypeDesc::Str);
    let unit = pb.ty(TypeDesc::Unit);
    let ping = pb.ty(TypeDesc::Struct {
        name: "Ping".into(),
        fields: vec![("n".into(), int)],
    });
    Tys {
        int,
        str,
        unit,
        ping,
    }
}

fn ping(n: i64) -> WireValue {
    WireValue::Struct {
        name: "Ping".into(),
        fields: vec![("n".into(), WireValue::Int(n))],
    }
}

/// A `Ping` handler that spins `spin_n` iterations, then logs `(n, tag)` (host fn 7), `n` from its event.
fn ping_logger(
    pb: &mut ProgramBuilder,
    t: &Tys,
    name: &str,
    spin_n: i16,
    tag: i16,
) -> quest_vm::asm::FnRef {
    let log = pb.host_site(7, HostFnKind::Command, vec![t.int, t.int], t.unit);
    let h = pb.declare(name, 1);
    let mut f = pb.body(h);
    let ev = f.param(0);
    f.mark_ref(ev);
    spin(&mut f, 2, spin_n);
    let (n, k) = (f.local(0), f.local(1));
    f.emit(Op::GetField, n, ev, 0u16);
    f.emit(Op::LoadSmall, k, tag as u16, 0u16);
    f.emit(Op::CallHost, log, n, n);
    ret(&mut f);
    f.end();
    h
}

fn logged(host: &RecordingHost) -> Vec<(i64, i64)> {
    host.commands
        .iter()
        .map(|(f, args)| {
            assert_eq!(*f, 7);
            match args.as_slice() {
                [WireValue::Int(n), WireValue::Int(k)] => (*n, *k),
                other => panic!("unexpected log args {other:?}"),
            }
        })
        .collect()
}

#[test]
fn init_runs_once_in_order_under_fuel() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let (sa, sb) = (pb.string("init a"), pb.string("init b"));
    let a = pb.global("a", t.int);
    let b = pb.global("b", t.int);
    let ia = pb.declare("init_a", 0);
    let ib = pb.declare("init_b", 0);
    {
        let mut f = pb.body(ia);
        let tmp = f.local(4);
        f.mark_ref(tmp);
        print(&mut f, sa, tmp);
        spin(&mut f, 0, 20);
        let r = f.ret();
        f.emit(Op::LoadSmall, r, 10u16, 0u16);
        ret(&mut f);
        f.end();
    }
    {
        // b = a + 1: wrong when b's initialiser runs before a's
        let mut f = pb.body(ib);
        let (tmp, one, r) = (f.local(0), f.local(1), f.ret());
        f.mark_ref(tmp);
        print(&mut f, sb, tmp);
        f.emit(Op::LoadSmall, one, 1u16, 0u16);
        f.emit(Op::AddI, r, SlotRef::Global(a), one);
        ret(&mut f);
        f.end();
    }
    pb.set_global_init(a, ia);
    pb.set_global_init(b, ib);
    let p = Arc::new(pb.finish().expect("program"));

    let mut whole = vm(&p);
    let mut wh = RecordingHost::default();
    assert_eq!(run_idle(&mut whole, &mut wh, u64::MAX), 1);

    let mut sliced = vm(&p);
    let mut sh = RecordingHost::default();
    let ticks = run_idle(&mut sliced, &mut sh, 3);
    assert!(ticks > 10, "the init spans ticks ({ticks})");
    for (v, h) in [(&whole, &wh), (&sliced, &sh)] {
        assert_eq!(v.slot(p.globals[0].slot), 10);
        assert_eq!(v.slot(p.globals[1].slot), 11);
        assert_eq!(h.prints, ["init a", "init b"]);
        assert!(h.reports.is_empty());
        assert_eq!(v.status(), Status::Idle);
        assert!(v.is_idle());
    }
    assert_eq!(sliced.script_bytes(), whole.script_bytes());
    // nothing runs a second time
    sliced.tick(u64::MAX, 1_000, &mut sh);
    assert_eq!(sh.prints.len(), 2);
}

#[test]
fn commands_reach_the_host_only_at_commit() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let (s1, s2) = (pb.string("hello"), pb.string("bye"));
    let say_site = pb.host_site(3, HostFnKind::Command, vec![t.str], t.unit);
    let done = pb.global("done", t.int);
    let init = pb.declare("init_done", 0);
    {
        let mut f = pb.body(init);
        ret(&mut f);
        f.end();
    }
    pb.set_global_init(done, init);
    let h = pb.declare("on_ping", 1);
    {
        let mut f = pb.body(h);
        let (ev, tmp) = (f.param(0), f.local(0));
        f.mark_ref(ev);
        f.mark_ref(tmp);
        say(&mut f, s1, say_site, tmp);
        spin(&mut f, 1, 40);
        say(&mut f, s2, say_site, tmp);
        f.emit(Op::LoadSmall, SlotRef::Global(done), 1u16, 0u16);
        ret(&mut f);
        f.end();
    }
    pb.handler("Ping", t.ping, h);
    let p = Arc::new(pb.finish().expect("program"));
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    assert_eq!(vm.deliver("Ping", &ping(1)), Delivery::Accepted);
    let mut silent = 0;
    for n in 0..1_000u64 {
        vm.tick(n, 20, &mut host);
        let ended = vm.slot(p.globals[0].slot) == 1;
        assert_eq!(ended, !host.commands.is_empty(), "tick {n}");
        if !ended {
            silent += 1;
            continue;
        }
        let texts: Vec<_> = host.commands.iter().map(|(f, a)| (*f, a.clone())).collect();
        assert_eq!(
            texts,
            [
                (3, vec![WireValue::Str("hello".into())]),
                (3, vec![WireValue::Str("bye".into())]),
            ]
        );
        break;
    }
    assert!(silent >= 4, "the handler spans 5 ticks ({silent} silent)");
}

#[test]
fn fault_undoes_the_segment() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let s = pb.string("never");
    let say_site = pb.host_site(3, HostFnKind::Command, vec![t.str], t.unit);
    let g = pb.global("g", t.int);
    let init = pb.declare("init_g", 0);
    {
        let mut f = pb.body(init);
        let r = f.ret();
        f.emit(Op::LoadSmall, r, 1u16, 0u16);
        ret(&mut f);
        f.end();
    }
    pb.set_global_init(g, init);
    pb.set_file("main.qs");
    let h = pb.declare("on_ping", 1);
    {
        let mut f = pb.body(h);
        let (ev, tmp, zero, out) = (f.param(0), f.local(0), f.local(1), f.local(2));
        f.mark_ref(ev);
        f.mark_ref(tmp);
        f.line(5);
        f.emit(Op::LoadSmall, SlotRef::Global(g), 5u16, 0u16);
        say(&mut f, s, say_site, tmp);
        f.emit(Op::LoadSmall, zero, 0u16, 0u16);
        f.line(7);
        f.emit(Op::DivI, out, zero, zero);
        ret(&mut f);
        f.end();
    }
    pb.handler("Ping", t.ping, h);
    let p = Arc::new(pb.finish().expect("program"));
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    run_idle(&mut vm, &mut host, 1_000);
    let heap_before = vm.heap_used_words();
    assert_eq!(vm.deliver("Ping", &ping(1)), Delivery::Accepted);
    run_idle(&mut vm, &mut host, 1_000);
    assert_eq!(vm.slot(p.globals[0].slot), 1);
    assert!(host.commands.is_empty());
    assert_eq!(host.reports.len(), 1);
    match &host.reports[0] {
        Report::HandlerAborted {
            task,
            cause: AbortCause::Fault { kind, stack, .. },
        } => {
            assert_eq!(task, "on_ping");
            assert_eq!(*kind, FaultKind::DivByZero);
            assert_eq!(stack[0].line, 7);
            assert_eq!(stack[0].file, "main.qs");
        }
        other => panic!("unexpected report {other:?}"),
    }
    assert_eq!(vm.heap_used_words(), heap_before);
    assert_eq!(vm.status(), Status::Idle);
}

/// The program's error enum: `NotEnough(have: int)` (tag 0) and `Oops` (tag 1).
fn error_ty(pb: &mut ProgramBuilder, int: TypeId) -> TypeId {
    pb.ty(TypeDesc::Enum {
        name: quest_vm::types::ERROR_ENUM_NAME.into(),
        variants: vec![
            ("NotEnough".into(), vec![("have".into(), int)]),
            ("Oops".into(), vec![]),
        ],
    })
}

#[test]
fn escaping_error_reports_name_data_trace() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    error_ty(&mut pb, t.int);
    let shape = pb.shape(1, 0);
    pb.set_file("main.qs");
    let take = pb.declare("take", 0);
    let mid = pb.declare("mid", 0);
    let h = pb.declare("on_ping", 1);
    {
        // raise NotEnough(have=3)
        let mut f = pb.body(take);
        let (err, three) = (f.err(), f.local(0));
        f.mark_ref(err);
        f.emit(Op::NewObj, err, shape, 0u16);
        f.emit(Op::LoadSmall, three, 3u16, 0u16);
        f.emit(Op::SetField, err, 0u16, three);
        f.line(20);
        f.emit(Op::TracePush, 1u16, 0u16, 0u16);
        ret(&mut f);
        f.end();
    }
    // `callee()?` in `caller`, the `?` on `line`
    let propagate = |pb: &mut ProgramBuilder, caller, callee, line| {
        let mut f = pb.body(caller);
        let own = f.err();
        f.mark_ref(own);
        if caller == h {
            let ev = f.param(0);
            f.mark_ref(ev);
        }
        let cerr = f.of(callee).err();
        f.call(callee);
        let ok = f.label();
        f.jump(Op::JmpIfNot, cerr, ok);
        f.emit(Op::Move, own, cerr, 0u16);
        f.line(line);
        f.emit(Op::TracePush, 0u16, 0u16, 0u16);
        ret(&mut f);
        f.bind(ok);
        ret(&mut f);
        f.end();
    };
    propagate(&mut pb, mid, take, 21);
    propagate(&mut pb, h, mid, 22);
    pb.handler("Ping", t.ping, h);
    let p = Arc::new(pb.finish().expect("program"));
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    let heap_before = {
        run_idle(&mut vm, &mut host, 1_000);
        vm.heap_used_words()
    };
    vm.deliver("Ping", &ping(1));
    run_idle(&mut vm, &mut host, 1_000);
    match host.reports.as_slice() {
        [
            Report::HandlerAborted {
                task,
                cause:
                    AbortCause::Error {
                        name,
                        data,
                        trace,
                        trace_dropped,
                    },
            },
        ] => {
            assert_eq!(task, "on_ping");
            assert_eq!(name, "NotEnough");
            assert_eq!(
                *data,
                WireValue::Variant {
                    name: "NotEnough".into(),
                    fields: vec![("have".into(), WireValue::Int(3))],
                }
            );
            let lines: Vec<u32> = trace.iter().map(|l| l.line).collect();
            assert_eq!(lines, [20, 21, 22]);
            assert!(trace.iter().all(|l| l.file == "main.qs"));
            assert_eq!(*trace_dropped, 0);
        }
        other => panic!("unexpected reports {other:?}"),
    }
    assert_eq!(vm.heap_used_words(), heap_before);
}

#[test]
fn trace_overflow_is_counted() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    error_ty(&mut pb, t.int);
    let h = pb.declare("on_ping", 1);
    {
        let mut f = pb.body(h);
        let (ev, err) = (f.param(0), f.err());
        f.mark_ref(ev);
        f.mark_ref(err);
        // `Oops`: tag 1 as an immediate, (1 << 1) | 1
        f.emit(Op::LoadSmall, err, 3u16, 0u16);
        for i in 0..20u16 {
            f.line(100 + u32::from(i));
            f.emit(Op::TracePush, u16::from(i == 0), 0u16, 0u16);
        }
        ret(&mut f);
        f.end();
    }
    pb.handler("Ping", t.ping, h);
    let p = Arc::new(pb.finish().expect("program"));
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    vm.deliver("Ping", &ping(1));
    run_idle(&mut vm, &mut host, 1_000);
    match host.reports.as_slice() {
        [
            Report::HandlerAborted {
                cause:
                    AbortCause::Error {
                        name,
                        data,
                        trace,
                        trace_dropped,
                    },
                ..
            },
        ] => {
            assert_eq!(name, "Oops");
            assert_eq!(
                *data,
                WireValue::Variant {
                    name: "Oops".into(),
                    fields: vec![],
                }
            );
            let lines: Vec<u32> = trace.iter().map(|l| l.line).collect();
            assert_eq!(lines, (100..116).collect::<Vec<u32>>());
            assert_eq!(*trace_dropped, 4);
        }
        other => panic!("unexpected reports {other:?}"),
    }
}

/// One `Ping` handler that spins `spin_n` iterations and then logs `(n, 0)`.
fn logger_program(spin_n: i16) -> Arc<Program> {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let h = ping_logger(&mut pb, &t, "on_ping", spin_n, 0);
    pb.handler("Ping", t.ping, h);
    Arc::new(pb.finish().expect("program"))
}

#[test]
fn queue_warns_only_when_busy() {
    let p = logger_program(30);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    assert_eq!(vm.deliver("Ping", &ping(1)), Delivery::Accepted);
    assert_eq!(vm.deliver("Ping", &ping(2)), Delivery::Accepted);
    assert_eq!(vm.tick(1, 20, &mut host).status, Status::Busy);
    assert_eq!(vm.deliver("Ping", &ping(3)), Delivery::Queued);
    run_idle(&mut vm, &mut host, 20);
    assert_eq!(logged(&host), [(1, 0), (2, 0), (3, 0)]);
    assert!(vm.is_idle());
    assert_eq!(vm.deliver("Ping", &ping(4)), Delivery::Accepted);
}

#[test]
fn queue_overflow_drops_the_newest() {
    let p = logger_program(30);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    assert_eq!(vm.deliver("Ping", &ping(0)), Delivery::Accepted);
    vm.tick(1, 20, &mut host);
    for n in 1..=4 {
        assert_eq!(vm.deliver("Ping", &ping(n)), Delivery::Queued);
    }
    let before = vm.mem_bytes().to_vec();
    assert_eq!(
        vm.deliver("Ping", &ping(5)),
        Delivery::Dropped(DropReason::QueueFull)
    );
    assert_eq!(vm.mem_bytes(), &before[..]);
    run_idle(&mut vm, &mut host, 20);
    assert_eq!(logged(&host), [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0)]);
}

#[test]
fn two_handlers_of_one_event_run_in_table_order() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let pong = pb.ty(TypeDesc::Struct {
        name: "Pong".into(),
        fields: vec![("n".into(), t.int)],
    });
    let a = ping_logger(&mut pb, &t, "a", 5, 1);
    let c = ping_logger(&mut pb, &t, "c", 5, 3);
    let b = ping_logger(&mut pb, &t, "b", 5, 2);
    pb.handler("Ping", t.ping, a);
    pb.handler("Pong", pong, c);
    pb.handler("Ping", t.ping, b);
    let p = Arc::new(pb.finish().expect("program"));
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    assert_eq!(vm.deliver("Ping", &ping(10)), Delivery::Accepted);
    let pong_ev = WireValue::Struct {
        name: "Pong".into(),
        fields: vec![("n".into(), WireValue::Int(20))],
    };
    assert_eq!(vm.deliver("Pong", &pong_ev), Delivery::Accepted);
    run_idle(&mut vm, &mut host, 7);
    assert_eq!(logged(&host), [(10, 1), (10, 2), (20, 3)]);
    assert!(host.reports.is_empty());
}

#[test]
fn bad_deliveries_are_dropped() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    // a user struct: its name is not an event of the host's
    pb.ty(TypeDesc::Struct {
        name: "Ledger".into(),
        fields: vec![],
    });
    let big = pb.ty(TypeDesc::Struct {
        name: "Big".into(),
        fields: vec![("s".into(), t.str)],
    });
    let h = ping_logger(&mut pb, &t, "on_ping", 1, 0);
    pb.handler("Ping", t.ping, h);
    let hb = pb.declare("on_big", 1);
    {
        let mut f = pb.body(hb);
        let ev = f.param(0);
        f.mark_ref(ev);
        ret(&mut f);
        f.end();
    }
    pb.handler("Big", big, hb);
    let p = Arc::new(pb.finish().expect("program"));
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    run_idle(&mut vm, &mut host, 1_000);
    let bytes = vm.mem_bytes().to_vec();
    let cases = [
        ("Nope", ping(1), DropReason::UnknownEvent),
        (
            "Ledger",
            WireValue::Struct {
                name: "Ledger".into(),
                fields: vec![],
            },
            DropReason::UnknownEvent,
        ),
        (
            "Quiet",
            WireValue::Struct {
                name: "Quiet".into(),
                fields: vec![],
            },
            DropReason::NoHandler,
        ),
        (
            "Ping",
            WireValue::Struct {
                name: "Ping".into(),
                fields: vec![],
            },
            DropReason::WrongShape,
        ),
        (
            "Big",
            WireValue::Struct {
                name: "Big".into(),
                fields: vec![("s".into(), WireValue::Str("x".repeat(10_000)))],
            },
            DropReason::InboxFull,
        ),
    ];
    for (event, payload, reason) in cases {
        assert_eq!(
            vm.deliver(event, &payload),
            Delivery::Dropped(reason),
            "{event}"
        );
        assert_eq!(vm.mem_bytes(), &bytes[..], "{event} changed the state");
    }
    assert!(vm.is_idle());
}

#[test]
fn tick_with_zero_fuel_changes_nothing() {
    let p = logger_program(30);
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    vm.deliver("Ping", &ping(1));
    for _ in 0..3 {
        let bytes = vm.mem_bytes().to_vec();
        let r = vm.tick(1_000, 0, &mut host);
        assert_eq!(r.fuel_used, 0);
        assert_eq!(r.status, Status::Busy);
        assert_eq!(vm.mem_bytes(), &bytes[..]);
        // and a step forward, so the next round is mid-handler
        vm.tick(1, 20, &mut host);
    }
    assert!(host.commands.is_empty() && host.reports.is_empty());
}

#[test]
fn now_going_backwards_is_clamped() {
    let p = logger_program(30);
    let run = |nows: &[u64]| {
        let mut vm = vm(&p);
        let mut host = RecordingHost::default();
        vm.deliver("Ping", &ping(1));
        for &now in nows {
            vm.tick(now, 20, &mut host);
        }
        vm.mem_bytes().to_vec()
    };
    let forward = run(&[100, 100, 100]);
    assert_eq!(run(&[100, 50, 0]), forward);
    assert_ne!(run(&[100, 101, 102]), forward, "now is part of the state");
}

#[test]
fn print_is_buffered_then_flushed() {
    let mut pb = ProgramBuilder::new(API);
    let (sa, sb) = (pb.string("first"), pb.string("second"));
    let m = pb.declare("main", 0);
    {
        let mut f = pb.body(m);
        let tmp = f.local(0);
        f.mark_ref(tmp);
        print(&mut f, sa, tmp);
        spin(&mut f, 1, 40);
        print(&mut f, sb, tmp);
        ret(&mut f);
        f.end();
    }
    pb.main(m);
    let p = Arc::new(pb.finish().expect("program"));
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    assert!(vm.start_main());
    assert!(!vm.start_main(), "main starts once");
    let mut ticks = 0;
    while vm.tick(ticks, 20, &mut host).status == Status::Busy {
        ticks += 1;
        if host.prints.is_empty() {
            continue;
        }
        assert!(ticks > 4, "the prints came before main ended");
    }
    assert_eq!(host.prints, ["first", "second"]);
    assert!(host.reports.is_empty());
}

#[test]
fn command_buffer_full_faults() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let long = pb.string(&"y".repeat(1_000));
    let say_site = pb.host_site(3, HostFnKind::Command, vec![t.str], t.unit);
    let h = pb.declare("on_ping", 1);
    {
        let mut f = pb.body(h);
        let (ev, tmp) = (f.param(0), f.local(0));
        f.mark_ref(ev);
        f.mark_ref(tmp);
        for _ in 0..20 {
            say(&mut f, long, say_site, tmp);
        }
        ret(&mut f);
        f.end();
    }
    pb.handler("Ping", t.ping, h);
    let p = Arc::new(pb.finish().expect("program"));
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    vm.deliver("Ping", &ping(1));
    run_idle(&mut vm, &mut host, 10_000);
    assert!(host.commands.is_empty());
    match host.reports.as_slice() {
        [
            Report::HandlerAborted {
                cause: AbortCause::Fault { kind, .. },
                ..
            },
        ] => assert_eq!(*kind, FaultKind::TooManyCommands),
        other => panic!("unexpected reports {other:?}"),
    }
    assert_eq!(vm.status(), Status::Idle);
}

#[test]
fn init_fault_marks_the_instance_failed() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let g = pb.global("g", t.int);
    let init = pb.declare("init_g", 0);
    {
        let mut f = pb.body(init);
        let (zero, r) = (f.local(0), f.ret());
        f.emit(Op::LoadSmall, zero, 0u16, 0u16);
        f.line(3);
        f.emit(Op::DivI, r, zero, zero);
        ret(&mut f);
        f.end();
    }
    pb.set_global_init(g, init);
    let h = ping_logger(&mut pb, &t, "on_ping", 1, 0);
    pb.handler("Ping", t.ping, h);
    let p = Arc::new(pb.finish().expect("program"));
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    assert_eq!(vm.deliver("Ping", &ping(1)), Delivery::Accepted);
    assert_eq!(vm.tick(1, 1_000, &mut host).status, Status::Failed);
    assert_eq!(vm.status(), Status::Failed);
    match host.reports.as_slice() {
        [
            Report::InitFailed {
                global,
                cause: AbortCause::Fault { kind, stack, .. },
            },
        ] => {
            assert_eq!(global, "g");
            assert_eq!(*kind, FaultKind::DivByZero);
            assert_eq!(stack[0].line, 3);
        }
        other => panic!("unexpected reports {other:?}"),
    }
    assert_eq!(
        vm.deliver("Ping", &ping(2)),
        Delivery::Dropped(DropReason::InstanceFailed)
    );
    // the queued event never runs
    assert_eq!(vm.tick(2, 1_000, &mut host).status, Status::Failed);
    assert!(host.commands.is_empty());
}

#[test]
fn rollback_mid_handler_is_bit_exact() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let s = pb.string("step");
    let say_site = pb.host_site(3, HostFnKind::Command, vec![t.str], t.unit);
    let h = pb.declare("on_ping", 1);
    {
        let mut f = pb.body(h);
        let (ev, tmp) = (f.param(0), f.local(0));
        f.mark_ref(ev);
        f.mark_ref(tmp);
        for _ in 0..3 {
            say(&mut f, s, say_site, tmp);
            spin(&mut f, 1, 25);
        }
        ret(&mut f);
        f.end();
    }
    pb.handler("Ping", t.ping, h);
    let p = Arc::new(pb.finish().expect("program"));
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    vm.deliver("Ping", &ping(1));
    for n in 0..3 {
        assert_eq!(vm.tick(n, 30, &mut host).status, Status::Busy);
    }
    let cp = vm.checkpoint();
    let at_cp = vm.mem_bytes().to_vec();
    let mut first = Vec::new();
    let mut n = 3;
    while vm.tick(n, 30, &mut host).status == Status::Busy {
        first.push(vm.mem_bytes().to_vec());
        n += 1;
    }
    first.push(vm.mem_bytes().to_vec());
    assert!(n >= 10, "the handler spans 10 ticks ({n})");
    let calls = host.commands.clone();
    assert_eq!(calls.len(), 3);

    vm.rollback_to(cp).expect("rollback");
    assert_eq!(vm.mem_bytes(), &at_cp[..]);
    let mut again = RecordingHost::default();
    let mut second = Vec::new();
    let mut n = 3;
    while vm.tick(n, 30, &mut again).status == Status::Busy {
        second.push(vm.mem_bytes().to_vec());
        n += 1;
    }
    second.push(vm.mem_bytes().to_vec());
    assert!(first == second, "the replay differs");
    assert_eq!(again.commands, calls);
}

/// Regression: an escaping error whose data is too large to convert for the report used to fail the
/// whole instance. It aborts the handler with that fault, and the block keeps running.
#[test]
fn oversized_error_data_aborts_the_handler() {
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    pb.ty(TypeDesc::Enum {
        name: quest_vm::types::ERROR_ENUM_NAME.into(),
        variants: vec![("Big".into(), vec![("blob".into(), t.str)])],
    });
    let shape = pb.shape(1, 1);
    // more bytes than one walk may visit words
    let blob = pb.string(&"z".repeat(WORK_WORDS_MAX as usize * 8 + 8));
    let big = pb.ty(TypeDesc::Struct {
        name: "Big".into(),
        fields: vec![],
    });
    let hb = pb.declare("on_big", 1);
    {
        // raise Big(blob=<the string>)
        let mut f = pb.body(hb);
        let (ev, err, tmp) = (f.param(0), f.err(), f.local(0));
        f.mark_ref(ev);
        f.mark_ref(err);
        f.mark_ref(tmp);
        f.emit(Op::NewObj, err, shape, 0u16);
        f.emit_t(Op::LoadStr, tmp, blob);
        f.emit(Op::SetField, err, 0u16, tmp);
        f.line(30);
        f.emit(Op::TracePush, 1u16, 0u16, 0u16);
        ret(&mut f);
        f.end();
    }
    pb.handler("Big", big, hb);
    let h = ping_logger(&mut pb, &t, "on_ping", 1, 0);
    pb.handler("Ping", t.ping, h);
    let p = Arc::new(pb.finish().expect("program"));
    let cfg = VmConfig {
        mem_bytes: 4 << 20,
        ..cfg()
    };
    let mut vm = Vm::new(p.clone(), cfg).expect("loads");
    let mut host = RecordingHost::default();
    let empty = WireValue::Struct {
        name: "Big".into(),
        fields: vec![],
    };
    assert_eq!(vm.deliver("Big", &empty), Delivery::Accepted);
    run_idle(&mut vm, &mut host, 100_000);
    match host.reports.as_slice() {
        [
            Report::HandlerAborted {
                task,
                cause:
                    AbortCause::Fault {
                        kind,
                        message,
                        stack,
                    },
            },
        ] => {
            assert_eq!(task, "on_big");
            assert_eq!(*kind, FaultKind::ValueTooLarge);
            assert_eq!(message, "error Big carries data too large to report");
            let lines: Vec<u32> = stack.iter().map(|l| l.line).collect();
            assert_eq!(lines, [30]);
        }
        other => panic!("unexpected reports {other:?}"),
    }
    assert_eq!(vm.status(), Status::Idle);
    assert_eq!(vm.deliver("Ping", &ping(5)), Delivery::Accepted);
    run_idle(&mut vm, &mut host, 1_000);
    assert_eq!(logged(&host), [(5, 0)]);
}
