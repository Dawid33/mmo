import { readFileSync } from 'node:fs';
const dir = new URL('.', import.meta.url).pathname;
const hostBytes = readFileSync(dir + (process.argv[2] || 'host/target') + '/wasm32-unknown-unknown/release/host.wasm');
const guestBytes = readFileSync(dir + 'guest.wasm');
console.log('host.wasm imports:', JSON.stringify(WebAssembly.Module.imports(new WebAssembly.Module(hostBytes))));
const host = (await WebAssembly.instantiate(hostBytes, {})).instance.exports;
const guest = (await WebAssembly.instantiate(guestBytes, { env: { host_add: (a, b) => (a + b) >>> 0 } })).instance.exports;
function time(label, per, f) {
  const t = performance.now(); const r = f(); const ms = performance.now() - t;
  console.log(label.padEnd(44), 'total', ms.toFixed(1).padStart(9), 'ms   per-unit', (ms * 1000 / per).toFixed(3).padStart(10), 'us   result', r >>> 0);
}
const TICKS = 2000;
for (let rep = 0; rep < 2; rep++) {  // second pass = V8 tiered-up numbers
  console.log(`--- pass ${rep + 1}`);
  time('rust->wasm direct on V8 (in host module)', TICKS, () => host.w_direct(TICKS));
  guest.init();
  time('guest.wasm direct on V8 JIT (option D)', TICKS, () => { let s = 0; for (let i = 0; i < TICKS; i++) s = guest.tick(); return s; });
  time('guest on V8: JS->wasm noop call', 1e6, () => { let s = 0; for (let i = 0; i < 1e6; i++) s = (s + guest.noop(i)) >>> 0; return s; });
  time('guest on V8: wasm->JS import call', 1e6, () => guest.hostcalls(1e6));
  for (const fuel of [0, 1]) {
    const tag = fuel ? 'wasmi-in-wasm +fuel' : 'wasmi-in-wasm';
    host.w_init(fuel);
    time(`${tag}: tick(1024 ents)`, TICKS, () => host.w_ticks(TICKS));
    time(`${tag}: noop call`, 1e6, () => host.w_noops(1e6));
    time(`${tag}: host import call`, 1e6, () => host.w_hostcalls(1e6));
    time(`${tag}: mem snapshot+restore`, 1e4, () => host.w_snapshots(1e4));
  }
}
