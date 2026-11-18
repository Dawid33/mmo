//! A saved state of a corpus program: the first byte picks the run case (`quest_corpus::RUN[byte % len]`,
//! the index `examples/dump_seeds.rs` writes), the rest is the state. `Vm::load` is `Err`, or a VM that runs
//! [`TICKS`] ticks of [`FUEL`] without a panic (ending `Failed`, a corrupt memory, is fine). A case that
//! does not compile or does not load in a plain `Runner` (a quest's home needs its roles) is skipped.
#![no_main]

use std::sync::{Arc, OnceLock};

use libfuzzer_sys::fuzz_target;
use quest_corpus::{RUN, tree_of};
use quest_stub::{Runner, StubConfig, vm_config};
use quest_vm::consts::MEM_BYTES_DEFAULT;
use quest_vm::{Program, Status, Vm};

/// Ticks a loaded state runs.
const TICKS: u64 = 200;
/// Fuel per tick.
const FUEL: u64 = 2_000;

/// Each run case's program, when it compiles and loads in a plain `Runner`.
fn programs() -> &'static [Option<Arc<Program>>] {
    static PROGRAMS: OnceLock<Vec<Option<Arc<Program>>>> = OnceLock::new();
    PROGRAMS.get_or_init(|| {
        let api = quest_stub::stub_api();
        RUN.iter()
            .map(|case| {
                let program = Arc::new(quest_lang::compile(&tree_of(case), &api).ok()?.program);
                Runner::new(program.clone(), MEM_BYTES_DEFAULT, StubConfig::default()).ok()?;
                Some(program)
            })
            .collect()
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((&pick, state)) = data.split_first() else {
        return;
    };
    let programs = programs();
    let Some(program) = &programs[usize::from(pick) % programs.len()] else {
        return;
    };
    let Ok(vm) = Vm::load(
        program.clone(),
        vm_config(MEM_BYTES_DEFAULT, 0, Vec::new()),
        state,
    ) else {
        return;
    };
    let mut runner = Runner::new(program.clone(), MEM_BYTES_DEFAULT, StubConfig::default())
        .expect("checked when the programs were made");
    runner.vm = vm;
    for _ in 0..TICKS {
        match runner.step(FUEL).status {
            Status::Idle | Status::Busy => {}
            Status::Failed => break,
        }
    }
});
