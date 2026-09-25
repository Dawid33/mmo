import { readFileSync } from 'node:fs';
const dir = new URL('.', import.meta.url).pathname;
const hostMod = new WebAssembly.Module(readFileSync(dir + 'webhost/target/wasm32-unknown-unknown/release/webhost.wasm'));
let guest = null;
const glue = {
  mod_tick: () => guest.tick(),
  mod_noop: (x) => guest.noop(x),
  copy_to_guest: (gp, hp, len) => new Uint8Array(guest.memory.buffer, gp, len).set(new Uint8Array(host.memory.buffer, hp, len)),
  copy_from_guest: (hp, gp, len) => new Uint8Array(host.memory.buffer, hp, len).set(new Uint8Array(guest.memory.buffer, gp, len)),
};
const host = new WebAssembly.Instance(hostMod, { glue }).exports;
const imports = {
  env: { host_add: host.h_host_add },
  meter: { finite_wasm_gas: host.h_gas, finite_wasm_stack: host.h_stack, finite_wasm_unstack: host.h_unstack,
    finite_wasm_memory_copy: host.h_memop, finite_wasm_memory_fill: host.h_memop, finite_wasm_memory_init: host.h_memop,
    finite_wasm_table_copy: host.h_memop, finite_wasm_table_fill: host.h_memop, finite_wasm_table_init: host.h_memop },
};
const hex = (n) => '0x' + (n >>> 0).toString(16).padStart(8, '0');
const TICKS = 2000;
for (const f of process.argv.slice(2)) {
  const module = new WebAssembly.Module(readFileSync(dir + f));
  guest = new WebAssembly.Instance(module, imports).exports;
  let sum = 0, us = 0;
  for (let pass = 0; pass < 2; pass++) {               // report the tiered-up pass
    host.set_gas_max(); if (guest.gas_deficit) guest.gas_deficit.value = -(2n ** 62n); guest.init();
    const t = performance.now(); sum = host.run_ticks(TICKS); us = (performance.now() - t) * 1000 / TICKS;
  }
  const used = guest.gas_deficit ? Number(guest.gas_deficit.value + 2n ** 62n) : host.used();
  const nan = guest.divbits(0, 0);
  host.set_gas_max(); if (guest.gas_deficit) guest.gas_deficit.value = -(2n ** 62n); guest.init();
  host.set_gas(5_000_000); if (guest.gas_deficit) guest.gas_deficit.value = -5_000_000n;
  let done = 0, last = 0;
  try { while (done < 5000) { last = host.run_ticks(1); done++; } } catch (e) { /* out of gas trap */ }
  console.log(`V8 (rust host<->glue) ${f}`.padEnd(40), `tick ${us.toFixed(2).padStart(9)} us  sum ${String(sum >>> 0).padStart(10)}  gas/2000t ${String(used).padStart(10)}  0/0 bits ${hex(nan)}  budget: ${done} ticks, last sum ${last >>> 0}, used ${guest.gas_deficit ? Number(guest.gas_deficit.value + 5_000_000n) : host.used()}`);
  if (guest.gas_deficit) guest.gas_deficit.value = -(2n ** 62n);
}
// Topology costs, measured on the last guest loaded.
host.set_gas_max();
const time = (label, per, fn) => { const t = performance.now(); const r = fn(); const ms = performance.now() - t;
  console.log(label.padEnd(58), 'per-unit', (ms * 1000 / per).toFixed(3).padStart(9), 'us', r === undefined ? '' : `(${r})`); };
for (let pass = 0; pass < 2; pass++) {
  console.log(`--- topology costs, pass ${pass + 1}`);
  time('host(wasm) -> JS glue -> guest(wasm) noop call', 1e6, () => host.run_noops(1e6));
  time('guest -> host export bound directly as import (hostcalls)', 1e6, () => guest.hostcalls(1e6));
  time('copy 32 KiB host->guest->host via JS glue', 1e4, () => host.run_copies(1e4, 131072, 32768));
  time('copy 256 B host->guest->host via JS glue', 1e5, () => host.run_copies(1e5, 131072, 256));
  const mem = new Uint8Array(guest.memory.buffer);
  const pristine = mem.slice(); const scratch = new Uint8Array(pristine.length);
  time(`reset guest memory from pristine image (${pristine.length} B)`, 1e4, () => { for (let i = 0; i < 1e4; i++) new Uint8Array(guest.memory.buffer).set(pristine); });
  time(`snapshot guest memory (slice, ${pristine.length} B)`, 2e3, () => { let s; for (let i = 0; i < 2e3; i++) s = new Uint8Array(guest.memory.buffer).slice(); return s.length; });
  time(`snapshot guest memory into reused buffer`, 1e4, () => { for (let i = 0; i < 1e4; i++) scratch.set(new Uint8Array(guest.memory.buffer)); });
  const module = new WebAssembly.Module(readFileSync(dir + process.argv.at(-1)));
  time('re-instantiate guest (new Instance + init)', 2e3, () => { for (let i = 0; i < 2e3; i++) { guest = new WebAssembly.Instance(module, imports).exports; if (guest.gas_deficit) guest.gas_deficit.value = -(2n ** 62n); guest.init(); } });
}
