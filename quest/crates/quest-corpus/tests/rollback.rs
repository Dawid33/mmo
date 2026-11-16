//! The rollback bar over generated programs (spec §6.6, §11 "Rollback"): at any tick, checkpoint the VM
//! and keep the host's state; run `k` more ticks; roll back and restore: the state hash is what it was;
//! run the same `k` ticks again: the same hash at every tick and the same output (plan Task 33).
//!
//! The stub is drawn too — a request latency of 0–5 ticks and at most one failure rule (`Lost`, `Changed`
//! or `Timeout` on the nth `world.add`) — so checkpoints fall while handlers are parked on requests, and a
//! `Timeout` case checkpoints near the VM's deadline (`PROMISE_TIMEOUT_TICKS`), so the window may cross the
//! tick the promise times out in. Whether a random case parks is luck; `rollback_across_a_park` is a fixed
//! case that always does.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use proptest::prelude::*;
use proptest::test_runner::{TestCaseError, TestRunner};
use quest_corpus::generate;
use quest_lang::{SourceTree, compile};
use quest_stub::{FailureKind, FailureRule, Runner, StubConfig, StubHost};
use quest_vm::consts::{MEM_BYTES_DEFAULT, PROMISE_TIMEOUT_TICKS};

/// `k` ticks from the runner's current tick: the state hash after each.
fn ticks(runner: &mut Runner, fuel: u64, k: u64) -> Vec<u64> {
    (0..k)
        .map(|_| {
            runner.step(fuel);
            runner.vm.hash64()
        })
        .collect()
}

/// The stub: latency 0–5 and at most one failure on the nth `world.add`.
fn stub() -> impl Strategy<Value = StubConfig> {
    let kind = prop_oneof![
        Just(FailureKind::Lost),
        Just(FailureKind::Changed),
        Just(FailureKind::Timeout),
    ];
    let rule = proptest::option::of((kind, 1u32..4)).prop_map(|r| {
        r.map(|(kind, nth)| FailureRule {
            op: Some("world.add".to_string()),
            nth,
            kind,
        })
    });
    (0u64..=5, rule).prop_map(|(latency, rule)| StubConfig {
        latency,
        failures: rule.into_iter().collect(),
        chest: Vec::new(),
    })
}

#[derive(Debug)]
struct Input {
    text: String,
    stub: StubConfig,
    fuel: u64,
    /// Ticks before the checkpoint.
    before: u64,
    /// Ticks run, rolled back and run again.
    k: u64,
}

fn input() -> impl Strategy<Value = Input> {
    (
        generate::program(),
        stub(),
        prop_oneof![Just(7u64), Just(50), 1u64..3000],
        // Mostly early, while `main` and the handlers still run and park.
        prop_oneof![3 => 0u64..12, 1 => 0u64..60],
        1u64..30,
    )
        .prop_map(|(text, stub, fuel, before, k)| {
            // A `Timeout` checkpoints within 30 ticks of the deadline of a request made early on.
            let timeout = stub.failures.iter().any(|r| r.kind == FailureKind::Timeout);
            let before = if timeout {
                PROMISE_TIMEOUT_TICKS - 30 + before % 40
            } else {
                before
            };
            Input {
                text,
                stub,
                fuel,
                before,
                k,
            }
        })
}

/// What a checked case saw.
struct Checked {
    /// A request was outstanding at the checkpoint: a handler parked across it.
    parked: bool,
    /// The output at the end of the window.
    output: Vec<String>,
}

/// One case: checkpoint after `before` ticks, run `k`, roll back and restore the host, run `k` again.
fn check(i: &Input) -> Result<Checked, TestCaseError> {
    let text = &i.text;
    let tree = SourceTree {
        files: [("main.qs".to_string(), text.clone())].into(),
    };
    let program = compile(&tree, &quest_stub::stub_api())
        .map_err(|d| TestCaseError::fail(format!("{d:?}\n{text}")))?
        .program;
    let mut runner = Runner::new(Arc::new(program), MEM_BYTES_DEFAULT, i.stub.clone())
        .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
    runner.vm.start_main();
    ticks(&mut runner, i.fuel, i.before);

    let parked = runner.host.has_pending();
    let cp = runner.vm.checkpoint();
    let (host, now, hash): (StubHost, u64, u64) =
        (runner.host.clone(), runner.now, runner.vm.hash64());
    let first = ticks(&mut runner, i.fuel, i.k);
    let first_output = runner.host.output.clone();

    runner
        .vm
        .rollback_to(cp)
        .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
    prop_assert_eq!(
        runner.vm.hash64(),
        hash,
        "the hash after rollback\n{}",
        text
    );
    (runner.host, runner.now) = (host, now);
    let again = ticks(&mut runner, i.fuel, i.k);
    prop_assert_eq!(&again, &first, "per-tick hashes after rollback\n{}", text);
    prop_assert_eq!(
        &runner.host.output,
        &first_output,
        "output after rollback\n{}",
        text
    );
    Ok(Checked {
        parked,
        output: first_output,
    })
}

#[test]
fn rollback_bar_on_generated_programs() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(generate::cases()));
    if let Err(e) = runner.run(&input(), |i| check(&i).map(|_| ())) {
        panic!("{e}");
    }
}

/// The property's random stubs park handlers across the checkpoint in only some cases; this one always
/// does. `h` parks on a request at tick 1 that latency 3 answers at tick 4; the checkpoint is before tick
/// 2, and the window resumes `h` and runs it to the end.
#[test]
fn rollback_across_a_park() {
    let text = "let g = 0

@on(PlayerClicked)
fn h(e):
    g = g + 1
    let a = await world.add(\"n\", 5) else 0
    g = g + a
    print(f\"after {g}\")

fn main():
    emit(PlayerClicked(player=Player(id=1, name=\"ana\")))
";
    let input = Input {
        text: text.to_string(),
        stub: StubConfig {
            latency: 3,
            ..StubConfig::default()
        },
        fuel: 2_000,
        before: 2,
        k: 6,
    };
    let checked = check(&input).unwrap_or_else(|e| panic!("{e}"));
    assert!(checked.parked, "`h` is parked at the checkpoint");
    assert_eq!(checked.output, ["after 6"], "`h` resumed within the window");
}
