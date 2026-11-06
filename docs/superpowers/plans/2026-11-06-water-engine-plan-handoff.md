# Handoff: write the engine plan for water (non-flow phases first)

**Date:** 2026-11-06 · **For:** a fresh session that writes the implementation plan
**Status:** research done, prototype built and measured, no engine water code exists.

## Your task

Write an implementation plan, using the `superpowers:writing-plans` skill. Save it to
`docs/superpowers/plans/YYYY-MM-DD-water-static-layer.md`. Ask the user how to execute it once
it is written.

**Scope: the non-flow phases.** These do not depend on any open decision or on the prototype's
open risks:

1. **Phase 0, a liquid block kind.** Water stops being solid: no collider, not ground, not hit
   by the edit ray. Ice stays solid.
2. **A static water layer outside chunk block data.**
   - Recipe water: sea, forged lakes and rivers are implied from the worldgen recipe, the way
     implied ground is.
   - Water is no longer written into chunks as blocks.
   - Bodies hold `{level, volume}` state, but nothing flows yet.
3. **Swimming.** Read the water layer in `movement::plan`. Buoyancy and drag for bodies,
   adventure-mode swimming, and DF depth semantics for pathing.
4. **Rendering.** A separate blended water pass, plus a water surface layer in the LOD rings.

**Out of scope here; separate later plans:** flow inside a region, flow across seams, and the
water cycle / ledger. Those wait on the follow-ups listed under "Before the flow plans" below.

Before planning, invoke `superpowers:brainstorming` with the user only if a design question
below is genuinely open. Most of it is already decided.

## Read first (in this order)

1. `docs/superpowers/specs/2026-11-06-water-revisit/00-synthesis.md`: the current design. It
   covers flow models, what players can do, communication between regions, build order, and
   open decisions.
2. `docs/superpowers/specs/2026-11-06-water-revisit/02-engine-today.md`: the engine audit with
   file:line references. **This is your map for Phase 0 and the static layer.**
3. `docs/superpowers/specs/2026-11-06-water-revisit/03-prototype-results.md`: measured results,
   and what they imply for the engine.
4. `docs/superpowers/specs/2026-09-22-voxel-water-efficiency/00-synthesis.md`, "User decisions"
   (binding) and §B rendering. Then `03-networking-rendering.md` §B for the water render pass and
   LOD water layer details.
5. `docs/2026-07-15-water-implementation-plan.md`: the July plan. Phase 0 (registry kind) and
   swimming in `movement::plan` are still valid. Its `WaterChunk` representation is
   superseded.
6. `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/08-fluids-and-environment.md`: DF
   depth semantics (walk at ≤3/7, swim at ≥4/7, drown at 7/7).
7. `CLAUDE.md`: everything, especially the testing rules, the build dials, and the sections on
   implied ground, voxel pull, LOD and movement.

The prototype code is the reference for the solver, not something to port. It lives in
`prototypes/water/` (standalone crate; `just water-proto-test`, `just water-proto --golden`,
`just water-proto-wasm`).

## Binding user decisions (never re-propose alternatives)

- **Water is deterministic and in-sim**, run on the server **and** every client as rollback
  state, like physics. Never server-solved.
- **Water is finite everywhere.** The sea is a finite world body with a volume ledger. Buckets
  move volume and never create it.
- The user agreed (2026-11-06) to the flow models:
  - bodies for standing water;
  - a checkerboard frontier CA only where water moves;
  - rivers as chains of reach bodies;
  - one start-of-step, rate-capped link rule for gates, pumps and seams.
- **Players make their own water bodies.** Pour, breach, dam, gate, pump, well, moat and
  pressure plumbing must all work. See the synthesis section "What players can do with water".
- **Hashing:** never weaken the server's state-hash interval. Water state must be `StateHash`
  (never std `Hash` or bincode bytes), using `BTreeMap` only, integer-only, and identical on
  native and wasm32.

## Open decisions (ask the user; they do not block the static phases)

From the synthesis "Decisions for you":

1. **Is the world ledger the water cycle?** Map-edge rivers, evaporation and springs would draw
   from and return to it, so rivers flow forever while total volume is conserved.
   - Recommended: yes.
   - The alternative is "forged rivers asleep until disturbed".
   - This affects only the flow plans. For the static phases, rivers are static either way.
2. **Should clients predict water arrivals across a seam?** Recommended: no. Wait for the
   server's arrival event.
3. **Depth resolution.** Recommended: show DF's 1–7 depth over a `u16` internal amount
   (`FULL` = 4096 per cell in the prototype).

## Engine facts the plan must handle (from `02-engine-today.md`; verify line numbers, they drift)

- **Water today is solid block 5.**
  - `assets/blocks/blocks.ron:17` has no `solid:` key, so the default is `solid: true`
    (`crates/game/src/registry.rs:37-38, 61`).
  - It has colliders (`block.rs:397, 441, 448`).
  - An all-water chunk is one cuboid (`state.rs:884`).
  - The character controller stands on the sea. There is no swim code anywhere.
- **The sim disagrees with physics.** Several sites hard-code `BlockId(5)` as "not ground":
  - `pathfind.rs:32-44`;
  - `player.rs:138`;
  - `item_piles.rs:210`;
  - `mining.rs:81`;
  - `slime.rs:42`;
  - the client's `commander/height.rs:30, 69-83`.
- **The edit ray and orders treat water as solid:** `edit.rs:93, 136`, `orders.rs:184, 198,
  482`.
- **About 77 `== AIR` / `!= AIR` / `is_air()` sites in 22 files.** 20 are in `lod.rs`. The audit
  §1.4 has a per-area table: colliders, edit/ray, orders, items/mining, piles/pathfind/restore,
  voxel bodies, voxel/LOD, client meshing, client fortress UI. Phase 0 must decide each site.
  The registry's `Air | Solid | Liquid` kind should replace ad-hoc `BlockId(5)` checks.
- **Worldgen writes water into chunks:**
  - sea fill up to `SEA_TOP_BLOCK` = 63 (`worldgen/src/pipeline.rs:23, 32`;
    `worldgen/src/lib.rs:142-147, 667-671`, `water_material` at `:274-276`);
  - forged lakes and rivers via `Patch::sample`'s `water_top` (`forged.rs:1225-1229`);
  - ice 1–4 blocks on cold lakes and sea (`forged.rs:594-600`; `lib.rs:229-237`; ice stays a
    solid block);
  - implied ground (`ground.rs`) never contains water;
  - `coarse_tile` emits water as an LOD material (`lib.rs:840-866`), and `lod.rs:56-81`
    `merge_group` treats it as any solid.
- **Rendering:** water goes through the opaque voxel path (`bridge.rs:299`: "water counts as
  opaque"). There is no translucent pass. The minimap colours water by depth
  (`commander/minimap.rs:119`).
- **Why water must leave chunk block data.** One block change at 16³ chunks costs:
  - a copy-on-write clone of the whole chunk `Voxels` shape, up to 4096 keys
    (`edit.rs:341-409`, `state.rs:560-584`);
  - re-merging the whole chunk's mesh plus its coarse twin;
  - a server terrain-fingerprint change, which rebuilds a `TerrainSnapshot` and the LOD keys
    every 25 ticks.

  So water must also stay out of colliders, out of `SetBlocks` and out of the terrain
  fingerprint.
- **Scale.** At 256 m regions, a sea 30 m deep is about 2×10⁶ cells, around 100 MB stored
  per-cell. So **recipe water must be implied** (no storage until disturbed), in the
  `ColumnGround` / `Rollback::implied_block` pattern. A body is `{level, volume}`, with
  membership read from the recipe until an edit disturbs it.
- **Closest existing pattern: item piles.** `GameData.item_piles: #[undo(map)]
  BTreeMap<BlockPos, ItemPile>` plus an expiry index (`state.rs:268-275`). One undo-safe write
  funnel (`item_piles.rs:431`). Settling runs on events, not per tick (`piles_after_write_safe`
  `:294-305`), with a cap and eviction. Reuse this shape for the water layer's sparse disturbed
  cells and its awake set.
- **Rollback cost model** (audit §2):
  - every tier-1 undo entry clones a value, takes the global log mutex, and is as large as the
    largest `Delta` variant (a `Client`);
  - the server never rolls back;
  - the client replays its whole unconfirmed log, with no fixed depth.

  Keep water undo **compact and in its own log or entries**, not cell-by-cell through the
  generic `Delta`.

## Prototype results that shape the engine design (`03-prototype-results.md`)

- **Bodies win.**
  - On standing water, bodies cost 30–80× less per step than the per-cell CA, and their undo is
    10–31× smaller.
  - Lakes stay flat while draining, and U-tubes level exactly.
  - Water is conserved exactly, and results are bit-identical on wasm.
- **Brick-journal undo lost** (5–17× more bytes). Use compact per-cell undo for water. A hybrid
  is a later engine experiment.
- **Body-restructure spikes are the rollback risk.** `big_drain` mean 20 µs, p99 258 µs, max
  1.5 ms, max 579 KB undo in one step. **Design requirements for the flow plans:**
  - a per-step cell budget on body restructuring (relevel, layer drop, absorb), not just on
    expansion;
  - body undo as a delta of membership spans, not a whole-body before-image.
- Solver rules found necessary, which the flow plans must keep:
  - **Relevel on a low join:** absorbing a cell below a body's top, with open ground above,
    relevels the body. Without it, U-tubes don't level.
  - **Mid-air donors only fall.** Without this, a pour from height smears into a film.
  - **Order-free sideways exchange:** compute from one snapshot per colour group, cap donors and
    receivers proportionally, and cap a receiver at the midpoint. Without this the flow is
    biased by scan direction.
  - **Links cap by room before taking, and take from the planned source key.** Return-the-excess
    can crash.
  - **Seam senders subtract their own in-flight sends** from the room they last saw. Remaining
    bounces are about 1–3% of seam flow.
- Not covered by the prototype, so the engine must design these: the sea ledger, map-edge flow,
  a seam with a parked neighbour (a pending-volume ledger at the manager), client prediction
  across seams, and chisel/half-block capacity.

## Before the flow plans (not before this plan)

These were recommended on 2026-11-06 and none was started:

1. **Prototype round:** budgeted body restructuring, plus a timed scenario that splits a body
   during a drain.
2. **Engine measurement:** one `PERF_LOG=1` embarked run at 256 m. It gives mispredicts per
   second and log length per client region thread, and checks whether September's "about one
   mispredict per tick per neighbour region (relayed ghosts)" still happens.
   - If it does, seam water would replay about 20 × L times a second, so fix that first.
   - **The user runs benchmarks themselves: ask before launching.**

## Plan-writing guidance specific to this repo

- **Per-region costs scale with area.** Play regions are 256 m (`just run` etc.); tests default
  to 64 m. Never write region or chunk sizes as literals.
- **Tests:**
  - Use `just test-one <crate> "<target> <filter>"` and `just smoke` (60 s limit).
  - Never run whole suites routinely.
  - Tag any test over 100 ms `#[ignore = "slow: …"]`.
  - Client unit tests are a slow opt-in; ask first.
  - Run `just determinism-wasm` after touching hashed or serialized sim state (water state
    will be).
  - Physics goldens: `just test-one game "--test it physics_goldens::"`.
- **Keep the `justfile` in sync** for any new workflow. Tunables are named constants (a user
  preference).
- **New per-world client resources** must be registered for session reset
  (`add_session_reset` / `SessionLocal` / `SessionScoped`), per CLAUDE.md.
- **Rendering:**
  - the record layout is locked to `voxel_pull*.wgsl`;
  - shaders are compiled in;
  - WebGL2 falls back to per-chunk meshes, and the water pass must work there too (03 §B: bake
    absorption per quad).
  - LOD water belongs in its own per-key surface layer, **out of the terrain fingerprint**.
- **Process:**
  - **No git worktrees, ever** (CLAUDE.md); work on `develop` in place.
  - The user has concurrent uncommitted edits. Stage only named paths, never `git add -A`.
  - Commit trailer: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - Design questions need trade-offs laid out in prose before options.

## How the prototype work was run (if you execute the plan the same way)

- Subagent-driven development: a fresh implementer per task, then a task review, a fix loop
  and a final review.
- A shared `implementer-instructions.md` / `reviewer-instructions.md` per plan workspace
  (`.superpowers/sdd/<plan>/`) kept dispatches short.
- The pre-flight conflict scan was done by an opus agent and found 5 blocking plan bugs. Worth
  repeating.
- Reviews on opus caught two real defects on the hard tasks (solver, links, seam): a mass leak
  and a reachable assert.
- Commit history of the prototype: `63c564e..ea98f71`. Results: `be30900`.
