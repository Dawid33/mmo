//! The scheduler: tasks, abortable segments, the event queue, buffered host commands (design D7).

mod common;

use std::sync::Arc;

use common::*;
use quest_vm::asm::{FnBuilder, ProgramBuilder, SlotRef};
use quest_vm::consts::MEM_BYTES_DEFAULT;
use quest_vm::consts::{TIMERS_MAX, WORK_WORDS_MAX};
use quest_vm::fuel::{FUEL_AFTER, FUEL_CANCEL, FUEL_ENQUEUE};
use quest_vm::host::{AbortCause, DropReason, HostFnKind, Report};
use quest_vm::layout::{Layout, LoadError};
use quest_vm::migrate::MigrateError;
use quest_vm::program::{Anchor, PartInfo, Program, QuestInfo, RoleInfo};
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

// ---- script events, timers, roles, instance owners --------------------------------------------------

/// [`Tys`], the script event `Ring { n: int }` (event 0), its shape and the log site `log(n, tag)` (host
/// fn 7).
struct QTys {
    t: Tys,
    ring: TypeId,
    ring_event: u16,
    shape: u16,
    log: u16,
}

fn qtys(pb: &mut ProgramBuilder) -> QTys {
    let t = tys(pb);
    let ring = pb.ty(TypeDesc::Struct {
        name: "Ring".into(),
        fields: vec![("n".into(), t.int)],
    });
    let ring_event = pb.script_event("Ring", ring);
    let shape = pb.shape(1, 0);
    let log = pb.host_site(7, HostFnKind::Command, vec![t.int, t.int], t.unit);
    QTys {
        t,
        ring,
        ring_event,
        shape,
        log,
    }
}

fn ring(n: i64) -> WireValue {
    WireValue::Struct {
        name: "Ring".into(),
        fields: vec![("n".into(), WireValue::Int(n))],
    }
}

/// A handler of `event` (type `ty`) owned by `owner` in `stage`: logs `(e.n, tag)`.
#[allow(clippy::too_many_arguments)]
fn logger(
    pb: &mut ProgramBuilder,
    q: &QTys,
    name: &str,
    event: &str,
    ty: TypeId,
    owner: u16,
    stage: Option<u16>,
    tag: i16,
) {
    let h = pb.declare(name, 1);
    let mut f = pb.body(h);
    let ev = f.param(0);
    f.mark_ref(ev);
    let (n, k) = (f.local(0), f.local(1));
    f.emit(Op::GetField, n, ev, 0u16);
    f.emit(Op::LoadSmall, k, tag as u16, 0u16);
    f.emit(Op::CallHost, q.log, n, n);
    ret(&mut f);
    f.end();
    pb.owned_handler(event, ty, h, owner, stage);
}

/// `out = Ring { n: n + add }` (locals `scratch`, `scratch + 1` are used).
fn make_ring(f: &mut FnBuilder, q: &QTys, n: SlotRef, add: i16, out: SlotRef, scratch: u16) {
    let (k, sum) = (f.local(scratch), f.local(scratch + 1));
    f.emit(Op::LoadSmall, k, add as u16, 0u16);
    f.emit(Op::AddI, sum, n, k);
    f.emit(Op::NewObj, out, q.shape, 0u16);
    f.emit(Op::SetField, out, 0u16, sum);
}

/// A global `name: int` initialised to 0 by `zero`.
fn int_global(pb: &mut ProgramBuilder, q: &QTys, name: &str, zero: quest_vm::asm::FnRef) -> u16 {
    let g = pb.global(name, q.t.int);
    pb.set_global_init(g, zero);
    g.0
}

/// A fn returning 0: an initialiser.
fn zero_fn(pb: &mut ProgramBuilder) -> quest_vm::asm::FnRef {
    let z = pb.declare("zero", 0);
    let mut f = pb.body(z);
    let r = f.ret();
    f.emit(Op::LoadSmall, r, 0u16, 0u16);
    ret(&mut f);
    f.end();
    z
}

/// A fn of no parameters that does nothing.
fn noop_fn(pb: &mut ProgramBuilder, name: &str) -> quest_vm::asm::FnRef {
    let s = pb.declare(name, 0);
    let mut f = pb.body(s);
    ret(&mut f);
    f.end();
    s
}

/// A quest named `Q` with the stages `A`, `B`, globals `Q::stage` / `Q::participants`, the given roles and
/// `parts` parts anchored on role 0, started by `start`.
fn quest_info(
    pb: &mut ProgramBuilder,
    q: &QTys,
    zero: quest_vm::asm::FnRef,
    roles: Vec<RoleInfo>,
    parts: usize,
    start: quest_vm::asm::FnRef,
) -> QuestInfo {
    let stage = int_global(pb, q, "Q::stage", zero);
    let participants = int_global(pb, q, "Q::participants", zero);
    QuestInfo {
        name: "Q".into(),
        roles,
        stages: vec!["A".into(), "B".into()],
        stage_global: stage,
        participants_global: participants,
        home_role: 0,
        repeat: None,
        parts: (0..parts)
            .map(|i| PartInfo {
                name: format!("part{i}"),
                anchor: Anchor::On,
                role: 0,
                read_set: vec![stage],
            })
            .collect(),
        requires: vec![],
        start_fn: start.0,
        read_set: vec![],
    }
}

/// The timer program. Global `last` holds the latest timer id.
/// - `Ping { n }` (a handler of stage `A`): `last = after(n, Ring(n))` — a stage-scoped timer;
/// - `Pong { n }` (any stage): `last = after(n, Ring(n + 100))`, then faults `DivByZero` when `n == 0`;
/// - `Big`: `cancel(last)`; `Quiet`: cancels the stage's timers;
/// - `Ring { n }`: logs `(n, 1)`.
fn timers_program() -> Arc<Program> {
    let mut pb = ProgramBuilder::new(API);
    let q = qtys(&mut pb);
    let zero = zero_fn(&mut pb);
    let start = noop_fn(&mut pb, "$start::Q");
    let info = quest_info(&mut pb, &q, zero, vec![], 0, start);
    let last = SlotRef::Global(quest_vm::asm::GlobalRef(int_global(
        &mut pb, &q, "last", zero,
    )));
    for (name, event, add, stage) in [("set_a", "Ping", 0, Some(0)), ("set", "Pong", 100, None)] {
        let h = pb.declare(name, 1);
        let mut f = pb.body(h);
        let ev = f.param(0);
        f.mark_ref(ev);
        let (n, ticks, val, id) = (f.local(0), f.local(1), f.local(2), f.local(3));
        f.mark_ref(val);
        f.emit(Op::GetField, n, ev, 0u16);
        f.emit(Op::Copy, ticks, n, 0u16);
        make_ring(&mut f, &q, n, add, val, 4);
        f.emit(Op::After, id, ticks, q.ring_event);
        f.emit(Op::Copy, last, id, 0u16);
        f.emit(Op::Drop, val, 0u16, 0u16);
        if add == 100 {
            let (z, cond, out) = (f.local(6), f.local(7), f.local(8));
            f.emit(Op::LoadSmall, z, 0u16, 0u16);
            f.emit(Op::EqI, cond, n, z);
            let skip = f.label();
            f.jump(Op::JmpIfNot, cond, skip);
            f.emit(Op::DivI, out, z, z);
            f.bind(skip);
        }
        ret(&mut f);
        f.end();
        pb.owned_handler(event, q.t.ping, h, 0, stage);
    }
    let cancel = pb.declare("cancel", 1);
    {
        let mut f = pb.body(cancel);
        f.mark_ref(f.param(0));
        f.emit(Op::CancelTimer, last, 0u16, 0u16);
        ret(&mut f);
        f.end();
    }
    pb.handler("Big", q.t.ping, cancel);
    let leave = pb.declare("leave_stage", 1);
    {
        let mut f = pb.body(leave);
        f.mark_ref(f.param(0));
        f.emit(Op::CancelStageTimers, 0u16, 0u16, 0u16);
        ret(&mut f);
        f.end();
    }
    pb.handler("Quiet", q.t.ping, leave);
    logger(&mut pb, &q, "rang", "Ring", q.ring, 0, None, 1);
    pb.set_quest(info);
    Arc::new(pb.finish().expect("timers program"))
}

/// One tick at `now` with unlimited fuel.
fn at(vm: &mut Vm, host: &mut RecordingHost, now: u64) {
    vm.tick(now, u64::MAX, host);
}

fn fault_kinds(host: &RecordingHost) -> Vec<FaultKind> {
    host.reports
        .iter()
        .filter_map(|r| match r {
            Report::HandlerAborted {
                cause: AbortCause::Fault { kind, .. },
                ..
            } => Some(*kind),
            _ => None,
        })
        .collect()
}

/// A save's bytes before its memory: magic, two versions, the program hash, the memory length.
const SAVE_HEADER_BYTES: usize = 8 + 4 + 4 + 8 + 4;

/// The id of the `serial`-th timer of epoch 1.
fn timer_id(serial: u64) -> u64 {
    1 << 32 | serial
}

#[test]
fn script_event_is_delivered() {
    let mut pb = ProgramBuilder::new(API);
    let q = qtys(&mut pb);
    logger(&mut pb, &q, "rang", "Ring", q.ring, 0, None, 1);
    // Ping { n }: emit Ring(n + 10)
    let h = pb.declare("ping", 1);
    {
        let mut f = pb.body(h);
        let ev = f.param(0);
        f.mark_ref(ev);
        let (n, val) = (f.local(0), f.local(1));
        f.mark_ref(val);
        f.emit(Op::GetField, n, ev, 0u16);
        make_ring(&mut f, &q, n, 10, val, 2);
        f.emit(Op::Enqueue, q.ring_event, val, 0u16);
        f.emit(Op::Drop, val, 0u16, 0u16);
        ret(&mut f);
        f.end();
    }
    pb.handler("Ping", q.t.ping, h);
    let p = Arc::new(pb.finish().expect("program"));
    assert!(!EVENTS.contains(&"Ring"));

    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    // a host may deliver a script event
    assert_eq!(vm.deliver("Ring", &ring(7)), Delivery::Accepted);
    assert_eq!(
        vm.deliver("Nope", &ring(7)),
        Delivery::Dropped(DropReason::UnknownEvent)
    );
    assert_eq!(
        vm.deliver("Ring", &WireValue::Int(7)),
        Delivery::Dropped(DropReason::WrongShape)
    );
    at(&mut vm, &mut host, 1);
    assert_eq!(logged(&host), [(7, 1)]);
    // and a handler may queue one for its own instance: it runs in the same tick, after the commit
    assert_eq!(vm.deliver("Ping", &ping(1)), Delivery::Accepted);
    at(&mut vm, &mut host, 2);
    assert_eq!(logged(&host), [(7, 1), (11, 1)]);
    assert!(host.reports.is_empty(), "{:?}", host.reports);
    assert!(vm.is_idle());
}

#[test]
fn enqueue_overflows_like_host_events() {
    let mut pb = ProgramBuilder::new(API);
    let q = qtys(&mut pb);
    logger(&mut pb, &q, "rang", "Ring", q.ring, 0, None, 1);
    // Pong { n }: emit Ring(n + i) for i in 0..5, then fault when n == 0
    let h = pb.declare("pong", 1);
    {
        let mut f = pb.body(h);
        let ev = f.param(0);
        f.mark_ref(ev);
        let (n, val) = (f.local(0), f.local(1));
        f.mark_ref(val);
        f.emit(Op::GetField, n, ev, 0u16);
        for i in 0..5 {
            make_ring(&mut f, &q, n, i, val, 2);
            f.emit(Op::Enqueue, q.ring_event, val, 0u16);
            f.emit(Op::Drop, val, 0u16, 0u16);
        }
        let (z, cond, out) = (f.local(4), f.local(5), f.local(6));
        f.emit(Op::LoadSmall, z, 0u16, 0u16);
        f.emit(Op::EqI, cond, n, z);
        let skip = f.label();
        f.jump(Op::JmpIfNot, cond, skip);
        f.emit(Op::DivI, out, z, z);
        f.bind(skip);
        ret(&mut f);
        f.end();
    }
    pb.handler("Pong", q.t.ping, h);
    let p = Arc::new(pb.finish().expect("program"));

    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    vm.deliver("Pong", &ping(20));
    at(&mut vm, &mut host, 1);
    // the queue holds four: the fifth is dropped, as a host event would be
    assert_eq!(logged(&host), [(20, 1), (21, 1), (22, 1), (23, 1)]);
    assert_eq!(
        host.reports,
        [Report::EventDropped {
            event: "Ring".into(),
            reason: DropReason::QueueFull,
        }]
    );
    // an aborted segment queues nothing
    let mut host = RecordingHost::default();
    vm.deliver("Pong", &ping(0));
    at(&mut vm, &mut host, 2);
    assert!(logged(&host).is_empty());
    assert_eq!(fault_kinds(&host), [FaultKind::DivByZero]);
    assert!(vm.is_idle());
}

/// A program with one part: `Ping` is handled by a home handler (tag 0) and a part handler (tag 1); the
/// script event `Ring` only by the part (tag 2).
fn owners_program() -> Arc<Program> {
    let mut pb = ProgramBuilder::new(API);
    let q = qtys(&mut pb);
    let zero = zero_fn(&mut pb);
    let start = noop_fn(&mut pb, "$start::Q");
    let dog = pb.global("Q::role.dog", q.t.int);
    pb.set_global_init(dog, zero);
    let role = RoleInfo {
        name: "dog".into(),
        global: dog.0,
        ty: q.t.int,
        optional: false,
        cond: None,
    };
    let info = quest_info(&mut pb, &q, zero, vec![role], 1, start);
    logger(&mut pb, &q, "home_ping", "Ping", q.t.ping, 0, None, 0);
    logger(&mut pb, &q, "part_ping", "Ping", q.t.ping, 1, None, 1);
    logger(&mut pb, &q, "part_ring", "Ring", q.ring, 1, None, 2);
    pb.set_quest(info);
    Arc::new(pb.finish().expect("owners program"))
}

fn dog_cfg(instance: u16) -> VmConfig {
    VmConfig {
        instance,
        roles: vec![("dog".into(), WireValue::Int(1))],
        ..cfg()
    }
}

#[test]
fn only_owner_handlers_run() {
    let p = owners_program();
    let mut home = Vm::new(p.clone(), dog_cfg(0)).expect("home");
    let mut part = Vm::new(p.clone(), dog_cfg(1)).expect("part");
    let (mut hh, mut ph) = (RecordingHost::default(), RecordingHost::default());
    for (vm, host) in [(&mut home, &mut hh), (&mut part, &mut ph)] {
        assert_eq!(vm.deliver("Ping", &ping(5)), Delivery::Accepted);
        at(vm, host, 1);
    }
    assert_eq!(logged(&hh), [(5, 0)]);
    assert_eq!(logged(&ph), [(5, 1)]);
    // the part's own event has no handler at home
    assert_eq!(
        home.deliver("Ring", &ring(6)),
        Delivery::Dropped(DropReason::NoHandler)
    );
    assert_eq!(part.deliver("Ring", &ring(6)), Delivery::Accepted);
    at(&mut part, &mut ph, 2);
    assert_eq!(logged(&ph), [(5, 1), (6, 2)]);
    // there is no second part
    assert!(matches!(
        Vm::new(p.clone(), dog_cfg(2)),
        Err(LoadError::Invalid(_))
    ));
    // a save loads only as the instance it was
    assert!(Vm::load(p.clone(), dog_cfg(1), &part.save()).is_ok());
    assert!(matches!(
        Vm::load(p, dog_cfg(0), &part.save()),
        Err(LoadError::Invalid(_))
    ));
}

#[test]
fn quest_start_runs_after_the_initialisers_in_the_home_only() {
    let mut pb = ProgramBuilder::new(API);
    let q = qtys(&mut pb);
    let zero = zero_fn(&mut pb);
    let say = |pb: &mut ProgramBuilder, name: &str, tag: i16| {
        let s = pb.declare(name, 0);
        let mut f = pb.body(s);
        let (n, k) = (f.local(0), f.local(1));
        f.emit(Op::LoadSmall, n, 0u16, 0u16);
        f.emit(Op::LoadSmall, k, tag as u16, 0u16);
        f.emit(Op::CallHost, q.log, n, n);
        ret(&mut f);
        f.end();
        s
    };
    let start = say(&mut pb, "$start::Q", 9);
    let main = say(&mut pb, "main", 8);
    let init = say(&mut pb, "init", 7);
    let dog = pb.global("Q::role.dog", q.t.int);
    pb.set_global_init(dog, zero);
    let g = pb.global("g", q.t.unit);
    pb.set_global_init(g, init);
    let role = RoleInfo {
        name: "dog".into(),
        global: dog.0,
        ty: q.t.int,
        optional: false,
        cond: None,
    };
    let info = quest_info(&mut pb, &q, zero, vec![role], 1, start);
    pb.main(main);
    pb.set_quest(info);
    let p = Arc::new(pb.finish().expect("program"));

    let mut home = Vm::new(p.clone(), dog_cfg(0)).expect("home");
    let mut part = Vm::new(p.clone(), dog_cfg(1)).expect("part");
    let (mut hh, mut ph) = (RecordingHost::default(), RecordingHost::default());
    for (vm, host) in [(&mut home, &mut hh), (&mut part, &mut ph)] {
        assert!(vm.start_main());
        at(vm, host, 1);
        assert!(vm.is_idle());
    }
    assert_eq!(logged(&hh), [(0, 7), (0, 9), (0, 8)]);
    assert_eq!(logged(&ph), [(0, 7), (0, 8)]);
    // an edit does not start the quest again
    let (mut next, _) = home.migrate(p.clone(), dog_cfg(0)).expect("migrates");
    let mut nh = RecordingHost::default();
    at(&mut next, &mut nh, 2);
    assert!(logged(&nh).is_empty());
    // a reset does
    home.reset().expect("reset");
    let mut rh = RecordingHost::default();
    at(&mut home, &mut rh, 3);
    assert_eq!(logged(&rh), [(0, 7), (0, 9)]);
}

#[test]
fn timer_fires_at_due_tick() {
    let p = timers_program();
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    at(&mut vm, &mut host, 1);
    vm.deliver("Pong", &ping(3));
    at(&mut vm, &mut host, 2);
    assert_eq!(vm.timers(), [(5, timer_id(1), "Ring".to_string())]);
    for now in 3..=4 {
        at(&mut vm, &mut host, now);
        assert!(logged(&host).is_empty(), "fired early, at {now}");
    }
    at(&mut vm, &mut host, 5);
    assert_eq!(logged(&host), [(103, 1)]);
    assert!(vm.timers().is_empty());
    assert!(host.reports.is_empty(), "{:?}", host.reports);
    assert!(vm.is_idle());
    // a late tick fires what is due by then
    vm.deliver("Pong", &ping(2));
    at(&mut vm, &mut host, 6);
    at(&mut vm, &mut host, 20);
    assert_eq!(logged(&host), [(103, 1), (102, 1)]);
}

#[test]
fn timers_fire_in_due_then_id_order() {
    let p = timers_program();
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    at(&mut vm, &mut host, 1);
    // due 6 (id 1), due 6 (id 2), due 5 (id 3)
    for (now, n) in [(2, 4), (3, 3), (4, 1)] {
        vm.deliver("Pong", &ping(n));
        at(&mut vm, &mut host, now);
    }
    assert_eq!(
        vm.timers(),
        [
            (5, timer_id(3), "Ring".to_string()),
            (6, timer_id(1), "Ring".to_string()),
            (6, timer_id(2), "Ring".to_string()),
        ]
    );
    at(&mut vm, &mut host, 5);
    assert_eq!(logged(&host), [(101, 1)]);
    at(&mut vm, &mut host, 6);
    assert_eq!(logged(&host), [(101, 1), (104, 1), (103, 1)]);
}

#[test]
fn stage_timer_cancelled() {
    let p = timers_program();
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    at(&mut vm, &mut host, 1);
    // a stage handler's timer (Ring 3) and a quest-body handler's (Ring 103)
    vm.deliver("Ping", &ping(3));
    vm.deliver("Pong", &ping(3));
    at(&mut vm, &mut host, 2);
    assert_eq!(vm.timers().len(), 2);
    vm.deliver("Quiet", &ping(0));
    at(&mut vm, &mut host, 3);
    assert_eq!(vm.timers(), [(5, timer_id(2), "Ring".to_string())]);
    for now in 4..=8 {
        at(&mut vm, &mut host, now);
    }
    assert_eq!(logged(&host), [(103, 1)]);
}

#[test]
fn cancel_timer_cancels_only_its_timer() {
    let p = timers_program();
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    at(&mut vm, &mut host, 1);
    vm.deliver("Pong", &ping(3)); // id 1, due 5
    at(&mut vm, &mut host, 2);
    vm.deliver("Pong", &ping(4)); // id 2, due 7: `last`
    at(&mut vm, &mut host, 3);
    vm.deliver("Big", &ping(0));
    at(&mut vm, &mut host, 4);
    assert_eq!(vm.timers(), [(5, timer_id(1), "Ring".to_string())]);
    // cancelling a timer that is gone does nothing
    vm.deliver("Big", &ping(0));
    for now in 5..=9 {
        at(&mut vm, &mut host, now);
    }
    assert_eq!(logged(&host), [(103, 1)]);
    assert!(host.reports.is_empty(), "{:?}", host.reports);
}

#[test]
fn timer_undone_with_its_segment() {
    let p = timers_program();
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    at(&mut vm, &mut host, 1);
    // Pong 0 sets a timer, then faults: the timer goes with the segment
    vm.deliver("Pong", &ping(0));
    at(&mut vm, &mut host, 2);
    assert_eq!(fault_kinds(&host), [FaultKind::DivByZero]);
    assert!(vm.timers().is_empty());
    for now in 3..=5 {
        at(&mut vm, &mut host, now);
    }
    assert!(logged(&host).is_empty());
    assert!(vm.is_idle());
    // and the sim's rollback undoes a committed one
    let before = vm.hash64();
    let cp = vm.checkpoint();
    vm.deliver("Pong", &ping(3));
    at(&mut vm, &mut host, 6);
    assert_eq!(vm.timers().len(), 1);
    vm.rollback_to(cp).expect("rollback");
    assert!(vm.timers().is_empty());
    assert_eq!(vm.hash64(), before);
}

#[test]
fn too_many_timers_faults() {
    let p = timers_program();
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    at(&mut vm, &mut host, 1);
    for now in 0..TIMERS_MAX as u64 {
        vm.deliver("Pong", &ping(50));
        at(&mut vm, &mut host, 2 + now);
    }
    assert!(host.reports.is_empty(), "{:?}", host.reports);
    assert_eq!(vm.timers().len(), TIMERS_MAX);
    vm.deliver("Pong", &ping(50));
    at(&mut vm, &mut host, 40);
    assert_eq!(fault_kinds(&host), [FaultKind::TooManyTimers]);
    assert_eq!(vm.timers().len(), TIMERS_MAX);
}

#[test]
fn timer_survives_save_load_and_is_hashed() {
    let p = timers_program();
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    at(&mut vm, &mut host, 1);
    vm.deliver("Pong", &ping(3));
    at(&mut vm, &mut host, 2);

    // The same state but for the timer's due tick (word 2 of record 0, in the save's copy of the memory):
    // the hash differs, and the timer fires at the new tick.
    let mut save = vm.save();
    let layout = Layout::plan(u32::from(p.static_words), MEM_BYTES_DEFAULT).expect("layout");
    let at_due = SAVE_HEADER_BYTES + layout.timers_base as usize + 2 * 8;
    assert_eq!(save[at_due..at_due + 8], 5u64.to_le_bytes());
    save[at_due..at_due + 8].copy_from_slice(&6u64.to_le_bytes());
    let mut later = Vm::load(p.clone(), cfg(), &save).expect("loads");
    assert_eq!(later.timers(), [(6, timer_id(1), "Ring".to_string())]);
    assert_ne!(later.hash64(), vm.hash64());
    let mut th = RecordingHost::default();
    for now in 3..=5 {
        at(&mut later, &mut th, now);
    }
    assert!(logged(&th).is_empty());
    at(&mut later, &mut th, 6);
    assert_eq!(logged(&th), [(103, 1)]);

    let mut loaded = Vm::load(p.clone(), cfg(), &vm.save()).expect("loads");
    assert_eq!(loaded.hash64(), vm.hash64());
    assert_eq!(loaded.timers(), vm.timers());
    let mut lh = RecordingHost::default();
    let mut vh = RecordingHost::default();
    for now in 3..=6 {
        at(&mut vm, &mut vh, now);
        at(&mut loaded, &mut lh, now);
        assert_eq!(loaded.hash64(), vm.hash64());
    }
    assert_eq!(logged(&lh), [(103, 1)]);
    assert_eq!(lh.transcript(), vh.transcript());
}

#[test]
fn timers_carried_by_an_edit() {
    let p = timers_program();
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    at(&mut vm, &mut host, 1);
    vm.deliver("Ping", &ping(3)); // scoped, id 1, due 5
    at(&mut vm, &mut host, 2);
    vm.deliver("Pong", &ping(4)); // id 2, due 7
    at(&mut vm, &mut host, 3);
    // the same program: both timers, ids, due ticks and scopes carried; the serial goes on
    let (mut next, report) = vm.migrate(p.clone(), cfg()).expect("migrates");
    assert_eq!(report.timers_dropped, 0);
    assert_eq!(next.timers(), vm.timers());
    next.deliver("Quiet", &ping(0));
    at(&mut next, &mut host, 4);
    assert_eq!(next.timers(), [(7, timer_id(2), "Ring".to_string())]);
    next.deliver("Pong", &ping(1));
    at(&mut next, &mut host, 5);
    assert_eq!(next.timers()[0], (6, timer_id(3), "Ring".to_string()));
    for now in 6..=8 {
        at(&mut next, &mut host, now);
    }
    assert_eq!(logged(&host), [(101, 1), (104, 1)]);

    // a program without the script event drops them
    let mut pb = ProgramBuilder::new(API);
    let t = tys(&mut pb);
    let h = ping_logger(&mut pb, &t, "on_ping", 0, 0);
    pb.handler("Ping", t.ping, h);
    let other = Arc::new(pb.finish().expect("program"));
    let (next, report) = vm.migrate(other, cfg()).expect("migrates");
    assert_eq!(report.timers_dropped, 2);
    assert!(next.timers().is_empty());
}

#[test]
fn reset_clears_timers() {
    let p = timers_program();
    let mut vm = vm(&p);
    let mut host = RecordingHost::default();
    at(&mut vm, &mut host, 1);
    vm.deliver("Pong", &ping(3));
    vm.deliver("Ping", &ping(3));
    at(&mut vm, &mut host, 2);
    assert_eq!(vm.timers().len(), 2);
    vm.reset().expect("reset");
    assert!(vm.timers().is_empty());
    for now in 3..=8 {
        at(&mut vm, &mut host, now);
    }
    assert!(logged(&host).is_empty());
}

/// Roles `farmer: int` and `optional miller: str`; the first global `seen = farmer + 1` shows the roles
/// are in place before any initialiser runs; the roles' own initialisers fault if they ever run.
fn roles_program() -> Arc<Program> {
    let mut pb = ProgramBuilder::new(API);
    let q = qtys(&mut pb);
    let zero = zero_fn(&mut pb);
    let start = noop_fn(&mut pb, "$start::Q");
    let opt_str = pb.ty(TypeDesc::Enum {
        name: "Option".into(),
        variants: vec![
            ("None".into(), vec![]),
            ("Some".into(), vec![("value".into(), q.t.str)]),
        ],
    });
    let bomb = pb.declare("$role_placeholder", 0);
    {
        let mut f = pb.body(bomb);
        let (z, out) = (f.local(0), f.local(1));
        f.emit(Op::LoadSmall, z, 0u16, 0u16);
        f.emit(Op::DivI, out, z, z);
        ret(&mut f);
        f.end();
    }
    let seen_init = pb.declare("init_seen", 0);
    let seen = pb.global("seen", q.t.int);
    pb.set_global_init(seen, seen_init);
    let farmer = pb.global("Q::role.farmer", q.t.int);
    let miller = pb.global("Q::role.miller", opt_str);
    pb.set_global_init(farmer, bomb);
    pb.set_global_init(miller, bomb);
    {
        let mut f = pb.body(seen_init);
        let (one, r) = (f.local(0), f.ret());
        f.emit(Op::LoadSmall, one, 1u16, 0u16);
        f.emit(Op::AddI, r, SlotRef::Global(farmer), one);
        ret(&mut f);
        f.end();
    }
    let roles = vec![
        RoleInfo {
            name: "farmer".into(),
            global: farmer.0,
            ty: q.t.int,
            optional: false,
            cond: None,
        },
        RoleInfo {
            name: "miller".into(),
            global: miller.0,
            ty: q.t.str,
            optional: true,
            cond: None,
        },
    ];
    let info = quest_info(&mut pb, &q, zero, roles, 0, start);
    pb.set_quest(info);
    Arc::new(pb.finish().expect("roles program"))
}

fn with_roles(roles: &[(&str, WireValue)]) -> VmConfig {
    VmConfig {
        roles: roles
            .iter()
            .map(|(n, v)| (n.to_string(), v.clone()))
            .collect(),
        ..cfg()
    }
}

#[test]
fn role_values_installed() {
    let p = roles_program();
    let slot = |name: &str| {
        let g = p.globals.iter().find(|g| g.name == name).expect(name);
        (g.slot, g.ty)
    };
    let value = |vm: &Vm, name: &str| {
        let (s, t) = slot(name);
        vm.debug_value(s, t).expect("readable")
    };
    let none = WireValue::Variant {
        name: "None".into(),
        fields: vec![],
    };
    let mut vm = Vm::new(p.clone(), with_roles(&[("farmer", WireValue::Int(4))])).expect("loads");
    let mut host = RecordingHost::default();
    at(&mut vm, &mut host, 1);
    assert!(host.reports.is_empty(), "{:?}", host.reports);
    assert!(vm.is_idle());
    assert_eq!(value(&vm, "Q::role.farmer"), WireValue::Int(4));
    assert_eq!(value(&vm, "Q::role.miller"), none);
    assert_eq!(value(&vm, "seen"), WireValue::Int(5));

    let given = with_roles(&[
        ("miller", WireValue::Str("Mo".into())),
        ("farmer", WireValue::Int(9)),
    ]);
    let mut vm = Vm::new(p.clone(), given).expect("loads");
    at(&mut vm, &mut host, 1);
    assert!(host.reports.is_empty(), "{:?}", host.reports);
    assert_eq!(value(&vm, "seen"), WireValue::Int(10));
    let some = WireValue::Variant {
        name: "Some".into(),
        fields: vec![("value".into(), WireValue::Str("Mo".into()))],
    };
    assert_eq!(value(&vm, "Q::role.miller"), some);
    // a reset installs them again; a save keeps them
    vm.reset().expect("reset");
    at(&mut vm, &mut host, 2);
    assert!(host.reports.is_empty(), "{:?}", host.reports);
    assert_eq!(value(&vm, "Q::role.miller"), some);
    assert_eq!(value(&vm, "seen"), WireValue::Int(10));
    let loaded = Vm::load(
        p.clone(),
        with_roles(&[("farmer", WireValue::Int(9))]),
        &vm.save(),
    )
    .expect("loads");
    assert_eq!(value(&loaded, "Q::role.farmer"), WireValue::Int(9));
}

#[test]
fn load_with_missing_roles_is_refused() {
    let p = roles_program();
    let farmer = || with_roles(&[("farmer", WireValue::Int(4))]);
    let vm = Vm::new(p.clone(), farmer()).expect("loads");
    let save = vm.save();
    // checked as `Vm::new` checks them: missing, misfitting, unknown
    for (roles, at_fault) in [
        (vec![], "farmer"),
        (vec![("farmer", WireValue::Str("x".into()))], "farmer"),
        (
            vec![("farmer", WireValue::Int(4)), ("baker", WireValue::Int(1))],
            "baker",
        ),
    ] {
        assert_eq!(
            Vm::load(p.clone(), with_roles(&roles), &save).err(),
            Some(LoadError::Role(at_fault.into()))
        );
    }
    // a load with its roles resets into a real fresh instance (the next epoch, its roles installed)
    let mut loaded = Vm::load(p.clone(), farmer(), &save).expect("loads");
    loaded.reset().expect("reset");
    let fresh = Vm::new(
        p.clone(),
        VmConfig {
            epoch: 2,
            ..farmer()
        },
    )
    .expect("fresh");
    assert_eq!(loaded.mem_bytes(), fresh.mem_bytes());
}

#[test]
fn migrate_keeps_its_instance() {
    let p = owners_program();
    let part = Vm::new(p.clone(), dog_cfg(1)).expect("part");
    assert!(matches!(
        part.migrate(p.clone(), dog_cfg(0)),
        Err(MigrateError::Refused(LoadError::Invalid(_)))
    ));
    let (mut next, _) = part.migrate(p.clone(), dog_cfg(1)).expect("migrates");
    let mut host = RecordingHost::default();
    next.deliver("Ping", &ping(5));
    at(&mut next, &mut host, 1);
    assert_eq!(logged(&host), [(5, 1)]);
}

/// `Enqueue`, `After` and `CancelTimer` of a one-field event: each its base cost plus one for the payload's
/// walk (`ceil(words / 8)`), `Enqueue` one more for the bytes it buffers.
#[test]
fn timer_and_enqueue_fuel_pinned() {
    let mut pb = ProgramBuilder::new(API);
    let q = qtys(&mut pb);
    let f = pb.declare("ops", 0);
    {
        let mut b = pb.body(f);
        let (n, ticks, val, id) = (b.local(0), b.local(1), b.local(2), b.local(3));
        b.mark_ref(val);
        b.emit(Op::LoadSmall, n, 7u16, 0u16);
        b.emit(Op::Copy, ticks, n, 0u16);
        make_ring(&mut b, &q, n, 0, val, 4);
        b.emit(Op::Enqueue, q.ring_event, val, 0u16);
        b.emit(Op::After, id, ticks, q.ring_event);
        b.emit(Op::CancelTimer, id, 0u16, 0u16);
        b.emit(Op::Drop, val, 0u16, 0u16);
        ret(&mut b); // 2
        b.end();
    }
    let p = Arc::new(pb.finish().expect("program"));
    let mut vm = vm(&p);
    let r = run_fn(&mut vm, 0);
    assert_eq!(r.end, quest_vm::vm::RunEnd::Returned);
    // The rest of the fn (loads, the event's construction, its drop, the return) costs 20.
    let ops = (FUEL_ENQUEUE + 1 + 1) + (FUEL_AFTER + 1) + FUEL_CANCEL;
    assert_eq!(ops, 28);
    assert_eq!(r.fuel_used, 20 + ops);
}

#[test]
fn missing_role_refused() {
    let p = roles_program();
    let refused = |roles: &[(&str, WireValue)]| Vm::new(p.clone(), with_roles(roles)).err();
    assert_eq!(refused(&[]), Some(LoadError::Role("farmer".into())));
    assert_eq!(
        refused(&[("miller", WireValue::Str("Mo".into()))]),
        Some(LoadError::Role("farmer".into()))
    );
    // a role the quest does not have, or one given twice
    assert_eq!(
        refused(&[("farmer", WireValue::Int(1)), ("baker", WireValue::Int(1))]),
        Some(LoadError::Role("baker".into()))
    );
    assert_eq!(
        refused(&[("farmer", WireValue::Int(1)), ("farmer", WireValue::Int(2))]),
        Some(LoadError::Role("farmer".into()))
    );
    // a program without a quest takes no roles
    let plain = sum_loop(1, false);
    assert_eq!(
        Vm::new(plain, with_roles(&[("farmer", WireValue::Int(1))])).err(),
        Some(LoadError::Role("farmer".into()))
    );
}

#[test]
fn misfit_role_refused() {
    let p = roles_program();
    let refused = |roles: &[(&str, WireValue)]| Vm::new(p.clone(), with_roles(roles)).err();
    assert_eq!(
        refused(&[("farmer", WireValue::Str("x".into()))]),
        Some(LoadError::Role("farmer".into()))
    );
    assert_eq!(
        refused(&[("farmer", WireValue::Int(1)), ("miller", WireValue::Int(3))]),
        Some(LoadError::Role("miller".into()))
    );
    // an optional role is given as its value, not as an `Option`
    let some = WireValue::Variant {
        name: "Some".into(),
        fields: vec![("value".into(), WireValue::Str("Mo".into()))],
    };
    assert_eq!(
        refused(&[("farmer", WireValue::Int(1)), ("miller", some)]),
        Some(LoadError::Role("miller".into()))
    );
}
