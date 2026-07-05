# Physics Step Journal — rapier under rollback without per-tick snapshots

**Date:** 2026-07-04
**Status:** Approved design, pending implementation
**Follows:** undo-api phase 4 (arena inverses), scalable-undo-hashing (cached voxel hash, server loop hygiene)

## Problem

`PhysicsController::on_tick` rolls back `step()` by logging a whole-`PhysicsState`
clone every tick (`snapshot_raw()`), because `step()` mutates broad/narrow-phase
caches with no per-entry delta available. That was the phase-4 resolution; its
costs remain:

1. The client's rollback window holds one full `PhysicsState` clone per tick —
   O(total colliders + total pairs) bytes × window length, growing with world
   size, unrelated to how much actually moved.
2. `undo_scope()`/`undo()` compute a `pre_hash` over the whole `PhysicsState`
   per transaction — O(total colliders) time per tick even after the cached
   voxel hash (BVH walk, collider set walk, graph_indices walk).

Goal (agreed): per-tick undo cost — bytes and time — scales with **active
bodies + touched contact pairs**, not with total world size. The rollback bar
is unchanged: broad/narrow-phase and solver caches stay part of the hashed,
bit-exact-restored state (warm-start impulses feed the next step; client resim
matches the server only if they restore exactly).

## Rejected alternatives

- **Copy-on-write storages in the fork** — automatic completeness, but rewrites
  the solver's hot data paths, huge upstream divergence, and hashing still needs
  separate treatment.
- **Keyframe + resimulate** — physics would roll back by re-stepping while every
  other field rolls back by undo log; re-stepping without re-running the other
  controllers desyncs them. Doesn't fit the transaction model.
- **Reclassify phase/solver caches as derived data** (rebuild on rollback) —
  rejected up front: weaker determinism guarantee, and warm-start state is
  semantically part of the simulation.
- **Tier-1 macro wrappers on rapier's internals** — the macro's log is generated
  inside `game` (circular dependency), wrappers can't observe writes through
  bare `&mut` in solver loops, and per-call logging would record every solver
  iteration instead of the per-tick old value. See "How the journal works".

## Design: mutation journal (`StepJournal`)

A `StepJournal` lives in the rapier fork. `PhysicsPipeline::step_journaled()`
(the existing `step()` delegates with no journal) threads `&mut StepJournal`
through its stages. The invariant:

> Before the **first** mutation of any hashed object in a tick, its old value
> is saved into the journal exactly once (dedup via a per-tick saved-set).
> Structural container ops additionally record exact LIFO inverses.
> `StepJournal::revert(...)` restores everything in LIFO order, bit-exact.

The journal is pure capture — it reads and clones, never influences the
simulation, so determinism is untouched by construction.

### Why this works (the correctness argument)

`hash(before) == hash(after undo)` holds iff every *hashed* byte that `step()`
writes is restored. Every hashed byte belongs to exactly one of:

1. **Value-restorable objects** — bodies, colliders, contact/intersection pair
   payloads, island vectors, modified/removed lists, wake-up sets, counters.
   Saving the old value before first write and assigning it back is a true
   inverse regardless of how many times the object was rewritten in between.
   Dedup ("first write wins") is what turns "thousands of solver writes" into
   "one saved clone per touched object" — the O(active) property.
2. **Structural container ops** — graph node/edge insertions and removals,
   arena inserts. Plain re-removal/re-insertion does NOT restore allocator and
   link state (swap-remove relocation, intrusive list heads, generations) — the
   same lesson as `Arena::revert_insert` from phase 4 — so these get exact
   LIFO inverse records (an append is undone by popping; a swap-remove is
   undone from the saved records of both affected slots).

Coverage is *empirically enforced*, not assumed: the existing rollback
machinery panics on hash mismatch at undo time, so any missed write site fails
loudly in the fork-level tests and the randomized rollback suite.

The per-tick mutation set is intrinsically O(active): the solver only touches
islands of awake bodies, contact manifolds only exist for near pairs, and the
verified choke points below discover exactly that set.

### Where written objects are discovered (verified against the fork)

Bodies — every `RigidBody` write in `step()` is reachable from five hook sites
(verified by tracing all write paths in `physics_pipeline.rs`, `user_changes.rs`,
`island_manager.rs`, `velocity_solver.rs`):
`modified_bodies` at step entry; `islands.wake_up()` (joint wake-ups and
narrow-phase wake-ups); the previous `active_set` at island-update entry (covers
kinematic velocity interpolation, solver writeback, sleep commits, CCD clamp,
`advance_to_final_positions`, both mass-props loops — all iterate subsets of it);
`handle_user_changes_to_rigid_bodies`; `clear_modified_bodies` (flag resets,
`modified_bodies` only). Multibody members would be a sixth path — unused; the
journal asserts the multibody set is empty (coarse wholesale capture if that
ever changes).

Colliders — writes happen only to entry `modified_colliders`, to colliders
pushed via `push_once` during position propagation, and in the
`clear_modified_colliders` full sweep. The sweep writes `changes = empty()` to
*every* collider, but it's a no-op for colliders whose flags are already empty
— the journal saves only those with non-empty flags (⊆ modified). So collider
capture is O(modified).

Narrow phase — `compute_contacts`/`compute_intersections` are payload-only
(rewrite edge weights in place): save old `ContactPair`/`IntersectionPair` per
touched edge, O(touching pairs). `add_pair` appends nodes/edges — exact inverse
is a pop (appends land at the vec tail and list heads; verified in
`data/graph.rs`). `remove_pair` (`remove_edge`) swap-relocates the last edge
and splices intrusive lists — inverse restores from saved records: the removed
`Edge` record, the relocated edge's old position/links, and the endpoint nodes'
old list heads. `graph_indices` (Coarena) slot writes: save old slot value.
Collider-removal cascades (`remove_node`) don't happen mid-game today: when
`removed_colliders` is non-empty the journal falls back to a wholesale
`NarrowPhase` capture for that tick (rare structural event, O(pairs)).

Islands — `IslandManager`'s hashed fields (`active_set`, `active_islands`,
additional-iterations, timestamp) are rebuilt each step and are already
O(active): captured wholesale per tick. Its `can_sleep`/`stack` scratch is
unhashed and ignored (existing fork precedent).

Joint sets — `to_wake_up` (hashed in both sets) is drained at step entry: save
drained contents. The game has no joints; if either set is non-empty at entry,
the journal captures both wholesale (O(joints)) rather than instrumenting the
joint solver now.

Lists and counters — `modified_bodies`/`modified_colliders`/`removed_colliders`
are hashed and drained/cleared/re-inserted during `step()`: the journal saves
the entry-time vectors (O(modified)).

### Broad phase: the one deliberate deviation from O(active)

Finding: `BroadPhaseBvh::update()` unconditionally runs `refit()`, which
**rewrites the entire node array every frame** (DFS-order compaction into a
workspace buffer + swap), and the default `SubtreeOptimizer` strategy rebuilds
~5% of the tree per frame *even when nothing moves*; `frame_index` increments
every frame. Node-level journaling of an algorithm that rewrites everything is
O(total) with extra steps. Instead:

1. **Disable the incremental optimizer** (`BvhOptimizationStrategy::None`, the
   fork default, set where `PhysicsState` constructs its `BroadPhaseBvh`).
   Insert-time rotations keep the tree in good-enough shape without per-tick
   churn; a one-time full `Bvh::rebuild` at world/region creation is the
   *escalation path* for tree quality — available if broad-phase queries ever
   profile hot, not done unconditionally. Terrain is static and body counts are
   small; optimizer churn is pure rollback noise.
2. **Clean-tick skip**: if there are no removed colliders, it's not the first
   pass, and no leaf AABB actually changed beyond the change-detection skin
   (`insert_or_update_partially` gains a "wrote?" return), skip optimize +
   refit + traversal + stale-pair GC and don't bump `frame_index`. Idle or
   sub-skin ticks mutate nothing → nothing to journal. The skip condition is a
   deterministic function of state, so cross-machine determinism holds.
3. **Dirty-tick wholesale capture**: on the first broad-phase mutation of a
   tick, save `(tree, pairs, frame_index)` by clone. This is O(collider count)
   bytes on ticks where something moved past the skin — accepted deviation:
   n is *collider* count (chunks + bodies, e.g. ~100–4000), not voxel count;
   the clone is a few-hundred-KB memcpy at the extreme, and the refit it
   shadows is already O(n) compute every dirty tick. If profiling at larger
   region sizes ever demands it, the escalation path is a change-list-driven
   ancestor refit (O(moved·depth)) with node-level journaling — recorded as
   future work, not built now.

Consequence of (1)+(2) vs. today: tree layout and pair-discovery order change
(hence solver order, hence bitwise trajectories). That is fine — determinism is
defined by the fork, both sides run the same code — but it lands as a behavior
change in one commit, alone, so bisection stays clean.

### Game-side integration (one rollback system)

The journal is delta *payload*; the existing macro-generated log keeps owning
transactions, ordering, hashing, verification, and forgetting — same pattern as
`SlotOp` for slotmaps and `revert_insert` for arenas:

```rust
// PhysicsController::on_tick — replaces snapshot_raw()
let mut scope = data.physics.undo_scope();       // pre_hash (gated, see below)
let p = scope.raw_fields();                       // existing raw-access license
let mut journal = StepJournal::default();
self.pipeline.step_journaled(/* p.* fields */, &mut journal);
scope.register(move |phys, _| journal.revert(phys.raw_undo_parts()));
```

Two small macro additions (no changes to existing wrapper semantics):

1. `raw_undo_parts()` — a raw per-field view (like `raw_fields()`) obtainable
   on the bare struct *inside undo closures only by convention*; running as an
   undo is the mutation license. Today's closures can only whole-struct-assign.
2. **Hash-verification gating**: a runtime flag (`VERIFY_HASHES`, an
   `AtomicBool` defaulting to `true`, flipped once via `set_hash_verification`)
   guards every `pre_hash` computation and rollback-time check;
   `undo_scope()`/`undo()` compute `pre_hash` only while the flag is set, and
   entries logged while it's off carry a sentinel and rollback skips the
   check. Both binaries set it under `cfg(not(debug_assertions))` at the top
   of `main()`, before any world/transaction exists. Tests (always debug)
   keep enforcing the bit-exact bar on every transaction; release stops
   paying O(total) hashing per tick. The state itself is restored bit-exact
   in all builds — only the always-on *self-check* is gated.

Server and client run the identical journaled path (the server's forget
already prunes the entries; capture cost is O(active) and keeps debug-build
verification meaningful on both sides).

## Non-goals

- Collider attach/detach exact inverses — `attach_*_collider_safe` keeps its
  whole-`PhysicsState` snapshot (rare, chunk-load/join frequency).
- Typed log tier for the step delta (`UndoOp::Step(...)`) and the shared
  undo-core crate — future evolutions; the tier-2 closure carry ships first.
- Node-level BVH journaling / change-list refit — escalation path only.
- Joint/multibody fine-grained journaling — wholesale fallback suffices.
- Any change to reconcile logic, protocol, or the renderer.

## Testing / acceptance

Fork-level (new `crates/game/tests/step_journal.rs`, `hash_restore.rs` style —
build components directly, `h()` = crc32 of `Hash`):

1. Idle scene (sleeping + fixed bodies): journal after a tick is empty (asserts
   the clean-tick skip and O(0) idle cost); revert restores hash.
2. Falling dynamic body onto a voxels floor: N steps, revert LIFO, hash-exact
   at every intermediate tick.
3. Sleep transition (settle → sleep) and wake transition (impact wakes sleeper)
   revert hash-exact — covers activation timers, zeroed velocities, island
   rebuild.
4. Kinematic position-based body (player-shaped) moving each tick.
5. Pair lifecycle: fly-by creating then destroying contacts — exercises
   `add_pair` pop-inverses and `remove_edge` record-inverses.
6. Mid-run collider insertion (chunk-create shaped): dirty BVH capture path.
7. Seeded randomized scenes (random impulses/kinematic moves, R ticks), undo
   all, hash-exact — the completeness fuzzer.
8. Journal-size property: at a 64-chunk world, idle tick ≈ 0 bytes; one moving
   body's tick bounded by a constant independent of chunk count.

Integration: flip `on_tick`, full existing battery (`log_model`, `simple`,
`random_ops`, `hash_restore`, client suite, multi-client) — the rollback
hash-verification panic is the completeness oracle. Perf: the 100-ticks
regression test from the hashing branch must hold or improve; note undo-log
bytes/tick before/after in the report. Live smoke: server + one client, move,
edit voxels, confirm reconcile stays clean.

## Files

- `crates/rapier/src/pipeline/step_journal.rs` (new) — `StepJournal`, revert.
- `crates/rapier/src/pipeline/physics_pipeline.rs` — `step_journaled`,
  hook threading; `user_changes.rs`, `island_manager.rs`, `narrow_phase.rs`,
  `data/graph.rs`, `data/coarena.rs` — capture hooks + exact inverses.
- `crates/rapier/src/geometry/broad_phase_bvh.rs` (+
  `crates/parry/src/partitioning/bvh/bvh_insert.rs` "wrote?" return) —
  clean-tick skip, dirty capture.
- `crates/macros/src/lib.rs` — `raw_undo_parts`, gated `pre_hash`.
- `crates/game/src/state.rs` — BVH strategy at construction;
  `crates/game/src/physics.rs` — journaled `on_tick`.
- `crates/game/tests/step_journal.rs` (new).
