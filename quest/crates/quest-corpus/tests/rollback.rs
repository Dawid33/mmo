//! The rollback bar over generated programs (spec §6.6, §11 "Rollback"): at any tick, checkpoint the VM
//! and keep the host's state; run `k` more ticks; roll back and restore: the state hash is what it was;
//! run the same `k` ticks again: the same hash at every tick and the same output (plan Task 33).
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use proptest::prelude::*;
use quest_corpus::r#gen;
use quest_lang::{SourceTree, compile};
use quest_stub::{Runner, StubConfig, StubHost};
use quest_vm::consts::MEM_BYTES_DEFAULT;

/// `k` ticks from the runner's current tick: the state hash after each.
fn ticks(runner: &mut Runner, fuel: u64, k: u64) -> Vec<u64> {
    (0..k)
        .map(|_| {
            runner.step(fuel);
            runner.vm.hash64()
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(r#gen::cases()))]

    #[test]
    fn rollback_bar_on_generated_programs(
        text in r#gen::program(),
        fuel in prop_oneof![Just(7u64), Just(50), 1u64..3000],
        before in 0u64..60,
        k in 1u64..30,
    ) {
        let tree = SourceTree { files: [("main.qs".to_string(), text.clone())].into() };
        let program = compile(&tree, &quest_stub::stub_api())
            .map_err(|d| TestCaseError::fail(format!("{d:?}\n{text}")))?
            .program;
        let mut runner = Runner::new(Arc::new(program), MEM_BYTES_DEFAULT, StubConfig::default())
            .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
        runner.vm.start_main();
        ticks(&mut runner, fuel, before);

        let cp = runner.vm.checkpoint();
        let (host, now, hash): (StubHost, u64, u64) = (runner.host.clone(), runner.now, runner.vm.hash64());
        let first = ticks(&mut runner, fuel, k);
        let first_output = runner.host.output.clone();

        runner.vm.rollback_to(cp).map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
        prop_assert_eq!(runner.vm.hash64(), hash, "the hash after rollback\n{}", text);
        (runner.host, runner.now) = (host, now);
        let again = ticks(&mut runner, fuel, k);
        prop_assert_eq!(&again, &first, "per-tick hashes after rollback\n{}", text);
        prop_assert_eq!(&runner.host.output, &first_output, "output after rollback\n{}", text);
    }
}
