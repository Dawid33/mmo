# Cell Loading — Phase 1 (cells inside one region) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A region can run in *cell mode*: its terrain is split into square cells that the server activates and deactivates around players through one authoritative `CellChanges` event per tick, applied identically by the server and every client replica — proven bit-exact by headless determinism tests.

**Architecture:** Everything lands in `crates/game`. A new tier-2 sim field `GameData.cells: CellState` records the cell size and the active set (legacy regions keep `cell_chunks == 0` and behave exactly as today). `Region::handle_event` gains a `CellChanges` arm that dematerializes/materializes cells with raw (unlogged) terrain ops at the confirmed head; the server fills a per-cell materialized hash, clients verify it. A pure `CellTickets` planner and an in-memory `CellStore` drive it from `RegionRunner` in cell mode. The production server/client stay on the legacy path in this phase (see "Scope").

**Tech Stack:** Rust (edition 2021 in `game`), the `#[rollback]` macro (`crates/macros`), vendored rapier/slotmapd forks, crossbeam, bincode, crc32fast.

**Spec:** `docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md` (sections 1, 2, 3-in-memory-only, 8; build-order phase 1).

## Scope

In: cell geometry, the `CellChanges` protocol, sim state, materialize/dematerialize, hash verification, stray freezing, edit/spawn gating, tickets, in-memory store, `RegionRunner` cell mode, determinism tests.

Out (later phases, do NOT start them here): `WorldManager`/server/client wiring and rendering (**phase 1b — to be planned; the spec's build order folded it into later phases implicitly**), island worldgen and region = island extent (phase 3), zstd/framing (phase 4), Postgres (phase 5), slime leash generalization (the stray-freeze backstop covers correctness in phase 1), persistent `EntityId` (persistence plan Task 1), the prefetch ring and checkpoints (meaningless for an in-memory store; phase 5). The spec's `InlineSchedule` jitter layer needs `WorldManager` wiring, so phase 1 substitutes seeded prediction-lead randomization at the runner level (Task 9); the `InlineSchedule` variant moves to phase 1b.

## Global Constraints

- Work on `develop` in the main checkout. **Never create a git worktree** (CLAUDE.md). The tree has many uncommitted user changes: stage only the files each task names (`git add <paths>`, never `git add -A`/`.`).
- `game` stays Bevy-free, IO-free, sync and wasm-safe.
- Build/test only through `just` (pinned to 80 % of cores). Verify with `just test-one game "<--test|--lib target> <filter>"` and `just smoke`. Never run whole suites.
- Tests slower than 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`; run ignored ones explicitly with `just test-one game "--test cell_activation -- --ignored"`.
- Rollback bar: `hash(before) == hash(after undo)` bit-exact for every logged path; raw (unlogged) ops are allowed ONLY inside the `CellChanges` arm, which both sides forget immediately.
- Tunables are named `pub const`s: `SIM_RADIUS_CELLS`, `CELL_HYSTERESIS`, `CELL_RELEASE_GRACE_TICKS`, `MAX_CELL_ACTIVATIONS_PER_TICK`, `SLIME_CAP_PER_CELL`.
- Never write the region edge as a literal — derive from `REGION_CHUNKS` / `REGION_SIZE`.
- Legacy behaviour must not change: with `cells.cell_chunks == 0` every existing test passes unchanged.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`.
- Known red test: `crates/client/tests/crossing.rs` (since `8e90481`) — not a gate.

## Review Focus

1. **Deactivating a cell a player stands in** → the sim must skip that cell (warn), never extract the player. Test in Task 4.
2. **A block edit targeting a dormant cell** (`set_block_in` creates a fresh chunk when none exists) → must be refused, or the store and the sim diverge. Test in Task 5.
3. **An entity exactly on a cell edge** (`x == k * cell_size`) → belongs to the higher cell by `floor`, identically everywhere. Test in Task 1.
4. **`CellChanges` arriving while the client has no pending predictions** (reconcile parks it in `input_buffer`) → applied on the next reconcile pass, replica converges. Test in Task 9.
5. **Activation requested for a cell missing from the store** → skipped (logged), never a panic; ticket retries. Test in Task 8.

## File Structure

| File | Responsibility |
|---|---|
| `crates/game/src/cells.rs` (create) | `CellCoords`, cell geometry helpers, `CellState`, `CellChanges`/`CellPayload`/`DormantCell`, Rollback materialize/dematerialize/hash/freeze/gates |
| `crates/game/src/cell_tickets.rs` (create) | `CellConfig`, tunables, pure `CellTickets` planner |
| `crates/game/src/cell_store.rs` (create) | in-memory `CellStore` |
| `crates/game/src/lib.rs` (modify) | module decls + glob re-exports |
| `crates/game/src/protocol.rs` (modify) | `GameEventKind::CellChanges`, `origin_client` |
| `crates/game/src/state.rs` (modify) | `cells` field in `GameData`; `pub` spawn consts |
| `crates/macros/src/lib.rs` (modify) | `UndoSlotMap::remove_raw` |
| `crates/game/src/region.rs` (modify) | `CellChanges` arm, `Region::new_cells`, `take_dormant`, `take_frozen`, `GameError::CellHashMismatch`, Tick stray freeze |
| `crates/game/src/slime.rs`, `crates/game/src/edit.rs` (modify) | cell-mode gates |
| `crates/game/src/region_runner.rs` (modify) | cell mode |
| `crates/game/tests/cell_activation.rs` (create) | determinism + integration tests |
| `CLAUDE.md`, `TODO.md` (modify) | docs |

---

### Task 1: Cell geometry

**Files:**
- Create: `crates/game/src/cells.rs`
- Modify: `crates/game/src/lib.rs` (add `pub mod cells;` after `pub mod camera;` and `pub use cells::*;` after `pub use camera::*;`)

**Interfaces:**
- Produces: `CellCoords { x: u32, z: u32 }` (`Copy, Ord, Hash, Serialize, Deserialize, Default, Debug`); `CellCoords::new(x, z)`; `cells_per_side(cell_chunks: usize) -> u32`; `cell_size_units(cell_chunks: usize) -> f32`; `cell_of_chunk(c: ChunkCoords, cell_chunks: usize) -> CellCoords`; `cell_of_pos(x: f32, z: f32, cell_chunks: usize) -> Option<CellCoords>` (`None` outside the region); `cell_of_block(pos: [i32; 3], cell_chunks: usize) -> Option<CellCoords>`; `cells_within(center: CellCoords, radius: u32, cell_chunks: usize) -> Vec<CellCoords>` (Chebyshev square clipped to the grid, sorted); `chebyshev(a, b) -> u32`.

- [ ] **Step 1: Write the failing tests** — at the bottom of `crates/game/src/cells.rs`:

```rust
#[cfg(test)]
mod geometry_tests {
    use super::*;

    #[test]
    fn grid_divides_the_region() {
        assert_eq!(cells_per_side(crate::REGION_CHUNKS), 1);
        assert_eq!(cells_per_side(crate::REGION_CHUNKS / 4), 4);
        assert_eq!(cell_size_units(8), 256.0);
    }

    #[test]
    #[should_panic]
    fn non_dividing_cell_size_panics() {
        cells_per_side(3);
    }

    #[test]
    fn chunk_and_position_map_to_the_same_cell() {
        let c = ChunkCoords::new(9, -2, 17);
        assert_eq!(cell_of_chunk(c, 8), CellCoords::new(1, 2));
        // Chunk (9, _, 17) starts at voxel (288, _, 544).
        assert_eq!(cell_of_pos(288.0, 544.0, 8), Some(CellCoords::new(1, 2)));
    }

    #[test]
    fn a_position_exactly_on_a_cell_edge_belongs_to_the_higher_cell() {
        let edge = cell_size_units(8);
        assert_eq!(cell_of_pos(edge, 0.0, 8), Some(CellCoords::new(1, 0)));
        assert_eq!(cell_of_pos(edge - 0.001, 0.0, 8), Some(CellCoords::new(0, 0)));
    }

    #[test]
    fn positions_outside_the_region_have_no_cell() {
        assert_eq!(cell_of_pos(-0.5, 10.0, 8), None);
        assert_eq!(cell_of_pos(crate::REGION_SIZE, 10.0, 8), None);
    }

    #[test]
    fn blocks_map_through_their_chunk() {
        // CHUNK_BLOCKS = 2 blocks per chunk: block 16 is chunk 8 → cell 1 at 8-chunk cells.
        assert_eq!(cell_of_block([16, 5, 0], 8), Some(CellCoords::new(1, 0)));
        assert_eq!(cell_of_block([-1, 5, 0], 8), None);
    }

    #[test]
    fn cells_within_is_clipped_and_sorted() {
        let n = cells_per_side(8); // 4 at the default 32-chunk region
        let around_corner = cells_within(CellCoords::new(0, 0), 1, 8);
        assert_eq!(
            around_corner,
            vec![CellCoords::new(0, 0), CellCoords::new(0, 1), CellCoords::new(1, 0), CellCoords::new(1, 1)]
        );
        let all = cells_within(CellCoords::new(1, 1), 10, 8);
        assert_eq!(all.len() as u32, n * n);
        assert!(all.windows(2).all(|w| w[0] < w[1]));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `just test-one game "--lib cells::geometry_tests"`
Expected: compile errors (`cells_per_side` etc. not found).

- [ ] **Step 3: Implement** — top of `crates/game/src/cells.rs`:

```rust
//! Cell loading inside one region (spec:
//! docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md).
//! A cell is a `cell_chunks` × `cell_chunks` square of chunk columns; the
//! server activates cells around players through `CellChanges` events.

use crate::{ChunkCoords, REGION_CHUNKS, REGION_SIZE};

/// Region-local cell address, `0..cells_per_side` on each axis.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash,
    serde::Serialize, serde::Deserialize,
)]
pub struct CellCoords {
    pub x: u32,
    pub z: u32,
}

impl CellCoords {
    pub fn new(x: u32, z: u32) -> Self {
        Self { x, z }
    }
}

/// Cells along one region edge. Panics unless `cell_chunks` is a power of
/// two that divides `REGION_CHUNKS`.
pub fn cells_per_side(cell_chunks: usize) -> u32 {
    assert!(
        cell_chunks.is_power_of_two() && cell_chunks <= REGION_CHUNKS,
        "cell_chunks {cell_chunks} must be a power of two dividing REGION_CHUNKS {REGION_CHUNKS}"
    );
    (REGION_CHUNKS / cell_chunks) as u32
}

/// Cell edge in world units (32 units per chunk).
pub fn cell_size_units(cell_chunks: usize) -> f32 {
    (cell_chunks * 32) as f32
}

pub fn cell_of_chunk(c: ChunkCoords, cell_chunks: usize) -> CellCoords {
    CellCoords::new((c.x / cell_chunks) as u32, (c.z / cell_chunks) as u32)
}

/// The cell containing region-local `(x, z)`, `None` outside the region.
/// `floor` semantics: a point exactly on an edge belongs to the higher cell.
pub fn cell_of_pos(x: f32, z: f32, cell_chunks: usize) -> Option<CellCoords> {
    if !(0.0..REGION_SIZE).contains(&x) || !(0.0..REGION_SIZE).contains(&z) {
        return None;
    }
    let size = cell_size_units(cell_chunks);
    Some(CellCoords::new((x / size).floor() as u32, (z / size).floor() as u32))
}

/// The cell containing block `pos` (block units), `None` outside the region.
pub fn cell_of_block(pos: [i32; 3], cell_chunks: usize) -> Option<CellCoords> {
    let cb = crate::CHUNK_BLOCKS as i32;
    let (cx, cz) = (pos[0].div_euclid(cb), pos[2].div_euclid(cb));
    let n = REGION_CHUNKS as i32;
    if cx < 0 || cz < 0 || cx >= n || cz >= n {
        return None;
    }
    Some(cell_of_chunk(ChunkCoords::new(cx as usize, 0, cz as usize), cell_chunks))
}

pub fn chebyshev(a: CellCoords, b: CellCoords) -> u32 {
    a.x.abs_diff(b.x).max(a.z.abs_diff(b.z))
}

/// Every cell within Chebyshev `radius` of `center`, clipped to the grid,
/// sorted.
pub fn cells_within(center: CellCoords, radius: u32, cell_chunks: usize) -> Vec<CellCoords> {
    let n = cells_per_side(cell_chunks);
    let lo = |v: u32| v.saturating_sub(radius);
    let hi = |v: u32| (v + radius).min(n - 1);
    let mut out = Vec::new();
    for x in lo(center.x)..=hi(center.x) {
        for z in lo(center.z)..=hi(center.z) {
            out.push(CellCoords::new(x, z));
        }
    }
    out
}
```

Check `crate::CHUNK_BLOCKS` exists (`grep -n "pub const CHUNK_BLOCKS" crates/game/src`); `edit.rs::locate` uses it.

- [ ] **Step 4: Run to verify pass**

Run: `just test-one game "--lib cells::geometry_tests"`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/cells.rs crates/game/src/lib.rs
git commit -m "feat(game): cell geometry for cell loading

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `CellChanges` protocol types

**Files:**
- Modify: `crates/game/src/cells.rs` (types), `crates/game/src/protocol.rs:370-396` (variant + `origin_client`), `crates/game/src/region.rs:139-150` (`ev_desc` arm)
- Check: `crates/client/src/instance.rs:661`, `crates/game/src/world_manager.rs:463` — add a `CellChanges` arm only if the match is exhaustive (compiler will say).

**Interfaces:**
- Produces:

```rust
pub struct DormantCell { pub chunks: Vec<(ChunkCoords, Chunk)>, pub entities: Vec<EntityBundle> }
pub struct CellPayload { pub cell: CellCoords, pub version: u64, pub materialized_hash: u32, pub content: DormantCell }
pub struct CellChanges { pub deactivate: Vec<CellCoords>, pub activate: Vec<CellPayload> }
GameEventKind::CellChanges(CellChanges)
DormantCell::content_crc(&self) -> u32
CellChanges::is_empty(&self) -> bool
```

- [ ] **Step 1: Write the failing tests** — append to `crates/game/src/cells.rs`:

```rust
#[cfg(test)]
mod protocol_tests {
    use super::*;
    use crate::{Chunk, GameEventKind};

    fn sample() -> CellChanges {
        CellChanges {
            deactivate: vec![CellCoords::new(2, 3)],
            activate: vec![CellPayload {
                cell: CellCoords::new(0, 0),
                version: 4,
                materialized_hash: 0,
                content: DormantCell {
                    chunks: vec![(ChunkCoords::new(1, 0, 1), Chunk::flat_floor(8))],
                    entities: vec![],
                },
            }],
        }
    }

    #[test]
    fn cell_changes_are_a_server_event() {
        assert_eq!(GameEventKind::CellChanges(sample()).origin_client(), None);
    }

    #[test]
    fn cell_changes_round_trip_through_the_wire_codec() {
        let kind = GameEventKind::CellChanges(sample());
        let bytes = bincode::serialize(&kind).unwrap();
        let back: GameEventKind = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back, kind);
    }

    #[test]
    fn content_crc_tracks_content() {
        let a = sample().activate.remove(0).content;
        let mut b = a.clone();
        assert_eq!(a.content_crc(), b.content_crc());
        b.chunks.clear();
        assert_ne!(a.content_crc(), b.content_crc());
        assert!(CellChanges::default().is_empty());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `just test-one game "--lib cells::protocol_tests"`
Expected: compile errors (`CellChanges` not found).

- [ ] **Step 3: Implement** — in `crates/game/src/cells.rs`, extend the `use` line to `use crate::{Chunk, ChunkCoords, EntityBundle, REGION_CHUNKS, REGION_SIZE};` and add:

```rust
/// A cell's full content while it is not in the sim: the server's store
/// entry, and the activation payload clients materialize.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DormantCell {
    /// Sorted by `ChunkCoords`.
    pub chunks: Vec<(ChunkCoords, Chunk)>,
    /// Frozen non-player entities, in extraction order.
    pub entities: Vec<EntityBundle>,
}

impl DormantCell {
    /// crc32 of the bincode encoding — the store's change detector.
    pub fn content_crc(&self) -> u32 {
        let mut h = crc32fast::Hasher::new();
        h.update(&bincode::serialize(self).expect("dormant cell serializes"));
        h.finalize()
    }
}

// `Chunk` has no PartialEq; compare cells by their encoding.
impl PartialEq for DormantCell {
    fn eq(&self, other: &Self) -> bool {
        bincode::serialize(self).ok() == bincode::serialize(other).ok()
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CellPayload {
    pub cell: CellCoords,
    /// Store version this payload was cut from.
    pub version: u64,
    /// `Rollback::cell_hash` of the cell as materialized in the SERVER sim.
    /// 0 when the server builds the event; the server's `handle_event`
    /// fills it before broadcast; clients verify it.
    pub materialized_hash: u32,
    pub content: DormantCell,
}

/// The one server-authored cell event per tick: deactivations first, then
/// activations, each sorted by `CellCoords`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CellChanges {
    pub deactivate: Vec<CellCoords>,
    pub activate: Vec<CellPayload>,
}

impl CellChanges {
    pub fn is_empty(&self) -> bool {
        self.deactivate.is_empty() && self.activate.is_empty()
    }
}
```

In `crates/game/src/protocol.rs`, add to `GameEventKind` after `UiAction`:

```rust
    /// Server-authored cell loading (cell-mode regions only): applied at the
    /// confirmed head by every replica, never predicted.
    CellChanges(crate::CellChanges),
```

and add `| GameEventKind::CellChanges(_)` to the `=> None` arm of `origin_client`. In `crates/game/src/region.rs` `ev_desc`, add:

```rust
        GameEventKind::CellChanges(c) => {
            format!("CellChanges(-{} +{})", c.deactivate.len(), c.activate.len())
        }
```

- [ ] **Step 4: Run to verify pass, then type-check everything**

Run: `just test-one game "--lib cells::protocol_tests"` → 3 passed.
Run: `just smoke` → passes (fix any non-exhaustive `match` it reports by adding a `GameEventKind::CellChanges(_)` arm with the same body as the `GhostUpdate` arm there).

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/cells.rs crates/game/src/protocol.rs crates/game/src/region.rs
# plus any file smoke forced you to touch, by name
git commit -m "feat(game): CellChanges server event

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Cell state in the sim, raw slot removal, cell-mode regions

**Files:**
- Modify: `crates/game/src/cells.rs` (`CellState`), `crates/game/src/state.rs:153-190` (field + import), `crates/macros/src/lib.rs:1067` (`remove_raw`), `crates/game/src/region.rs` (`Region::new_cells`)
- Test: `crates/game/tests/cell_activation.rs` (create)

**Interfaces:**
- Consumes: `CellCoords`, `cells_per_side` (Task 1).
- Produces: `CellState { pub cell_chunks: usize, pub active: BTreeSet<CellCoords> }` (`Default` = legacy, `cell_chunks == 0`); `CellState::enabled(&self) -> bool`; field `GameData.cells` (tier-2 `Undo<CellState>`; read via deref, write via crate-internal `raw_mut()`); `UndoSlotMap::remove_raw(&mut self, k) -> Option<V>`; `Region::new_cells(id: RegionId, cell_chunks: usize, game_update_send: Option<Sender<GameDataUpdate>>, local_client_id: Option<ClientId>) -> Region` (no terrain, no active cells).

- [ ] **Step 1: Write the failing tests** — create `crates/game/tests/cell_activation.rs`:

```rust
//! Cell loading inside one region — spec
//! docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md.
use game::{state_hash, Region, RegionCoords};

pub fn r0() -> RegionCoords {
    RegionCoords::new(0, 0)
}

/// 8-chunk cells: a 4×4 grid in the default 32-chunk region.
pub const CC: usize = 8;

#[test]
fn legacy_regions_are_not_in_cell_mode() {
    let r = Region::from_chunks(r0(), Vec::new());
    assert!(!r.data().cells.enabled());
    assert_eq!(r.data().cells.cell_chunks, 0);
}

#[test]
fn a_cell_mode_region_starts_empty() {
    let r = Region::new_cells(r0(), CC, None, None);
    assert!(r.data().cells.enabled());
    assert!(r.data().cells.active.is_empty());
    assert_eq!(r.data().ecs.chunk.iter().filter(|(_, c)| c.is_some()).count(), 0);
}

#[test]
fn cell_state_is_part_of_the_state_hash() {
    let a = Region::new_cells(r0(), CC, None, None);
    let b = Region::from_chunks(r0(), Vec::new());
    assert_ne!(state_hash(a.data()), state_hash(b.data()));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `just test-one game "--test cell_activation"`
Expected: compile errors (`cells` field / `new_cells` missing).

- [ ] **Step 3: Implement**

In `crates/game/src/cells.rs` add (and `use std::collections::BTreeSet;`):

```rust
/// Sim-side cell bookkeeping (tier-2 field `GameData.cells`). Mutated ONLY
/// raw, inside the `CellChanges` arm, which every replica forgets at once.
#[derive(Debug, Clone, Default, Hash, serde::Serialize, serde::Deserialize)]
pub struct CellState {
    /// 0 = legacy region (fully materialized, no cells).
    pub cell_chunks: usize,
    pub active: BTreeSet<CellCoords>,
}

impl CellState {
    pub fn enabled(&self) -> bool {
        self.cell_chunks != 0
    }
}
```

In `crates/game/src/state.rs` inside `mod game_data`: add `CellState` to the `use super::{...}` list (and `pub use crate::cells::CellState;`-style import at the top of `state.rs`: `use crate::cells::CellState;`), then add the field after `arrivals`:

```rust
        /// Cell loading (cell-mode regions); default = legacy region.
        cells: CellState,
```

In `crates/macros/src/lib.rs`, next to `insert_raw` (~line 1067) add:

```rust
                /// Raw remove: no undo entry, no renderer emit. Only correct
                /// inside a transaction that is forgotten immediately and never
                /// rolled back (cell dematerialization); anywhere else this
                /// desyncs the undo log from the data.
                pub fn remove_raw(&mut self, k: K) -> ::std::option::Option<V> {
                    self.data.remove(k)
                }
```

In `crates/game/src/region.rs` `impl Region`, after `from_columns`:

```rust
    /// An empty cell-mode region: no terrain, no active cells. Terrain
    /// arrives only through `CellChanges`.
    pub fn new_cells(
        id: RegionId,
        cell_chunks: usize,
        game_update_send: Option<Sender<GameDataUpdate>>,
        local_client_id: Option<ClientId>,
    ) -> Self {
        crate::cells_per_side(cell_chunks); // validates the size
        let mut data = Rollback::new(None);
        data.cells.raw_mut().cell_chunks = cell_chunks;
        Region::new(data, game_update_send, id, local_client_id)
    }
```

(`raw_mut` is `pub(crate)`; this is inside `game`. If `Rollback` doesn't deref-mut to the field here, use the same access path `seed_terrain_genesis` uses, e.g. `data.data.cells.raw_mut()`.)

- [ ] **Step 4: Run to verify pass, and that legacy code is untouched**

Run: `just test-one game "--test cell_activation"` → 3 passed.
Run: `just test-one game "--test state_hash"` and `just test-one game "--test rollback_restore"` → pass.
Run: `just smoke` → passes.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/cells.rs crates/game/src/state.rs crates/macros/src/lib.rs crates/game/src/region.rs crates/game/tests/cell_activation.rs
git commit -m "feat(game): cell state in the sim and cell-mode regions

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Materialize, dematerialize, verify

**Files:**
- Modify: `crates/game/src/cells.rs` (Rollback impl), `crates/game/src/region.rs` (`CellChanges` arm, `pending_dormant`, `take_dormant`, `GameError::CellHashMismatch` in `protocol.rs:~246`)
- Test: `crates/game/tests/cell_activation.rs`

**Interfaces:**
- Consumes: Tasks 1–3; `Rollback::seed_terrain_genesis` (`state.rs:565`), `Rollback::apply_arrival` (`state.rs:971`), `Rollback::extract_entity_safe` (`state.rs:1158`).
- Produces:
  - `Rollback::cell_chunk_keys(&self, cell) -> Vec<(ChunkCoords, EntityKey)>` (sorted by coords)
  - `Rollback::cell_entity_keys(&self, cell) -> Vec<EntityKey>` (kind-bearing, non-ghost, sorted)
  - `Rollback::cell_has_player(&self, cell) -> bool`
  - `Rollback::cell_hash(&self, cell) -> u32`
  - `Rollback::materialize_cell(&mut self, payload: &CellPayload)`
  - `Rollback::dematerialize_cell(&mut self, cell, region: RegionCoords) -> DormantCell`
  - `Region::take_dormant(&mut self) -> Vec<(CellCoords, DormantCell)>` (server only; empty on clients)
  - `GameError::CellHashMismatch(CellCoords)`

- [ ] **Step 1: Write the failing tests** — append to `crates/game/tests/cell_activation.rs`:

```rust
use game::{
    CellChanges, CellCoords, CellPayload, Chunk, ChunkCoords, DormantCell, GameError,
    GameEventKind,
};

/// One flat-floor chunk at the corner column of every cell.
pub fn cell_content(cell: CellCoords) -> DormantCell {
    let (x, z) = (cell.x as usize * CC, cell.z as usize * CC);
    DormantCell {
        chunks: vec![
            (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8)),
            (ChunkCoords::new(x + 1, 0, z), Chunk::flat_floor(4)),
        ],
        entities: vec![],
    }
}

pub fn activate(cells: &[CellCoords]) -> GameEventKind {
    GameEventKind::CellChanges(CellChanges {
        deactivate: vec![],
        activate: cells
            .iter()
            .map(|&cell| CellPayload { cell, version: 1, materialized_hash: 0, content: cell_content(cell) })
            .collect(),
    })
}

pub fn deactivate(cells: &[CellCoords]) -> GameEventKind {
    GameEventKind::CellChanges(CellChanges { deactivate: cells.to_vec(), activate: vec![] })
}

/// Server-side apply: handle + forget, returning the broadcast event.
pub fn server_apply(r: &mut Region, kind: GameEventKind) -> game::GameEvent {
    let ev = r.handle_event(kind).unwrap();
    r.forget_last_event();
    ev
}

#[test]
fn server_fills_the_materialized_hash() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    let ev = server_apply(&mut s, activate(&[CellCoords::new(1, 2)]));
    let GameEventKind::CellChanges(c) = ev.kind else { panic!() };
    assert_ne!(c.activate[0].materialized_hash, 0);
    assert_eq!(c.activate[0].materialized_hash, s.data().cell_hash(CellCoords::new(1, 2)));
    assert!(s.data().cells.active.contains(&CellCoords::new(1, 2)));
}

#[test]
fn a_client_replica_matches_the_server_bit_exact() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut s = Region::new_cells(r0(), CC, None, None);
    let mut c = Region::new(s.data().clone(), Some(send), r0(), Some(0));
    for step in [
        activate(&[CellCoords::new(0, 0), CellCoords::new(3, 3)]),
        deactivate(&[CellCoords::new(0, 0)]),
        activate(&[CellCoords::new(0, 0)]),
    ] {
        let ev = server_apply(&mut s, step);
        c.handle_event(ev.kind).unwrap();
        c.forget_last_event();
        assert_eq!(state_hash(s.data()), state_hash(c.data()));
    }
}

#[test]
fn a_tampered_hash_is_rejected_by_a_client() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut s = Region::new_cells(r0(), CC, None, None);
    let mut c = Region::new(s.data().clone(), Some(send), r0(), Some(0));
    let ev = server_apply(&mut s, activate(&[CellCoords::new(2, 2)]));
    let GameEventKind::CellChanges(mut changes) = ev.kind else { panic!() };
    changes.activate[0].materialized_hash ^= 1;
    match c.handle_event(GameEventKind::CellChanges(changes)) {
        Err(GameError::CellHashMismatch(cell)) => assert_eq!(cell, CellCoords::new(2, 2)),
        other => panic!("expected CellHashMismatch, got {other:?}"),
    }
}

#[test]
fn dematerialize_returns_the_content_and_empties_the_cell() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    let cell = CellCoords::new(1, 1);
    server_apply(&mut s, activate(&[cell]));
    server_apply(&mut s, deactivate(&[cell]));
    assert!(!s.data().cells.active.contains(&cell));
    assert!(s.data().cell_chunk_keys(cell).is_empty());
    let dormant = s.take_dormant();
    assert_eq!(dormant.len(), 1);
    assert_eq!(dormant[0].0, cell);
    assert_eq!(dormant[0].1, cell_content(cell));
    assert!(s.take_dormant().is_empty(), "drained");
}

#[test]
fn reactivation_round_trips_content() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    let cell = CellCoords::new(0, 3);
    server_apply(&mut s, activate(&[cell]));
    let before = s.data().terrain_snapshot().terrain_hash();
    server_apply(&mut s, deactivate(&[cell]));
    let (_, content) = s.take_dormant().remove(0);
    server_apply(
        &mut s,
        GameEventKind::CellChanges(CellChanges {
            deactivate: vec![],
            activate: vec![CellPayload { cell, version: 2, materialized_hash: 0, content }],
        }),
    );
    assert_eq!(before, s.data().terrain_snapshot().terrain_hash());
}

#[test]
fn a_cell_holding_a_player_is_not_deactivated() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    // PLAYER_SPAWN_X/Z = 128 voxels → cell (0,0) at 256-unit cells.
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    server_apply(&mut s, GameEventKind::CreateClient(0));
    server_apply(&mut s, deactivate(&[CellCoords::new(0, 0)]));
    assert!(s.data().cells.active.contains(&CellCoords::new(0, 0)));
    assert!(s.data().player_entites.contains_key(&0));
    assert!(s.take_dormant().is_empty());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `just test-one game "--test cell_activation"`
Expected: compile errors (`cell_hash`, `take_dormant`, `CellHashMismatch` missing).

- [ ] **Step 3: Implement**

`crates/game/src/protocol.rs`, `enum GameError`: add `CellHashMismatch(crate::CellCoords),`.

`crates/game/src/cells.rs` — add the Rollback impl (extend imports: `use crate::{EntityKey, RegionCoords, Rollback};`):

```rust
impl Rollback {
    fn chunk_coords_of(&self, key: EntityKey) -> Option<ChunkCoords> {
        let handle = (*self.data.ecs.rigidbody.try_get(key))?;
        let t = self.data.physics.bodies.get(handle)?.translation();
        // Chunk bodies sit at exact multiples of 32 (seed_terrain_genesis / create_mesh).
        Some(ChunkCoords::new(
            (t.x.0 / 32.0) as usize,
            (t.y.0 / 32.0).floor() as i32,
            (t.z.0 / 32.0) as usize,
        ))
    }

    /// Chunk entities in `cell`, sorted by coordinates.
    pub fn cell_chunk_keys(&self, cell: CellCoords) -> Vec<(ChunkCoords, EntityKey)> {
        let cc = self.data.cells.cell_chunks;
        let mut out: Vec<_> = self
            .data
            .ecs
            .chunk
            .iter()
            .filter(|(_, c)| c.is_some())
            .filter_map(|(k, _)| self.chunk_coords_of(k).map(|c| (c, k)))
            .filter(|(c, _)| cell_of_chunk(*c, cc) == cell)
            .collect();
        out.sort_by_key(|(c, _)| (c.x, c.y, c.z));
        out
    }

    /// Kind-bearing entities (players, mobs) whose body is in `cell`,
    /// ghosts excluded, in key order.
    pub fn cell_entity_keys(&self, cell: CellCoords) -> Vec<EntityKey> {
        let cc = self.data.cells.cell_chunks;
        let ghosts: BTreeSet<EntityKey> = self.data.ghosts.iter().map(|(_, g)| g.entity).collect();
        self.data
            .ecs
            .kind
            .iter()
            .filter(|(k, kind)| kind.is_some() && !ghosts.contains(k))
            .filter_map(|(k, _)| {
                let h = (*self.data.ecs.rigidbody.try_get(k))?;
                let t = self.data.physics.bodies.get(h)?.translation();
                (cell_of_pos(t.x.0, t.z.0, cc) == Some(cell)).then_some(k)
            })
            .collect()
    }

    pub fn cell_has_player(&self, cell: CellCoords) -> bool {
        let players: BTreeSet<EntityKey> = self.data.player_entites.values().copied().collect();
        self.cell_entity_keys(cell).iter().any(|k| players.contains(k))
    }

    /// Hash of the cell AS MATERIALIZED: chunk content, entity keys, bodies
    /// and colliders of its terrain, plus its entities' bodies. Computed by
    /// the server after activation and re-computed by clients.
    pub fn cell_hash(&self, cell: CellCoords) -> u32 {
        use std::hash::Hash;
        let mut h = crc32fast::Hasher::new();
        cell.hash(&mut h);
        for (coords, key) in self.cell_chunk_keys(cell) {
            coords.hash(&mut h);
            key.hash(&mut h);
            self.data.ecs.chunk.try_get(key).hash(&mut h);
            if let Some(handle) = *self.data.ecs.rigidbody.try_get(key) {
                handle.hash(&mut h);
                if let Some(body) = self.data.physics.bodies.get(handle) {
                    body.hash(&mut h);
                    for ch in body.colliders() {
                        ch.hash(&mut h);
                        self.data.physics.colliders.get(*ch).hash(&mut h);
                    }
                }
            }
        }
        for key in self.cell_entity_keys(cell) {
            key.hash(&mut h);
            self.data.ecs.kind.try_get(key).hash(&mut h);
            if let Some(handle) = *self.data.ecs.rigidbody.try_get(key) {
                self.data.physics.bodies.get(handle).hash(&mut h);
            }
        }
        h.finalize()
    }

    /// Activation. Terrain goes through the raw genesis path; entities
    /// through the (logged) arrival path. Only valid inside the
    /// `CellChanges` arm, whose transaction is forgotten at once.
    pub fn materialize_cell(&mut self, payload: &CellPayload) {
        self.seed_terrain_genesis(payload.content.chunks.clone());
        for bundle in payload.content.entities.iter().cloned() {
            self.apply_arrival(bundle);
        }
        self.data.cells.raw_mut().active.insert(payload.cell);
    }

    /// Deactivation: freeze entities, capture terrain, remove it raw.
    /// Caller guarantees no player is in the cell.
    pub fn dematerialize_cell(&mut self, cell: CellCoords, region: RegionCoords) -> DormantCell {
        let entities: Vec<EntityBundle> = self
            .cell_entity_keys(cell)
            .into_iter()
            .map(|k| self.extract_entity_safe(k, region))
            .collect();
        let chunk_keys = self.cell_chunk_keys(cell);
        let mut chunks = Vec::with_capacity(chunk_keys.len());
        for (coords, key) in chunk_keys {
            let chunk = (*self.data.ecs.chunk.try_get(key)).clone().expect("chunk entity");
            chunks.push((coords, chunk));
            self.remove_chunk_raw(key);
        }
        self.data.cells.raw_mut().active.remove(&cell);
        DormantCell { chunks, entities }
    }

    /// Raw inverse of one `seed_terrain_genesis` chunk: body + attached
    /// colliders, component slots, entity slot.
    fn remove_chunk_raw(&mut self, key: EntityKey) {
        if let Some(handle) = *self.data.ecs.rigidbody.try_get(key) {
            let p = self.data.physics.raw_mut();
            p.bodies.raw_mut().remove(
                handle,
                p.islands.raw_mut(),
                p.colliders.raw_mut(),
                p.implules_joint_set.raw_mut(),
                p.multi_body_joint_set.raw_mut(),
                true,
            );
        }
        self.data.ecs.camera.raw_mut().remove(key);
        self.data.ecs.isometry.raw_mut().remove(key);
        self.data.ecs.rigidbody.raw_mut().remove(key);
        self.data.ecs.chunk.raw_mut().remove(key);
        self.data.ecs.kind.raw_mut().remove(key);
        self.data.ecs.motion.raw_mut().remove(key);
        self.data.ecs.slime.raw_mut().remove(key);
        self.data.ecs.entities.remove_raw(key);
    }
}
```

Access-path notes for the implementer: mirror exactly how `seed_terrain_genesis` (`state.rs:565-632`) reaches `raw_mut()` on `physics`, `bodies`, `colliders` and components (it uses `self.ecs....raw_mut()` and `self.data.physics.raw_mut()`); if a field accessor differs (e.g. `Component::raw_mut()` returns the `SparseSecondaryMap`, whose `remove` is fine), follow that file, not this snippet. If `raw_mut` on `physics` sub-fields does not exist, reach them the way genesis does for `bodies`/`colliders` and add the equivalent for `islands` and the joint sets.

`crates/game/src/region.rs`: add field `pending_dormant: Vec<(crate::CellCoords, crate::DormantCell)>` (init `Vec::new()` in `Region::new`), change `let event = GameEvent::new(...)` to `let mut event = ...` in `handle_event`, and add the arm before `GameEventKind::EntityArrived`:

```rust
            GameEventKind::CellChanges(mut changes) => {
                let server = self.local_client_id.is_none();
                for cell in changes.deactivate.iter().copied() {
                    if !self.data.cells.active.contains(&cell) {
                        continue;
                    }
                    if self.data.cell_has_player(cell) {
                        log::warn!("[cells r={:?}] refusing to deactivate {cell:?}: a player is in it", self.id);
                        continue;
                    }
                    let dormant = self.data.dematerialize_cell(cell, self.id);
                    if server {
                        self.pending_dormant.push((cell, dormant));
                    }
                }
                for payload in changes.activate.iter_mut() {
                    if self.data.cells.active.contains(&payload.cell) {
                        continue;
                    }
                    self.data.materialize_cell(payload);
                    let hash = self.data.cell_hash(payload.cell);
                    if server {
                        payload.materialized_hash = hash;
                    } else if hash != payload.materialized_hash {
                        return Err(GameError::CellHashMismatch(payload.cell));
                    }
                }
                event.kind = GameEventKind::CellChanges(changes);
            }
```

and a drain:

```rust
    /// Server: cells dematerialized since the last call, for the store.
    pub fn take_dormant(&mut self) -> Vec<(crate::CellCoords, crate::DormantCell)> {
        std::mem::take(&mut self.pending_dormant)
    }
```

Check the end of `handle_event` returns `Ok(event)` (the patched value). Add `use std::collections::BTreeSet;` in `cells.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `just test-one game "--test cell_activation"` → all pass. Note timing: if any test >100 ms, tag it slow per Global Constraints.
Run: `just smoke` → passes.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/cells.rs crates/game/src/region.rs crates/game/src/protocol.rs crates/game/tests/cell_activation.rs
git commit -m "feat(game): materialize and dematerialize cells with a verified hash

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Cell-mode gates — stray freeze, edits, slime spawns

**Files:**
- Modify: `crates/game/src/cells.rs` (`freeze_strays_safe`, `cell_active_at_block`), `crates/game/src/region.rs` (Tick arm + `pending_frozen` + `take_frozen`), `crates/game/src/edit.rs:197` (`set_block_safe` gate), `crates/game/src/slime.rs:238-250` (spawn gate + per-cell cap)
- Test: `crates/game/tests/cell_activation.rs`

**Interfaces:**
- Consumes: Task 4.
- Produces: `Rollback::freeze_strays_safe(&mut self, region) -> Vec<(CellCoords, EntityBundle)>` (logged, predicted like departures); `Region::take_frozen(&mut self) -> Vec<(CellCoords, EntityBundle)>` (replaced every tick); `pub const SLIME_CAP_PER_CELL: usize = 8` in `slime.rs`.

- [ ] **Step 1: Write the failing tests** — append:

```rust
use game::parry::math::{Real, Vector};

#[test]
fn a_mob_in_a_dormant_cell_is_frozen_by_the_tick() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    // Place a slime inside dormant cell (2,2): centre at 2.5 cells.
    let at = 2.5 * game::cell_size_units(CC);
    s.with_data(|rb| {
        rb.spawn_slime_safe(Vector::new(Real::from(at), Real::from(40.0), Real::from(at)), 1, Real::from(0.0), 0);
    });
    server_apply(&mut s, GameEventKind::Tick);
    let frozen = s.take_frozen();
    assert_eq!(frozen.len(), 1);
    assert_eq!(frozen[0].0, CellCoords::new(2, 2));
    assert_eq!(s.data().ecs.slime.iter().filter(|(_, x)| x.is_some()).count(), 0);
}

#[test]
fn freezing_a_stray_rolls_back_bit_exact() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    let at = 2.5 * game::cell_size_units(CC);
    s.with_data(|rb| {
        rb.spawn_slime_safe(Vector::new(Real::from(at), Real::from(40.0), Real::from(at)), 1, Real::from(0.0), 0);
    });
    let mut c = Region::new(s.data().clone(), Some(send), r0(), Some(0));
    let before = state_hash(c.data());
    c.handle_event(GameEventKind::Tick).unwrap();
    c.rollback_last_event();
    assert_eq!(before, state_hash(c.data()));
}

#[test]
fn edits_in_dormant_cells_are_refused() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    let block = game::Block::new(game::BlockId(1));
    let mut placed_dormant = true;
    let mut placed_active = false;
    s.with_data(|rb| {
        // Cell (3,3) starts at chunk 24 = block 48.
        placed_dormant = rb.set_block_safe([50, 3, 50], block);
        placed_active = rb.set_block_safe([3, 20, 3], block);
    });
    assert!(!placed_dormant, "no chunk may be created in a dormant cell");
    assert!(placed_active);
    assert!(s.data().cell_chunk_keys(CellCoords::new(3, 3)).is_empty());
}

#[test]
fn slime_spawns_are_gated_to_active_cells() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    // Voxel (100,100) is cell (0,0); voxel (700,700) is cell (2,2).
    assert!(s.data().slime_spawn_allowed(100, 100));
    assert!(!s.data().slime_spawn_allowed(700, 700));
    let legacy = Region::from_chunks(r0(), Vec::new());
    assert!(legacy.data().slime_spawn_allowed(700, 700), "legacy regions are ungated");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `just test-one game "--test cell_activation"`
Expected: compile errors (`take_frozen`, `slime_spawn_allowed` missing).

- [ ] **Step 3: Implement**

`crates/game/src/cells.rs`, inside `impl Rollback`:

```rust
    /// Cell-mode only: whether block `pos` lies in an active cell.
    /// Legacy regions: always true.
    pub fn block_in_active_cell(&self, pos: [i32; 3]) -> bool {
        let cc = self.data.cells.cell_chunks;
        if cc == 0 {
            return true;
        }
        cell_of_block(pos, cc).is_some_and(|c| self.data.cells.active.contains(&c))
    }

    /// End-of-tick backstop: every non-player, non-ghost entity standing in
    /// a dormant cell is extracted (undo-tracked, so clients predict it) and
    /// returned for the server's store. Legacy regions: no-op.
    pub fn freeze_strays_safe(&mut self, region: RegionCoords) -> Vec<(CellCoords, EntityBundle)> {
        let cc = self.data.cells.cell_chunks;
        if cc == 0 {
            return Vec::new();
        }
        let players: BTreeSet<EntityKey> = self.data.player_entites.values().copied().collect();
        let ghosts: BTreeSet<EntityKey> = self.data.ghosts.iter().map(|(_, g)| g.entity).collect();
        let mut strays = Vec::new();
        for (k, kind) in self.data.ecs.kind.iter() {
            if kind.is_none() || players.contains(&k) || ghosts.contains(&k) {
                continue;
            }
            let Some(h) = *self.data.ecs.rigidbody.try_get(k) else { continue };
            let Some(body) = self.data.physics.bodies.get(h) else { continue };
            let t = body.translation();
            // Outside the region is the departure scan's job.
            if let Some(cell) = cell_of_pos(t.x.0, t.z.0, cc) {
                if !self.data.cells.active.contains(&cell) {
                    strays.push((cell, k));
                }
            }
        }
        strays
            .into_iter()
            .map(|(cell, k)| (cell, self.extract_entity_safe(k, region)))
            .collect()
    }
```

`crates/game/src/region.rs`: add `pending_frozen: Vec<(crate::CellCoords, EntityBundle)>` (init empty), and in the Tick arm right after `self.data.expire_ghosts();`:

```rust
                // Replaced every tick, like pending_departures.
                self.pending_frozen = self.data.freeze_strays_safe(self.id);
```

plus

```rust
    /// Entities frozen into dormant cells by the LAST tick (server: to store;
    /// client: discard). Read right after driving a tick.
    pub fn take_frozen(&mut self) -> Vec<(crate::CellCoords, EntityBundle)> {
        std::mem::take(&mut self.pending_frozen)
    }
```

`crates/game/src/edit.rs` `set_block_safe`, first line:

```rust
        if !self.block_in_active_cell(pos) {
            return false;
        }
```

`crates/game/src/slime.rs`: add

```rust
/// Cell-mode regions: a cell stops spawning once it holds this many slimes.
pub const SLIME_CAP_PER_CELL: usize = 8;
```

and in `impl Rollback`:

```rust
    /// Spawn gate at region-local voxel column `(vx, vz)`. Legacy regions:
    /// always allowed (the per-region cap applies instead). Cell mode: the
    /// column's cell must be active and hold fewer than SLIME_CAP_PER_CELL.
    pub fn slime_spawn_allowed(&self, vx: i32, vz: i32) -> bool {
        let cc = self.data.cells.cell_chunks;
        if cc == 0 {
            return true;
        }
        let Some(cell) = crate::cell_of_pos(vx as f32, vz as f32, cc) else { return false };
        if !self.data.cells.active.contains(&cell) {
            return false;
        }
        let in_cell = self
            .cell_entity_keys(cell)
            .into_iter()
            .filter(|k| self.data.ecs.slime.try_get(*k).is_some())
            .count();
        in_cell < SLIME_CAP_PER_CELL
    }
```

In `maybe_spawn_slime_safe`, directly after `let Some(plan) = ...`:

```rust
        if !self.slime_spawn_allowed(plan.vx, plan.vz) {
            return;
        }
```

and wrap the existing `SLIME_CAP_PER_REGION` check so it only runs when `self.data.cells.cell_chunks == 0`.

- [ ] **Step 4: Run to verify pass**

Run: `just test-one game "--test cell_activation"` → all pass.
Run: `just test-one game "--test slime"` and `just test-one game "--test block_edit"` → pass (legacy unchanged).

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/cells.rs crates/game/src/region.rs crates/game/src/edit.rs crates/game/src/slime.rs crates/game/tests/cell_activation.rs
git commit -m "feat(game): freeze strays, gate edits and slime spawns to active cells

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Ticket planner

**Files:**
- Create: `crates/game/src/cell_tickets.rs`
- Modify: `crates/game/src/lib.rs` (`pub mod cell_tickets;` + `pub use cell_tickets::*;`)

**Interfaces:**
- Consumes: `CellCoords`, `cell_of_pos`, `cells_within`, `chebyshev` (Task 1).
- Produces: consts `SIM_RADIUS_CELLS: u32 = 2`, `CELL_HYSTERESIS: u32 = 1`, `CELL_RELEASE_GRACE_TICKS: u32 = 40`, `MAX_CELL_ACTIVATIONS_PER_TICK: usize = 1`; `CellConfig { cell_chunks, sim_radius, hysteresis, release_grace_ticks, max_activations_per_tick }` with `CellConfig::new(cell_chunks)` using the consts; `TicketPlan { deactivate: Vec<CellCoords>, activate: Vec<CellCoords> }`; `CellTickets::default()`, `CellTickets::plan(&mut self, active: &BTreeSet<CellCoords>, players: &[(f32, f32)], cfg: &CellConfig) -> TicketPlan`; `required_cells(pos: (f32, f32), cell_chunks) -> Vec<CellCoords>`.

- [ ] **Step 1: Write the failing tests** — bottom of `crates/game/src/cell_tickets.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CellConfig {
        CellConfig::new(8) // 4×4 grid of 256-unit cells
    }
    fn centre(x: u32, z: u32) -> (f32, f32) {
        let s = crate::cell_size_units(8);
        ((x as f32 + 0.5) * s, (z as f32 + 0.5) * s)
    }

    #[test]
    fn required_cells_bypass_the_budget() {
        let mut t = CellTickets::default();
        let plan = t.plan(&BTreeSet::new(), &[centre(1, 1)], &cfg());
        // 3×3 around (1,1) = 9 required, plus 1 budgeted wanted cell.
        assert_eq!(plan.activate.len(), 9 + MAX_CELL_ACTIVATIONS_PER_TICK);
        for c in required_cells(centre(1, 1), 8) {
            assert!(plan.activate.contains(&c));
        }
    }

    #[test]
    fn wanted_cells_come_nearest_first_one_per_tick() {
        let mut t = CellTickets::default();
        let mut active: BTreeSet<CellCoords> = required_cells(centre(0, 0), 8).into_iter().collect();
        let plan = t.plan(&active, &[centre(0, 0)], &cfg());
        assert_eq!(plan.activate.len(), 1);
        // Distance-2 ring, smallest coords first among equals.
        assert_eq!(plan.activate[0], CellCoords::new(0, 2));
        active.extend(plan.activate);
        let plan2 = t.plan(&active, &[centre(0, 0)], &cfg());
        assert_eq!(plan2.activate, vec![CellCoords::new(1, 2)]);
    }

    #[test]
    fn release_waits_for_the_grace_period() {
        let mut t = CellTickets::default();
        let active: BTreeSet<CellCoords> = [CellCoords::new(3, 3)].into_iter().collect();
        let far = centre(0, 0); // (3,3) is distance 3 < radius+hysteresis=3? → keep
        let plan = t.plan(&active, &[far], &cfg());
        assert!(plan.deactivate.is_empty(), "within radius + hysteresis is kept");
        let mut released = None;
        for tick in 0..CELL_RELEASE_GRACE_TICKS + 1 {
            let p = t.plan(&active, &[], &cfg());
            if !p.deactivate.is_empty() {
                released = Some(tick);
                break;
            }
        }
        assert_eq!(released, Some(CELL_RELEASE_GRACE_TICKS - 1));
    }

    #[test]
    fn coming_back_resets_the_grace_countdown() {
        let mut t = CellTickets::default();
        let active: BTreeSet<CellCoords> = [CellCoords::new(3, 3)].into_iter().collect();
        for _ in 0..CELL_RELEASE_GRACE_TICKS - 2 {
            assert!(t.plan(&active, &[], &cfg()).deactivate.is_empty());
        }
        t.plan(&active, &[centre(3, 3)], &cfg());
        for _ in 0..CELL_RELEASE_GRACE_TICKS - 2 {
            assert!(t.plan(&active, &[], &cfg()).deactivate.is_empty());
        }
    }

    #[test]
    fn plans_are_sorted_and_never_repeat_active_cells() {
        let mut t = CellTickets::default();
        let active: BTreeSet<CellCoords> = [CellCoords::new(1, 1)].into_iter().collect();
        let plan = t.plan(&active, &[centre(1, 1), centre(2, 2)], &cfg());
        assert!(!plan.activate.contains(&CellCoords::new(1, 1)));
        let mut sorted = plan.activate.clone();
        sorted.sort();
        assert_eq!(plan.activate, sorted, "activate is sorted by CellCoords");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `just test-one game "--lib cell_tickets::"`
Expected: compile errors.

- [ ] **Step 3: Implement** — top of `crates/game/src/cell_tickets.rs`:

```rust
//! Server-side cell tickets (spec §1): which cells should be active, given
//! player positions. Pure and deterministic; its output becomes one
//! `CellChanges` event per tick. Not sim state.

use std::collections::{BTreeMap, BTreeSet};

use crate::{cell_of_pos, cells_within, chebyshev, CellCoords};

/// Cells within this Chebyshev radius of a player are wanted active.
pub const SIM_RADIUS_CELLS: u32 = 2;
/// Extra radius a cell may drift out to before its release countdown starts.
pub const CELL_HYSTERESIS: u32 = 1;
/// Ticks a cell must stay beyond radius + hysteresis before it deactivates.
pub const CELL_RELEASE_GRACE_TICKS: u32 = 40;
/// Wanted (non-required) activations per tick.
pub const MAX_CELL_ACTIVATIONS_PER_TICK: usize = 1;

#[derive(Debug, Clone)]
pub struct CellConfig {
    pub cell_chunks: usize,
    pub sim_radius: u32,
    pub hysteresis: u32,
    pub release_grace_ticks: u32,
    pub max_activations_per_tick: usize,
}

impl CellConfig {
    pub fn new(cell_chunks: usize) -> Self {
        crate::cells_per_side(cell_chunks);
        Self {
            cell_chunks,
            sim_radius: SIM_RADIUS_CELLS,
            hysteresis: CELL_HYSTERESIS,
            release_grace_ticks: CELL_RELEASE_GRACE_TICKS,
            max_activations_per_tick: MAX_CELL_ACTIVATIONS_PER_TICK,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TicketPlan {
    /// Sorted.
    pub deactivate: Vec<CellCoords>,
    /// Sorted.
    pub activate: Vec<CellCoords>,
}

/// A player's own cell and its 8 neighbours (clipped): always active.
pub fn required_cells(pos: (f32, f32), cell_chunks: usize) -> Vec<CellCoords> {
    match cell_of_pos(pos.0, pos.1, cell_chunks) {
        Some(c) => cells_within(c, 1, cell_chunks),
        None => Vec::new(),
    }
}

#[derive(Debug, Default)]
pub struct CellTickets {
    /// Consecutive plans each active cell has spent outside keep range.
    out_of_range: BTreeMap<CellCoords, u32>,
}

impl CellTickets {
    pub fn plan(
        &mut self,
        active: &BTreeSet<CellCoords>,
        players: &[(f32, f32)],
        cfg: &CellConfig,
    ) -> TicketPlan {
        let player_cells: Vec<CellCoords> = players
            .iter()
            .filter_map(|p| cell_of_pos(p.0, p.1, cfg.cell_chunks))
            .collect();
        let dist = |c: CellCoords| player_cells.iter().map(|p| chebyshev(*p, c)).min();

        let mut required = BTreeSet::new();
        let mut wanted = BTreeSet::new();
        for p in &player_cells {
            required.extend(cells_within(*p, 1, cfg.cell_chunks));
            wanted.extend(cells_within(*p, cfg.sim_radius, cfg.cell_chunks));
        }

        let mut activate: BTreeSet<CellCoords> =
            required.iter().filter(|c| !active.contains(c)).copied().collect();
        let mut candidates: Vec<CellCoords> = wanted
            .iter()
            .filter(|c| !active.contains(c) && !required.contains(c))
            .copied()
            .collect();
        candidates.sort_by_key(|c| (dist(*c).unwrap_or(u32::MAX), *c));
        activate.extend(candidates.into_iter().take(cfg.max_activations_per_tick));

        let keep = cfg.sim_radius + cfg.hysteresis;
        let mut deactivate = Vec::new();
        for c in active {
            if dist(*c).is_some_and(|d| d <= keep) {
                self.out_of_range.remove(c);
                continue;
            }
            let n = self.out_of_range.entry(*c).or_insert(0);
            *n += 1;
            if *n >= cfg.release_grace_ticks {
                self.out_of_range.remove(c);
                deactivate.push(*c);
            }
        }
        self.out_of_range.retain(|c, _| active.contains(c));

        TicketPlan { deactivate, activate: activate.into_iter().collect() }
    }
}
```

Fix the test's expectation comments against this implementation if an off-by-one appears (the test pins: the release fires on the plan where the countdown reaches `release_grace_ticks`, i.e. loop index `CELL_RELEASE_GRACE_TICKS - 1`; `(3,3)` from `(0,0)` is distance 3 = keep, so it is kept in the first `plan`).

- [ ] **Step 4: Run to verify pass**

Run: `just test-one game "--lib cell_tickets::"` → 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/cell_tickets.rs crates/game/src/lib.rs
git commit -m "feat(game): cell ticket planner

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: In-memory cell store

**Files:**
- Create: `crates/game/src/cell_store.rs`
- Modify: `crates/game/src/lib.rs` (`pub mod cell_store;` + `pub use cell_store::*;`)

**Interfaces:**
- Consumes: `CellCoords`, `cell_of_chunk`, `cells_per_side`, `DormantCell`, `CellPayload` (Tasks 1–2); `TerrainSnapshot::from_columns` (`protocol.rs:270`).
- Produces: `CellStore::from_chunks(chunks: Vec<(ChunkCoords, Chunk)>, cell_chunks) -> Self` (every grid cell present, possibly empty); `CellStore::from_columns(columns: Vec<(ColumnCoords, Column)>, cell_chunks) -> Self`; `payload(&self, cell) -> Option<CellPayload>`; `put(&mut self, cell, content: DormantCell)`; `add_entity(&mut self, cell, bundle: EntityBundle)`; `version(&self, cell) -> Option<u64>`; `len(&self) -> usize`.

- [ ] **Step 1: Write the failing tests** — bottom of `crates/game/src/cell_store.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Chunk;

    fn chunks() -> Vec<(ChunkCoords, Chunk)> {
        vec![
            (ChunkCoords::new(9, 0, 1), Chunk::flat_floor(8)),
            (ChunkCoords::new(0, 0, 0), Chunk::flat_floor(4)),
            (ChunkCoords::new(1, -1, 0), Chunk::flat_floor(2)),
        ]
    }

    #[test]
    fn every_grid_cell_exists_and_chunks_land_sorted_in_their_cell() {
        let s = CellStore::from_chunks(chunks(), 8);
        assert_eq!(s.len(), 16);
        let p = s.payload(CellCoords::new(0, 0)).unwrap();
        let coords: Vec<_> = p.content.chunks.iter().map(|(c, _)| *c).collect();
        assert_eq!(coords, vec![ChunkCoords::new(0, 0, 0), ChunkCoords::new(1, -1, 0)]);
        assert_eq!(s.payload(CellCoords::new(1, 0)).unwrap().content.chunks.len(), 1);
        assert!(s.payload(CellCoords::new(3, 3)).unwrap().content.chunks.is_empty());
        assert_eq!(p.materialized_hash, 0);
        assert_eq!(p.version, 0);
    }

    #[test]
    fn put_bumps_the_version_only_on_change() {
        let mut s = CellStore::from_chunks(chunks(), 8);
        let c = CellCoords::new(0, 0);
        let same = s.payload(c).unwrap().content;
        s.put(c, same);
        assert_eq!(s.version(c), Some(0));
        s.put(c, DormantCell::default());
        assert_eq!(s.version(c), Some(1));
    }

    #[test]
    fn unknown_cells_have_no_payload() {
        let s = CellStore::from_chunks(chunks(), 8);
        assert!(s.payload(CellCoords::new(9, 9)).is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `just test-one game "--lib cell_store::"` → compile errors.

- [ ] **Step 3: Implement** — top of `crates/game/src/cell_store.rs`:

```rust
//! In-memory dormant-cell store (spec §3, hot layer only; Postgres is
//! phase 5). Server-side: the source of every activation payload.

use std::collections::BTreeMap;

use crate::{
    cell_of_chunk, cells_per_side, CellCoords, CellPayload, Chunk, ChunkCoords, Column,
    ColumnCoords, DormantCell, EntityBundle, TerrainSnapshot,
};

#[derive(Debug, Clone)]
struct StoredCell {
    version: u64,
    content: DormantCell,
}

#[derive(Debug, Clone)]
pub struct CellStore {
    cells: BTreeMap<CellCoords, StoredCell>,
}

impl CellStore {
    pub fn from_chunks(chunks: Vec<(ChunkCoords, Chunk)>, cell_chunks: usize) -> Self {
        let n = cells_per_side(cell_chunks);
        let mut cells: BTreeMap<CellCoords, StoredCell> = BTreeMap::new();
        for x in 0..n {
            for z in 0..n {
                cells.insert(CellCoords::new(x, z), StoredCell { version: 0, content: DormantCell::default() });
            }
        }
        for (coords, chunk) in chunks {
            let cell = cell_of_chunk(coords, cell_chunks);
            cells.get_mut(&cell).expect("chunk inside the region").content.chunks.push((coords, chunk));
        }
        for c in cells.values_mut() {
            c.content.chunks.sort_by_key(|(k, _)| (k.x, k.y, k.z));
        }
        Self { cells }
    }

    pub fn from_columns(columns: Vec<(ColumnCoords, Column)>, cell_chunks: usize) -> Self {
        Self::from_chunks(TerrainSnapshot::from_columns(columns).chunks, cell_chunks)
    }

    /// The activation payload for `cell` (hash left 0 for the server sim to fill).
    pub fn payload(&self, cell: CellCoords) -> Option<CellPayload> {
        self.cells.get(&cell).map(|s| CellPayload {
            cell,
            version: s.version,
            materialized_hash: 0,
            content: s.content.clone(),
        })
    }

    /// Store a deactivated cell's content; bumps the version iff it changed.
    pub fn put(&mut self, cell: CellCoords, content: DormantCell) {
        let entry = self.cells.entry(cell).or_insert(StoredCell { version: 0, content: DormantCell::default() });
        if entry.content.content_crc() != content.content_crc() {
            entry.version += 1;
            entry.content = content;
        }
    }

    /// A stray entity frozen into a dormant cell.
    pub fn add_entity(&mut self, cell: CellCoords, bundle: EntityBundle) {
        if let Some(entry) = self.cells.get_mut(&cell) {
            entry.content.entities.push(bundle);
            entry.version += 1;
        } else {
            log::error!("[cells] frozen entity for unknown cell {cell:?} dropped");
        }
    }

    pub fn version(&self, cell: CellCoords) -> Option<u64> {
        self.cells.get(&cell).map(|s| s.version)
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `just test-one game "--lib cell_store::"` → 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/cell_store.rs crates/game/src/lib.rs
git commit -m "feat(game): in-memory dormant cell store

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: `RegionRunner` cell mode

**Files:**
- Modify: `crates/game/src/region_runner.rs`, `crates/game/src/state.rs:499-500` (make `PLAYER_SPAWN_X`/`PLAYER_SPAWN_Z` `pub`), `crates/game/src/cells.rs` (`Rollback::player_positions`)
- Test: `crates/game/tests/cell_activation.rs`

**Interfaces:**
- Consumes: Tasks 4–7.
- Produces: `RegionRunner::new_with_cells(id, region: Region, out, store: CellStore, config: CellConfig) -> Self` (region must be `Region::new_cells`); `RegionRunner::cell_store(&self) -> Option<&CellStore>`; `Rollback::player_positions(&self) -> Vec<(f32, f32)>` (by `ClientId`). Behaviour: each `tick()` first emits at most one `CellChanges` (tickets), then the `Tick`, then files frozen strays and dematerialized cells into the store; `CreateClient` pre-activates the required cells around the spawn column; `EntityArrived` around the bundle pose.

- [ ] **Step 1: Write the failing tests** — append:

```rust
use crossbeam::channel::{unbounded, Receiver};
use game::{CellConfig, CellStore, RegionInput, RegionOutput, RegionRunner};

/// A flat 4×4-cell region: one floor chunk per column at y = 0.
pub fn flat_store() -> CellStore {
    let mut chunks = Vec::new();
    for x in 0..game::REGION_CHUNKS {
        for z in 0..game::REGION_CHUNKS {
            chunks.push((ChunkCoords::new(x, 0, z), Chunk::flat_floor(8)));
        }
    }
    CellStore::from_chunks(chunks, CC)
}

pub fn cell_runner() -> (RegionRunner, Receiver<(RegionCoords, RegionOutput)>) {
    let (out, rx) = unbounded();
    let runner = RegionRunner::new_with_cells(
        r0(),
        Region::new_cells(r0(), CC, None, None),
        out,
        flat_store(),
        CellConfig::new(CC),
    );
    (runner, rx)
}

pub fn processed(rx: &Receiver<(RegionCoords, RegionOutput)>) -> Vec<game::GameEvent> {
    rx.try_iter()
        .filter_map(|(_, o)| match o {
            RegionOutput::EventProcessed(e) => Some(e),
            _ => None,
        })
        .collect()
}

#[test]
fn create_client_activates_the_spawn_cells_first() {
    let (mut runner, rx) = cell_runner();
    runner.handle_input(RegionInput::Event(GameEventKind::CreateClient(0)));
    let evs = processed(&rx);
    assert!(matches!(evs[0].kind, GameEventKind::CellChanges(_)));
    assert!(matches!(evs[1].kind, GameEventKind::CreateClient(0)));
    let active = &runner.region_mut().data().cells.active;
    // Spawn (128,128) is cell (0,0): required = (0,0),(0,1),(1,0),(1,1).
    assert_eq!(active.len(), 4);
}

#[test]
fn ticks_grow_the_active_set_one_wanted_cell_at_a_time() {
    let (mut runner, rx) = cell_runner();
    runner.handle_input(RegionInput::Event(GameEventKind::CreateClient(0)));
    let _ = processed(&rx);
    let mut sizes = Vec::new();
    for _ in 0..6 {
        runner.tick();
        sizes.push(runner.region_mut().data().cells.active.len());
    }
    // Radius 2 from cell (0,0) = 3×3 = 9 cells; 4 required, then +1 per tick.
    assert_eq!(sizes, vec![5, 6, 7, 8, 9, 9]);
    let evs = processed(&rx);
    let cell_events = evs.iter().filter(|e| matches!(e.kind, GameEventKind::CellChanges(_))).count();
    assert_eq!(cell_events, 5, "no empty CellChanges once the set is complete");
}

#[test]
fn deactivated_cells_go_back_to_the_store_with_a_new_version_only_if_changed() {
    let (mut runner, rx) = cell_runner();
    runner.handle_input(RegionInput::Event(GameEventKind::CreateClient(0)));
    for _ in 0..8 {
        runner.tick();
    }
    // Remove the player: everything releases after the grace period.
    runner.region_mut().with_data(|rb| {
        let key = *rb.data.player_entites.get(&0).unwrap();
        rb.remove_entity_safe(key, Some(0));
        rb.data.player_entites.remove(&0);
    });
    for _ in 0..game::CELL_RELEASE_GRACE_TICKS + 2 {
        runner.tick();
    }
    assert!(runner.region_mut().data().cells.active.is_empty());
    // Untouched terrain: versions stay 0.
    assert_eq!(runner.cell_store().unwrap().version(CellCoords::new(0, 0)), Some(0));
    let _ = processed(&rx);
}

#[test]
fn a_cell_missing_from_the_store_is_skipped_not_fatal() {
    let (out, _rx) = unbounded();
    let mut runner = RegionRunner::new_with_cells(
        r0(),
        Region::new_cells(r0(), CC, None, None),
        out,
        CellStore::from_chunks(Vec::new(), game::REGION_CHUNKS), // a 1-cell grid: cells (0,1) etc. unknown
        CellConfig::new(CC),
    );
    runner.handle_input(RegionInput::Event(GameEventKind::CreateClient(0)));
    runner.tick();
    assert!(runner.region_mut().data().cells.active.contains(&CellCoords::new(0, 0)));
    assert!(!runner.region_mut().data().cells.active.contains(&CellCoords::new(1, 1)));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `just test-one game "--test cell_activation"` → compile errors (`new_with_cells` missing).

- [ ] **Step 3: Implement**

`crates/game/src/state.rs:499-500`: `pub const PLAYER_SPAWN_X` / `pub const PLAYER_SPAWN_Z`.

`crates/game/src/cells.rs`, in `impl Rollback`:

```rust
    /// Region-local (x, z) of every player body, by ClientId.
    pub fn player_positions(&self) -> Vec<(f32, f32)> {
        self.data
            .player_entites
            .values()
            .filter_map(|e| {
                let h = (*self.data.ecs.rigidbody.try_get(*e))?;
                let t = self.data.physics.bodies.get(h)?.translation();
                Some((t.x.0, t.z.0))
            })
            .collect()
    }
```

`crates/game/src/region_runner.rs`: add imports `CellChanges, CellConfig, CellStore, CellTickets, required_cells, PLAYER_SPAWN_X, PLAYER_SPAWN_Z` and:

```rust
/// Cell-mode state of a runner (spec §1, §3): tickets + the dormant store.
pub struct CellMode {
    store: CellStore,
    tickets: CellTickets,
    config: CellConfig,
}
```

Add field `cells: Option<CellMode>` to `RegionRunner` (`None` in `new`), and:

```rust
    pub fn new_with_cells(
        id: RegionCoords,
        region: Region,
        out: Sender<(RegionCoords, RegionOutput)>,
        store: CellStore,
        config: CellConfig,
    ) -> Self {
        assert_eq!(region.data().cells.cell_chunks, config.cell_chunks, "region/config cell size mismatch");
        let mut runner = Self::new(id, region, out);
        runner.cells = Some(CellMode { store, tickets: CellTickets::default(), config });
        runner
    }

    pub fn cell_store(&self) -> Option<&CellStore> {
        self.cells.as_ref().map(|c| &c.store)
    }

    /// Emit one CellChanges (if non-empty) for `deactivate` + `activate`,
    /// then file dematerialized cells back into the store.
    fn apply_cell_changes(&mut self, deactivate: Vec<CellCoords>, activate: Vec<CellCoords>) {
        let Some(mode) = self.cells.as_mut() else { return };
        let mut changes = CellChanges { deactivate, activate: Vec::new() };
        for cell in activate {
            match mode.store.payload(cell) {
                Some(p) => changes.activate.push(p),
                None => log::error!("[cells r={:?}] no store entry for {cell:?}; skipped", self.id),
            }
        }
        if changes.is_empty() {
            return;
        }
        self.apply(GameEventKind::CellChanges(changes));
        let dormant = self.region.take_dormant();
        if let Some(mode) = self.cells.as_mut() {
            for (cell, content) in dormant {
                mode.store.put(cell, content);
            }
        }
    }

    /// Required cells around `pos` that are not active yet, activated now,
    /// outside the budget (joins and arrivals).
    fn ensure_required(&mut self, pos: (f32, f32)) {
        let Some(mode) = self.cells.as_ref() else { return };
        let active = &self.region.data().cells.active;
        let missing: Vec<CellCoords> = required_cells(pos, mode.config.cell_chunks)
            .into_iter()
            .filter(|c| !active.contains(c))
            .collect();
        self.apply_cell_changes(Vec::new(), missing);
    }

    /// The per-tick ticket pass (before the Tick event).
    fn run_tickets(&mut self) {
        let Some(mode) = self.cells.as_mut() else { return };
        let players = self.region.data().player_positions();
        let plan = mode.tickets.plan(&self.region.data().cells.active, &players, &mode.config);
        self.apply_cell_changes(plan.deactivate, plan.activate);
    }
```

(`CellCoords` also needs importing.) In `handle_input`, replace the `kind => self.apply(kind)` arm of `RegionInput::Event` with:

```rust
                kind => {
                    match &kind {
                        GameEventKind::CreateClient(_) => self.ensure_required((PLAYER_SPAWN_X, PLAYER_SPAWN_Z)),
                        GameEventKind::EntityArrived(b) => {
                            let t = b.isometry.translation;
                            self.ensure_required((t.x.0, t.z.0));
                        }
                        _ => {}
                    }
                    self.apply(kind)
                }
```

In `tick()`, first line: `self.run_tickets();`. After `let (departures, ghosts) = self.region.take_transfers();` add:

```rust
        let frozen = self.region.take_frozen();
        if let Some(mode) = self.cells.as_mut() {
            for (cell, bundle) in frozen {
                mode.store.add_entity(cell, bundle);
            }
        }
```

Note on the test `deactivated_cells_go_back...`: removing the player through `with_data` is test surgery; if `remove_entity_safe` already clears `player_entites`, drop the extra `remove`. Adjust access (`rb.data.player_entites`) to what `Rollback` exposes in tests (see `tests/handoff_state.rs`).

- [ ] **Step 4: Run to verify pass**

Run: `just test-one game "--test cell_activation"` → all pass (tag >100 ms ones slow).
Run: `just test-one game "--test region_runner"` → pass (legacy runner unchanged).
Run: `just smoke` → passes.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/region_runner.rs crates/game/src/state.rs crates/game/src/cells.rs crates/game/tests/cell_activation.rs
git commit -m "feat(game): region runner cell mode with tickets and store

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: Client convergence under prediction

**Files:**
- Test: `crates/game/tests/cell_activation.rs`
- Modify: `CLAUDE.md` (one paragraph under `crates/game`), `TODO.md` (phase status)

**Interfaces:**
- Consumes: everything above; `Region::reconcile` (`region.rs:180`); `game::set_hash_mode` / `HashMode` (check exact names with `grep -n "pub fn set_hash_mode\|pub enum HashMode" crates/game/src`).

- [ ] **Step 1: Write the tests** — append:

```rust
use game::InputEvent;

/// Client joined from the runner's current state, predicting `lead` ticks
/// ahead of the server and reconciling every server event.
struct Harness {
    runner: RegionRunner,
    rx: Receiver<(RegionCoords, RegionOutput)>,
    client: Region,
    _recv: Receiver<game::GameDataUpdate>,
}

impl Harness {
    fn new() -> Self {
        game::set_hash_mode(game::HashMode::Full);
        let (mut runner, rx) = cell_runner();
        runner.handle_input(RegionInput::Event(GameEventKind::CreateClient(0)));
        let _ = processed(&rx);
        let (send, recv) = unbounded();
        let snapshot = runner.region_mut().data().clone();
        let client = Region::new(snapshot, Some(send), r0(), Some(0));
        Self { runner, rx, client, _recv: recv }
    }

    /// Client predicts `lead` ticks, then the server runs one tick and the
    /// client reconciles everything it emitted.
    fn step(&mut self, lead: usize, input: Option<InputEvent>) {
        if let Some(i) = input.clone() {
            self.client.handle_event(GameEventKind::PlayerInput(0, i.clone())).unwrap();
            self.runner.handle_input(RegionInput::Event(GameEventKind::PlayerInput(0, i)));
        }
        while self.client.pending_event_ids().len() < lead {
            self.client.handle_event(GameEventKind::Tick).unwrap();
        }
        self.runner.tick();
        for ev in processed(&self.rx) {
            self.client.reconcile(ev).unwrap();
        }
    }

    /// Drain predictions: server catches up, then hashes must match.
    fn settle_and_compare(&mut self) {
        for _ in 0..4 {
            self.runner.tick();
            for ev in processed(&self.rx) {
                self.client.reconcile(ev).unwrap();
            }
        }
        // Converged once every prediction is confirmed.
        assert!(self.client.pending_event_ids().is_empty(), "client still ahead");
        assert_eq!(
            state_hash(self.runner.region_mut().data()),
            state_hash(self.client.data()),
            "client replica diverged"
        );
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_predicting_client_converges_while_cells_load() {
    let mut h = Harness::new();
    for _ in 0..8 {
        h.step(2, None);
    }
    h.settle_and_compare();
    assert_eq!(h.client.data().cells.active.len(), 9);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_client_with_no_pending_predictions_still_applies_cell_changes() {
    let mut h = Harness::new();
    // No client prediction at all: server events park, then apply once the
    // client predicts again.
    h.runner.tick();
    let evs = processed(&h.rx);
    assert!(evs.iter().any(|e| matches!(e.kind, GameEventKind::CellChanges(_))));
    for ev in evs {
        h.client.reconcile(ev).unwrap();
    }
    for _ in 0..3 {
        h.step(2, None);
    }
    h.settle_and_compare();
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walking_across_cells_converges_for_every_seed() {
    use game::Key;
    for seed in 0..8u64 {
        let mut h = Harness::new();
        let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        // Enter fps mode, then hold W with random prediction leads.
        h.step(1, Some(InputEvent::FpsCam(true)));
        h.step(1, Some(InputEvent::Key { key: Key::KeyW, pressed: true }));
        for _ in 0..120 {
            let lead = 1 + (next() % 3) as usize;
            h.step(lead, None);
        }
        h.settle_and_compare();
        let active = h.client.data().cells.active.len();
        assert!(active >= 4, "seed {seed}: cells followed the player (active={active})");
    }
}
```

Adjust `InputEvent::Key` / `FpsCam` field shapes to `crates/game/src/input.rs` if they differ (the `multi_client.rs` test uses `InputEvent::Key { key, pressed }`). If walking 120 ticks does not reach a new cell with 256-unit cells, raise the tick count until the player crosses at least one cell edge (assert `cells.active` changed between start and end). Record the seed in the assertion message, as above.

- [ ] **Step 2: Run them**

Run: `just test-one game "--test cell_activation -- --ignored"`
Expected: 3 passed. If one fails, it names the seed; treat it as a determinism bug in Tasks 4–8 (use `superpowers:systematic-debugging`), never loosen the hash assertion.

- [ ] **Step 3: Docs**

In `CLAUDE.md`, in the `crates/game` bullet after the `region_runner.rs` sentence, add:

```
Cell loading (phase 1 of `docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md`, game-only — production still uses fully materialized regions): `cells.rs` (`CellCoords`, `CellState` in `GameData.cells` — `cell_chunks == 0` is a legacy region; `CellChanges` server event, materialize/dematerialize with a server-filled, client-verified `cell_hash`), `cell_tickets.rs` (pure ticket planner, tunables `SIM_RADIUS_CELLS` etc.), `cell_store.rs` (in-memory dormant cells); `RegionRunner::new_with_cells` drives them. Tests: `just test-one game "--test cell_activation"` (+ `-- --ignored` for the convergence runs).
```

In `TODO.md`, add under the multi-region follow-ups: `- Island regions + cell loading: phase 1 (game-only cell mode) DONE; next: phase 1b WorldManager/server/client wiring + rendering (not yet planned), then phase 2 (measure + cheaper activation).`

- [ ] **Step 4: Final checks**

Run: `just smoke` → passes.
Run: `just test-one game "--test cell_activation"` → passes.
Run: `just test-threads` → passes (runner changed; legacy threading unaffected).

- [ ] **Step 5: Commit**

```bash
git add crates/game/tests/cell_activation.rs CLAUDE.md TODO.md
git commit -m "test(game): client replicas converge while cells load

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```
