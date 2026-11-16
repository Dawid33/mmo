//! Fuel and pause points over the corpus and generated programs (spec §6.2, §11 "Fuel/pause"; plan Task 33): a program run with a
//! small fuel budget per tick ends with the same output and script state as with unlimited fuel, and two
//! runs pause at the same points (the same per-tick fuel and state hash).
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use quest_corpus::{CaseConfig, RUN, config_of, trace_of, tree_of};
use quest_lang::{SourceTree, compile};
use quest_stub::{Runner, StubConfig};
use quest_vm::Status;
use quest_vm::consts::MEM_BYTES_DEFAULT;

/// The budgets compared with unlimited fuel.
const FUELS: [u64; 3] = [7, 50, 333];

/// Ticks beyond `total fuel / budget` a run may take (ticks spent waiting on requests and timeouts).
const TICKS_SLACK: u64 = 10_000;

/// The most fuel a generated program may take in all to be used here (it is run at 7 per tick).
const GENERATED_FUEL_MAX: u64 = 1_000_000;

/// The end of a run.
struct End {
    output: Vec<String>,
    script: Vec<u8>,
    /// Fuel used over all ticks.
    fuel: u64,
}

/// Runs `program` against a stub built from `cfg` with `fuel` per tick until it is quiescent or failed: the
/// output, the final script regions and the fuel used, or `None` after `ticks` ticks. (No per-tick hash,
/// unlike the harness's trace: at 7 fuel a case runs for hundreds of thousands of ticks.)
fn finish(tree: &SourceTree, cfg: &CaseConfig, fuel: u64, ticks: u64) -> Option<End> {
    let program = compile(tree, &quest_stub::stub_api())
        .expect("compiles")
        .program;
    let stub = StubConfig {
        latency: cfg.latency,
        failures: cfg.failures.clone(),
        chest: cfg.chest.clone(),
    };
    let mut runner = Runner::new(Arc::new(program), MEM_BYTES_DEFAULT, stub).expect("loads");
    runner.vm.start_main();
    let mut used = 0u64;
    for _ in 0..ticks {
        let report = runner.step(fuel);
        used = used.saturating_add(report.fuel_used);
        if report.status == Status::Failed || runner.quiescent() {
            return Some(End {
                script: runner.vm.script_bytes().to_vec(),
                output: runner.host.output,
                fuel: used,
            });
        }
    }
    None
}

/// The run with unlimited fuel (at most `TICKS_SLACK` ticks).
fn unlimited(tree: &SourceTree, cfg: &CaseConfig) -> End {
    finish(tree, cfg, u64::MAX, TICKS_SLACK).expect("finishes with unlimited fuel")
}

/// Each budget of [`FUELS`] against the unlimited run: a line per difference.
fn differences(name: &str, tree: &SourceTree, cfg: &CaseConfig, unlimited: &End) -> Vec<String> {
    let mut bad = Vec::new();
    for fuel in FUELS {
        let ticks = unlimited.fuel / fuel + TICKS_SLACK;
        let Some(end) = finish(tree, cfg, fuel, ticks) else {
            bad.push(format!(
                "{name} at fuel {fuel}: not finished after {ticks} ticks"
            ));
            continue;
        };
        if end.output != unlimited.output || end.script != unlimited.script {
            bad.push(format!(
                "{name} at fuel {fuel}: output {:?}, unlimited {:?}; script bytes equal: {}",
                end.output,
                unlimited.output,
                end.script == unlimited.script
            ));
        }
    }
    bad
}

/// How many generated programs are also run.
const GENERATED: usize = 8;

#[test]
fn fuel_equivalence() {
    // Every corpus case under its own config (promises, latency and failures included)…
    let mut bad = Vec::new();
    for case in RUN {
        let (tree, cfg) = (tree_of(case), config_of(case));
        bad.extend(differences(case.name, &tree, &cfg, &unlimited(&tree, &cfg)));
    }
    // …and the first few generated programs (from a fixed seed) under the default stub that take at most
    // `GENERATED_FUEL_MAX` fuel (a generated program may take tens of millions: nested loops of calls).
    {
        use proptest::strategy::{Strategy, ValueTree};
        let mut rng = proptest::test_runner::TestRunner::deterministic();
        let cfg = CaseConfig::default();
        let mut used = 0;
        for n in 0.. {
            if used == GENERATED {
                break;
            }
            let text = quest_corpus::generate::program()
                .new_tree(&mut rng)
                .expect("a program")
                .current();
            let tree = SourceTree {
                files: [("main.qs".to_string(), text)].into(),
            };
            let end = unlimited(&tree, &cfg);
            if end.fuel <= GENERATED_FUEL_MAX {
                bad.extend(differences(&format!("generated #{n}"), &tree, &cfg, &end));
                used += 1;
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn pause_points_repeat() {
    for case in RUN {
        let cfg = config_of(case);
        let a = trace_of(case, &cfg).expect(case.name);
        let b = trace_of(case, &cfg).expect(case.name);
        assert_eq!(a.ticks, b.ticks, "{}: per-tick fuel and hashes", case.name);
        assert_eq!(a.output, b.output, "{}", case.name);
    }
}
