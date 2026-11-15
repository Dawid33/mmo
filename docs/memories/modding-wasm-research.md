---
name: modding-wasm-research
description: "RESEARCH + 2 SPIKES DONE 2026-09-20 on wasm-compatible modding; user chose in-sim deterministic wasm mods run by the browser's native engine; report + spike sources in docs/superpowers/specs/; nothing implemented"
metadata: 
  node_type: memory
  type: project
  originSessionId: 25c16dd7-eaf1-4fb4-a0e9-2ff779d75d7e
  modified: 2026-09-20T17:28:57.384Z
---

Modding investigation finished 2026-09-20. Report: `docs/superpowers/specs/2026-09-20-modding-wasm-research.md`; spike sources: `docs/superpowers/specs/2026-09-20-modding-wasm-spike/` (both untracked, not committed). No game code changed.

**User decisions (2026-09-20):** (1) code mods run ONLY inside the predicted, rolled-back sim — the server-side "mods emit events" tier I recommended was rejected; (2) `?offline` browser play must run code mods from day one; (3) efficient execution in the browser outranks author-language choice — mods are wasm modules, any source language.

**Resulting recommendation:** one first-party `ModEngine` trait; browser = the browser's own WebAssembly engine via thin hand-written JS glue (guest imports bound directly to the client wasm module's exports), native = wasmtime, wasmi = reference/differential-test engine. Determinism comes from a first-party load-time instrumentation pass every peer runs on the server-sent bytes (inline NaN canonicalization + inline gas-deficit global + finite-wasm stack accounting), NOT from engine features; the pass version is part of the protocol version. State model: stateless guests (memory reset per call) + host-owned undo-logged mod tables — forced by region entity handoff.

Measured (1024-entity guest tick; laptop power-clamped so ratios only, see [[laptop-power-clamp]]):
- Same instrumented bytes gave bit-identical checksums, gas totals and out-of-gas trap point on V8, wasmtime 48, wasmi 2.0, wasmi 1.1.
- Fully instrumented inline: V8 23.8 µs (plain 11.2), wasmtime 22.8 (plain 18.2), wasmi 2.0 197, wasmi 1.1 457. Call-based instrumentation (finite-wasm as-is + helper fn) is 14–22× — must be inline.
- wasmi nested in wasm32: 1.1 ≈ 215 µs, 2.0 ≈ 1.1–1.7 ms (its tail-call dispatch stack-overflows on wasm32; portable fallback slow). Browser-native path ≈ 10–15× faster than any interpreter-in-wasm.
- Browser topology: host→glue→guest call ~9 ns, guest→host direct-bound ~3 ns, 32 KiB round-trip copy 1.4 µs, 1 MiB memory reset 40 µs, re-instantiate 52 µs.

Prerequisites independent of the VM (still first): block property table (shared with water plan Phase 0), manifest hash + content transfer at join, runtime texture loading on wasm, a place/break verb, opening closed enums (EntityKind/ItemKind/GameEventKind), seeded RNG field in GameData.

**Why:** in-sim cost = tick × reconcile depth × regions on the browser main thread (see [[client-prediction-runaway]]), so JIT speed in the browser decides the engine.

**How to apply:** don't re-research runtimes or re-run the spikes. Unverified: Firefox/Safari, ARM NaN sign, integration of the raw-export binding trick with the wasm-bindgen/Bevy build, escape-point-only NaN canon. Next step is a design spec per phase (report §7), starting with the content foundation. Respect [[game-direction-entities]] and [[design-questions-need-tradeoffs]].
