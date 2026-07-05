# Embedded Scripting Language — Research Notes

**Date:** 2026-07-05
**Status:** Research / decision input. NOT a finalized design. No code written yet.
**Author:** Investigation via two `deep-research` multi-agent passes.

---

## 0. Why this document exists

We want an embedded scripting language to express game logic (initially
first-party, eventually player-authored mods) that fits the project's two
defining constraints:

1. **Cross-machine determinism** between the x86_64 Linux server and the
   `wasm32-unknown-unknown` browser client — the same bit-exact bar the
   vendored `simba`/`rapier`/`ordered-float` forks already enforce for the
   physics/rollback layer.
2. **Incremental rollback**: script state must be rewindable per tick, ideally
   participating in the same dual model the sim already has —
   cheap full-state snapshot/restore (tier-2 `change()`) *and* automatic typed
   delta undo (tier-1 `UndoCell`/`UndoMap`/`UndoSlotMap`, `hash(before) ==
   hash(after undo)` enforced).

The original prompt asked specifically whether **JavaScript** could satisfy
these. Short answer: only as an interpreter-compiled-to-wasm, and with real
performance risk. The research redirected the decision toward
**WebAssembly as the scripting substrate**, with the guest language chosen
separately on top of it.

### Provenance / confidence caveat

- **Round 1** (substrate: which engine class) ran to completion:
  22 sources, 107 claims, **25 top claims adversarially verified (24 confirmed,
  1 refuted).** High confidence.
- **Round 2** (guest languages on the wasm substrate) was **deliberately
  stopped before the verification pass**: 34 sources, 167 claims extracted,
  **0 verified.** Everything in §3–§5 is single-source and un-cross-checked —
  directionally reliable, but confirm load-bearing numbers before betting on
  them. Items most in need of verification are flagged **[UNVERIFIED]**.

To finish verifying Round 2, resume the workflow (cached search/fetch replay,
only the verify pass runs live):
`Workflow({scriptPath: ".../workflows/scripts/deep-research-wf_7e0f9276-db1.js", resumeFromRunId: "wf_7e0f9276-db1"})`

---

## 1. The decision in one page

- **Substrate:** run scripts as **WebAssembly modules** inside the Rust host,
  not as a native-Rust script VM (Lua/Rhai/Boa/etc.). Wasm is the only
  candidate class with an engine-supported path to *all* of: configurable
  bit-exact determinism, cheap tick-boundary rollback, and sandboxing/fuel.
- **Interpreter:** **`wasmi`** on the browser client (pure-Rust wasm
  interpreter that itself compiles to `wasm32-unknown-unknown`); `wasmi` or
  `wasmtime` on the server. Running the *same* interpreter both sides is the
  safe route to determinism; `wasmtime` on the server is a later
  speed optimization to validate for float/fuel equivalence.
- **First-party game logic → Rust compiled to wasm.** Fastest option, shares
  types with the sim, reuses the deterministic-float discipline we already have.
- **Modder-facing scripting → AssemblyScript is the best fit for our
  constraints** (host-scheduled GC, in-browser compilation, snapshot-clean
  linear memory). **JavaScript via QuickJS-in-wasm** is the familiar-to-modders
  fallback, but carries interpreter-under-interpreter performance risk that has
  bitten a comparable project (Jumpy). **Python is ruled out** (too heavy).
- **Rollback model:** primary mechanism is the **Factorio pattern** — stateless
  scripts, all persistent mutable state in host-owned rollback containers (our
  tier-1 `Undo*` types) exposed via host functions. Full **linear-memory
  snapshot** is the tier-2 fallback for scripts that need private heap state.

---

## 2. Round 1 — Substrate: wasm vs native script VMs (VERIFIED)

### 2.1 The core finding

WebAssembly runtimes are the only candidate class with a documented,
engine-supported path to determinism + bounded execution + rollback
*simultaneously*. Native-Rust script VMs each fail at least one hard criterion.

### 2.2 Determinism is configurable and documented (verified 3-0)

The wasm spec has exactly **two** nondeterminism sources, both with switches in
Wasmtime:

- **NaN bit patterns** → `Config::cranelift_nan_canonicalization` (adds
  per-float-op overhead).
- **Relaxed SIMD** → `relaxed_simd_deterministic`, or disable
  `wasm_relaxed_simd` (relaxed-SIMD ops "cannot be made to execute both
  identically and performantly across architectures").

Wasm float arithmetic is otherwise spec-deterministic. This dovetails with
Rapier's determinism docs (which our vendored forks implement): WASM targets
are listed among IEEE-754-2008-compliant platforms, and the same rule applies —
**transcendental math must route through libm-style software implementations,
never platform `std` floats** (`std::f64::sin` etc. are documented by Rust as
platform-dependent). A wasm substrate inherits the discipline we already built.

Source: `docs.wasmtime.dev/examples-deterministic-wasm-execution.html`,
`rapier.rs/docs/user_guides/rust/determinism/`, Rust std f64 docs.

### 2.3 Bounded execution: fuel, not epochs (verified 3-0)

Wasmtime fuel is "completely deterministic: the same program run with the same
amount of fuel will always be interrupted at the same location." Docs
explicitly recommend **fuel over epoch interruption** for determinism. Cost:
fuel instrumentation reported ~2–3× vs epochs. This is our per-tick /
untrusted-script CPU bound.

### 2.4 Rollback is proven practice, but user-space (verified 3-0)

- Wasmtime has **no built-in instance snapshot/restore** API (maintainer
  cfallin, 2023, re-confirmed Dec 2025; Wizer is only pre-init snapshotting).
- **But** between ticks (no wasm frames on the stack), snapshot = copy linear
  memory + non-const globals, and cfallin confirms it restores "even with
  different generated code, on a different architecture."
- **Shipped prior art: Gamercade** (wasm fantasy console, wasmtime + GGRS) does
  GGPO-style rollback exactly this way — clone memories/globals, restore via
  `copy_from_slice`.
- Caveats: funcref tables / externrefs / host-side resources are NOT captured by
  memory+globals; `memory.grow` means snapshot size must be tracked; Gamercade
  is hobby-scale.

This gives us the full-snapshot (tier-2) mechanism directly. Incremental
page-delta undo would be additional engineering (see §6 open questions).

### 2.5 Why the native-Rust JS engines fail (verified 3-0)

- **Boa** (pure Rust, *does* compile to `wasm32-unknown-unknown`): self-described
  **experimental**, pre-1.0 (v0.21.1, Mar 2026). Has `RuntimeLimits`
  (loop/recursion/stack) but **no fuel, no snapshot API, no determinism docs.**
  You'd build all three guarantees yourself on a moving pre-1.0 engine.
- **rquickjs** (QuickJS-NG binding, mature): supports only `wasm32-wasip1/wasip2`
  — a C-engine binding that **cannot run in the browser client**
  (`wasm32-unknown-unknown`). Structurally disqualified (build-failure issue #93).

### 2.6 JavaScript is feasible only via QuickJS-in-wasm (verified 3-0)

`vercel-labs/quickjs-wasi` (v3.0.2, Jun 2026): the entire VM — GC heap, atom
table, pending promise queue — lives in wasm linear memory, so a wholesale
memory copy is a complete, restorable heap snapshot. Determinism via
intercepting one WASI import (`clock_time_get`) controls both `Date.now()` and
the `Math.random()` seed. This is the *wasm option with JS on top*, not a
separate path. Full snapshots only (~256 KB baseline), restore requires the
identical module binary.

### 2.7 Piccolo, and the Factorio alternative (verified 3-0)

- **Piccolo** (pure-Rust Lua): best-designed fuel model of the native VMs (all
  execution inside `Executor::step(fuel)`, built for sandboxing untrusted
  scripts), but **explicitly experimental** (last release v0.3.3, Jun 2024), and
  its gc-arena heap has **no snapshot / serialization / cross-platform
  determinism** story — the exact criteria-2/3 gaps.
- **Factorio** = canonical prior art for the architecture our rollback system
  already suggests: **never serialize the Lua heap.** Scripts re-execute
  statelessly on load; only the host-owned `storage` table is restored;
  determinism enforced by lifecycle *convention* (`on_load` permits exactly
  three reconstruction ops — "anything else leads to desyncs"). Maps directly
  onto: mutable script state in host-owned rollback containers participating in
  our typed-delta undo log. (Note: Factorio *also* patches its Lua fork for
  instruction-level determinism, e.g. deterministic `pairs()` order — a
  patched-VM burden any mlua-style approach inherits.)
- **Veloren** (closest genre match — shipped Rust voxel multiplayer) chose
  **wasm plugins** over an embedded language, corroborating the substrate choice.

### 2.8 Round-1 refuted / gaps

- **Refuted (0-3):** a specific enumeration of `script-bench-rs` engine
  versions. No benchmark *numbers* survived — so the 50-TPS criterion is
  assessed qualitatively only.
- **Coverage gaps:** no verified claims on `wasmi` specifically, Rhai, Rune,
  Steel, mlua/Luau sandboxing, or V8. The "wasm wins" conclusion rests on
  wasmtime-side evidence + structural failure of the JS alternatives.

---

## 3. Round 2 — Guest languages on the wasm substrate (UNVERIFIED)

**All of §3–§5 is single-source, extracted but not adversarially verified.**

### 3.1 The dividing line: linear memory vs WasmGC

Our snapshots copy linear memory + globals. That is a hard gate:

- **Snapshot-safe (heap in linear memory):** Rust, Zig, C/C++, AssemblyScript,
  Nelua, Nim, Odin, Grain, MoonBit, TinyGo, and **any interpreter-in-wasm**
  (QuickJS/Javy, MicroPython, Lua-compiled-to-wasm) — the embedded runtime *is*
  linear-memory state.
- **Breaks the substrate (WasmGC, heap outside linear memory):** Kotlin/Wasm,
  Dart, Java/J2Wasm, OCaml (`wasm_of_ocaml`), Guile Hoot Scheme. Their objects
  live in an engine-managed heap the snapshot can't see. **[UNVERIFIED but
  high-prior]**
- **Second disqualifier for that group:** `wasmi` v0.32 (May 2024) does **not**
  implement the `gc` proposal at all — WasmGC languages can't even load on the
  browser interpreter. Both prior suspicions from the prompt confirmed by the
  sources.

### 3.2 Prior art validates the whole architecture

- **WASM-4** (fantasy console, wasm cartridges) shipped **GGPO-style rollback in
  v2.4 (Apr 2022)**, peer-to-peer **in the browser over WebRTC**. Rollback works
  for *every existing cartridge with zero game-code changes* — the host
  snapshots linear memory at the substrate level; the guest language never
  cooperates in serialization. 64 KB linear-memory cap; save-states are
  linear-memory copies. **This is our snapshot-tier design, already shipped.**
- **Gamercade** (wasmtime + GGRS): save/load copies ranges of guest linear
  memory to a snapshot vector — **but its snapshot code does NOT save/restore
  mutable wasm globals.** Concrete pre-flagged bug class for us. (PoC-scale:
  185 stars, last release 0.1.0 Sep 2022.)
- **Jumpy** treated the whole linear memory as the snapshot unit copied at tick
  boundaries (same as us) but **abandoned the multi-module wasm plan (Dec 2023)**
  over missing wasm module-linking / shared-memory standards, retreating to Lua
  on Bones ECS. **Lesson: stay single-module; multi-module mod-to-mod shared
  state is the trap — not snapshotting, which worked.**
- Languages these communities actually converged on (unprompted): WASM-4 →
  AssemblyScript, C/C++, Rust, Go, Zig, Nelua, Nim, Odin, D; Gamercade bindings →
  Rust, Zig, AssemblyScript, Nelua, C3. **Zero WasmGC languages, zero
  interpreted-scripting stacks.** Zig was the single most-used language in
  WASM-4 Game Jam #2 (Aug 2022).

---

## 4. Candidate ranking by role (UNVERIFIED)

### Role (a): first-party game logic → **Rust**

- Fastest compile-to-wasm option: sort benchmark Rust ~2,982 ms vs
  AssemblyScript ~6,405 ms (~2×) vs TinyGo ~9,717 ms (~3×). **[UNVERIFIED]**
- Snapshot-clean, shares types with the Rust sim, reuses vendored
  deterministic-float forks.
- Only cost is module size (ships allocator, dlmalloc ~6 KB; Rust module
  ~30–74 KB vs AssemblyScript ~8 KB). Negligible for logic delivered once.
- **Zig** is the credible alternative (lighter, community-proven on WASM-4,
  reported crash-free) but duplicates what Rust already gives us.

### Role (b): modder-facing scripting

**AssemblyScript — best fit for our specific constraints:**
- All managed state + 20-byte-per-object GC bookkeeping in linear memory; no
  WasmGC by default (`--enable gc` is opt-in WIP).
- **`--runtime minimal` runs GC only when the host calls `__collect`, at points
  with nothing on the wasm stack** → *we* schedule collection at tick
  boundaries, removing GC as an intra-tick nondeterminism source. **This is the
  single most valuable Round-2 finding — no other dynamic-feeling option gives
  the host that lever. [UNVERIFIED — verify first.]**
- **Compiler self-hosts to wasm and runs in-browser** → modders compile mods to
  wasm in-browser with zero local toolchain. WASM-4's community picked AS for
  exactly this. Smallest modules (~4.7 KB).
- Risk: one porting account hit hard-to-debug AS runtime instability (random
  freezes / garbage output) and abandoned it. Runtime isn't bulletproof.

**JavaScript via QuickJS-in-wasm (Javy / quickjs-wasi) — familiar fallback,
performance risk:**
- Snapshot-clean, determinism from overriding `clock_time_get` (~7 imports total
  to shim); quickjs-wasi actively maintained (v3.0.2, Jun 2026).
- **Interpreter-under-interpreter tax:** QuickJS-in-wasm ~3× slower than
  Rust-to-wasm **under Wasmtime (a JIT)**; under **`wasmi` (pure interpreter)
  that ~3× stacks on wasmi's own ~10–12× over native.** Precedent: **Jumpy's JS
  scripting couldn't fit 8 rollback re-sim frames in a 16 ms window** and was
  dropped for that reason. **[UNVERIFIED — this is the decisive risk for JS.]**
- Size: static QuickJS ≥800 KB; shrink via dynamic-linking user bytecode
  (~220 bytes) against one shared ~1.4 MB QuickJS provider loaded once.

**Lua compiled to wasm — plausible middle ground, under-researched:** small,
loved for modding, "stable" wasm-target tier, but little concrete
determinism/snapshot data surfaced and it carries the same
interpreter-under-interpreter multiplier. **Biggest single gap — dig here next.**

**Python — ruled out:** CPython-in-wasm ~4.3× slower than native CPython (itself
slow), still links full libpython into linear memory; py2wasm pinned to 3.11.
Too heavy for a 20 ms tick.

---

## 5. Performance reality to budget against (UNVERIFIED, weakest cluster)

Mostly one blog + one paywalled ACM paper. Confirm before relying on.

- **`wasmi` interpreter baseline: ~10–12× slower than native** for compute
  (wasm3 ~11.8×; wasmi generally slower than wasm3, though v0.32's
  register-bytecode rewrite claims up to 5× over v0.31).
- **wasmtime/JIT is only ~4× faster than a wasm3-class interpreter** on CoreMark
  → the *server* on wasmtime keeps script cost to a bounded constant factor, but
  the *browser client on wasmi* eats the full interpreter penalty. **This
  client/server asymmetry (same-interpreter for determinism vs. speed) is the
  central tension.**
- **Net:** compiled-to-wasm guest logic (Rust/Zig/AS) under wasmi is fine for
  50 Hz. A *scripting-language interpreter* under wasmi (QuickJS, Lua,
  MicroPython) multiplies again and is the real risk during rollback re-sim
  bursts.

---

## 6. Open questions / next steps before committing

1. **wasmi-in-browser vs wasmi-native bit-exactness + fuel-count equivalence** on
   a float-heavy script. Does the browser's wasm engine (running wasmi.wasm)
   produce bit-identical float + identical fuel accounting to native wasmi on the
   server? (Round-1 open question: wasmtime-with-NaN-canonicalization was *not*
   verified bit-identical to browser V8/SpiderMonkey — hence run the same
   interpreter both sides.)
2. **Snapshot cost per tick** at realistic script heap sizes (hundreds of KB →
   tens of MB): is full linear-memory copy cheap enough at 50 Hz, or is
   dirty-page / delta tracking needed to integrate with the existing
   hash-verified typed-delta undo log?
3. **Verify the AssemblyScript `--runtime minimal` / `__collect` host-scheduled
   GC claim** — it's the linchpin of the modder-language recommendation and is
   currently unverified.
4. **Lua-in-wasm determinism** — the biggest research gap; what patches
   (deterministic table iteration, seeded PRNG, libm-routed math) are needed.
5. **Globals in the snapshot unit** — Gamercade's omission is a concrete warning:
   ensure our linear-memory snapshot also captures mutable wasm globals.
6. **Finish Round-2 verification** (resume command in §0).

---

## 7. Sources

**Round 1 (verified):** docs.wasmtime.dev (deterministic execution),
docs.rs/wasmtime, WebAssembly 3.0 spec, github.com/bytecodealliance/wasmtime#3017,
github.com/gamercade-io/gamercade_console, github.com/vercel-labs/quickjs-wasi,
github.com/boa-dev/boa, github.com/DelSkayn/rquickjs, github.com/kyren/piccolo,
rapier.rs determinism docs, Rust std f64 docs,
lua-api.factorio.com/data-lifecycle, book.veloren.net plugin docs,
github.com/khvzak/script-bench-rs, github.com/fishfolk/jumpy#489.

**Round 2 (unverified):** assemblyscript.org, wasm4.org, gamercade GitHub,
shopify.engineering (Javy/QuickJS), wasmi-labs.github.io, v8.dev (WasmGC),
developer.chrome.com, ecostack.dev (lang/wasm size+speed benchmark),
00f.net (interpreter benchmarks), wasmer.io, dl.acm.org (wasm-vs-VM paper,
paywalled — abstract only), crlf.link (Jumpy postmortem),
docs.wasmtime.dev, placeholder-software.github.io.
