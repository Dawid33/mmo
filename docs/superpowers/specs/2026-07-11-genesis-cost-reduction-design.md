# Region-genesis cost reduction (test-full speedup) — design

**Date:** 2026-07-11
**Goal:** cut `just test-full` wall time roughly in half by removing redundant
work from region genesis in game/fork code. No test-side changes; tests keep
running with hash mode `Full` and keep exercising the same code paths.

## Measurements (2026-07-11, debug profile, single-threaded)

Full suite: 3225s CPU over 445 tests. 88% of it is 16 tests — the
client/server convergence rigs (`reconcile_keepup`, `crossing`,
`harness_smoke`), each standing up two full 3×3 region windows
(LocalServer's copy plus the client's). perf attribution of
`reconcile_keepup::diag_a_lockstep_single_input` (186s):

| % of test | bucket |
|---|---|
| ~27% | undo digests during `seed_terrain_genesis` — Full mode overrides the slot-sized delta digests with whole-field crc32 per logged entry; quadratic in chunks-per-region |
| ~17% | voxel mass properties (`from_voxels`) computed at collider attach + first-tick `recompute_mass_properties_from_colliders`, then discarded because chunk bodies are `Fixed` |
| ~14% | `ColliderBuilder::voxels` (`Voxels::new` neighborhood computation) — genuine work, out of scope |
| ~13% | `worldgen::generate_region` — genuine work, cacheable |
| ~10% | `game::lod::downsample2` — genuine work, out of scope |
| ~0% | solver / StepJournal / narrow phase — per-tick physics is not the problem |

`seed_terrain_genesis` is 46.9% inclusive. Per-tick simulation is negligible;
the suite's cost is region genesis, paid dozens of times across the rigs.

## Change 1 — unlogged ECS writes in `seed_terrain_genesis`

`crates/game/src/state.rs`. Genesis already treats physics as pre-base-state:
raw `bodies.raw_mut().insert(body)` and raw `insert_with_parent`, justified in
the doc comment as "genesis is the base state and is never rolled back below
itself, so raw (unlogged) inserts are correct". The ECS half of the same loop
still goes through the logged path (`create_entity_safe`, `chunk.set_safe`,
`rigidbody.set_safe`), producing one undo entry + digest per write — under
Full mode, a whole-field crc32 of the growing component maps per entry.

Change: give the genesis loop raw ECS writes — entity insert plus the
`chunk`/`rigidbody` (and the other `create_entity_safe`-created) component
slots via `raw_mut()` accessors, zero undo entries, zero digests. This is
cheaper in every hash mode, not just Full, and shrinks the undo log the rigs
carry.

Constraints to verify during implementation:

- **Renderer emits.** `create_entity_safe`'s `#[emit]` updates ride the undo
  deltas; raw writes produce none. Genesis runs server-side before any
  subscriber sees the region (clients receive a `SerializedRegion` snapshot,
  not the delta stream), so no consumer should observe genesis-time
  `GameDataUpdate`s — the plan must include an explicit check (and a test if
  one doesn't exist) that region load on the client renders correctly from
  the snapshot alone.
- **Undo invariant.** `hash(before) == hash(after undo)` must still hold for
  transactions that begin after genesis; a rollback that bottoms out at the
  genesis base state must restore bit-exact. Covered by the existing
  `log_model` / `rollback_restore` / `hash_restore` suites plus the
  convergence rigs themselves.

## Change 2 — skip mass properties for fixed bodies (vendored rapier fork)

Chunk terrain bodies are `RigidBodyType::Fixed`;
`update_world_mass_properties` zeroes their effective inverse mass/inertia
immediately after computation, so every `from_voxels` pass (two full sweeps
over the voxel cells + a 3×3 shifted-inertia matrix per solid cell, in
OrderedFloat/libm math) is discarded work. Root cause of the ~318ms/region
first-tick join spike (see `first-tick-massprops-spike` memory; probe:
`worldgen/examples/narrow_phase_probe.rs` — 69.2ms of a 72.9ms first tick).

Two skip sites, both already have the body type in hand:

- `crates/rapier/src/dynamics/rigid_body_components.rs:1130`
  (`attach_collider`): skip the eager `mass_properties(shape)` computation and
  `local_mprops` accumulation when the parent body is fixed.
- `crates/rapier/src/pipeline/user_changes.rs:177`
  (`handle_user_changes_to_rigid_bodies`): skip
  `recompute_mass_properties_from_colliders` when `rb.body_type` is fixed.
  This also kills the client-side first-tick recompute driven by serialized
  dirty flags (`modified_bodies` and per-body change flags ship inside
  `SerializedRegion`).

Guard: when a body's TYPE changes to non-fixed (`set_body_type`), trigger a
full `recompute_mass_properties_from_colliders` so a body that becomes
dynamic gets correct mass. The recompute condition in `user_changes.rs`
gains `RigidBodyChanges::TYPE` (recompute when now non-fixed) alongside the
existing `LOCAL_MASS_PROPERTIES | COLLIDERS` triggers.

Determinism: both client and server share the fork, so `local_mprops` stays
bit-identical on both sides (default instead of computed) and state hashes
stay aligned. No cross-version snapshot compatibility is required (regions
park in memory per session).

This is also a production win: joins, server genesis, and per-voxel-edit
collider rebuilds all stop paying `from_voxels` for terrain.

## Change 3 — worldgen disk cache for `LocalServer`

The uncommitted `crates/server/src/worldgen_cache.rs` caches
`generate_region` output in `target/worldgen-cache/<key>/`, keyed by a crc32
over the seed, block manifest, and every file under `crates/worldgen/src` —
a cache hit is bit-identical to regeneration. The test rigs bypass it because
they run `LocalServer` (client crate), and `client` does not depend on
`server`.

Change: move the module to `crates/worldgen` (it caches that crate's output
and keys on its sources; `worldgen` already depends on `game` for the types).
The server keeps using it via the new path. `LocalServer`'s generation
closure (`crates/client/src/local_server.rs:56`) wraps `generate_region`
with the same load-or-generate-and-store logic. `cfg`-gated to a no-op on
`wasm32` (the browser offline path has no filesystem).

Effect on the suite: the cache persists in `target/`, so after the first
generation of the shared 3×3 window every subsequent test run — and every
other test in the same run — decodes bincode instead of generating.

## Out of scope

- Kernel micro-optimizations (`downsample2`, `Voxels::new`, worldgen inner
  loops): genuine computation; re-profile after these changes land.
- Test-side surgery (smaller worlds, fewer ticks, hash-mode changes): tests
  keep `Full` as the verification bar per standing directive.
- The `overload_grows_event_log_unbounded` regime and prediction-runaway fix:
  separate project (`client-prediction-runaway` memory).

## Verification

- Before/after wall-clock of `just test-full` (baseline: ~13.5 min at
  `--test-threads=4`; target: ~6–7 min).
- Correctness bar: `just test-full` green, including `log_model`,
  `hash_restore`, `rollback_restore`, `hash_mode`, and the convergence rigs
  (which assert client/server state-hash convergence every run).
- A worldgen-cache hit must produce a bit-identical region. This holds by
  construction (cached bytes are the exact bincode of the generated columns;
  decode failure is handled as a miss), and the move to `crates/worldgen`
  must keep a test that a store→load round-trip equals fresh generation for
  at least one region.
