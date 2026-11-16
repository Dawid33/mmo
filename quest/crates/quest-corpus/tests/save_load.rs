//! Save and load mid-run (spec §11 "Serialization"; plan Task 33): for every run case and each of its first
//! 200 ticks, a run that saves its VM before that tick, loads the save into a fresh instance and continues
//! goes on exactly as the uninterrupted run — parked handlers and pending promises included.
//!
//! The uninterrupted run reproduces the case's golden (checked here once), and the resumed run matches it
//! tick by tick: the same fuel used, the same memory bytes (so the same golden hash), the same final state
//! hash, output and reports — so the remaining golden lines are identical. Memory is compared rather than
//! the golden's FNV hash because hashing 256 KiB at every tick of every resumed run is what would make
//! this suite slow.

use std::sync::Arc;

use quest_corpus::{CaseConfig, RUN, config_of, golden_of, report_line, tree_of};
use quest_lang::compile;
use quest_stub::{Runner, StubConfig};
use quest_vm::consts::MEM_BYTES_DEFAULT;
use quest_vm::{Program, Status, Vm, VmConfig};

/// How many leading ticks of each case are tried.
const TICKS_TRIED: u64 = 200;

/// The configuration `Runner::new` gives its VM.
fn vm_config() -> VmConfig {
    let api = quest_stub::stub_api();
    VmConfig {
        mem_bytes: MEM_BYTES_DEFAULT,
        epoch: 1,
        host_api_version: api.version,
        events: api.events.into_iter().map(|e| e.name).collect(),
    }
}

/// A runner as the harness makes it, with `main` asked for.
fn runner(program: &Arc<Program>, cfg: &CaseConfig) -> Runner {
    let stub = StubConfig {
        latency: cfg.latency,
        failures: cfg.failures.clone(),
        chest: cfg.chest.clone(),
    };
    let mut r = Runner::new(program.clone(), MEM_BYTES_DEFAULT, stub).expect("the case loads");
    r.vm.start_main();
    r
}

/// Runs the case with a save and load before tick `at`, beside an uninterrupted run, as the harness runs
/// it (until quiescent, failed or the config's tick limit); the first difference, if any.
fn resumed_differs(program: &Arc<Program>, cfg: &CaseConfig, at: u64) -> Option<String> {
    let (mut plain, mut resumed) = (runner(program, cfg), runner(program, cfg));
    for tick in 0..cfg.ticks {
        if tick == at {
            let bytes = resumed.vm.save();
            resumed.vm = match Vm::load(program.clone(), vm_config(), &bytes) {
                Ok(vm) => vm,
                Err(e) => return Some(format!("load failed: {e:?}")),
            };
        }
        let (a, b) = (plain.step(cfg.fuel), resumed.step(cfg.fuel));
        if a.fuel_used != b.fuel_used || plain.vm.mem_bytes() != resumed.vm.mem_bytes() {
            return Some(format!(
                "tick {tick}: fuel {} vs {}, memory equal: {}",
                b.fuel_used,
                a.fuel_used,
                plain.vm.mem_bytes() == resumed.vm.mem_bytes()
            ));
        }
        if a.status == Status::Failed || plain.quiescent() {
            break;
        }
    }
    let reports = |r: &Runner| -> Vec<String> { r.host.reports.iter().map(report_line).collect() };
    if plain.vm.hash64() != resumed.vm.hash64()
        || plain.host.output != resumed.host.output
        || reports(&plain) != reports(&resumed)
        || !resumed.quiescent() && plain.quiescent()
    {
        return Some(format!(
            "at the end: output {:?} vs {:?}, reports {:?} vs {:?}",
            resumed.host.output,
            plain.host.output,
            reports(&resumed),
            reports(&plain)
        ));
    }
    None
}

#[test]
fn save_load_at_every_tick() {
    let mut bad = Vec::new();
    for case in RUN {
        // The uninterrupted run is the golden one.
        let golden = golden_of(case);
        assert_eq!(Some(golden.as_str()), case.golden, "{}: golden", case.name);
        let ticks = golden.lines().filter(|l| l.starts_with("tick ")).count() as u64;
        let cfg = config_of(case);
        let program = Arc::new(
            compile(&tree_of(case), &quest_stub::stub_api())
                .expect(case.name)
                .program,
        );
        for at in 0..ticks.min(TICKS_TRIED) {
            if let Some(why) = resumed_differs(&program, &cfg, at) {
                bad.push(format!("{} saved before tick {at}: {why}", case.name));
                break;
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
