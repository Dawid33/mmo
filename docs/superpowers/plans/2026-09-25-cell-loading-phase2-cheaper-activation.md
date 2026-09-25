# Cell loading phase 2 — cheaper activation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Take the O(whole active set) work out of cell activation, release,
block edits and the periodic terrain-hash check, and measure what is left.

**Architecture:** A persistent, undo-logged chunk index in `GameData`
(the spec §2 `chunk_index`) replaces every full ECS scan that recovers chunk
coordinates from body translations. A per-cell edit counter (also sim state)
lets the server's `RegionRunner` keep each active cell's terrain term cached
and recompute only cells that were edited, so the fingerprint check stops
hashing every materialized chunk.

**Tech Stack:** Rust, the `#[rollback]` macro's tier-1 `UndoMap`, rapier fork.

**Spec:** `docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md`
(§2 "Performance risk #1", Build order phase 2). Handoff:
`docs/superpowers/plans/2026-09-25-island-cells-remaining-phases-handoff.md`.

## Measurement first (done while writing this plan)

`crates/worldgen/examples/cell_profile.rs` (new) — composition, and with
`--time` the activation/scan timings. 1024-chunk region, 16-chunk (32 m) cells,
the 5×5 around the cell-mode spawn:

- 19 256 chunks (≈770/cell, 540–1 360), 22 % uniform (one cuboid collider),
  78 % partial (1–3 `Voxels` colliders); ≈24 k colliders.
- **0 all-water chunks** — here or at four far-away cells: the procedural
  world around region (0,0) is land. All-water collapse buys nothing until
  islands bring an ocean (spec phases 3/6); it is DEFERRED to those phases
  (it is also a gameplay change — the sim treats water as solid today, see
  `registry.rs` "the sim only distinguishes AIR from solid").
- Payload ≈70 KiB raw / 15 KiB lz4 per cell — not a problem.
- Timings (`--time`) NOT run: the user runs benchmarks on this box; ask them.

Consequences for this plan: the spike candidates we can remove without
numbers are the O(all chunks) scans (≈4 per activation + one full chunk index
per block edit, on clients re-run on every re-simulation) and the full
fingerprint (15–20 ms measured live). Genesis itself (entity + body +
`Voxels` collider build per chunk) is the unknown the `--time` run answers;
"compact `Voxels`" waits on it.

## Global Constraints

- Rollback bar: `hash(before) == hash(after undo)`, bit-exact; the new fields
  are tier-1 (`#[undo(map)]`) so their undo is typed and exact.
- Raw (unlogged) mutation only on paths that are already raw: genesis and
  the `CellChanges` arm (confirmed head, forgotten at once).
- `game` stays Bevy-free; no new dependencies.
- Named constants for tunables; none are introduced here.
- Verify with `just test-one game "--test <target> <filter>"` (+
  `-- --include-ignored` for slow cell tests) and `just smoke`; never whole
  suites. Finish with `just region_chunks=1024 cells=16 build`.

## Review Focus

1. A chunk created by an edit (placing into empty air) must be in the index,
   dematerialize with its cell, and vanish from the index on undo —
   Task 1 test `an_edit_created_chunk_is_indexed_and_undo_removes_it`.
2. Legacy (non-cell) regions use the same index: edits there must still work —
   covered by the existing `block_edit.rs` suite (run it).
3. A deserialized snapshot (join, park/restore) must carry the index —
   Task 1 test `the_index_survives_a_snapshot_round_trip`.
4. An edit then release in the same tick must store the edited term, not the
   cached one — Task 2 test `an_edit_released_in_the_same_tick_keeps_its_term`.
5. Undo of an edit on a client rolls the edit counter back, so client and
   server state hashes stay equal — existing
   `cells_deactivate_under_prediction_and_replicas_stay_bit_exact` + Task 2's
   rollback assertion.

---

### Task 0: commit the measurement tool

**Files:** Create `crates/worldgen/examples/cell_profile.rs` (already written).

- [x] Commit: `git add crates/worldgen/examples/cell_profile.rs docs/superpowers/plans/2026-09-25-cell-loading-phase2-cheaper-activation.md && git commit -m "feat(worldgen): cell_profile example — cell composition + activation timing"`

### Task 1: persistent chunk index

**Files:**
- Modify: `crates/macros/src/lib.rs` (UndoMap: `insert_raw`, `remove_raw`)
- Modify: `crates/game/src/state.rs` (`GameData.chunk_index`, genesis,
  `create_mesh`, `remove_entity_safe`, `ChunkIndexKey`, `chunk_key_at`)
- Modify: `crates/game/src/cells.rs` (`cell_chunk_keys` via the index,
  `remove_chunk_raw`)
- Modify: `crates/game/src/edit.rs` (drop `chunk_index()` scan)
- Test: `crates/game/tests/cell_activation.rs`, `crates/game/tests/block_edit.rs`

**Interfaces:**
- Produces: `GameData.chunk_index: UndoMap<ChunkIndexKey, EntityKey>` where
  `ChunkIndexKey = (usize, usize, i32)` is `(x, z, y)` — column-major so one
  cell is `cell_chunks` contiguous ranges (one per x);
  `Rollback::chunk_key_at(ChunkCoords) -> Option<EntityKey>`;
  `Rollback::cell_chunk_keys(cell)` unchanged signature, output still sorted
  by `(x, y, z)`.

- [x] **Step 1: tests** (in `cell_activation.rs`):

```rust
#[test]
fn the_chunk_index_matches_the_cells_terrain() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(0, 0), CellCoords::new(1, 0)]));
    let d = s.data();
    assert_eq!(d.data.chunk_index.len(), cell_content(CellCoords::new(0, 0)).chunks.len() * 2);
    for (coords, key) in d.cell_chunk_keys(CellCoords::new(1, 0)) {
        assert_eq!(d.chunk_key_at(coords), Some(key));
    }
    server_apply(&mut s, deactivate(&[CellCoords::new(0, 0)]));
    assert_eq!(s.data().data.chunk_index.len(), cell_content(CellCoords::new(1, 0)).chunks.len());
}

#[test]
fn an_edit_created_chunk_is_indexed_and_undo_removes_it() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    let before = state_hash(s.data());
    let n = s.data().data.chunk_index.len();
    // Block y = 40 is chunk y = 20: empty air above the floor.
    let placed = s.with_data(|rb| rb.set_block_safe([1, 40, 1], game::Block::new(game::BlockId(1))));
    assert!(placed);
    assert_eq!(s.data().data.chunk_index.len(), n + 1);
    assert!(s.data().chunk_key_at(ChunkCoords::new(0, 20, 0)).is_some());
    s.with_data(|rb| rb.undo());   // use the file's existing undo helper
    assert_eq!(s.data().data.chunk_index.len(), n);
    assert_eq!(state_hash(s.data()), before);
}

#[test]
fn the_index_survives_a_snapshot_round_trip() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    let bytes = bincode::serialize(s.data()).unwrap();
    let back: game::Rollback = bincode::deserialize(&bytes).unwrap();
    assert_eq!(back.data.chunk_index.len(), s.data().data.chunk_index.len());
}
```

(Adapt helper names — `with_data` returns the closure value or takes a
`&mut` capture as elsewhere in the file; undo is whatever the file's
existing rollback tests use.)

- [x] **Step 2:** `just test-one game "--test cell_activation chunk_index"` → FAIL (no field).
- [x] **Step 3: macro** — add to the `impl UndoMap` block:

```rust
/// Unlogged insert, for raw (genesis / confirmed-head) paths only.
pub fn insert_raw(&mut self, k: K, v: V) -> ::std::option::Option<V> {
    self.data.insert(k, v)
}
pub fn remove_raw(&mut self, k: &K) -> ::std::option::Option<V> {
    self.data.remove(k)
}
```

- [x] **Step 4: field + maintenance** — in `GameData`:

```rust
/// Terrain chunk entity by `(x, z, y)` chunk coordinates (column-major so
/// a cell is `cell_chunks` contiguous ranges). Replaces scans that
/// recovered coordinates from body translations.
#[undo(map)]
chunk_index: BTreeMap<ChunkIndexKey, EntityKey>,
```

`pub type ChunkIndexKey = (usize, usize, i32);` and
`pub fn chunk_index_key(c: ChunkCoords) -> ChunkIndexKey { (c.x, c.z, c.y) }`.
Genesis: `self.chunk_index.insert_raw(chunk_index_key(coords), e)`.
`create_mesh`: `self.chunk_index.insert(chunk_index_key(coords), e)` (logged).
`remove_chunk_raw(key)`: takes the coords too, `remove_raw`.
`remove_entity_safe`: if the entity has a chunk, find its coords from the body
(as today) and `remove` (logged) before the component removals.
`Rollback::chunk_key_at(c) = self.data.chunk_index.get(&chunk_index_key(c)).copied()`.

- [x] **Step 5: consumers** — `cell_chunk_keys`: for each x in the cell,
  `range((x, z0, i32::MIN)..(x, z0 + cc, i32::MIN))`, map back to
  `ChunkCoords`, then sort by `(x, y, z)`. `edit.rs`: delete `chunk_index()`;
  `block_solid` / `raycast_block_in` / `set_block_in` drop the `index`
  parameter and call `chunk_key_at`. `materialize_cell`'s render path uses
  `chunk_key_at` instead of building its own map.
- [x] **Step 6:** run `just test-one game "--test cell_activation"`,
  `"--test cell_activation -- --include-ignored"`, `"--test block_edit"`,
  `"--test hash_restore"`, `"--test random_ops"` → PASS.
- [x] **Step 7:** commit `perf(cells): persistent chunk index replaces full chunk scans`.

### Task 2: incremental terrain fingerprint

**Files:**
- Modify: `crates/game/src/state.rs` (`GameData.cell_edits`)
- Modify: `crates/game/src/edit.rs` (bump on a successful edit, cell mode)
- Modify: `crates/game/src/region_runner.rs` (`CellTerms.live/edits`,
  `terrain_fingerprint`, `apply_cell_changes`)
- Test: `crates/game/tests/cell_activation.rs`

**Interfaces:**
- Consumes: `cell_chunk_keys` (fast after Task 1).
- Produces: `GameData.cell_edits: UndoMap<CellCoords, u32>` — per cell, the
  number of successful edits ever (wrapping); `RegionRunner::terrain_fingerprint`
  unchanged meaning.

- [x] **Step 1: tests**

```rust
#[test]
fn an_edit_bumps_its_cells_edit_counter_and_undo_restores_it() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    let before = state_hash(s.data());
    assert!(s.with_data(|rb| rb.set_block_safe([1, 1, 1], game::Block::new(game::BlockId(0)))));
    assert_eq!(s.data().data.cell_edits.get(&CellCoords::new(0, 0)), Some(&1));
    s.with_data(|rb| rb.undo());
    assert_eq!(state_hash(s.data()), before);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn an_edit_released_in_the_same_tick_keeps_its_term() {
    // Edit, then drop the player and release — the edit must still be
    // pushed once and survive release (no stale cached term).
    // (Same shape as an_edit_pushes_one_terrain_change_that_survives_release
    // but with no ticks between the edit and remove_player.)
}
```

Write the second test body out in full from the existing
`an_edit_pushes_one_terrain_change_that_survives_release`, moving
`remove_player` directly after the edit.

- [x] **Step 2:** run → FAIL.
- [x] **Step 3:** field `#[undo(map)] cell_edits: BTreeMap<crate::CellCoords, u32>`;
  in `set_block_in`, on each `return true`, if cell mode:
  `let n = self.data.cell_edits.get(&cell).copied().unwrap_or(0); self.data.cell_edits.insert(cell, n.wrapping_add(1));`
- [x] **Step 4:** runner — `CellTerms { pristine, dormant, live, edits }`.
  Activation: first time `live = pristine = dormant = cell_terrain_term`;
  re-activation `live = dormant`; both record `edits = cell_edits[cell]`.
  Release: `dormant = cell_terrain_term(cell)` (computed before the event —
  unchanged, now cheap). Fingerprint (cell mode, no legacy scan):
  for each cell with terms: if active and `cell_edits[cell] != edits`,
  recompute `live` (needs `&mut self`: fingerprint becomes `&mut`);
  sum `(active ? live : dormant) - pristine`. Legacy regions keep
  `self.region.terrain_fingerprint()`.
- [x] **Step 5:** run `--test cell_activation -- --include-ignored`,
  `--test region_runner -- --include-ignored` → PASS.
- [x] **Step 6:** commit `perf(cells): incremental terrain fingerprint from per-cell edit counters`.

### Task 3: wrap-up

- [x] `just smoke`; `just test-threads` (manager/runner touched).
- [x] TODO.md + handoff status (phase 2: scans + fingerprint done; genesis
  timing pending the user's `--time` run; water collapse deferred to 3/6).
- [x] `just region_chunks=1024 cells=16 build`.
- [x] commit docs.
