# Scalable Undo Hashing + Server Loop Hygiene

**Date:** 2026-07-04
**Status:** Approved design, pending implementation
**Blocks:** merging the parked `microvoxel-scale` branch.

## Problem (measured, not theoretical)

The microvoxel scale change (1 unit = 1/16 m, 8×8-chunk floor ≈ 2.1M voxels)
made live servers unable to serve joins, while all tests stayed green.
Instrumented findings:

1. Every server tick, `PhysicsController::on_tick` logs a whole-PhysicsState
   snapshot whose `pre_hash` crc32-walks **all voxel collider contents**
   (~5.7–13.5 ms/tick at 64 chunks; the dominant server CPU per perf).
   Clients pay the same walk on every reconcile rollback (multi-client test
   suite: 0.02 s → 6 s).
2. `World::basic` boots in ~5 s (O(N²) in chunks: chunk N's creation
   snapshot hashes the state containing N−1 chunks); ~22 s when the CPU is
   throttled.
3. The tick thread free-runs during boot, queueing hundreds of tick events
   in the same FIFO channel as client packets — a joining client's first
   request sits behind the whole backlog. Under throttle the backlog never
   drains: joins never complete.
4. The server never forgets Tick transactions: the undo log (holding a
   PhysicsState clone per tick) and memory grow without bound.

Root cause of 1–2: undo verification hashes scale with **total** state,
not **mutated** state. Voxel collider *contents* are immutable during
`step()` — only body/collider poses and phase structures change — yet the
content bytes are re-hashed every tick.

Verified enablers: parry's `SharedShape(pub Arc<dyn Shape>)` means snapshot
clones are Arc-cheap already (hashing, not cloning, is the cost);
`Voxels` currently `#[derive(Hash)]`s over all chunk state arrays; the
macro's `hash_data` is crc32 over the field's `Hash` impl.

## Design

### 1. Cached content hash in vendored parry `Voxels`

(`crates/parry/src/shape/voxels/`; consistent with existing fork patches
like `revert_insert` — the forks exist to support this rollback machinery.)

- `Voxels` gains `cached_hash: u32`, computed from full voxel content:
  set in `new`/`from_points`/other constructors, recomputed by every
  mutator in `voxels_edition.rs` (eager full recompute — edits are rare,
  per-tick reads are the hot path; incremental updating is a later
  optimization if editing gameplay demands it).
- Replace `#[derive(Hash)]` with a manual `impl Hash` that feeds **only**
  `cached_hash` (plus `voxel_size`). The hash stays deterministic and
  content-based, so the rollback bar (`hash(before) == hash(after undo)`,
  bit-exact) and cross-machine state comparison keep their meaning.
- Serde: the cache field serializes with the derive (deviation from the
  earlier skip-and-recompute idea: every mutation path maintains the
  invariant, snapshots only travel between identical binaries, and keeping
  the derive minimizes fork surface).
- The content-hash function itself must be deterministic and independent of
  HashMap iteration order (iterate parry-chunks in sorted key order).

Effect: per-tick physics `pre_hash`, rollback verification, and boot-time
chunk-creation snapshots drop from O(total voxels) to O(collider count).
Expected: tick cost back to sub-millisecond range; boot near-linear;
client reconciles cheap again.

### 2. Server loop hygiene (`crates/server/src/main.rs`)

- **Tick thread starts after world creation**: move the spawn below
  `World::basic()` so no boot backlog exists.
- **Tick coalescing**: the timer thread sends a tick only if the previous
  one was consumed (e.g. a `pending_ticks: Arc<AtomicU64>` incremented on
  send, decremented by the game loop; skip send when > 1). Bounded lag —
  a slow loop drops tick *generation* instead of queueing unboundedly, and
  client packets are never starved behind a tick pileup.
- **Forget Tick transactions**: after `progress_world_one_tick` in the
  `ServerTickTimer` arm, call `world.forget_last_event(&region)` for each
  region result (the server never rolls back). Bounds the undo log and
  memory. Client behavior unchanged (clients already forget via reconcile
  confirmation).

## Non-goals

- Incremental voxel-hash updates on edit (no edit gameplay yet).
- Caching `Chunk` (render voxel data) hashes in `GameData` — only walked
  on rare chunk-component entries; revisit if it ever shows in a profile.
- Any change to the undo API surface, the macro, or the reconcile logic.
- Client-side loop changes.

## Testing / acceptance

- Vendored-fork tests (in parry or `crates/game/tests/hash_restore.rs`
  style): `Voxels` hash equality across clone, serialize/deserialize
  roundtrip, and edit-then-revert; hash inequality for different contents.
- Existing suites green, and the multi-client suite runtime returns to
  near its pre-microvoxel scale (order of 0.1 s, not 6 s) — timing noted
  in the report, not asserted in CI.
- New regression test: on the 64-chunk world, 100 ticks complete in well
  under 1 s (asserts the O(mutated) property with margin for slow CI).
- Server loop: unit-testable coalescing (pending counter never exceeds 1);
  live check that a client joining immediately after server start gets its
  region within a couple of seconds.
- Final gate: un-park `microvoxel-scale`, rebase onto this work, and the
  previously-failing live two-client join must complete promptly with two
  players moving on the 16 m floor.

## Files

- `crates/parry/src/shape/voxels/voxels.rs` (+ `voxels_edition.rs`) —
  cached hash, manual `Hash`, serde skip/recompute.
- `crates/server/src/main.rs` — tick thread placement, coalescing,
  tick-transaction forgetting.
- `crates/game/tests/` — hash-cache invariants, tick-cost regression test.
