//! Writes the fuzz targets' seeds from the corpus (`quest/fuzz/seeds.sh` runs it): under `<out>/`,
//! `program_bytes/<case>` — every compiling run case's `Program::to_bytes()`; `vm_load/<case>` — the case's
//! index in [`RUN`] as one byte, then its VM saved halfway through its run (cases that load in a plain
//! `Runner`); `wire_decode/<name>` — encoded wire values: each world case's role values and a few built here.
//!
//! `cargo run --example dump_seeds -p quest-corpus -- <out>`.
#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::Path;
use std::sync::Arc;

use quest_corpus::{RUN, config_of, tree_of};
use quest_stub::{People, Runner, StubConfig, parse_people, parse_wire};
use quest_vm::Status;
use quest_vm::consts::MEM_BYTES_DEFAULT;
use quest_vm::wire::WireValue;

fn write(dir: &Path, name: &str, bytes: &[u8]) {
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

fn encoded(v: &WireValue) -> Vec<u8> {
    let mut out = Vec::new();
    v.encode(&mut out);
    out
}

/// A runner of `program` under the case's stub config with `main` asked for.
fn runner(program: &Arc<quest_vm::Program>, case: &quest_corpus::Case) -> Option<Runner> {
    let cfg = config_of(case);
    let stub = StubConfig {
        latency: cfg.latency,
        failures: cfg.failures,
        chest: cfg.chest,
        ..StubConfig::default()
    };
    let mut r = Runner::new(program.clone(), MEM_BYTES_DEFAULT, stub).ok()?;
    r.vm.start_main();
    Some(r)
}

/// Steps until failed, quiescent or `limit` ticks; the ticks run.
fn run(r: &mut Runner, fuel: u64, limit: u64) -> u64 {
    for t in 0..limit {
        if r.step(fuel).status == Status::Failed || r.quiescent() {
            return t + 1;
        }
    }
    limit
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("usage: dump_seeds <out dir>");
    let out = Path::new(&out);
    let api = quest_stub::stub_api();
    assert!(RUN.len() <= 256, "vm_load picks a case with one byte");
    for (index, case) in RUN.iter().enumerate() {
        let Ok(compiled) = quest_lang::compile(&tree_of(case), &api) else {
            continue;
        };
        let program = Arc::new(compiled.program);
        write(&out.join("program_bytes"), case.name, &program.to_bytes());

        let cfg = config_of(case);
        if let Some(mut r) = runner(&program, case) {
            let half = run(&mut r, cfg.fuel, cfg.ticks) / 2;
            let mut r = runner(&program, case).expect("it loaded once");
            run(&mut r, cfg.fuel, half);
            let mut seed = vec![index as u8];
            seed.extend_from_slice(&r.vm.save());
            write(&out.join("vm_load"), case.name, &seed);
        }

        let people = case
            .people
            .map_or_else(|| Ok(People::default()), parse_people)
            .unwrap_or_default();
        for (role, text) in &cfg.roles {
            if let Ok(v) = parse_wire(text, &people) {
                write(
                    &out.join("wire_decode"),
                    &format!("{}-{role}", case.name),
                    &encoded(&v),
                );
            }
        }
    }
    let built = [
        ("unit", WireValue::Unit),
        ("bool", WireValue::Bool(true)),
        ("int", WireValue::Int(-42)),
        ("float", WireValue::Float(1.5)),
        ("nan", WireValue::Float(f64::NAN)),
        ("str", WireValue::Str("gold".into())),
        (
            "nested",
            WireValue::List(vec![
                WireValue::Dict(vec![(WireValue::Str("k".into()), WireValue::Int(1))]),
                WireValue::Struct {
                    name: "Person".into(),
                    fields: vec![("id".into(), WireValue::Int(3))],
                },
                WireValue::Variant {
                    name: "Some".into(),
                    fields: vec![("0".into(), WireValue::Bool(false))],
                },
            ]),
        ),
    ];
    for (name, v) in &built {
        write(&out.join("wire_decode"), name, &encoded(v));
    }
}
