# Handoff: implement the world store (Plan A) and the tiled forged world (Plan B)

**Date:** 2026-11-07
**For:** a fresh session whose job is to **implement** two written and reviewed plans, in order:
1. **Plan A:** `docs/superpowers/plans/2026-11-07-world-store-r0-r1.md` (10 tasks). The world store on Postgres and SQLite, one host.
2. **Plan B:** `docs/superpowers/plans/2026-11-07-forged-world-in-db.md` (6 tasks). The forged world stored as tiles in that store.

Plan B depends on Plan A's store crate. Plan B's Tasks 1–3 touch only `worldgen` and could run earlier, but run the plans in order unless the user says otherwise.

## 1. Where things stand

- **Prerequisites are in:**
  - the seam-transparent regions program (`EntityId`, `WorldClock`, the `in_flight` table, bundles, halo, intents);
  - the water static layer, landed through `3036f22`.
- **Both plans and this handoff are untracked files.** Ask the user whether to commit them first; commit only the three plan or handoff files, never `git add -A`.
- **Nothing of either plan is implemented.** The plans' `file:line` references were taken at `1879620`, before water landed, so **re-verify each one before using it.**

## 2. Read first

1. Plan A, whole, especially "Decisions settled in planning", "Sequencing" and "Review Focus".
2. Plan B, whole.
3. `CLAUDE.md`: the build/test rules, `crates/server` (world autosave, the code Plan A replaces), `crates/worldgen` (forged worlds, Plan B), and the water paragraph in `crates/game`.
4. Background only, if a decision needs its reasoning: `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` §0.9–§0.10, and the planning handoff `docs/superpowers/plans/2026-11-06-persistence-plans-handoff.md`.

## 3. Decisions already made (do not re-open)

- **Old world save files are deleted, not converted** (user, 2026-11-07). This is Plan A Task 10.
  - `just clean-old-worlds` lists, then with `--yes` deletes, `worlds/**/*.regions/`, `*.players/`, `*.history(.bak)` and stray region `.bak` files.
  - Forge files (`*.world`), `world.json` and `preview.png` stay; the server imports a forge file into the store on first start.
  - **Run the deletion only after Plan A Task 7 has landed**, after showing the user the dry-run list and getting their confirmation. Until then the running game still reads those files.
- Every decision in both plans' "Decisions settled in planning" sections. Main ones:
  - one row per 16³ chunk (measured: rugged regions ~700 chunks, ~8 KiB each zstd; per-chunk is 7–10 % over one region blob);
  - one concrete `StoreHandle` and one store thread owning a current-thread tokio runtime;
  - the client never links sqlx;
  - the save cut is a future world tick T, and the in-flight rule is "stamp < T and absent from the target's snapshot";
  - player records are written only in cuts;
  - tiles of 256 cells with no halo, coast distance stored, no coarse layer, the `WorldMap` stored, the forge file kept as an archive row.

## 4. Water-specific findings for Plan A (checked 2026-11-07, after water landed)

- **`GameData.water_bodies` must be classified *persist* (into `RegionMeta`), not derive-on-load.**
  - `label_water_bodies` (`crates/game/src/water.rs:65`) runs only at genesis, in `Region::from_columns` (`region.rs` ~:1155–1162).
  - It labels `ColumnGround.water_body` and fills `water_bodies` by reading the genesis chunks. Ice, a solid block in a recipe cell, splits bodies.
  - A region rebuilt from stored chunks would relabel from *edited* chunks and could disagree. So store both the labelled `ground` (it already carries `water_body`) and `water_bodies`; `restore_portable` inserts them raw and **does not** call `label_water_bodies`.
  - Add a water body to `snapshot_roundtrip_is_equivalent`.
- **Swimming state** lives in `Motion` (`Immersion`), which already travels in the bundle through `Carried`, so no new classification is needed. Confirm with the exhaustive `classify` destructure.
- **`REGION_FILE_VERSION` is 23 now.** Irrelevant to the plans: Task 7 deletes `region_store.rs`.
- **LOD:** `LodData` carries `LodWater` (recipe only, outside the terrain fingerprint). Plan A Task 8's `Stored` LOD build must emit it exactly as a `Region` build does; that is already in Plan A's Sequencing section.
- **Codec goldens** (Plan A Task 4) must be recorded on the current tree, since `ColumnGround` has the water fields.

## 5. How to work

- **Execution method:** the user has not chosen yet. Offer subagent-driven (`superpowers:subagent-driven-development`; recommended, because the tasks hand each other exact interfaces and a mistake that ships corrupts or loses saved worlds) or native (`superpowers:executing-plans`).
- **Workspace:**
  - `develop`, the main checkout, **no git worktrees**, including for subagents (CLAUDE.md, memory).
  - The user has concurrent uncommitted work: `git add` only a task's files, and run `git status` before each commit.
  - Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Verification:**
  - Use exactly the commands in each task: `just test-one …`, `just smoke`, `just test-threads` after manager / region-thread / handoff changes, `just determinism-wasm` where a task says so.
  - Never run whole suites. A client test binary is an explicit opt-in: ask.
  - Postgres tests need `just pg-up` (Plan A Task 1) and run through `just test-pg`. Without `MMO_TEST_DATABASE_URL` they skip and pass.
- **Ask the user before:**
  - any benchmark, memory measurement or live game run (one client, `adapter=Intel`);
  - forging a 100 km world;
  - `just clean-old-worlds --yes`.
- **Subagent hygiene** (memory): verify the real git state and test output yourself after each implementer; don't trust their reports or the transient LSP diagnostics after their edits.
- **When a plan step turns out wrong** (a name moved, an assumption fails): fix the plan text in the same commit as the task, and say so in the report.
- **Last task of each plan:** CLAUDE.md paragraphs, as separate `docs(CLAUDE.md): …` commits, as the tasks specify. At the end, offer to update the memory notes (the `multi-instance-k8s-postgres-research` entry and a new "world store landed" note).

## 6. Report at the end

- The commits per task.
- Test commands run and their results.
- Plan deviations.
- Anything left for the user: the `clean-old-worlds --yes` confirmation, live runs, 100 km memory measurements.
