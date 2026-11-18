//! Any bytes as a program: `Program::from_bytes` is `Ok` or `Err`; a program that loads and fits the stub's
//! host API (`quest_stub::bind`, what a host checks before it makes an instance) runs `main` for
//! [`TICKS`] ticks of [`FUEL`] in a `Runner` without a panic.
#![no_main]

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use quest_stub::{Runner, StubConfig};
use quest_vm::consts::MEM_BYTES_DEFAULT;
use quest_vm::{Program, Status};

/// Ticks each loaded program runs.
const TICKS: u64 = 200;
/// Fuel per tick.
const FUEL: u64 = 2_000;

fuzz_target!(|data: &[u8]| {
    let Ok(program) = Program::from_bytes(data) else {
        return;
    };
    if quest_stub::bind(&program).is_err() {
        return;
    }
    let Ok(mut runner) = Runner::new(Arc::new(program), MEM_BYTES_DEFAULT, StubConfig::default())
    else {
        return;
    };
    runner.vm.start_main();
    for _ in 0..TICKS {
        match runner.step(FUEL).status {
            Status::Idle | Status::Busy => {}
            Status::Failed => break,
        }
    }
});
