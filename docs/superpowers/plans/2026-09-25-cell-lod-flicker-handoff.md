# Handoff — LOD flicker / seams in large cell-mode regions (2026-09-25)

Repro: `just region_chunks=1024 cells=16 run` (a 2 km region, cell loading on,
only the home region loads — `LOD_RING_TABLE` for >128 is `[(0, LARGE_HOME_FACTOR)]`,
i.e. one factor-16 LOD key covering the whole home region). Add
`VOXEL_FAR_RENDER=raster|brick` to A/B the two far renderers, and
`VOXEL_LOD_RINGS=off` to remove LOD entirely (user saw **zero** flicker that way,
so every symptom below involves LOD).

Context: spec `docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md`,
plans `docs/superpowers/plans/2026-09-25-cell-loading-phase{1,1b,2-large-regions}.md`.

## Symptoms the user reported (in order)

1. Overlapping/flickering textures in the player's cell ("two blocks in the same place") —
   fixed earlier by home-LOD masking under active cells (`HomeCellMask`, `mask_home_lod`,
   `game::lod_cell_masked`) and a home ring at factor ≤ `CELL_CHUNKS`.
2. Strip flicker along hillside contours in the distance, "between current and LOD",
   also while standing still. `0a1ec15` (brick depth push, `FAR_DEPTH_PUSH_*` in
   `far_voxel.wgsl`) did NOT fix it; it also shows in raster.
3. After `395a9b7` (masked LOD cells kept resident + solid to neighbours): a **gap**
   between near chunks and LOD chunks (screenshot: sky showing through a line), and the
   flicker "further out between LOD chunks" (red dirt stripes along contours).

## What landed this session

- `d68867e` — revert of `395a9b7`. Why it was wrong: near chunks treat missing
  neighbours as air and draw only OUTWARD-facing edge walls (culled from inside). The
  walls you see from inside are the LOD's inward-facing walls toward masked cells —
  so masked cells MUST stay air to their LOD neighbours. Making them solid removed
  those walls → see-through gap wherever the LOD surface is above the near surface.
- `aae81f9` — **root cause of the distance flicker**: `RegionRunner`'s terrain
  fingerprint summed only materialized chunks, so every cell activation/release
  pushed `TerrainChanged` → manager `invalidate_region_lod` → rebuilt the whole
  home LOD key (~4k cells) → client `apply_lod_events` spawned a new root and
  `retire_lod_roots` despawned the old one as soon as ONE new child meshed →
  most of the LOD vanished and re-meshed progressively on every cell load.
  Fix: cell-mode fingerprint is relative to pristine worldgen
  (`CellMode.terms: HashMap<CellCoords, CellTerms{pristine, dormant}>`,
  `Rollback::cell_terrain_term`, `Rollback::chunk_terrain_term`,
  `RegionRunner::terrain_fingerprint`). Tests:
  `loading_and_releasing_pristine_cells_pushes_no_terrain_change`,
  `an_edit_pushes_one_terrain_change_that_survives_release` in
  `crates/game/tests/cell_activation.rs` (33/33 green; region_runner, lod_streaming,
  cell_manager green; smoke green at 1024/16).

Live evidence (user run after aae81f9, Intel iGPU, not the 6700): server `[server]`
lines show `lod built=1` once at join, then `built=0` for all 28 windows (~2.5 min)
while cells streamed — the rebuild churn is gone. **The user has NOT yet reported
how it looks** — first thing to ask.

## Open / next steps

1. **Get the visual verdict** on aae81f9 + d68867e: is the distance flicker gone? is
   the near/LOD gap gone? Try both `VOXEL_FAR_RENDER=raster` and `brick`.
2. If flicker remains while standing still: it is not churn any more (server shows no
   rebuilds). Candidates, in order:
   - Near/LOD coplanar surfaces at the masked boundary: LOD any-solid rounds the
     surface UP a factor-16 cell (1 block) vs near half-blocks; tops aren't coplanar,
     but LOD inward walls vs near geometry at the cell edge could be. Check with
     `VOXEL_FAR_RENDER=raster` + a screenshot at the boundary.
   - Retirement machinery still despawns the old root after ≥1 meshed child
     (`retire_lod_roots`, `has_meshed_child`) — any legit re-send (a real edit) still
     pops the whole region. Proper fix: diff re-received payloads per LOD cell and
     replace only changed cells (or keep the old root until the new one is fully
     meshed). Needed anyway: an edit currently re-sends a 4k-cell key.
   - Red dirt stripes along contours may simply be real riser faces of whole-block
     LOD steps (no half blocks at factor 16) aliasing at distance — steady when still =
     expected; shimmering when still = a bug.
3. Server-side cost of a real edit at 2 km: whole-key rebuild + resend. Consider
   dirty-rect LOD updates (per-cell LOD keys inside the home region).

## Constraints (from CLAUDE.md / user)

- Work on `develop`, no worktrees. `just test-one` / `just smoke`; never whole suites.
- Ask before running client test binaries, benchmarks, or launching the game; never
  build a different `region_chunks`/`cells` config while the user's game runs
  (check `pgrep -af target/debug/(client|server)`). After changes, finish with
  `just region_chunks=1024 cells=16 build` so the user's run is fresh.
- Other open items (not this issue): server tick spikes from cell genesis (~64 ms) and
  terrain hash (~15–20 ms/check); phase 3 island worldgen; zstd snapshots; Postgres
  cell store; ocean region + fast travel; pods.
