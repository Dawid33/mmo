# Modding support that works in the wasm client — research + spike (2026-09-20)

Line numbers were checked against `develop` on 2026-09-20 while the tree was being edited
concurrently — treat them as approximate and grep the symbol if one has drifted.

Status: **research / spike findings, no code changed.** Nothing here is approved design; each
phase at the end needs its own spec. Inputs: a codebase seam survey, a web survey of mod
runtimes (versions checked against crates.io on 2026-09-20), and a throwaway benchmark of
wasmi running natively and *inside* a `wasm32-unknown-unknown` host under V8 (appendix A).

## Decisions taken 2026-09-20 and the revised recommendation

After the first pass of this report the user decided:

1. **Code mods run only inside the predicted, rolled-back simulation** (Tier 2 below). The
   server-side "mods emit events" tier is dropped.
2. **`?offline` browser play must run code mods from day one.**
3. **Efficient execution in the browser outranks author-language choice**: mods are wasm
   modules (anything that compiles to wasm) and must run in the browser as fast as possible.

Those three together overturn the engine recommendation of the first pass. In-sim mods cost
`tick cost × reconcile depth × simulated regions` on the browser main thread, so the 10–15×
gap between an interpreter-inside-wasm and the browser's own JIT is no longer a detail. The
obstacle to the browser-native engine was determinism (NaN bits, no fuel, engine-specific
stack depth). Spike 2 (§4b) shows that obstacle is removable: **one first-party load-time
instrumentation pass** (inline NaN canonicalization + inline gas counter + finite-wasm stack
accounting) made the *same bytes* produce bit-identical results, identical gas totals and an
identical out-of-gas trap point on V8, wasmtime 48, wasmi 2.0 and wasmi 1.1 — for ~2.1× on V8
and ~1.25× on wasmtime.

**Revised recommendation**

- **Engines behind one first-party `ModEngine` trait:** browser = the browser's `WebAssembly`
  engine through a thin hand-written JS glue; native server/client = **wasmtime** (JIT);
  **wasmi** kept as the reference engine for differential tests and JIT-less platforms. Engine
  features (`fuel`, `cranelift_nan_canonicalization`, wasmi `deterministic`) are *not* used for
  anything sim-visible — determinism comes from the instrumented bytes, so engines stay swappable.
- **Every peer instruments the server-sent module itself** (validate → feature allowlist →
  NaN canon → gas → stack). A client must not trust the server to have done it: metering is the
  client's only defence against a hostile or buggy mod spinning the main thread. The pass is
  pure Rust (`wasmparser` + `wasm-encoder`, both already wasm32-clean as wasmi depends on them)
  and took 0.1–0.3 ms on the 2 KB spike guest.
- **Inline instrumentation, not calls.** finite-wasm's import-call gas costs 3.5× on V8 and
  4.8× on wasmtime; a helper-function NaN canon costs 2–4×; stacked they cost 14–22× (the helper
  itself gets metered). The inline forms cost 1.13× (gas) and ~2× (NaN, on a float-dense loop).
- **State model: stateless guests + host-owned, undo-logged mod tables** (option (i) in §5C).
  Region handoff forces it (per-entity mod data must travel in `EntityBundle`), it keeps mod
  state inside the existing hash/park/snapshot machinery, and it keeps guest memory out of the
  undo log. Guest memory is scratch: reset to the post-init image before each call (measured
  40 µs per 1 MiB in the browser — link guests with a small stack to make that a few µs) and
  re-instantiate after a trap (52 µs).
- **Host API can be chatty.** guest→host calls bound straight to the host module's exports cost
  ~3 ns in the browser (no JS in between), host→guest through the JS trampoline ~9 ns, bulk
  copies between the two linear memories 1.4 µs per 32 KiB round trip. Scalar getters plus a
  few bulk "read N entities" calls are both affordable; no need to contort the API around
  call avoidance.
- **Everything engine-dependent must be unreachable:** stack limit enforced by instrumentation
  well below the smallest engine's real limit, memory declared with a fixed max the host always
  grants, no threads / SIMD / relaxed-SIMD / memory64 / start-function side effects (reject at
  validation).
- The prerequisites in §2/§7 phase 1–3 are unchanged and still come first; §7 phase 4 (Tier 1
  host) is deleted and phase 5 becomes the mod host.

Sections 3–8 below are the first-pass analysis, kept for the reasoning; where they recommend
wasmi-everywhere or server-side mods they are superseded by this section.

## TL;DR (first pass — see the section above for what changed)

1. **"Supports wasm" is a hard filter on the runtime, and it leaves one practical engine.**
   The browser client is itself a wasm module, so a JIT runtime (wasmtime; wasmer `sys`) cannot
   be the host there. What runs everywhere is a pure-Rust interpreter — **wasmi** — or the
   browser's own `WebAssembly` engine reached through JS (no fuel, NaN-payload nondeterminism,
   a second host implementation). Pure-Rust scripting VMs (Rhai, piccolo) also run everywhere
   but each needs its own determinism audit and locks modders into one language.
2. **Measured: pin wasmi 1.1, not 2.0, if the browser matters.** wasmi 2.0.0 (2026-09-01) is
   2.4× faster than 1.1 natively, but nested in wasm it is **5–6× slower than 1.1**: its fast
   tail-call dispatch stack-overflows on wasm32 (even with `+tail-call`), and the portable
   fallback is slow. wasmi 1.1 nested in wasm loses only ~1.2× vs native. All engines and hosts
   produced bit-identical results, floats included.
3. **The bigger decision is *where mod code runs*, not which VM.** Running mod logic inside the
   predicted, rolled-back simulation (every client, re-executed on every reconcile) is the
   expensive, risky version. Running it **only where the authoritative sim runs** — the native
   server, and the embedded `LocalServer` in `?offline` browser mode — and feeding its decisions
   into the normal `GameEvent` stream needs *no* determinism from the mod, no rollback of mod
   state, and no mod runtime in the online browser client at all. It matches this engine's
   grain: an NPC driven by a mod is just an entity whose input stream comes from the server
   instead of from a client.
4. **Recommended order:** Tier 0 data packs (server-sent, hash-verified content) → Tier 1
   server-side wasm mods that emit engine-defined events → Tier 2 in-sim deterministic mods
   only if a concrete mod needs zero-latency prediction. Tier 0 and the prerequisites it
   exposes (block property table, a place/break verb, content negotiation at join, runtime
   texture loading on wasm) are most of the near-term work and are valuable without any VM.

## 1. What the wasm target forces

The constraint is not "mods are written in wasm" but "the *host* may be wasm32-unknown-unknown":

- No JIT, no `mmap`/executable memory, no threads (the browser sim runs inside a Bevy `Update`
  system on the main thread — `crates/client/src/sim_driver.rs:94`), no filesystem (block
  manifest and textures are `include_str!`/`include_bytes!` on wasm —
  `crates/client/src/blocks.rs:11`, `crates/client/src/renderer/mod.rs:203`).
- Mods are server-chosen and arrive over the network, so on the client they are **untrusted
  code**: sandboxing, CPU metering and memory caps are requirements, not nice-to-haves.
- Anything that executes inside the shared simulation must be bit-deterministic across x86-64,
  aarch64 and the browser, side-effect-free, and cheap to re-execute: the client rolls back and
  replays its whole pending event log on a misprediction (`crates/game/src/region.rs:257-339`).

## 2. Where the codebase stands

Already in place (good news):

- **Block identity is data-driven**: `BlockRegistry` from `assets/blocks/blocks.ron`, stable
  `u16` ids, carried unchanged by chunks, LOD RLE, the wire and the mesher
  (`crates/game/src/registry.rs`). Texture layers are built by `build_layers(registry, loader)`
  with a generic loader closure and a magenta fallback (`renderer/block_textures.rs:78`).
- `Block.data: [u8; 3]` is reserved, already serialized/hashed/rolled back (`block.rs:51-54`).
- **Worldgen is an injected closure** (`RegionGenerator`, `world_manager.rs:43`); `Materials`
  are resolved by name from the registry.
- **Behaviour already has a list seam**: `controllers: Vec<Box<dyn Controller>>`
  (`region.rs:145`, trait at `lib.rs:126`) — hardcoded to camera + physics today.
- The `RegionInput`/`RegionOutput` message pair is a clean place to inject server-originated
  events; the server never rolls back (`RegionRunner::tick` applies then `forget_last_event()`).
- A rollback-safe runtime chunk edit path exists (`Rollback::create_mesh`, `state.rs:462`) but
  only tests call it.

Missing (each is a prerequisite for some tier, independent of the VM choice):

| Gap | Evidence | Blocks |
|---|---|---|
| Block *behaviour* is `id != AIR` in ~18 places (solid/opaque/collidable conflated) | `voxel.rs:153`, `block.rs:153`, `state.rs:703`; water plan Phase 0 already asks for a property table | any non-cube-solid modded block |
| No content negotiation: client and server each read their own `blocks.ron`; mismatch desyncs silently | `server/src/lib.rs:296`, `client/src/blocks.rs:14` | Tier 0 |
| wasm client cannot load content at runtime | `include_bytes!` tables | Tier 0 on web |
| No gameplay verb: no place/break/interact; mouse buttons are received then dropped | `input.rs:124` | anything interactive |
| Closed enums everywhere: `GameEventKind`, `EntityKind{Player}`, `ItemKind{Empty,Dirt,Stone}`, 9-variant `Key`, five compile-time ECS components | `protocol.rs:299`, `state.rs:88-138,183` | entities/items from mods |
| No seeded RNG in `GameData`; no protocol/`SerializedRegion` version tag | grep: zero RNG in sim; `region_runner.rs:18` | Tier 2; safe upgrades |

## 3. Runtime options — trade-offs

**wasmi (pure-Rust interpreter).** The only mainstream wasm engine that builds for a wasm32
host (verified: `host.wasm` has zero imports and runs under V8). Built-in fuel, `StoreLimits`,
linear memory exposed as a byte slice (snapshot = memcpy), `deterministic` + `libm` features
in 2.0, audited up to 0.38, used in production by Zellij since 0.44. Cost: it is an interpreter —
17–40× native Rust natively; see §4 for the nested numbers. No Component Model.

**wasmtime.** Best native performance, Component Model, deterministic knobs
(NaN canonicalization, fuel). Cannot host inside the browser build; Pulley-on-wasm32 is
unsupported and would be slower than wasmi anyway. Only viable as a *native-side* engine
behind a trait — which is safe only where cross-engine determinism is not required (Tier 1).

**Browser-native `WebAssembly` via js-sys (or wasmer's `js` backend, which is the same thing
with more dependency).** JIT speed (11–15 µs vs wasmi's 200 µs on the spike workload), but: no
fuel (needs load-time gas injection — `finite-wasm` is maintained, `wasm-instrument` is stale),
NaN payload bits are engine-defined (needs a rewriting pass or a float ban), stack-overflow
depth differs per engine, two linear memories joined by JS copies, and it is a second host
implementation to keep behaviourally identical to the native one. Fine for presentation-only
client mods later; a poor fit for anything sim-visible.

**Pure-Rust scripting (Rhai, piccolo).** Rhai documents wasm32-unknown-unknown and has
`set_max_operations`; piccolo has the best execution model (fuelled stepping) but is a
maintenance risk (last release 2024-06). mlua is emscripten-only, Roto is a JIT. None can
snapshot VM state, every one needs a determinism audit (hash seeds, float formatting, GC), and
modders get exactly one language. They could later sit *on top of* the same host API as a
friendlier front end; they are not a better foundation than wasm.

**Abstraction crates.** `wasm_runtime_layer` 0.7 (wasmi/wasmtime/wasmer/js backends) is
maintained and a good reference; `wasm_component_layer` and `wasm-bridge` are stale. Veloren
(wasmer→wasmtime) and Zellij (wasmer→wasmtime→wasmi) both swapped engines after shipping —
the lesson is to keep the engine behind **our own narrow trait** (load, call, memory r/w, fuel,
limits) and the ABI a versioned, serialized byte boundary, not to adopt someone's abstraction.

| Option | Native | In wasm32 host | Deterministic | Metering | Perf (spike tick) | Risk |
|---|---|---|---|---|---|---|
| wasmi 1.1 | yes | **yes, ~1.2× nested cost** | same engine everywhere | fuel | 185 µs / 215 µs | low |
| wasmi 2.0 | yes | yes but 5–6× slower than 1.1; tail-call mode crashes | + `deterministic`/`libm` features | fuel (~0% cost) | 78 µs / 1100–1700 µs | 3-week-old rewrite |
| wasmtime | yes | **no** | with config | fuel | JIT | low, native only |
| browser-native / wasmer-js | n/a | yes | no (NaN, stack depth) | none built in | 11–15 µs | second host impl |
| Rhai / piccolo | yes | yes | audit per VM | op limits / fuel | AST/bytecode interp | one language; piccolo upkeep |

## 4. Spike measurements

Workload: a 1.9 KB `no_std` Rust guest; `tick()` updates 1024 entities (f32 steering with
`sqrt`, integration, xorshift, a branchy integer rule, bit-level checksum). Release, fat LTO,
single thread. **This laptop was power-clamped (~0.9 GHz observed), so trust ratios, not
absolute times.** Every row returned the same checksum `2191162682`.

| Configuration | per tick (1024 ents) | vs native Rust |
|---|---:|---:|
| Native Rust, no VM | 4.5 µs | 1× |
| Guest JIT-compiled by V8 directly (browser-native path) | 11–15 µs | ~3× |
| wasmi 2.0 native, tail-call dispatch | 78 µs | 17× |
| wasmi 2.0 native, `deterministic`+`libm` | 110 µs | 25× |
| wasmi 2.0 native, `portable-dispatch` | 234 µs | 52× |
| wasmi 1.1 native | 185 µs | 41× |
| **wasmi 1.1 inside wasm32 on V8** | **200–230 µs** | ~48× |
| **wasmi 2.0 inside wasm32 on V8** (auto → portable) | **1100–1700 µs** | ~300× |
| wasmi 2.0 inside wasm32, built with `+tail-call` | **crash**: `Maximum call stack size exceeded` in the handler chain | — |

Other numbers: fuel metering cost ≈ 0% (2.0 native); host→guest export call 30–50 ns and
guest→host import call 17–35 ns on wasmi (native and nested alike) — a chatty host API is
affordable, ~10⁴ calls ≈ 0.4 ms; snapshot **and** restore of the full 1.06 MiB guest memory
≈ 71 µs native / 72–100 µs nested (1 MiB of that is rustc's default shadow stack — a guest
linked with a 64 KiB stack would be ~10× cheaper). V8 tiering flags (`--no-liftoff`,
`--liftoff-only`) made no difference to the nested numbers, so it is the dispatch design, not
V8 failing to optimize.

Reading: wasmi 2.0's build script enables tail-call dispatch on wasm32 when the `tail-call`
target feature is set, but rustc/LLVM does not guarantee `return_call` for those handlers, so
the interpreter recurses until the stack dies; without the feature it falls back to portable
dispatch, which costs 3× natively and another ~5× under V8. Until that is fixed upstream
(likely needs Rust's explicit tail calls), **wasmi 1.1 is the browser-capable choice**. Against
the 50 ms tick (`TICK_RATE`), 0.2 ms per 1000 entity-callbacks is comfortable for code that runs
once per tick; it is *not* comfortable multiplied by reconcile depth × regions (§5, option C).

## 4b. Spike 2 — browser-native engine with load-time determinism instrumentation

Same guest and workload as §4, plus `divbits(a,b)` returning the bits of `a/b`. A throwaway
tool (`fw`, appendix C) rewrites the module: **nan** = canonicalize after every NaN-capable
float op (`add sub mul div sqrt min max ceil floor trunc nearest promote demote`), either via an
appended helper function or **inline** with two scratch locals
(`local.tee t; f32.const canon; local.get t; local.get t; f32.eq; select`); **gas** =
finite-wasm 0.6.1 (1 unit/instruction, per-basic-block charges, plus stack accounting), either
as emitted (imported `finite_wasm_gas(i64)` call) or **inline** — a post-pass replacing that
call with a *deficit* global (`global.get d; i64.add; global.set d; d > 0 → unreachable`; the
add is commutative so the charge can stay on the stack). Browser topology: a Rust wasm "host"
module stands in for the client; the guest is a separate instance whose metering/host imports
are bound **directly to the host module's exports**, and the host calls the guest through a JS
trampoline import. Native: one harness running the same bytes on wasmi 2.0, wasmi 1.1 and
wasmtime 48 (Cranelift), none of their determinism features enabled.

Determinism (the point of the exercise) — identical on **all four engines**, every variant:

| Variant | checksum after 2000 ticks | gas per 2000 ticks | 5 M gas budget | bits of `0.0/0.0` |
|---|---|---|---|---|
| plain | 2191162682 | — | never traps | `0xffc00000` (x86 default NaN; ARM differs) |
| nan / nan-inline | 2191162682 | — | never traps | `0x7fc00000` |
| gas / gas-inline | 2191162682 | 616 810 119 | traps in tick 17, last good sum 1693239111 | `0xffc00000` |
| nan+gas inline | 2191162682 | 911 745 375 | traps in tick 11, last good sum 3440260733 | `0x7fc00000` |

Cost per 1024-entity tick (µs; power-clamped laptop, compare ratios):

| Variant | V8 (browser topology) | wasmtime 48 | wasmi 2.0 native | wasmi 1.1 native |
|---|---:|---:|---:|---:|
| plain | 11.2 | 18.2 | 79 | 221 |
| nan, helper call | 26 | 72 | 440 | 1026 |
| **nan, inline** | 23 | 28 | 152 | 305 |
| gas, import call (finite-wasm as-is) | 39–43 | 88 | 455 | 775 |
| **gas, inline global** | 12.7 | 13.8 | 170 | 417 |
| nan + gas, calls | 158 | 403 | 2339 | 3896 |
| **nan + gas, inline** | **23.8** | **22.8** | 197 | 457 |

For comparison, wasmi 1.1 *nested in wasm32* ran the **plain** guest at 200–230 µs (§4); with
NaN canonicalization it would be ≈1.4× that. The browser-native path is therefore ~10–15×
faster than the best interpreter-in-wasm option at equal determinism.

Browser topology costs (V8): host(wasm)→JS glue→guest call 8–12 ns; guest→host export bound
directly as an import 2–3 ns; copy host→guest→host through the glue 0.11 µs for 256 B,
1.3–1.5 µs for 32 KiB; reset guest memory from a pristine image 38–40 µs (1.06 MiB, 1 MiB of
it rustc's default shadow stack); snapshot into a reused buffer 37–42 µs (`slice()` allocating:
135–150 µs); re-instantiate + `init` 52–55 µs.

Reading:
- The interpreters hate instrumentation (wasmi 2.0: 79 → 197 µs) while the JITs mostly absorb
  it; another reason the interpreter is the fallback, not the primary.
- Inline NaN canon doubles a float-dense loop on V8. A cheaper sound variant exists —
  canonicalize only where NaN bits can *escape* (stores, `reinterpret`, `copysign`, call
  arguments/returns) instead of after every op — untested here.
- Gas totals differ between the nan and non-nan variants because instrumentation is metered
  too; that is fine as long as every peer runs the identical pass — which makes **the pass
  version part of the protocol version**.
- Not covered: Firefox/Safari, ARM (where the uncanonicalized NaN sign actually differs),
  integration with the real wasm-bindgen/Bevy build (the raw-export binding trick needs the
  client's wasm instance exports and a JS snippet alongside wasm-bindgen's glue — plausible,
  unverified), wasmtime host-call overhead, and a guest with `memory.grow`/tables/indirect calls.

## 5. The real fork: where does mod code execute?

**A. Data only (Tier 0).** Blocks, textures, items, recipes, worldgen parameters as
server-sent registries — Minecraft data packs / Veloren's server-sent plugins. No VM, no
determinism or sandbox risk; covers most "content" mods. Needs the §2 gaps: property table,
manifest hash + transfer at join, runtime texture loading on wasm. Limit: no new behaviour.

**B. Server-side mods emitting events (Tier 1).** Mod instances live next to the
authoritative `RegionRunner` (region thread on the server; `InlineSpawner` in the browser's
`?offline` mode). After each tick a mod reads region state through a host API and returns
*commands*; the host validates them and injects **engine-defined `GameEvent`s** (set block,
spawn/despawn entity, entity input/intent, inventory change, chat…). Those flow through the
existing id-assignment → echo → client reconcile path, so every client applies them
deterministically **without running any mod code**.
- Gains: the mod need not be deterministic (the event log is the single source of truth); no
  rollback of mod state (the server never rolls back); no re-execution; untrusted code never
  runs on online clients; the online wasm client needs **no VM at all** — the wasm constraint
  only bites in offline mode, where wasmi 1.1 measured fine. Native could even use wasmtime
  behind the trait later because cross-engine agreement is irrelevant here.
- Costs: mod reactions lag by a tick + RTT and are never predicted (how every Bukkit/Luanti
  server mod already feels); every mod effect costs bandwidth, so the event vocabulary must be
  intent-level ("NPC walks toward X", like player input) rather than per-tick state; mod state
  lives outside `GameData`, so persistence across region park/restore and **entity handoff
  between regions** needs a server-side store keyed by entity that travels with `EntityArrived`.
- Fits the stated direction ("players are entities like NPCs; prefer entity-generic
  mechanisms"): a mod-driven NPC is an entity whose `PlayerInput`-like stream comes from a mod.

**C. In-sim deterministic mods (Tier 2).** A `ModController` in the controller list, executed
on server and every client, inside the rollback transaction. Zero latency, zero bandwidth,
mods can change movement/collision rules the local player feels instantly.
- Costs: one pinned engine version everywhere (fuel exhaustion, stack depth and `memory.grow`
  are sim-visible, so "same wasmi, exact version" becomes part of the protocol version);
  NaN canonicalization at the boundary; no clock/IO; RNG from a new seeded rollbackable field;
  mod state must be rollbackable. Two ways, both measured cheap per call but different in the
  undo log: (i) *stateless guests* — memory reset to the post-init image before each call,
  persistent state in host-owned `#[undo(map)]` tables with typed deltas (Factorio `storage`,
  Roblox attributes, Bones/Jumpy all landed here); (ii) *whole-memory-is-state* (WASM-4) — a
  before-image of the capped linear memory per mod per tick in the undo log, shipped in region
  snapshots and hashed. (i) is also forced by this engine's region handoff: per-entity mod
  state must travel in `EntityBundle`, which guest memory cannot.
- And the multiplier: client cost = mod tick cost × reconcile depth × simulated regions, on the
  browser main thread. The known prediction-runaway issue (client ~13 events behind, full-log
  re-sim) would turn 0.2 ms into several ms per server tick per mod. Do not start here.

B and C can share the guest ABI, SDK, engine trait, distribution and limits; C adds the
determinism regime. That is the argument for building B first rather than it being throwaway.

## 6. Recommended shape

- **Guest format:** plain `wasm32-unknown-unknown` core modules, not Components (no browser or
  wasmi host can run components; `wasm_component_layer` is stale). A versioned, serialized byte
  boundary (`alloc` export + ptr/len buffers, bincode or postcard), a Rust guest SDK crate with
  macros, API version in the mod manifest (Zed keeps several API versions alive this way).
  WIT can be adopted later as an IDL without changing the wire.
- **Engine:** `wasmi = "=1.1.x"` behind a first-party `ModEngine` trait in a new Bevy-free
  crate (`crates/modhost`), fuel + `StoreLimits` (fixed max memory, no growth past cap), no
  WASI, no imports except our API. Re-evaluate 2.x when its wasm32 dispatch is fixed.
- **Host API in two surfaces:** *simulation* (read world/entities, return commands) and, later,
  *presentation* (client-only UI/effects — the one place the browser-native JIT is attractive).
- **Distribution:** mod list + content hashes in the join handshake between
  `RequestPlayerRegion` and `PlayerRegion` (`instance.rs:296` ↔ `world_manager.rs:305`), blobs
  fetched by hash and cached; the same exchange must carry the block-manifest hash so a
  mismatch fails loudly instead of desyncing. Cap module size, memory, tables, import surface;
  Factorio's server-sent-bytecode RCE (fixed 1.1.101) is the cautionary tale.
- **Failure semantics:** a trap or out-of-fuel discards that call's command buffer and
  disables the mod for the region with a logged reason — never a partial apply.
- **Authoring aid (Tier 2 only):** a Factorio-style "heavy mode" — serialize, restore, re-run,
  hash-compare every tick — so authors find hidden state without a second player. This engine's
  `HASH_MODE=full` machinery is most of it already.

## 7. Suggested phasing (each its own spec → plan)

1. **Content foundation (no VM):** block property table (shared with water plan Phase 0),
   multi-manifest merge with per-mod id ranges, manifest hash + content transfer at join,
   runtime texture loading on wasm, protocol/`SerializedRegion` version tag.
2. **Interaction verb:** place/break/interact as a predicted `GameEvent` — needed by the base
   game and the first thing any mod wants to hook.
3. **Open the closed enums that content needs:** data-driven items; `EntityKind` beyond
   `Player`; an entity-generic input/intent event.
4. **Tier 1 mod host:** `crates/modhost` (wasmi 1.1 behind the trait), guest SDK, read API +
   command buffer → events, per-region instances, server-side mod store with park/handoff,
   limits and failure semantics; offline-browser path via `InlineSpawner`.
5. **Tier 2**, only with a concrete mod that needs prediction: `ModController`, seeded RNG
   field, host-owned undo-logged mod tables, pinned-engine determinism regime, heavy mode.

## 8. Decisions that were the user's (answered — see the top section)

- Is **Tier 1 (server-side, unpredicted)** acceptable as the code-mod model for the
  foreseeable future, with Tier 2 deferred until a mod demands it? (My recommendation: yes.)
- Must `?offline` browser play support code mods from day one? It is what forces a
  wasm-hostable engine at all; online-only would even allow wasmtime natively.
- Who are the mod authors — Rust/any-wasm-language developers (core-module ABI is enough) or
  casual scripters (argues for a Rhai/Lua front end on top, later)?

## 9. Unverified / risks

- Spike ran under Node 25's V8 only; Firefox (SpiderMonkey) and Safari (JSC) nested-interpreter
  performance is unmeasured, as is a real in-page run under `just web`.
- wasmi 1.1's long-term maintenance once 2.x is the focus; 2.0 itself is unaudited.
- Web-survey claims not re-checked here are marked in the research notes as UNVERIFIED (notably:
  no explicit doc statement that wasmer-`js` lacks metering; `wasm_runtime_layer` exposing fuel
  uniformly; piccolo iteration-order determinism).
- Tier 1 bandwidth depends entirely on how intent-level the event vocabulary is; needs a design
  pass with a concrete NPC mod in mind.
- No shipped game was found running wasm mods inside an undo-log rollback sim; Tier 2 would be
  novel work (closest: WASM-4 netplay's whole-memory snapshots, Bones/Jumpy's host-owned state).

## Appendix A — spike source (throwaway)

Sources are kept in `docs/superpowers/specs/2026-09-20-modding-wasm-spike/`; the core is reproduced here so the numbers can be
re-run. Layout: `guest/` (cdylib, `libm` dep, `panic=abort`, fat LTO), `host/` (cdylib+rlib,
`wasmi` with `default-features=false, features=["std","stable","validate","auto-dispatch"]`;
for 1.x: `features=["std"]`), `bench.mjs`.

```sh
(cd guest && cargo build --release --target wasm32-unknown-unknown) && cp guest/target/wasm32-unknown-unknown/release/guest.wasm .
(cd host && cargo build --release && ./target/release/host)                  # native rows
(cd host && cargo build --release --target wasm32-unknown-unknown --lib)     # nested rows
node bench.mjs                                                               # [target dir] optional
# variants: --features portable | --features det | RUSTFLAGS="-C target-feature=+tail-call"
```

`guest/src/work.rs` (shared by the guest and the native baseline):

```rust
pub const N: usize = 1024;
#[derive(Clone, Copy)] #[repr(C)]
pub struct Ent { pub pos: [f32; 3], pub vel: [f32; 3], pub hp: u32, pub seed: u32 }
pub const ZERO: Ent = Ent { pos: [0.0; 3], vel: [0.0; 3], hp: 0, seed: 0 };
pub fn init(ents: &mut [Ent; N]) {
    let mut s: u32 = 0x9E37_79B9;
    for e in ents.iter_mut() {
        for k in 0..3 { s ^= s << 13; s ^= s >> 17; s ^= s << 5;
            e.pos[k] = ((s >> 8) as f32) / 65536.0 - 128.0; e.vel[k] = 0.0; }
        e.hp = 100; e.seed = s;
    }
}
pub fn tick(ents: &mut [Ent; N]) -> u32 {
    let mut sum: u32 = 0;
    for e in ents.iter_mut() {
        let d = [-e.pos[0], -e.pos[1], -e.pos[2]];
        let len = libm::sqrtf(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
        if len > 0.001 { for k in 0..3 { e.vel[k] += d[k] / len * 0.01; } }
        for k in 0..3 { e.vel[k] *= 0.995; e.pos[k] += e.vel[k] * 0.05; }
        e.seed ^= e.seed << 13; e.seed ^= e.seed >> 17; e.seed ^= e.seed << 5;
        if e.seed & 0xFF < 3 { e.hp = if e.hp > 1 { e.hp - 1 } else { 100 }; }
        for k in 0..3 { sum = sum.wrapping_mul(0x0100_0193) ^ e.pos[k].to_bits(); }
        sum = sum.wrapping_add(e.hp);
    }
    sum
}
```

The guest `lib.rs` exports `init`, `tick`, `noop(u32)->u32`, `hostcalls(n)` (loops over an
imported `env.host_add`). The host wraps them in wasmi `TypedFunc`s
(`Engine::new(&config)` with `consume_fuel`, `Linker::func_wrap`, `instantiate_and_start`,
`get_typed_func`, `get_memory`), times 2000 ticks / 10⁶ calls / 10⁴ memory snapshot+restores,
and on wasm32 exposes the same loops as `extern "C"` exports that `bench.mjs` times with
`performance.now()` next to the guest instantiated directly on V8.

## Appendix C — spike 2 tooling (throwaway)

`fw <in.wasm> <out.wasm> [nan|nan-inline] [gas|gas-inline]` — deps: `finite-wasm 0.6.1`
(feature `instrument`), `wasm-encoder 0.228` (feature `wasmparser`). Three `Reencode` passes:

- **NaN canon:** pre-scan counts types / imported+defined functions and records each defined
  function's param count; `parse_type_section` / `parse_function_section` /
  `parse_code_section` append the two helper signatures/bodies *after* the originals (no index
  shifts); `parse_function_body` re-emits each operator and follows the NaN-capable float ops
  with `call $canon32|64`, or — inline mode — appends one `f32` and one `f64` scratch local
  (index = params + declared locals) and emits `local.tee t; fNN.const <canonical>; local.get t;
  local.get t; fNN.eq; select`.
- **Gas/stack:** `finite_wasm::Analysis::new().with_stack(sizes).with_gas(fees).analyze(wasm)?
  .instrument("meter", wasm)`. The fee visitor must return `Fee::ZERO` for `end` (finite-wasm
  asserts it) — 1 for everything else in the spike. It imports `finite_wasm_gas(i64)`,
  `finite_wasm_stack/unstack(i64,i64)` and six bulk-op hooks `(i32,i64,i64)->i32` as function
  indices 0..8.
- **Inline gas post-pass:** appends a mutable `i64` global exported as `gas_deficit`
  (budget B ⇒ set it to −B; used = d + B) and replaces every `call 0` with
  `global.get d; i64.add; global.set d; global.get d; i64.const 0; i64.gt_s; if unreachable end`.

Harnesses: `host2` (native; wasmi 2.0 + wasmi 1.1 via a renamed dependency + wasmtime 48 with
`default-features=false, features=["cranelift","runtime","std"]`; identical `Linker` wiring via
a macro; metering state in the `Store` data; traps returned as errors) and `webhost` +
`bench2.mjs` (a dependency-free Rust cdylib exporting `h_gas/h_stack/h_unstack/h_memop/
h_host_add` and importing `glue.mod_tick/mod_noop/copy_to_guest/copy_from_guest`; the script
binds guest imports to `host.exports.*` directly and times everything with
`performance.now()`). i64 metering arguments never touch JS, so no BigInt cost; only the
one-off budget writes to the exported global use BigInt.

## Appendix B — main sources

wasmi 2.0 post <https://wasmi-labs.github.io/blog/posts/wasmi-v2.0/> · wasmi usage/wasm32
<https://github.com/wasmi-labs/wasmi/blob/main/docs/usage.md> · wasmtime determinism
<https://docs.wasmtime.dev/examples-deterministic-wasm-execution.html> · wasm nondeterminism
<https://github.com/WebAssembly/design/blob/main/Nondeterminism.md> · wasm-bindgen wasm-in-wasm
<https://rustwasm.github.io/docs/wasm-bindgen/examples/wasm-in-wasm.html> · finite-wasm
<https://github.com/near/finite-wasm> · wasm_runtime_layer
<https://github.com/DouglasDwyer/wasm_runtime_layer> · Rhai wasm builds
<https://rhai.rs/book/start/builds/wasm.html> · Factorio storage / desync
<https://lua-api.factorio.com/latest/auxiliary/storage.html>,
<https://wiki.factorio.com/Desynchronization> · Factorio RCE
<https://memorycorruption.net/posts/rce-lua-factorio/> · Veloren changelog
<https://github.com/veloren/veloren/blob/master/CHANGELOG.md> · Zellij 0.44 (→wasmi)
<https://github.com/zellij-org/zellij/releases/tag/v0.44.0> · Zed extensions
<https://zed.dev/blog/zed-decoded-extensions> · WASM-4 netplay state
<https://github.com/aduros/wasm4/blob/main/runtimes/web/src/state.ts> · Jumpy scripting
discussion <https://github.com/fishfolk/jumpy/discussions/489> · Roblox server authority
<https://create.roblox.com/docs/projects/server-authority>
