# Physics Upstream Roadmap: fork fixes now, rapier 0.36 upgrade next

**Spec:** `docs/superpowers/specs/2026-09-22-vendored-deps-update-investigation.md`

Two plans, run in order:

| # | Plan | What it delivers | Rough effort | Changes sim output? |
|---|---|---|---|---|
| 1 | `2026-09-22-physics-fork-fixes.md` | slotmapd `clone_from` leak; nalgebra `SymmetricEigen`; pin + commit the voxels #404/#373 fixes; parry GJK/EPA fixes (#193 #396 #315 #429 #180 #415) | 1.5–2 days | Task 4 only: contact/cast edge cases (more correct TOIs and touching normals) |
| 2 | `2026-09-22-rapier-035-upgrade.md` | rapier 0.36.0 + parry 0.31.1 on glam (retargeted 2026-09-26), hybrid rollback journal, zero-copy `StateHash` hashing, fewer forks, final fork-divergence review | 4–7 weeks | Yes: engine switch; movement held to the Phase 0 behaviour goldens |

## Why this order

- **Plan 1 is not wasted work.**
  - Everything it ports is *in* upstream parry 0.30.2.
  - Its tests (`gjk_upstream.rs`, `voxels_upstream.rs`, `nalgebra_fixes.rs`) become port-parity checks: they must stay green across the switch, which is how we know upstream's versions behave like the ones we validated.
  - It fixes live gameplay weeks before the upgrade lands. #429 (TOI short by up to ~3.6 mm on our 8–16-unit cuboids) is a candidate cause of the uphill 1-voxel terrace speed dip.
- **Plan 2 Phase 0 records behaviour on the engine we trust**, before anything changes: movement trajectories, a canonical scene hash, and a native-vs-wasm32 hash.
- **Plan 2 lands side by side** (`crates/{rapier,parry,physics}-next`), so `develop` builds and `just smoke` passes at every commit, until one short switch phase.

## Timeline

```
Plan 1 ──► P2.0 rig ──► P2.1 spikes ──► ⚑ user gate (D4, D5, D1) ──► P2.2 vendor
         ──► P2.3 fork patches ──► P2.4 journal + digests ──► P2.5 switch ──► P2.6 docs/sweep/divergence review
```

| Phase | Effort |
|---|---|
| P2.0 | 2–3 d |
| P2.1 | 2–3 d |
| P2.2 | 0.5 d |
| P2.3 | 1–1.5 wk |
| P2.4 | 1.5–2 wk |
| P2.5 | 1 wk |
| P2.6 | 1–2 d |

## Decisions (all confirmed by the user, 2026-09-27)

| # | Decision |
|---|---|
| D1 | Hybrid journal: first-touch body/collider capture + per-tick wholesale islands/narrow/broad clones. |
| D2 | Own zero-copy `StateHash` trait + derive (not bincode digests — the user measured those far slower than `Hash` impls). |
| D4 | Math forks (`simba`, `nalgebra`, `approx`, `ordered-float`) → crates.io or dropped; no new forks. Reverses the CLAUDE.md rule. |
| D5 | Side-by-side `*-next` crates on `develop`. |
| D6 | `length_unit = UNITS_PER_METER`. |
| D7 | Keep `depenetration_lift`; gate upstream #485 off with a `depenetrate` flag. |

Still needing the user: every timing run (Task 4.4 Step 4 included), the Phase 5 live run, `test-full`, and the cleanups Task 6.3 proposes.

## Known risks

1. **The staged solver writes bodies through a raw pointer** and bypasses the capture hooks. Mitigations: a debug assert and the revert fuzzer (Task 4.2).
2. **Epoch-keyed memos in `PhysicsPipeline` go stale after a rewind.** Mitigation: invalidate them after every revert (Task 4.3).
3. **std `acos`/`cos` in glamx `SymmetricEigen3` breaks native/wasm parity for voxel mass properties.** Mitigations: keep the fixed-body mass-props skip (Task 3.1); the wasm golden (Task 0.3).
4. **Upstream's KCC depenetration (#485) and touching semantics (#106) differ from our patches.** Handled by D7 and D8, pinned by `sunk_body.rs` and `shape_cast_grazing.rs`.
5. **Bevy's glam 0.32 vs rapier's glam 0.33.** The two stay separate types; convert only through arrays in `renderer/convert.rs`.

## Not planned

- **Dropping the `block-mesh` fork for a direct `ndshape` dependency.** Independent of all this; do it whenever.
- **rapier's `parallel` or `simd8` features.** Regions are the threading unit.

**Status 2026-09-27:** Plan 1 (H1+H2) landed as 47e4ebe..bee7a9b. Plan 2 Phase 0 (H3) landed bdcce66..dcb3aab; Phase 1 (H4) landed 04dd668..bf8da0a and the gate passed (D1–D7 confirmed). H5 landed bb7d5b5..98a21dc (import commit bb7d5b5). H6 landed 933eadf..5612429 plus a docs commit (Tasks 3.2–3.4, H5 review minors, H6 review fix). H7 landed 394aac5..271c9ee plus a docs commit (Tasks 4.1–4.3 and the H7 review fixes: the revert self-invalidates the pipeline and CCD memos, `step_journaled` refuses `parallel`). H8 landed b38904a..c94268d (Task 4.4 Steps 1–3: `state-hash` crates, fork derives and hand impls, journal digests, native = wasm32 `StateHash` constant, review fix); its Step 4 cost check then ran with the user's go-ahead: `StateHash` is 0.68–1.03× the old `Hash` per structure (1.7–9× faster still through a buffered hasher). H9 landed 17c2887..beed84c (Tasks 5.1–5.3: the game, client, server and worldgen on the rapier 0.36 / parry 0.31 forks; old forks and the nalgebra/simba/approx/ordered-float forks deleted; `just smoke` green). Expected red until H10 re-records: the `wasm_determinism` and `scene_hash_golden` constants, plus `physics_goldens` terrace_uphill, which D6 moves. Next: H10, starting with that golden.

## Handoff sequence (one agent per row, strictly in order)

Every handoff starts with the common preamble below. It then names a plan, a task range and a stop point. An agent never continues past its stop point; it reports what landed (commit hashes), what failed, and any value it re-baselined.

**Common preamble:**

> Read `CLAUDE.md`, `docs/superpowers/plans/2026-09-22-physics-upstream-roadmap.md` and the plan named below; the plan's Global Constraints and Review Focus apply to you.
> Work on `develop` in `/home/dawids/Projects/mmo`. No worktrees, no branches, no `git stash`. Stage by hunk, because the user has uncommitted work in the checkout.
> Verify with `just test-one` / `just smoke` only. Ask before any timing run, benchmark, live game run or whole-suite run.
> Use superpowers:executing-plans. Stop at the stop point and report.

| # | Plan | Tasks | Stop point / gate | Size |
|---|---|---|---|---|
| H1 | physics-fork-fixes | 1, 2, 3 (slotmapd leak, nalgebra eigen, voxels #404/#373 pins) | After Task 3 commit | ½ day |
| H2 | physics-fork-fixes | 4, 5 (GJK/EPA bundle, docs) | After Task 5; report any moved expected values | 1–1.5 days |
| H3 | rapier-035-upgrade | Phase 0: 0.1–0.4 (behaviour goldens, scene hash, wasm determinism, parity notes) | After 0.4 | 2–3 days |
| H4 ✅ | rapier-035-upgrade | Phase 1: 1.1, 1.2 (spikes), then 1.3 (write the decision summary) | **USER GATE:** passed 2026-09-27 | 2–3 days |
| H5 ✅ | rapier-035-upgrade | 2.1 (vendor `*-next`), 3.1 (fixed mprops skip), 3.5 (pair filter), 3.6 (length_unit note) | After 3.6 | 1–2 days |
| H6 ✅ | rapier-035-upgrade | 3.2, 3.3, 3.4 (voxels cast, continuous-surfaces + #106 filters, KCC snap/depenetration) | After 3.4 | 1 week |
| H7 ✅ | rapier-035-upgrade | 4.1, 4.2, 4.3 (arena primitives, hybrid StepJournal + fuzzer, memo invalidation) | After 4.3 | 1–1.5 weeks |
| H8 ✅ | rapier-035-upgrade | 4.4 (`StateHash` crates + derives in the forks, cached voxels/BVH digests; also `StepJournal::pre_digest`/`revert_digest` and the two digest scenario tests H7 left out) | After 4.4 (its cost check needs the user's go-ahead) | 3–4 days |
| H9 ✅ | rapier-035-upgrade | 5.1, 5.2, 5.3 (switch, migrate game incl. `cells.rs`, client/server/worldgen) | `just smoke` green | 3–5 days |
| H10 | rapier-035-upgrade | 5.4, 5.5 (behavioural verification, wasm, live check) | **USER GATE:** live run with the user | 2–3 days |
| H11 | rapier-035-upgrade | Phase 6: 6.1 docs/memory, 6.2 `test-full` (ask first), 6.3 fork-divergence review | **USER GATE:** approve the 6.3 cleanup list | 2 days |
