//! Checkpoints and rollback, the state hash, saving and loading a VM mid-execution (spec §6.4, §6.6, §6.8).

mod common;

use std::sync::Arc;

use common::*;
use proptest::prelude::*;
use quest_vm::BYTECODE_VERSION;
use quest_vm::FaultKind;
use quest_vm::consts::HEADER_WORDS;
use quest_vm::host::PromiseId;
use quest_vm::host::{AbortCause, Report};
use quest_vm::layout::{H_CURRENT_FN, H_FUEL_DEBT, H_MEM_BYTES, Layout, LoadError};
use quest_vm::program::Program;
use quest_vm::vm::{Status, Vm, VmConfig};
use quest_vm::wire::WireValue;

/// Smaller than the default memory: these tests hash it after every tick.
const MEM: u32 = 65_536;
/// Fuel per tick of the scripted run: `Ping`'s spin takes several ticks.
const FUEL: u64 = 25;
const SPIN: i16 = 30;
/// Ticks of the scripted run; everything it starts has ended by the last.
const TICKS: u64 = 50;

// Offsets in a save.
const AT_BYTECODE_VERSION: usize = 8;
const AT_HOST_API_VERSION: usize = 12;
const AT_MEM_LEN: usize = 24;
const AT_MEM: usize = 28;

fn small_cfg() -> VmConfig {
    VmConfig {
        mem_bytes: MEM,
        ..cfg()
    }
}

fn new_vm(p: &Arc<Program>) -> Vm {
    Vm::new(p.clone(), small_cfg()).expect("the scenario loads")
}

/// An input given to the VM before a tick.
#[derive(Clone, Debug)]
enum Input {
    Deliver(&'static str, i64),
    /// Answers the `request % n`-th request the host has seen (nothing when it has seen none).
    Resolve {
        request: usize,
        ok: bool,
        value: i64,
    },
}

fn apply(vm: &mut Vm, host: &RecordingHost, input: &Input) {
    match *input {
        Input::Deliver(event, n) => {
            vm.deliver(event, &sc_event(n));
        }
        Input::Resolve { request, ok, value } => {
            if host.requests.is_empty() {
                return;
            }
            let id = host.requests[request % host.requests.len()].0;
            let outcome = if ok {
                Ok(WireValue::Int(value))
            } else {
                Err(WireValue::Variant {
                    name: "NotFound".into(),
                    fields: vec![],
                })
            };
            vm.resolve(id, outcome);
        }
    }
}

/// The scripted inputs: a `Ping` (multi-tick, then parked on its promise), a `Pong` queued behind it, a
/// `Pong` that faults, a second `Ping`; both promises answered late, one with an error.
fn script(t: u64) -> Vec<Input> {
    match t {
        1 => vec![Input::Deliver("Ping", 4)],
        2 => vec![Input::Deliver("Pong", 1)],
        4 => vec![Input::Deliver("Pong", 0)],
        6 => vec![Input::Deliver("Ping", 9)],
        24 => vec![Input::Resolve {
            request: 0,
            ok: true,
            value: 5,
        }],
        34 => vec![Input::Resolve {
            request: 1,
            ok: false,
            value: 0,
        }],
        _ => vec![],
    }
}

/// Tick `t` of the scripted run: its inputs, then the tick.
fn step(vm: &mut Vm, host: &mut RecordingHost, t: u64) -> Status {
    for input in script(t) {
        apply(vm, host, &input);
    }
    vm.tick(t, FUEL, host).status
}

#[test]
fn save_load_at_every_tick_resumes_identically() {
    let p = scenario(SPIN);
    let mut vm = new_vm(&p);
    let mut host = RecordingHost::default();
    let mut hashes = vec![vm.hash64()];
    let mut statuses = vec![];
    for t in 1..=TICKS {
        statuses.push(step(&mut vm, &mut host, t));
        hashes.push(vm.hash64());
    }
    // the scenario does what it claims
    assert!(vm.is_idle());
    assert_eq!(host.requests.len(), 2);
    assert_eq!(host.reports.len(), 1, "the faulting Pong");
    assert!(
        statuses[1..4].iter().all(|s| *s == Status::Busy),
        "{statuses:?}"
    );
    assert!(
        statuses[18..23].iter().all(|s| *s == Status::Idle),
        "parked: {statuses:?}"
    );
    let want = host.transcript();

    let mut vm = new_vm(&p);
    let mut host = RecordingHost::default();
    for t in 0..=TICKS {
        if t > 0 {
            step(&mut vm, &mut host, t);
        }
        let save = vm.save();
        let mut loaded = Vm::load(p.clone(), small_cfg(), &save).expect("the save loads");
        assert_eq!(
            loaded.log_len(),
            0,
            "a loaded VM starts with an empty undo log"
        );
        assert_eq!(loaded.mem_bytes(), vm.mem_bytes());
        assert_eq!(loaded.hash64(), hashes[t as usize], "loaded at {t}");
        let mut loaded_host = host.clone();
        for u in t + 1..=TICKS {
            step(&mut loaded, &mut loaded_host, u);
            assert_eq!(
                loaded.hash64(),
                hashes[u as usize],
                "saved at {t}, tick {u}"
            );
        }
        assert_eq!(loaded_host.transcript(), want, "saved at {t}");
    }
}

/// A save of the scripted run after `ticks` ticks.
fn save_at(p: &Arc<Program>, ticks: u64) -> Vec<u8> {
    let mut vm = new_vm(p);
    let mut host = RecordingHost::default();
    for t in 1..=ticks {
        step(&mut vm, &mut host, t);
    }
    vm.save()
}

#[test]
fn load_refuses_another_program() {
    let save = save_at(&scenario(SPIN), 3);
    let other = scenario(SPIN + 1);
    assert_eq!(
        Vm::load(other, small_cfg(), &save).err(),
        Some(LoadError::ProgramMismatch)
    );
}

#[test]
fn load_refuses_other_versions() {
    let p = scenario(SPIN);
    let save = save_at(&p, 3);
    let patched = |at: usize, v: u32| {
        let mut s = save.clone();
        s[at..at + 4].copy_from_slice(&v.to_le_bytes());
        s
    };
    assert_eq!(
        Vm::load(
            p.clone(),
            small_cfg(),
            &patched(AT_BYTECODE_VERSION, BYTECODE_VERSION + 1)
        )
        .err(),
        Some(LoadError::VersionMismatch {
            found: BYTECODE_VERSION + 1
        })
    );
    assert_eq!(
        Vm::load(
            p.clone(),
            small_cfg(),
            &patched(AT_HOST_API_VERSION, API + 1)
        )
        .err(),
        Some(LoadError::HostApiMismatch { found: API + 1 })
    );
    let other_host = VmConfig {
        host_api_version: API + 1,
        ..small_cfg()
    };
    assert_eq!(
        Vm::load(p, other_host, &save).err(),
        Some(LoadError::HostApiMismatch { found: API })
    );
}

#[test]
fn load_refuses_wrong_length() {
    let p = scenario(SPIN);
    let save = save_at(&p, 3);
    let invalid = |bytes: &[u8]| {
        matches!(
            Vm::load(p.clone(), small_cfg(), bytes),
            Err(LoadError::Invalid(_))
        )
    };
    assert!(invalid(&save[..save.len() - 8]), "truncated");
    assert!(invalid(&save[..AT_MEM + 8]), "cut inside the memory");
    let mut longer = save.clone();
    longer.push(0);
    assert!(invalid(&longer), "a trailing byte");
    let mut len_field = save.clone();
    len_field[AT_MEM_LEN..AT_MEM_LEN + 4].copy_from_slice(&(MEM - 8).to_le_bytes());
    assert!(
        invalid(&len_field),
        "the length field disagrees with the bytes"
    );
    let mut header = save.clone();
    let at = AT_MEM + H_MEM_BYTES as usize * 8;
    header[at..at + 8].copy_from_slice(&u64::from(MEM / 2).to_le_bytes());
    assert!(invalid(&header), "the header's mem_bytes disagrees");
}

#[test]
fn load_checks_mem_bytes() {
    let p = scenario(SPIN);
    let save = save_at(&p, 3);
    let larger = VmConfig {
        mem_bytes: MEM * 2,
        ..small_cfg()
    };
    assert!(matches!(
        Vm::load(p.clone(), larger, &save),
        Err(LoadError::Invalid(_))
    ));
    assert!(Vm::load(p, small_cfg(), &save).is_ok());
}

/// A save made mid-task (tick 1 starts `Ping`, whose spin outlasts its fuel) with header word `w` set to
/// `v`: it loads, and its next tick fails the instance with a `Corrupt` report.
fn corrupt_on_tick(w: u32, v: u64) {
    let p = scenario(SPIN);
    let mut save = save_at(&p, 1);
    let at = AT_MEM + w as usize * 8;
    save[at..at + 8].copy_from_slice(&v.to_le_bytes());
    let mut vm = Vm::load(p, small_cfg(), &save).expect("the envelope and the layout are intact");
    let mut host = RecordingHost::default();
    assert_eq!(vm.tick(2, FUEL, &mut host).status, Status::Failed);
    assert!(
        host.reports.iter().any(|r| matches!(
            r,
            Report::HandlerAborted {
                cause: AbortCause::Fault {
                    kind: FaultKind::Corrupt,
                    ..
                },
                ..
            }
        )),
        "{:?}",
        host.reports
    );
}

#[test]
fn load_of_absurd_fuel_debt_is_corrupt() {
    corrupt_on_tick(H_FUEL_DEBT, u64::MAX / 2);
}

#[test]
fn load_of_unknown_fn_is_corrupt() {
    corrupt_on_tick(H_CURRENT_FN, 999);
}

/// Where a bit flip lands: anywhere, the envelope + header + static slots, the start of the heap, or the
/// runtime regions (inbox, queue, promises, parked, commands, trace).
fn flip_range(sel: u8, p: &Program, len: usize) -> (usize, usize) {
    let l = Layout::plan(u32::from(p.static_words), MEM).expect("layout");
    let at = |off: u32| AT_MEM + off as usize;
    let range = match sel {
        0 => (0, len),
        1 => (0, at(l.heap_base)),
        2 => (at(l.heap_base), at(l.heap_base) + 1024),
        _ => (at(l.inbox_base), at(MEM)),
    };
    (range.0.min(len), range.1.min(len))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn load_of_damaged_bytes_never_panics(
        ticks in 0..TICKS,
        truncate in proptest::option::of(any::<u32>()),
        flips in proptest::collection::vec((0u8..4, any::<u32>(), 0u8..8), 1..8),
    ) {
        let p = scenario(SPIN);
        let mut save = save_at(&p, ticks);
        for (sel, at, bit) in flips {
            let (lo, hi) = flip_range(sel, &p, save.len());
            if lo < hi {
                save[lo + at as usize % (hi - lo)] ^= 1 << bit;
            }
        }
        if let Some(n) = truncate {
            save.truncate(n as usize % (save.len() + 1));
        }
        if let Ok(mut vm) = Vm::load(p.clone(), small_cfg(), &save) {
            let mut host = RecordingHost::default();
            vm.deliver("Ping", &sc_event(1));
            vm.deliver("Pong", &sc_event(0));
            for serial in 1..4u64 {
                vm.resolve(PromiseId(1 << 32 | serial), Ok(WireValue::Int(2)));
            }
            for t in 0..50 {
                vm.tick(100 + t, 1_000, &mut host);
            }
        }
    }

    #[test]
    fn rollback_bar(
        events in any::<bool>(),
        spin in 0i16..40,
        fuel in proptest::collection::vec(1u64..120, 30),
        inputs in proptest::collection::vec(proptest::option::of(prop_oneof![
            (0i64..3).prop_map(|n| Input::Deliver("Ping", n)),
            (0i64..3).prop_map(|n| Input::Deliver("Pong", n)),
            (any::<usize>(), any::<bool>(), 0i64..100)
                .prop_map(|(request, ok, value)| Input::Resolve { request, ok, value }),
        ]), 30),
        at in 0usize..30,
        k in 1usize..30,
    ) {
        // the event-driven scenario, or `main` alone (calls across frames) run by `debug_run`
        let p = if events { scenario(spin) } else { sum_loop(40, true) };
        let mut vm = new_vm(&p);
        if !events {
            vm.debug_start(0);
        }
        let step = |vm: &mut Vm, host: &mut RecordingHost, t: usize| {
            if let Some(input) = &inputs[t % inputs.len()] {
                apply(vm, host, input);
            }
            let f = fuel[t % fuel.len()];
            if events {
                vm.tick(t as u64 + 1, f, host);
            } else {
                vm.debug_run(f, host);
            }
        };
        let mut host = RecordingHost::default();
        for t in 0..at {
            step(&mut vm, &mut host, t);
        }
        let cp = vm.checkpoint();
        let (before, host_before) = (vm.hash64(), host.clone());
        let mut first = vec![];
        for t in at..at + k {
            step(&mut vm, &mut host, t);
            first.push((vm.hash64(), host.transcript()));
        }
        prop_assert_eq!(vm.rollback_to(cp), Ok(()));
        prop_assert_eq!(vm.hash64(), before);
        host = host_before;
        for (i, t) in (at..at + k).enumerate() {
            step(&mut vm, &mut host, t);
            prop_assert_eq!(&(vm.hash64(), host.transcript()), &first[i], "tick {}", t);
        }
    }
}

/// Fuel per tick of the long loop.
const LOOP_FUEL: u64 = 100;
/// One tick's undo log is bounded by its fuel (spec §6.6): a unit of fuel buys at most one instruction,
/// which logs at most 3 words (the word written, and on its first write in a segment its shadow and dirty
/// word); the tick's own bookkeeping writes header words, each at most once.
const LOG_BOUND: usize = LOOP_FUEL as usize * 3 + HEADER_WORDS as usize;

#[test]
fn forget_before_bounds_the_log() {
    let p = scenario(i16::MAX);
    let mut vm = new_vm(&p);
    let mut host = RecordingHost::default();
    vm.deliver("Ping", &sc_event(1));
    let mut most = 0;
    for t in 1..=1_000 {
        let cp = vm.checkpoint();
        vm.forget_before(cp);
        assert_eq!(
            vm.tick(t, LOOP_FUEL, &mut host).status,
            Status::Busy,
            "tick {t}"
        );
        most = most.max(vm.log_len());
    }
    assert!(most < LOG_BOUND, "{most} logged words in one tick");
}
