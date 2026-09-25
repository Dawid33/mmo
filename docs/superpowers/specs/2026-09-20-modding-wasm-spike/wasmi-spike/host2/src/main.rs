//! Throwaway: run the SAME instrumented guest bytes on several native engines and compare.
use std::time::Instant;

#[derive(Default)]
pub struct Meter { gas: u64, used: u64, stack: u64 }
const STACK_LIMIT: u64 = 64 * 1024;

macro_rules! engine {
    ($modname:ident, $krate:ident, $inst:ident, $err:expr) => {
        mod $modname {
            use super::*;
            use $krate::*;
            pub fn run(label: &str, bytes: &[u8], ticks: u32, budget: u64) {
                let engine = Engine::default();
                let module = Module::new(&engine, bytes).unwrap();
                let mut store = Store::new(&engine, Meter { gas: u64::MAX, ..Default::default() });
                let mut linker = <Linker<Meter>>::new(&engine);
                linker.func_wrap("env", "host_add", |a: u32, b: u32| -> u32 { a.wrapping_add(b) }).unwrap();
                linker.func_wrap("meter", "finite_wasm_gas", |mut c: Caller<'_, Meter>, n: u64| -> Result<(), Error> {
                    let m = c.data_mut();
                    if m.gas < n { return Err($err("out of gas")); }
                    m.gas -= n; m.used += n; Ok(())
                }).unwrap();
                linker.func_wrap("meter", "finite_wasm_stack", |mut c: Caller<'_, Meter>, ops: u64, frame: u64| -> Result<(), Error> {
                    let m = c.data_mut(); m.stack += ops + frame;
                    if m.stack > STACK_LIMIT { return Err($err("stack limit")); } Ok(())
                }).unwrap();
                linker.func_wrap("meter", "finite_wasm_unstack", |mut c: Caller<'_, Meter>, ops: u64, frame: u64| { c.data_mut().stack -= ops + frame; }).unwrap();
                for name in ["finite_wasm_memory_copy", "finite_wasm_memory_fill", "finite_wasm_memory_init",
                             "finite_wasm_table_copy", "finite_wasm_table_fill", "finite_wasm_table_init"] {
                    linker.func_wrap("meter", name, |n: u32, _a: u64, _b: u64| -> u32 { n }).unwrap();
                }
                let inst = linker.$inst(&mut store, &module).unwrap();
                let init = inst.get_typed_func::<(), ()>(&mut store, "init").unwrap();
                let tick = inst.get_typed_func::<(), u32>(&mut store, "tick").unwrap();
                let divbits = inst.get_typed_func::<(f32, f32), u32>(&mut store, "divbits").unwrap();
                let deficit = inst.get_global(&mut store, "gas_deficit");
                if let Some(g) = &deficit { g.set(&mut store, Val::I64(-(1i64 << 62))).unwrap(); }
                init.call(&mut store, ()).unwrap();
                let t = Instant::now();
                let mut sum = 0;
                for _ in 0..ticks { sum = tick.call(&mut store, ()).unwrap(); }
                let us = t.elapsed().as_secs_f64() * 1e6 / ticks as f64;
                let used = match &deficit { Some(g) => (g.get(&mut store).i64().unwrap() + (1i64 << 62)) as u64, None => store.data().used };
                let nan = divbits.call(&mut store, (0.0, 0.0)).unwrap();
                // Deterministic exhaustion: fresh budget, count ticks until the trap.
                init.call(&mut store, ()).unwrap();
                { let m = store.data_mut(); m.gas = budget; m.used = 0; m.stack = 0; }
                if let Some(g) = &deficit { g.set(&mut store, Val::I64(-(budget as i64))).unwrap(); }
                let (mut done, mut last) = (0u32, 0u32);
                while let Ok(s) = tick.call(&mut store, ()) { done += 1; last = s; if done >= 5000 { break; } }
                println!("{label:<28} tick {us:>9.2} us  sum {sum:>10}  gas/2000t {used:>10}  0/0 bits {nan:#010x}  budget: {done} ticks, last sum {last}, used {}",
                    match &deficit { Some(g) => (g.get(&mut store).i64().unwrap() + budget as i64) as u64, None => store.data().used });
            }
        }
    };
}
engine!(e_wasmi2, wasmi, instantiate_and_start, |m: &str| Error::new(m.to_string()));
engine!(e_wasmi1, wasmi1, instantiate_and_start, |m: &str| Error::new(m.to_string()));
engine!(e_wasmtime, wasmtime, instantiate, |m: &str| Error::msg(m.to_string()));
fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let name = std::path::Path::new(f).file_name().unwrap().to_str().unwrap().to_string();
        e_wasmi2::run(&format!("wasmi2.0   {name}"), &bytes, 2000, 5_000_000);
        e_wasmi1::run(&format!("wasmi1.1   {name}"), &bytes, 2000, 5_000_000);
        e_wasmtime::run(&format!("wasmtime48 {name}"), &bytes, 2000, 5_000_000);
    }
}
