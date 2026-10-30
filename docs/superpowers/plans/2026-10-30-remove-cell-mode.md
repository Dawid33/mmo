# Remove Cell Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete cell mode (island-sized regions loaded in server-authored cells) and every
branch it added, leaving the whole-region + seam model as the only world model.

**Architecture:** Removal runs outside-in so every task compiles on its own: first the client
and server/worldgen consumers stop using the cell API, then the `game` crate drops the cell
types, sim state and event, then the build dial, LOD table and docs. Code that arrived with
cell mode but serves legacy regions too stays (see Global Constraints).

**Tech Stack:** Rust stable 1.99.1; `game` (rollback sim), `server`, `worldgen`, Bevy 0.19.1
`client`.

**Spec / decision:** Dawid, 2026-10-30: "we will do it the old way … remove cell mode as it
doesn't fit the game play. It is not feasible for clients to simulate the whole world; in the
future the server might be made up of many computers." Cell mode = full-replica lockstep of a
whole island, every client simulating every active cell on one thread. The design being
retired is `docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md`. What stays
is the region-actor model: one region = one thread = one `RegionRunner`, seams via
`scan_boundaries` / `EntityArrived` / ghosts, the `RegionInput`/`RegionOutput` message pair
as the future machine boundary.

## Global Constraints

- Work on `develop` in the main checkout. No worktrees.
- Verify with `just smoke` (60 s cap), `just test-one <crate> "<target> <filter>"`,
  `just test-threads` (touches `WorldManager` / `region_threads`) and `just determinism-wasm`
  (`GameData` and `GameEventKind` change shape). Never the whole suites. Client unit tests
  (`just test-one client "--lib …"`) are a slow opt-in: ask Dawid first. `smoke` type-checks
  the client.
- **KEEP (arrived with cell mode, used by legacy regions):** `GameData.chunk_index` +
  `ChunkIndexKey` (implied ground, edits), `Region::commit_server_event` (Play's server-authored
  events), `RegionSpawner::generates_columns` + `RegionSeed::Deferred` (region-thread worldgen),
  `RegionRunner::terrain_fingerprint`, `worldgen::columns_from_patch`, island worldgen
  (`worldgen/src/islands.rs` quadtree + `just island=`), `Region::pending_departures` /
  `pending_ghosts`.
- **DO NOT TOUCH (same word, unrelated):** `far_voxel::pass` `active_cells` /
  `active_cell_count()`, `render_bench` `active_cells`, `lod.rs` `LodCell` /
  `copy_region_cells_into_tile` / `cells_per_region`, rapier `cell_model`.
- No cell-mode compatibility shims, no `#[allow(dead_code)]` leftovers. If something becomes
  unused, delete it.
- Hash goldens change ON PURPOSE (state layout changes). Re-record them in the task that
  changes the layout. Put the old and new values in that commit message.
- Each commit message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **An old shell still has `VOXEL_CELL_BLOCKS` exported.** It must fail the build with a
   message naming the removal, not silently build legacy. Pinned in Task 4 by the build-time
   assert. Check it with `VOXEL_CELL_BLOCKS=32 just check`, which should fail with that message.
2. **The legacy end-of-tick path loses `freeze_strays_safe`.** Departures and ghosts must be
   unchanged. Pinned by `just test-threads` and `just test-one game "--test handoff_scan"` in Task 3.
3. **Slime cap and spawn gate.** Legacy behaviour must be byte-identical
   (`SLIME_CAP_PER_REGION`, no per-cell gate). Pinned by `just test-one game "--test slime"`
   in Task 3. The scene-hash goldens must change ONLY because of the layout change: re-record
   them after confirming `physics_goldens` (positions) is unchanged.
4. **Edits on the region's outer block ring and bedrock refusals.** These were `cell_chunks == 0`
   conjunctions and become unconditional. Pinned by `just test-one game "--test underground_edit"`
   in Task 3.
5. **LOD for a non-home region and home-region LOD handoff.** The home region must still be
   excluded from LOD exactly as before. `instance.rs`'s cell branch only *removed* home from the
   excluded set. Pinned by `just test-one game "--test lod_streaming"` in Task 3. The client
   side is covered by the `smoke` type-check, plus `just test-one client "--lib lod_rings::"`
   with Dawid's OK in Task 1.

---

### Task 1: Client stops using cell mode

**Files:**
- Modify: `crates/client/src/instance.rs` (field `last_active_cells` :97/:508, fn
  `emit_active_cells` :956 and its calls :772/:1558, the `CELL_CHUNKS != 0` block in
  `update_lod_window` :1086, the `CellChanges` arms :695-698 and :1306-1327 incl. the
  `cell_changes` perf event, the `CellHashMismatch` arm :1427)
- Modify: `crates/client/src/renderer/lod_rings.rs` (the home-LOD mask: struct with
  `cells` / `active` / `cell_chunks` :118-140, the system at :162-180, the
  `lod_cell_masked` call :263, its tests :649-770)
- Modify: `crates/client/src/renderer/bridge.rs` (`ActiveCells` arm :635; the
  `.filter(|_| game::CELL_CHUNKS == 0)` at :582 goes, keeping the legacy behaviour)
- Modify: `crates/client/src/renderer/slice_plane.rs` (`slice_side_for` :177-181 becomes the
  constant `REGION_BLOCKS as u32`; the `&& game::CELL_CHUNKS == 0` at :234 goes; delete test
  `slice_side_collapses_in_cell_mode` :322)
- Modify: `crates/client/src/renderer/fortress_view.rs` (`overlay_dims_for` :109-123 loses its
  parameter and cell branch; delete test `overlay_dims_collapse_in_cell_mode` :447)
- Modify: `crates/client/src/renderer/commander/tiles.rs:274` (delete the early-return block)
- Modify: `crates/client/src/world_scale.rs:91` (delete the forced `near_radius = 0`)
- Modify: `crates/client/src/local_server.rs:69-90` (always `InlineSpawner::default()`, no
  `generate_cell` / `island_info`)

**Interfaces:**
- Consumes: nothing new.
- Produces: no client code names `CELL_CHUNKS`, `CellCoords`, `CellChanges`,
  `CellHashMismatch`, `ClientUpdateEvent::ActiveCells`, `lod_cell_masked`,
  `worldgen::generate_cell` or `game::IslandDirectory`. Task 3 deletes them from `game`.
  Signatures after this task: `fn slice_side() -> u32` (or inline the constant at its one call
  site) and `pub fn overlay_dims() -> UVec3`.

- [ ] **Step 1: Remove the branches listed above.** Keep each legacy arm's behaviour exactly.
  A `CELL_CHUNKS == 0` condition becomes `true`, a `!= 0` block is deleted.
- [ ] **Step 2: Check nothing remains.**
  Run: `grep -rnE "CELL_CHUNKS|CELL_BLOCKS|CellCoords|CellChanges|CellHashMismatch|ActiveCells|lod_cell_masked|generate_cell|IslandDirectory|island_info" crates/client`
  Expected: no output.
- [ ] **Step 3: Verify.**
  Run: `just smoke`. Expected: PASS (it type-checks the client lib, bin and tests).
  With Dawid's OK: `just test-one client "--lib lod_rings::"`,
  `just test-one client "--lib slice_plane::"`, `just test-one client "--lib fortress_view::"`.
  Expected: PASS.
- [ ] **Step 4: Commit** `refactor(client): drop cell-mode branches`.

### Task 2: Server, worldgen and tools stop using cell mode

**Files:**
- Modify: `crates/server/src/lib.rs:350-365` (delete the `cell mode:` log line,
  `.with_cells`, `.with_cell_generator`, `.with_islands`)
- Modify: `crates/server/src/region_threads.rs` (delete fields `cell_chunks`,
  `cell_generator`, `islands`, methods `with_cells`, `with_cell_generator`, `with_islands`,
  `cell_mode`; `spawn` builds runners through the legacy path. Keep `generator` /
  `generates_columns`)
- Modify: `crates/worldgen/src/lib.rs` (delete `generate_cell` :142-172 and test
  `a_cell_generated_alone_matches_the_whole_region_build` :445+; fix the `columns_from_patch`
  doc line that names it)
- Modify: `crates/worldgen/src/islands.rs:229-260` (`island_info` keeps working but returns a
  worldgen-owned `pub struct IslandInfo { pub id: (u8, i64, i64), pub origin_m: [f64; 2], pub size_cells: u32, pub spawn: [u32; 2] }`,
  moved with its doc comments from `crates/game/src/cells.rs:130-143`; its test at :385 stays.
  Nothing consumes it after this plan. It stays as the procedural world's dry-spawn finder for
  a future spawn rule; delete it instead if review prefers)
- Delete: `crates/worldgen/examples/cell_profile.rs` (and its `[[example]]` entry, if
  `crates/worldgen/Cargo.toml` has one)
- Modify: `crates/worldgen/examples/export_region_colliders.rs:62` (delete the guard)
- Modify: `crates/worldgen/tests/underground.rs:33`, `crates/worldgen/tests/scene_hash_golden.rs:58`
  (drop the `game::CELL_CHUNKS == 0 &&` conjunct)

**Interfaces:**
- Consumes: Task 1 (`local_server.rs` no longer calls `generate_cell` / `island_info`).
- Produces: `worldgen::islands::IslandInfo`. `ThreadRegionSpawner` only has `default()` and
  `with_generator(SharedRegionGenerator)`.

- [ ] **Step 1: Remove the items listed above.**
- [ ] **Step 2: Check nothing remains.**
  Run: `grep -rnE "CELL_CHUNKS|CELL_BLOCKS|CellCoords|generate_cell|with_cells|with_cell_generator|with_islands|cell_mode|game::IslandInfo|IslandDirectory" crates/server crates/worldgen tools scripts`
  Expected: no output.
- [ ] **Step 3: Verify.**
  Run: `just test-one worldgen "--lib islands::"`, `just test-one worldgen "--test underground"`,
  `just smoke`, `just test-threads`. Expected: all PASS.
- [ ] **Step 4: Commit** `refactor(server,worldgen): drop cell-mode spawner wiring and generate_cell`.

### Task 3: `game` drops cells (types, state, event, runner, manager)

**Files:**
- Delete: `crates/game/src/cells.rs`, `crates/game/src/cell_tickets.rs`,
  `crates/game/src/cell_store.rs`, `crates/game/tests/cell_activation.rs`,
  `crates/game/tests/cell_manager.rs` (and their `[[test]]` entries in
  `crates/game/Cargo.toml`, if any)
- Modify: `crates/game/src/lib.rs` (`pub mod cell_store` / `cell_tickets` / `cells` :18-20 and their globs :57-59; `ClientUpdateEvent::ActiveCells` :197)
- Modify: `crates/game/src/state.rs` (delete `GameData` fields `cells: CellState` :207 and
  `cell_edits` :217; delete `player_spawn_xz`; the spawn at :941 uses
  `(PLAYER_SPAWN_X, PLAYER_SPAWN_Z)`; `implied_block` :812 loses its
  `self.cells.cell_chunks != 0 ||`. Keep `chunk_index`)
- Modify: `crates/game/src/region.rs` (delete `pending_frozen`, `pending_dormant`,
  `take_frozen`, `take_dormant`, `Region::new_cells`, the `freeze_strays_safe` line :547, the
  `CellChanges` arm :592-630 and its debug label :156)
- Modify: `crates/game/src/region_runner.rs` (delete `CellMode`, `CellTerms`, field `cells`,
  `new_with_cells`, `cell_store`, `apply_cell_changes` and the ticket/prefetch/fingerprint
  code that uses them; `terrain()` returns `self.region.terrain_snapshot()`;
  `RegionSeed::into_runner(self, id, out, generate)` loses its last three parameters and always
  builds `RegionRunner::new`)
- Modify: `crates/game/src/world_manager.rs` (delete `RegionSpawner::cell_mode` and both
  impls; `InlineSpawner` loses `cell_chunks`, `cell_generator`, `islands`, `with_cells`,
  `with_cell_generator`, `with_islands`, and its `generates_columns` override (back to the
  trait default); delete the early return in `maintain` :715; drop `CellChanges` from the
  :594 match)
- Modify: `crates/game/src/protocol.rs` (`GameEventKind::CellChanges` :458 and its arm :481;
  `GameError::CellHashMismatch` :318)
- Modify: `crates/game/src/edit.rs` (:153 and :166 become unconditional legacy checks;
  `set_block_in` :247 is just `set_block_uncounted`, so merge them; delete the
  `block_in_active_cell` gate :263)
- Modify: `crates/game/src/slime.rs` (`slime_spawn_allowed` and its callers: keep the legacy
  per-region cap; delete `SLIME_CAP_PER_CELL`)
- Modify: `crates/game/src/orders.rs:219,230` (delete the `cell_chunks == 0` conjuncts)
- Modify: `crates/macros/src/lib.rs:1203` (doc comment naming `CellChanges`; reword it to
  name the remaining confirmed-head server events, `ClientLeft` / `CreateClientAt`)
- Modify tests: `crates/game/tests/underground_edit.rs` (delete `cell_mode_digs_keep_band_semantics`
  :314 and unused imports), `crates/game/tests/region_from_columns.rs` (delete
  `cell_mode_has_no_implied_ground` :65), `crates/game/tests/world_manager.rs` (delete the
  forged-`CellChanges` block :464-475), `crates/game/tests/wasm_determinism.rs` (delete
  `cell_scene`, `CELL_HASH`, `cell_hash_native`, `cell_hash_wasm` and the doc lines that name
  them)

**Interfaces:**
- Consumes: Tasks 1-2 (no consumer outside `game` names a cell item).
- Produces: `RegionSeed::into_runner(self, id: RegionId, out: Sender<(RegionCoords, RegionOutput)>, generate: &dyn Fn(RegionCoords) -> Vec<(ColumnCoords, Column)>) -> RegionRunner`.
  `RegionRunner::terrain()` = the sim's chunks only.

- [ ] **Step 1: Remove the items listed above.** Compile with
  `just test-one game "--lib --no-run"` until clean.
- [ ] **Step 2: Check nothing remains.**
  Run: `grep -rnE "cell_chunks|CellState|CellCoords|CellChanges|CellStore|CellTickets|CellPayload|DormantCell|cell_edits|cell_hash|freeze_strays|new_with_cells|new_cells|IslandDirectory|IslandInfo|player_spawn_xz|ActiveCells|CellHashMismatch|SLIME_CAP_PER_CELL|block_in_active_cell" crates/game crates/macros`
  Expected: no output.
- [ ] **Step 3: Run the behaviour tests (must pass unchanged).**
  Run: `just test-one game "--test handoff_scan"`, `"--test handoff_state"`,
  `"--test handoff_protocol"`, `"--test handoff_manager"`, `"--test slime"`,
  `"--test underground_edit"`, `"--test region_from_columns"`, `"--test world_manager"`,
  `"--test lod_streaming"`, `"--test play_region"`, `"--test physics_goldens"`, then
  `just test-threads` and `just smoke`.
  Expected: all PASS. If `physics_goldens` moves, stop: removing cells must not change motion.
- [ ] **Step 4: Re-record the hash goldens.**
  Run: `just determinism-wasm`. Expected: FAIL only on the region-scene crc32 + state-hash
  constants in `wasm_determinism.rs`, because `GameData` lost two fields and `GameEventKind` a
  variant. Native and wasm must agree with each other.
  Set the constants to the printed values and re-run. Expected: PASS.
  Run: `just test-one worldgen "--test scene_hash_golden -- --include-ignored"`. If its golden
  for the current chunk size fails the same way, re-record it the same way.
- [ ] **Step 5: Commit** `refactor(game): remove cell mode (cells, tickets, store, CellChanges)`,
  with the old and new golden values in the body.

### Task 4: Build dial, LOD table, region-size cap, docs

**Files:**
- Modify: `crates/game/src/protocol.rs`
  - Delete `CELL_BLOCKS`, `CELL_CHUNKS`, `parse_cell_blocks` and the >256 assert :110-145.
  - `REGION_BLOCKS_CHOICES: [usize; 3] = [64, 128, 256]`; rewrite its doc so it no longer
    mentions cell mode.
  - Add next to the `VOXEL_CELL_CHUNKS` refusal (:80):
    `assert!(option_env!("VOXEL_CELL_BLOCKS").is_none(), "VOXEL_CELL_BLOCKS is gone: cell mode was removed 2026-10-30 (regions are whole; see docs/superpowers/plans/2026-10-30-remove-cell-mode.md)")`.
  - Delete the parse test at :527.
- Modify: `crates/game/src/lod.rs`
  - Delete `LARGE_HOME_FACTOR` :308-317.
  - The `LOD_RING_TABLE` fallback arm becomes `_ => panic!("no LOD_RING_TABLE for this REGION_BLOCKS")`.
  - Delete the >256 / cell branches in the ring-table tests :1035, :1057-1066, :1071.
- Modify: `.cargo/config.toml` (delete the `VOXEL_CELL_BLOCKS` line :20; the comment at :17
  also names it as refused)
- Modify: `justfile` (delete the `cells` variable + export :86-92; edit the region-size comment
  :46-53 to drop `cells=` and cap at 256; fix the `determinism-wasm` comment :648 so it no longer
  mentions `cell_hash`; check that no recipe passes `cells=`)
- Modify: `CLAUDE.md`
  - Region-size dial paragraph: delete the "In cell mode" sentences; 256 is the ceiling.
  - `crates/game` bullet: delete the "Cell loading" passage, "Cell mode has no ground
    semantics", and the cell clauses in the region_runner / LOD notes.
  - `worldgen` bullet: `islands::island_info` no longer reaches `CellState.spawn`.
- Modify: `TODO.md` (delete the "Island regions + cell loading" line; add one line under
  multi-region follow-ups: "Cell mode removed 2026-10-30 (whole regions + seams; clients
  never simulate more than their window)")
- Modify: `docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md` (add a
  `Status:` line at the top: "SUPERSEDED 2026-10-30, removed; see
  plans/2026-10-30-remove-cell-mode.md")

**Interfaces:**
- Consumes: Task 3 (nothing reads `CELL_BLOCKS` / `CELL_CHUNKS`).
- Produces: legal `REGION_BLOCKS` = {64, 128, 256}. A later "512 m regions" change adds 512 to
  the list together with its `LOD_RING_TABLE` row.

- [ ] **Step 1: Make the edits above.**
- [ ] **Step 2: The stale dial is refused.**
  Run: `VOXEL_CELL_BLOCKS=32 just check`.
  Expected: build error containing "VOXEL_CELL_BLOCKS is gone".
  Run: `just region_blocks=512 check`.
  Expected: build error from `parse_region_blocks` ("must be one of REGION_BLOCKS_CHOICES").
- [ ] **Step 3: Check nothing remains.**
  Run: `grep -rnE "CELL_BLOCKS|CELL_CHUNKS|cells=|cell mode|LARGE_HOME_FACTOR" crates justfile .cargo CLAUDE.md TODO.md scripts tools`
  Expected: only the refusal assert and its message.
- [ ] **Step 4: Verify.**
  Run: `just test-one game "--lib lod::"`, `just region_blocks=256 smoke`, `just smoke`,
  `just determinism-wasm`. Expected: all PASS.
- [ ] **Step 5: Commit** `chore: remove the cell-mode build dial; docs`.
