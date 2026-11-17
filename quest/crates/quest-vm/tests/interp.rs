//! The interpreter core: fuel, pause and resume, static frames, integer and float arithmetic (design D5, D6).

mod common;

use std::sync::Arc;

use common::*;
use quest_vm::asm::ProgramBuilder;
use quest_vm::fuel::{FUEL_CALL, FUEL_HOST_QUERY, FUEL_SIMPLE};
use quest_vm::vm::DebugRun;
use quest_vm::vm::{RunEnd, Vm, VmConfig};
use quest_vm::{FaultKind, LoadError, Op};

const CANONICAL_NAN: u64 = 0x7ff8_0000_0000_0000;

fn int(v: i64) -> u64 {
    v as u64
}

fn float(v: f64) -> u64 {
    v.to_bits()
}

/// Runs `main` to its end and returns its return value; panics on anything else.
fn returned(p: &Arc<quest_vm::Program>) -> u64 {
    let mut vm = vm(p);
    let r = run_fn(&mut vm, 0);
    assert_eq!(r.end, RunEnd::Returned);
    vm.slot(frame_slot(p, 0, 1))
}

/// Runs `main` and expects a fault of `kind` whose first stack entry is on `line`.
fn faults(p: &Arc<quest_vm::Program>, kind: FaultKind, line: u32) {
    let mut vm = vm(p);
    match run_fn(&mut vm, 0).end {
        RunEnd::Faulted { kind: k, stack, .. } => {
            assert_eq!(k, kind);
            assert_eq!(stack[0].line, line);
        }
        other => panic!("expected a {kind:?} fault, got {other:?}"),
    }
}

#[test]
fn sum_loop() {
    let p = common::sum_loop(100, false);
    assert_eq!(returned(&p), 4950);
}

#[test]
fn fuel_pause_resume_same_result() {
    let p = common::sum_loop(100, true);
    let mut whole = vm(&p);
    let (one, _) = run_in_slices(&mut whole, u64::MAX);
    assert_eq!(one.end, RunEnd::Returned);
    assert_eq!(whole.slot(frame_slot(&p, 0, 1)), 4950);
    for budget in [1, 2, 3, 7, 50, 1000] {
        let mut sliced = vm(&p);
        let (r, runs) = run_in_slices(&mut sliced, budget);
        assert_eq!(r.end, RunEnd::Returned, "budget {budget}");
        // an overshoot is a debt repaid by the next run: only the last charge can exceed the fuel given
        assert!(r.fuel_used < runs * budget + FUEL_CALL, "budget {budget}");
        assert_eq!(r.fuel_used, one.fuel_used, "budget {budget}");
        assert_eq!(
            sliced.script_bytes(),
            whole.script_bytes(),
            "budget {budget}"
        );
    }
}

#[test]
fn pause_point_is_deterministic() {
    let p = common::sum_loop(100, true);
    let (mut a, mut b) = (vm(&p), vm(&p));
    for v in [&mut a, &mut b] {
        v.debug_start(0);
        assert_eq!(
            v.debug_run(37, &mut RecordingHost::default()).end,
            RunEnd::OutOfFuel
        );
    }
    assert_eq!(a.mem_bytes(), b.mem_bytes());
}

#[test]
fn infinite_loop_burns_exactly_its_budget() {
    let mut pb = ProgramBuilder::new(API);
    let mut f = pb.func("main", 0);
    let top = f.label();
    f.bind(top);
    f.jump(Op::Jmp, 0u16, top);
    f.end();
    let p = Arc::new(pb.finish().unwrap());
    let mut vm = vm(&p);
    vm.debug_start(0);
    let mut host = RecordingHost::default();
    for _ in 0..10 {
        let r = vm.debug_run(100, &mut host);
        assert_eq!(r.fuel_used, 100);
        assert_eq!(r.end, RunEnd::OutOfFuel);
    }
}

#[test]
fn int_faults() {
    use FaultKind::{DivByZero, IntOverflow};
    let cases = [
        (Op::AddI, int(i64::MAX), int(1), IntOverflow),
        (Op::SubI, int(i64::MIN), int(1), IntOverflow),
        (Op::MulI, int(i64::MIN), int(-1), IntOverflow),
        (Op::DivI, int(i64::MIN), int(-1), IntOverflow),
        (Op::RemI, int(i64::MIN), int(-1), IntOverflow),
        (Op::DivI, int(1), int(0), DivByZero),
        (Op::RemI, int(1), int(0), DivByZero),
    ];
    for (op, x, y, kind) in cases {
        faults(&binop(op, x, y, 7), kind, 7);
    }
    faults(&unop(Op::NegI, int(i64::MIN), 9), IntOverflow, 9);
}

#[test]
fn int_division_truncates() {
    assert_eq!(returned(&binop(Op::DivI, int(-7), int(2), 1)), int(-3));
    assert_eq!(returned(&binop(Op::RemI, int(-7), int(2), 1)), int(-1));
}

#[test]
fn nan_is_canonical() {
    let mut pb = ProgramBuilder::new(API);
    let zero = pb.const_u64(float(0.0));
    let mut f = pb.func("main", 0);
    let (z, nan, ret) = (f.local(0), f.local(1), f.ret());
    f.emit_t(Op::LoadConst, z, zero);
    f.emit(Op::DivF, nan, z, z);
    f.emit(Op::NegF, ret, nan, 0u16);
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
    f.end();
    let p = Arc::new(pb.finish().unwrap());
    let mut vm = vm(&p);
    assert_eq!(run_fn(&mut vm, 0).end, RunEnd::Returned);
    assert_eq!(vm.slot(frame_slot(&p, 0, 4)), CANONICAL_NAN, "0.0 / 0.0");
    assert_eq!(vm.slot(frame_slot(&p, 0, 1)), CANONICAL_NAN, "-(0.0 / 0.0)");
}

#[test]
fn float_to_int() {
    assert_eq!(returned(&unop(Op::FloatToInt, float(2.9), 1)), int(2));
    assert_eq!(returned(&unop(Op::FloatToInt, float(-2.9), 1)), int(-2));
    // -2^63 is the smallest i64; 2^63 (what 9223372036854775807.0 rounds to) is past the largest
    assert_eq!(
        returned(&unop(Op::FloatToInt, float(-9223372036854775808.0), 1)),
        int(i64::MIN)
    );
    for bad in [f64::NAN, 1e300, -1e300, 9223372036854775807.0] {
        faults(
            &unop(Op::FloatToInt, float(bad), 4),
            FaultKind::BadFloatToInt,
            4,
        );
    }
}

/// `f` (fn 0) calls `g(x)`, which calls `h(x + 1)`, which returns `1000 / its argument`; `f` returns that.
fn calls(x: i16) -> Arc<quest_vm::Program> {
    let mut pb = ProgramBuilder::new(API);
    pb.set_file("calls.qs");
    let f = pb.declare("f", 0);
    let g = pb.declare("g", 1);
    let h = pb.declare("h", 1);
    {
        let mut b = pb.body(f);
        let (gf, ret) = (b.of(g), b.ret());
        b.line(10);
        b.emit(Op::LoadSmall, gf.param(0), x as u16, 0u16);
        b.call(g);
        b.emit(Op::Copy, ret, gf.ret(), 0u16);
        b.emit(Op::Ret, 0u16, 0u16, 0u16);
        b.end();
    }
    {
        let mut b = pb.body(g);
        let (hf, ret, one) = (b.of(h), b.ret(), b.local(0));
        b.line(20);
        b.emit(Op::LoadSmall, one, 1u16, 0u16);
        b.emit(Op::AddI, hf.param(0), b.param(0), one);
        b.call(h);
        b.emit(Op::Copy, ret, hf.ret(), 0u16);
        b.emit(Op::Ret, 0u16, 0u16, 0u16);
        b.end();
    }
    {
        let mut b = pb.body(h);
        let (k, ret) = (b.local(0), b.ret());
        b.line(30);
        b.emit(Op::LoadSmall, k, 1000u16, 0u16);
        b.emit(Op::DivI, ret, k, b.param(0));
        b.emit(Op::Ret, 0u16, 0u16, 0u16);
        b.end();
    }
    Arc::new(pb.finish().unwrap())
}

#[test]
fn calls_use_static_frames() {
    let p = calls(9);
    let mut vm = vm(&p);
    assert_eq!(run_fn(&mut vm, 0).end, RunEnd::Returned);
    assert_eq!(vm.slot(frame_slot(&p, 0, 1)), 100);
    // the arguments sit in the callees' parameter slots, the results in their return slots
    assert_eq!(vm.slot(frame_slot(&p, 1, 3)), 9);
    assert_eq!(vm.slot(frame_slot(&p, 2, 3)), 10);
    assert_eq!(vm.slot(frame_slot(&p, 2, 1)), 100);

    let p = calls(-1);
    let mut faulting = common::vm(&p);
    match run_fn(&mut faulting, 0).end {
        RunEnd::Faulted { kind, stack, .. } => {
            assert_eq!(kind, FaultKind::DivByZero);
            let lines: Vec<(&str, u32)> = stack.iter().map(|l| (l.file.as_str(), l.line)).collect();
            assert_eq!(
                lines,
                [("calls.qs", 30), ("calls.qs", 20), ("calls.qs", 10)]
            );
        }
        other => panic!("expected a fault, got {other:?}"),
    }
}

#[test]
fn version_mismatch_refused() {
    let p = common::sum_loop(3, false);
    let other_api = VmConfig {
        host_api_version: API + 1,
        ..cfg()
    };
    assert_eq!(
        Vm::new(p.clone(), other_api).err(),
        Some(LoadError::HostApiMismatch { found: API })
    );
    let mut q = (*p).clone();
    q.bytecode_version += 1;
    assert_eq!(
        Vm::new(Arc::new(q.clone()), cfg()).err(),
        Some(LoadError::VersionMismatch {
            found: q.bytecode_version
        })
    );
}

/// The host fn of [`fallible_query`]: `q(n: int) -> int raises OutOfReach`.
const Q: u16 = 4;

/// `main` (fn 0): word 4 = 99, then `word 4 = q(5)` (the error in word 5), and returns the error word. The
/// program's errors are `Gone` and `OutOfReach`; `q` declares only `OutOfReach`.
fn fallible_query() -> Arc<quest_vm::Program> {
    let mut pb = ProgramBuilder::new(API);
    let int = pb.ty(quest_vm::types::TypeDesc::Int);
    pb.ty(quest_vm::types::TypeDesc::Enum {
        name: quest_vm::types::ERROR_ENUM_NAME.into(),
        variants: vec![("Gone".into(), vec![]), ("OutOfReach".into(), vec![])],
    });
    let site = pb.fallible_host_site(
        Q,
        quest_vm::host::HostFnKind::Query,
        vec![int],
        int,
        vec!["OutOfReach".into()],
    );
    let mut f = pb.func("main", 0);
    let (arg, value, err, ret) = (f.local(0), f.local(1), f.local(2), f.ret());
    f.emit(Op::LoadSmall, arg, 5u16, 0u16);
    f.emit(Op::LoadSmall, value, 99u16, 0u16);
    f.line(7);
    f.emit(Op::CallHost, site, arg, value);
    f.emit(Op::Copy, ret, err, 0u16);
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
    f.end();
    Arc::new(pb.finish().expect("fallible query program"))
}

fn error(name: &str) -> quest_vm::wire::WireValue {
    quest_vm::wire::WireValue::Variant {
        name: name.into(),
        fields: vec![],
    }
}

/// Runs [`fallible_query`] with `q` answered by `answer`: the run, then (value word, error word).
fn ask(
    p: &Arc<quest_vm::Program>,
    answer: Result<quest_vm::wire::WireValue, quest_vm::host::QueryError>,
) -> (DebugRun, u64, u64) {
    let mut vm = vm(p);
    let mut host = RecordingHost {
        answer: Some(Box::new(move |f, args| {
            assert_eq!((f, args), (Q, &[quest_vm::wire::WireValue::Int(5)][..]));
            answer.clone()
        })),
        ..RecordingHost::default()
    };
    vm.debug_start(0);
    let r = vm.debug_run(u64::MAX, &mut host);
    assert_eq!(host.queries.len(), 1);
    (
        r,
        vm.slot(frame_slot(p, 0, 4)),
        vm.slot(frame_slot(p, 0, 5)),
    )
}

#[test]
fn query_error_takes_the_error_path_and_costs_fixed_fuel() {
    use quest_vm::host::QueryError;
    let p = fallible_query();
    let (ok, value, err) = ask(&p, Ok(quest_vm::wire::WireValue::Int(7)));
    assert_eq!((ok.end.clone(), value, err), (RunEnd::Returned, 7, 0));

    // A declared error: the error word holds `OutOfReach` (tag 1, data-less: `1 << 1 | 1`), the value word
    // is untouched, and the call costs what the answered one did.
    let (raised, value, err) = ask(&p, Err(QueryError::Raised(error("OutOfReach"))));
    assert_eq!((raised.end, value, err), (RunEnd::Returned, 99, 3));
    assert_eq!(raised.fuel_used, ok.fuel_used);
    // LoadSmall ×2, CallHost (+1 for its argument, +1 for the value or error it loads), Copy, Ret.
    assert_eq!(
        ok.fuel_used,
        2 * FUEL_SIMPLE + FUEL_HOST_QUERY + 2 + FUEL_SIMPLE + FUEL_CALL
    );

    // An error the function does not declare, and a host bug, are faults at the call.
    for (answer, message) in [
        (
            QueryError::Raised(error("Gone")),
            format!("host fn {Q} raised Gone, which it does not declare"),
        ),
        (
            QueryError::Failed("no such person".into()),
            "no such person".into(),
        ),
    ] {
        match ask(&p, Err(answer)).0.end {
            RunEnd::Faulted {
                kind,
                message: m,
                stack,
            } => {
                assert_eq!((kind, m, stack[0].line), (FaultKind::HostFault, message, 7));
            }
            other => panic!("expected a host fault, got {other:?}"),
        }
    }
}

/// fn 0 issues the command `c(5)` (host fn 6, no errors); fn 1 the command `p(5)` (host fn 7, raises
/// `NoRoom`), its error in word 5.
fn two_commands() -> Arc<quest_vm::Program> {
    use quest_vm::host::HostFnKind::Command;
    let mut pb = ProgramBuilder::new(API);
    let int = pb.ty(quest_vm::types::TypeDesc::Int);
    let unit = pb.ty(quest_vm::types::TypeDesc::Unit);
    pb.ty(quest_vm::types::TypeDesc::Enum {
        name: quest_vm::types::ERROR_ENUM_NAME.into(),
        variants: vec![("NoRoom".into(), vec![])],
    });
    let plain = pb.host_site(6, Command, vec![int], unit);
    let checked = pb.fallible_host_site(7, Command, vec![int], unit, vec!["NoRoom".into()]);
    for site in [plain, checked] {
        let mut f = pb.func("issue", 0);
        let (arg, value) = (f.local(0), f.local(1));
        f.emit(Op::LoadSmall, arg, 5u16, 0u16);
        f.emit(Op::CallHost, site, arg, value);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
    }
    Arc::new(pb.finish().expect("two commands program"))
}

#[test]
fn infallible_command_is_never_checked() {
    let p = two_commands();
    let run = |f: u16, refuse: bool| {
        let mut vm = vm(&p);
        let mut host = RecordingHost::default();
        if refuse {
            host.check_answer = Some(Box::new(|_, _| {
                Err(quest_vm::host::QueryError::Raised(error("NoRoom")))
            }));
        }
        vm.debug_start(f);
        let r = vm.debug_run(u64::MAX, &mut host);
        assert_eq!(r.end, RunEnd::Returned);
        (r.fuel_used, host.checks, vm.slot(frame_slot(&p, f, 5)))
    };
    let (plain, checks, _) = run(0, false);
    assert_eq!(checks, vec![]);
    let (checked, checks, err) = run(1, false);
    assert_eq!(
        (checks, err),
        (vec![(7, vec![quest_vm::wire::WireValue::Int(5)])], 0)
    );
    // The check is the only difference in cost.
    assert_eq!(checked, plain + quest_vm::fuel::FUEL_HOST_CHECK);
    // Refused: the error word holds `NoRoom` (tag 0: `0 << 1 | 1`).
    assert_eq!(run(1, true).2, 1);
}
