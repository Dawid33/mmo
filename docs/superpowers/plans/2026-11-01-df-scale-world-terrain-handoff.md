# Handoff: implement the DF-scale world terrain

For the agent that implements this. Written 2026-11-01 by the session that researched, designed and
planned it; the user approved the design and asked for this handoff.

## Your job
Execute `docs/superpowers/plans/2026-11-01-df-scale-world-terrain.md` task by task (21 tasks,
9 phases), using `superpowers:subagent-driven-development`, or `superpowers:executing-plans` if you
implement yourself. Ask the user which execution mode they want if they haven't said. The plan
implements `docs/superpowers/specs/2026-11-01-df-scale-world-terrain-design.md`.

**Read the spec fully before Task 1**, then the plan's Global Constraints and Review Focus. Those
apply to every task.

## Read first
1. `CLAUDE.md`, especially:
   - Building and Running (the `just` caps, `test-one`, `smoke`, "never run whole suites");
   - Forged worlds (`crates/worldgen`);
   - Vendored Forks / determinism;
   - Session lifecycle (client).
2. The spec (all of it).
3. The plan (all of it).
4. Only as needed:
   - DF's biome table and fields: `docs/superpowers/specs/2026-09-20-df-worldgen-research/01-pipeline-and-fields.md` §2–3.
   - The forge's own docs: the module comments in `crates/worldgen/src/forge/mod.rs`,
     `erosion.rs`, `knobs.rs`.

## What the user decided (don't re-ask)
- DF Medium scale (~100 km) as the generator's default; "forging time doesn't matter", but they
  asked for the forge to be optimised (spec §12, plan Phase A).
- Real kilometre-plus ranges.
- Biomes visible in play: new blocks, ground cover, ice, glaciers, trees.
- Long view distance is **later**, not in this plan.
- `zstd` is approved (native-only; see the plan's constraints).

My calls, recorded in the spec's "Decisions from the builder check" (fine to keep):
- a quick 16 km / 32 m default for the server, tests and the bare CLI;
- the playable world ends at the map edge, and land continues visually past land edges;
- a ≤ 3× mountain-region budget, with an overrun recorded rather than fixed;
- memory targets;
- no migration of old world files.

## Facts measured this session (so you don't re-measure)
- **Forge**: single-threaded, 500 iterations.

  | World | Time | Peak RSS | File (v2 format) |
  |---|---|---|---|
  | 16 km / 16 m | 29 s | 171 MB | 11 MB |
  | 100 km / 32 m | 9 min 19 s | 1.4 GB | 107 MB |
  | 100 km / 16 m | 69 min | 5.5 GB | 422 MB |

  More rows are in the spec's table.
- **Profile** at 16 km / 16 m:
  - `erosion::route` (Priority-Flood + `BinaryHeap`) 66%;
  - `step` 17%;
  - `Geology::surface_rock` 7%;
  - `discharge` 3.5%;
  - per-iteration allocation 4%.
- **Engine height audit** (spec §7):
  - Ground is capped by `RAMP_TO_Y = 320` (`worldgen/src/pipeline.rs:99`) and `SCAN_TOP = 320`
    (`worldgen/src/lib.rs:44`).
  - Pathfinding stops at block 512 (`game/src/pathfind.rs:29`).
  - The forge squeezes relief to ≤ 256 m (`max_peak_m`, `post::fit_height_band`).
  - The geology column ends at −1,800 m (`forge/geology.rs:41`).
  - Chunk y is `i32` everywhere, and stored chunk bands grow with slope, not with total relief.
- **Dependencies**: `rayon` 1.11 is already in `Cargo.lock` (via parry/rapier). The `zstd` crate
  links C and won't build for `wasm32`, hence native-only.

## Watch-outs
- **The procedural world must stay bit-identical.** After any change near `pipeline.rs`,
  `lib.rs`, `material.rs`, `habitat.rs` or `trees/`, run:
  - `just test-one worldgen "--test it scene_hash_golden::"`;
  - `just determinism-wasm`.

  If a golden moves, stop and ask the user.
- **Ask the user before** long forges (> 2 min), timing benchmarks, or building the client test
  binary (a slow Bevy build). They run their own benches on this box.
- **The tree has unrelated uncommitted work from the user.** Stage only your task's files. Never
  `git add -A`, never stash or reset their changes.
- **No worktrees**: work on `develop` in the main checkout.
- **Subagents** sometimes report success without committing, or derail on background commands.
  Verify real `git log` / `git status` after each task; don't trust the report alone.
- **New-diagnostics noise**: an LSP diagnostic right after an edit is often a stale snapshot.
  Trust `just` build output.
- **Live checks need a GPU session.** Use `adapter=Intel` (the 6700 eGPU is often unplugged, and
  the unpinned fallback is a ~15 fps T600). Task 21 is done with the user.
- **Keep the justfile and CLAUDE.md in sync** in the same commit as any recipe or behaviour
  change (`just forge-bench`, `world-map --layer`, defaults, world-file version).

## Order and checkpoints
- **Phase A (Tasks 1–5)**: forge speed. Report the optimised 100 km / 16 m time to the user
  before Phase B. The target is ≤ 10 min, ≤ 8 GB. If it's missed, show the `perf` breakdown and
  ask.
- **Phase B (Task 6)**: world file v3. Old worlds get refused with a re-forge message.
- **Phases C–F (Tasks 7–14)**: height band, shape, climate, biomes.
- **Phase G (Tasks 15–17)**: biomes visible in play.
- **Phase H (Tasks 18–20)**: world map, generator, tools.
- **Phase I (Task 21)**: with the user. Forge a 100 km world, play a mountain region, check the
  budgets, screenshot each biome.

After each phase: `just smoke`, plus the procedural goldens, plus a one-paragraph status to the
user.

## After this
The next project is the society layer (civilizations, territories, religions):
`docs/superpowers/plans/2026-11-01-civilizations-territories-handoff.md`. It builds on this
plan's biome, region and climate fields and the v3+ world file. Don't start it here.
