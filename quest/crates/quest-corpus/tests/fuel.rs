//! Fuel and pause points over the corpus (spec §6.2, §11 "Fuel/pause"; plan Task 33): a program run with a
//! small fuel budget per tick ends with the same output and script state as with unlimited fuel, and two
//! runs pause at the same points (the same per-tick fuel and state hash).

use std::sync::Arc;

use quest_corpus::{Case, RUN, config_of, trace_of, tree_of};
use quest_lang::compile;
use quest_stub::{Runner, StubConfig};
use quest_vm::Status;
use quest_vm::consts::MEM_BYTES_DEFAULT;

/// The budgets compared with unlimited fuel.
const FUELS: [u64; 3] = [7, 50, 333];

/// Ticks enough for any corpus case at 7 fuel per tick.
const TICKS_MAX: u64 = 2_000_000;

/// Runs the case under its own config but `fuel` per tick until it is quiescent or failed: the output and
/// the final script regions. (No per-tick hash, unlike the harness's trace: at 7 fuel a case runs for
/// hundreds of thousands of ticks.)
fn finish(case: &Case, fuel: u64) -> (Vec<String>, Vec<u8>) {
    let cfg = config_of(case);
    let program = compile(&tree_of(case), &quest_stub::stub_api())
        .unwrap_or_else(|d| panic!("{}: {d:?}", case.name))
        .program;
    let stub = StubConfig {
        latency: cfg.latency,
        failures: cfg.failures,
        chest: cfg.chest,
    };
    let mut runner = Runner::new(Arc::new(program), MEM_BYTES_DEFAULT, stub).expect(case.name);
    runner.vm.start_main();
    for _ in 0..TICKS_MAX {
        let report = runner.step(fuel);
        if report.status == Status::Failed || runner.quiescent() {
            return (runner.host.output, runner.vm.script_bytes().to_vec());
        }
    }
    panic!(
        "{} at fuel {fuel}: not finished after {TICKS_MAX} ticks",
        case.name
    );
}

#[test]
fn fuel_equivalence() {
    // Cases without request latency (the plan's "without promises"): a request is answered the tick after
    // it was made, whatever the budget.
    let cases: Vec<&Case> = RUN.iter().filter(|c| config_of(c).latency == 0).collect();
    assert!(cases.len() >= 10, "only {} cases", cases.len());
    let mut bad = Vec::new();
    for case in cases {
        let unlimited = finish(case, u64::MAX);
        for fuel in FUELS {
            let (output, script) = finish(case, fuel);
            if output != unlimited.0 || script != unlimited.1 {
                bad.push(format!(
                    "{} at fuel {fuel}: output {output:?}, unlimited {:?}; script bytes equal: {}",
                    case.name,
                    unlimited.0,
                    script == unlimited.1
                ));
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
