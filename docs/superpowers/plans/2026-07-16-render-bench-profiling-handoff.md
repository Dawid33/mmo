# Render Benchmark Profiling Handoff

**Date:** 2026-07-16

**Branch:** `develop`

**Paused HEAD:** `3142cd732cd1d1b99f6c7c615114bb4221ab2b54` (`3142cd7`)

**Status:** Paused during Task 3's third re-review; Tasks 4–7 have not started.

## Objective

Improve `just render-bench` so it produces detailed, machine-readable profiling data and a deterministic human report. The planned output includes frame/FPS timing, rendered quad/triangle/vertex workload, visibility and draw counts, memory accounting, Bevy render diagnostics, provenance, repetition aggregation, optional baseline comparison, and optional Chrome/Tracy capture workflows. This remains report-only: no performance pass/fail gates.

## Authoritative Documents

- Design: `docs/superpowers/specs/2026-07-16-render-bench-profiling-design.md`
- Implementation plan: `docs/superpowers/plans/2026-07-16-render-bench-profiling.md`
- SDD progress ledger: `.superpowers/sdd/progress.md`
- Current Task 3 reports and reviews:
  - `.superpowers/sdd/task-3-report.md`
  - `.superpowers/sdd/task-3-review.md`
  - `.superpowers/sdd/task-3-fix-report.md`
  - `.superpowers/sdd/task-3-rereview.md`
  - `.superpowers/sdd/task-3-runtime-fix-report.md`
  - `.superpowers/sdd/task-3-rereview-2.md`
  - `.superpowers/sdd/task-3-run-authority-report.md`

The `.superpowers/sdd/` directory is ignored scratch state but is present in this workspace.

## Completed Work

### Task 1 — typed metric summaries

Commit `0243486` (`refactor(client): type render benchmark metric summaries`).

- Added typed float/count summary structures.
- Preserved the existing nearest-rank percentile contract.
- Independent review passed.
- Controller verification: 12/12 render-bench tests passed at that checkpoint.

### Task 2 — authoritative quad workload

Commit `6bc6a58` (`perf(client): expose exact terrain quad workload`).

- Added `TerrainQuadCount` at mesh-generation authority points.
- Added exact resident/submitted quad and vertex metrics for pull rendering.
- Extended asynchronous GPU readback to visible chunks plus visible quads.
- Preserved the bounded, nonblocking readback ring; no synchronous GPU wait was introduced.
- Independent review passed.
- Controller verification: 31 voxel-pull, 90 meshing, and 2 fallback tests passed.

### Task 3 — schema-v2 telemetry and authority fixes

Task 3 spans four commits:

- `da57a58` — `feat(client): emit render benchmark schema v2 telemetry`
- `bd29a69` — `fix(client): align render benchmark profiling schema`
- `4adf763` — `fix(client): report authoritative render benchmark mode`
- `3142cd7` — `fix(client): lock render benchmark backend authority`

Implemented so far:

- Schema-v2 nested output with atomic same-directory replacement.
- Stable typed unavailable reasons and unique metric paths.
- Independent frame-wall and FPS samples.
- CPU/GPU render-pass diagnostics and pipeline statistics maps.
- Provenance, requested/resolved backend metadata, thread counts, camera path, adapter data, and repetition identity.
- Exact pull-renderer geometry and draw telemetry; unsupported counts remain null rather than guessed.
- Logical versus reserved GPU memory accounting with saturating totals.
- Direct and indirect terrain draw counts kept separate.
- Actual render-world near mode and visibility source carried through telemetry.
- Generation-tagged asynchronous callbacks reject stale samples across renderer-mode transitions.
- Near renderer mode/generation and far runtime backend participate in benchmark readiness and run authority.
- Warmup restarts and clears samples on backend changes; measurement fails rather than mixing samples.
- Far runtime state distinguishes `brick`, `raster`, `mixed`, and `unavailable`; pure brick readiness rejects raster fallback.
- Missing required frame samples produce a failed artifact and non-success exit decision.

Latest controller verification at `3142cd7`:

- `cargo test -p client renderer::render_bench --lib` — 34 passed.
- Implementer verification for the same patch:
  - `cargo test -p client renderer::voxel_pull --lib` — 33 passed.
  - `cargo test -p client renderer::far_voxel --lib` — 93 passed.
  - `cargo test -p client --test near_gpu_missing_hzb` — 2 passed.
  - `cargo check -p client` — passed.
  - Scoped rustfmt and `git diff --check` — passed.

Workspace-wide strict Clippy/formatting are already blocked by pre-existing patched-vendored and unrelated formatting issues; do not rewrite vendored forks to make these commands green.

## Exact Pause Point

A fresh third Task 3 re-review had just started for commit range `4adf763..3142cd7`. Its review package is:

`.superpowers/sdd/review-4adf763..3142cd7.diff`

The reviewer was intentionally interrupted when the user requested a pause. It did not produce `.superpowers/sdd/task-3-rereview-3.md`, so Task 3 is **not yet approved** even though focused tests pass.

The third re-review must verify both previously open Major findings:

1. A near mode/generation change cannot mix samples in a successful artifact. Trace readiness, authority locking, warmup restart/sample clearing, measurement failure, and complete-result backend selection.
2. Far brick/raster/mixed runtime state is authoritative. Trace transient `FarRasterFallback`/raster-ready markers, pure-brick readiness, draw metric selection, unavailable reasons, failure metadata, and complete-result backend selection.

Also check for false readiness, reasonless timeout, guessed raster direct draws, mislabeled failed/complete output, and regressions in the already-fixed snapshot-generation logic.

## Resume Procedure

1. Confirm `HEAD == 3142cd7` and inspect `git status --short`.
2. Do not touch or stage the unrelated documentation listed below.
3. Start a fresh read-only Task 3 reviewer using:
   - `.superpowers/sdd/task-3-rereview-2.md`
   - `.superpowers/sdd/task-3-run-authority-report.md`
   - `.superpowers/sdd/review-4adf763..3142cd7.diff`
   - Task 3/Task 4 plan sections and the design specification.
4. Write the verdict to `.superpowers/sdd/task-3-rereview-3.md`.
5. If Critical/Major findings remain, use a fresh focused fixer, run scoped tests, commit exact task files only, and re-review.
6. When Task 3 passes, update `.superpowers/sdd/progress.md` and begin Task 4 from `.superpowers/sdd/task-4-brief.md`.
7. Continue Tasks 4–7 in order, with a fresh implementer and reviewer per task.
8. Before final completion, run the broad final review and the repository-required verification, especially `just test-full`; attempt the real hardware render-benchmark smoke if the environment supports it.

## Remaining Planned Tasks

### Task 4 — repetition and baseline report tool

Create `scripts/render_bench_report.py`, unit tests, and schema-v2 fixtures. It must aggregate medians across runs, retain run spread, emit neutral absolute/percentage deltas, reject incompatible/failed/schema-v1 input, and return success regardless of performance direction. The brief already exists at `.superpowers/sdd/task-4-brief.md`.

### Task 5 — canonical orchestration and provenance

Update the `justfile`/helpers so one `just render-bench` workflow creates repetition JSON artifacts plus a deterministic Markdown report, with optional baseline input and complete provenance.

### Task 6 — user-facing documentation

Document normal reporting plus optional Chrome trace and Tracy workflows, including overhead caveats and the distinction between headline-comparable normal runs and instrumented diagnostic runs.

### Task 7 — end-to-end verification

Run report fixtures, orchestration checks, focused Rust tests, `just fmt-check`, `just clippy`, `just test-full`, and an adapter-backed smoke run when possible. Record pre-existing repository-wide failures precisely rather than modifying vendored deterministic forks.

## Known Review Notes to Carry Forward

- Task 2 Low: the `visible_quads` shader atomic runs in production even when benchmark readback is disabled; consider telemetry-specialized shader variants in final review, but it was not a Task 2 blocker.
- Task 2 Low: the asynchronous retention test does not explicitly assert submitted quad/vertex retention.
- Far visible brick proxy-instance readback is intentionally absent; report it as unavailable until a nonblocking authoritative readback exists.
- Empty pipeline-diagnostic maps use `sample_not_observed`; do not claim `adapter_feature_unavailable` without authoritative feature evidence.
- Legacy near and raster-far exact main-view direct draw counts remain unavailable rather than guessed.

## Unrelated Worktree State — Preserve Exactly

At pause time these user-owned documentation changes were present and unrelated to render-bench profiling:

- Modified: `docs/superpowers/plans/2026-07-16-voxel-render-quality-benchmark.md`
- Untracked: `docs/superpowers/plans/2026-07-16-voxel-hybrid-brick-renderer.md`
- Untracked: `docs/superpowers/plans/2026-07-16-voxel-raster-lod-optimization.md`
- Untracked: `docs/voxel-renderer-potential-optimisations-report.md`
- Untracked: `docs/voxel-rendering-architecture-report.md`
- Untracked: `docs/voxel-wiki-rendering-reference-audit.md`

Never stage, reformat, overwrite, or delete these files as part of this profiling task.
