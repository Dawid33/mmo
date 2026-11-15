# Handoff: implement the static water layer

**Date:** 2026-11-07 · **For:** a fresh session that executes the plan
**Status:** plan written, checked against the landed code, reviewed for gaps. Nothing is
implemented yet.

## Your task

Execute `docs/superpowers/plans/2026-11-06-water-static-layer.md`, Tasks 1–10, in order.

- **Recommended method:** `superpowers:subagent-driven-development`. That means a fresh
  implementer per task, a task review, a fix loop, and a final whole-branch review on opus.
  - Tasks 5, 6 and 8 cross the `game`, `worldgen`, `server` and `client` crates through shared
    interfaces.
  - A determinism or hash mistake is expensive to find later.
- If Dawid has not confirmed the method in the new session, **ask him first**: subagent-driven
  or native (`superpowers:executing-plans`).

## Read first, in this order

1. **The plan:** `docs/superpowers/plans/2026-11-06-water-static-layer.md`. Its "Global
   Constraints", "Fit with seam-transparent regions" and "Static-phase semantics" sections
   apply to every task.
2. **The design it implements:** `docs/superpowers/specs/2026-11-06-water-revisit/00-synthesis.md`
   and `02-engine-today.md`. The engine audit is older than the seam work, so its line numbers
   are stale. The plan names sites by function.
3. **`CLAUDE.md`**, especially:
   - the testing rules;
   - "Seam-transparent regions" (halo, `WorldView`, intents, bodies, the seam harness);
   - implied ground;
   - voxel pull and the WebGL2 fallback;
   - LOD;
   - movement.

You don't need to re-read the earlier research (September water efficiency, the July plan, the
prototype). The plan already carries what matters from them.

## What the plan builds (one line per task)

1. **`BlockKind::Liquid`.** Water stops being solid: no collider, and the ray and orders see
   through it. `is_liquid` / `blocks_ray` replace every `is_water` / `BlockId(5)`.
2. **`Rollback::water_units_at(WorldBlockPos)`** over own ∪ halo, plus DF depth rules in
   pathfinding (wade ≤ 3/7, not standable ≥ 4/7).
3. **Swimming.** `Immersion` in `Motion`, set for every mover by one
   `tick_immersion_safe` pass placed after `tick_intents_safe` in `region.rs`.
4. **Buoyancy.** A `tick_buoyancy_safe` pass over every owned `EntityKind::Body`. It sets
   `user_force` and damping, which `BodyState` already carries across seams.
5. **Recipe water.** `ColumnGround.water_top` / `water_body`; bodies `{level, volume}` labelled
   at genesis into `GameData.water_bodies`. Water blocks are still written in this task.
6. **Client water pass.** A per-tile `Mesh` + `ExtendedMaterial<StandardMaterial,
   WaterExtension>` (blended, depth write on, cut discard), and the chunk mesher treats liquid
   as empty.
7. **Underwater fog.** The height grid and the minimap read recipe water.
8. **`LodWater`** in `LodData`, built from the recipe and never from the terrain fingerprint.
   LOD terrain treats liquid as air.
9. **Worldgen stops writing water blocks** (ice stays). `water_units_at` becomes recipe-only.
10. **Docs:** `CLAUDE.md` and `TODO.md`.

Every commit leaves `develop` playable: water leaves chunk data only in Task 9.

## Facts checked on 2026-11-07 (develop `1879620`), so you don't re-derive them

- **The halo carries neighbour recipes.** `GameData::halo_ground`, seeded by
  `HaloUpdate.ground`, mirrors the neighbours' `ColumnGround`. New `ColumnGround` fields reach it
  for free.
- **Halo fixed colliders** go through `crate::collider_cells` (`halo.rs`), so Task 1's
  `is_solid` change covers them.
- **One fix covers both raycasts.** `WorldView::raycast` → `raycast_block_in` →
  `block_solid` → `cell_solid` (`edit.rs`).
- **`CameraController::on_tick` only gets `Undo<GameData>`**, with no `halo_block`. That is why
  immersion is a pre-pass on `Rollback`, not a controller parameter.
- **Region tick order** (`region.rs` ~770): `tick_intents_safe` → `tick_slimes_safe` →
  walkers → people → controllers (physics) → animators → `tick_voxel_bodies_safe`. The two new
  passes go right after `tick_intents_safe`.
- **Bodies.** Generic `EntityKind::Body` (`body.rs`) has no per-tick pass. Its weight is a
  constant `user_force` (`BODY_GRAVITY`). Voxel bodies use `VOXEL_BODY_GRAVITY`. Both equal
  `movement::GRAVITY`.
- **Hard-coded water today:**
  - `pathfind.rs` `WATER` / `is_water`, also used by `mining.rs` `mineable` / `mineable_seen`,
    `item_piles.rs` `pile_support` and `player.rs` restore;
  - `slime.rs` `SLIME_NO_SPAWN_BLOCK`;
  - client `block_target.rs`, and `commander/mod.rs` `id_of("water")`.
- **`REGION_FILE_VERSION` is 21.** The plan bumps it twice, each time +1 from the tree.
- **Tests:**
  - `handoff_schedule` lives in the `it` binary
    (`just test-one game "--test it -- --include-ignored handoff_schedule::<name>"`);
  - `voxel_body` has its own `[[test]]`;
  - `just test-threads` runs the justfile's `seam_tests` list, so every new seam test must
    match it;
  - every water test calls `common::install_real_block_props()`.
- **Concurrent work:** terrain slopes have landed (half blocks are gone, slopes never sit at or
  under water). Worldgen `ColSample` still has `water_top`, `top` and `ice_from`.

## Process rules (from CLAUDE.md and Dawid's preferences)

- **Workspace:**
  - Work on `develop` in the main checkout. **No worktrees**, not even for subagents.
  - Stage only the files a task names; Dawid has uncommitted work in the tree. Run
    `git status` before each commit.
  - Commit trailer: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Testing:**
  - Use `just test-one …` + `just smoke`; never whole suites.
  - **Ask Dawid before** client unit tests (`just test-one client …`), any live run (use
    `adapter=Intel`) or any benchmark.
  - Run `just determinism-wasm` after Tasks 1, 3, 4, 5 and 9.
  - Run `just test-threads` after Tasks 1–5 and 9.
  - Re-pin a golden only with the reason in the commit message.
- **Never weaken the server state-hash interval.**
- **Subagent pitfalls seen in this repo:**
  - Implementers that start background commands can derail before they commit. Verify the real
    git state yourself.
  - Post-edit LSP diagnostics are often stale. Trust the compiler, not the diagnostics or the
    report.
- **Before dispatching:** an opus pre-flight conflict scan of the plan against the tree found
  5 blocking bugs in the water prototype plan, so it is worth repeating here. Only the seam
  facts above were checked; a pre-flight scan has not been run on this plan.

## Decisions already made (don't re-open)

- Static semantics:
  - edits never change water state;
  - placing a block into water makes that cell dry, and breaking the block brings the water
    back;
  - breaking ice reveals water;
  - a dug pocket under the seabed stays dry;
  - bodies are never mutated.
- Rendering is a per-tile Bevy `Mesh` with a blended material on every backend, not a second
  pulled pipeline (trade-off in the plan).
- Water depth is shown as DF 1–7 over a `u16` amount (`WATER_FULL` = 4096 = 16³ voxels).
- Swimming and buoyancy are entity-generic: every mover swims and every body floats.
- No drowning; there is no health system.

## Still open (only affect the later flow plans; mention at the end, don't block)

1. Is the world ledger the water cycle? (recommended: yes)
2. Should clients predict water arriving across a seam? Worth revisiting now that predicted
   transfers land in the server's slot.
3. Before the flow plans:
   - a `PERF_LOG=1` embarked run at 256 m, which Dawid runs himself;
   - a prototype round for budgeted body restructuring.

## When done

- Run the final whole-branch review.
- Update `TODO.md` with the static limits (Task 10).
- Report the commit range and any golden re-pins with their reasons.
- List the checks still pending Dawid: live visual checks of Tasks 6–9, client unit tests if
  he skipped them, and the browser (WebGL2) check.
