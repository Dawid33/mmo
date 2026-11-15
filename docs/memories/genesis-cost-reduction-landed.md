---
name: genesis-cost-reduction-landed
description: "IMPLEMENTED 2026-07-12 (develop 34ec12b..760c32f): mass-props skip + unlogged genesis ECS writes + worldgen cache in LocalServer; test-full green 14m39s@3thr; don't re-plan. Slow-test rigs peak ~6.7 GB RSS each — test_threads must stay ~3 on 31 GB"
metadata: 
  node_type: memory
  type: project
  originSessionId: bf7e15a9-15c6-4bef-a03b-2edfd151b5ec
---

The genesis-cost-reduction plan (docs/superpowers/plans/2026-07-11-genesis-cost-reduction.md)
is fully landed on develop, 089d4a6..760c32f:

- 34ec12b — fork skips collider mass-props for Fixed bodies (attach_collider +
  user_changes recompute; `RigidBodyChanges::TYPE` added to the recompute
  trigger for the colliders-removed-then-dynamic corner). Resolves
  [[first-tick-massprops-spike]].
- 6d4ddf9 — `seed_terrain_genesis` ECS writes are raw/unlogged
  (`UndoSlotMap::insert_raw` + `Undo<Ecs>::create_entity_raw`), matching the
  physics inserts' base-state precedent. Killed the quadratic Full-mode
  whole-field digests during genesis.
- 7905935 — WorldgenCache moved server→`worldgen::cache`, `generate_cached`
  wrapper, LocalServer wired in, runtime-disabled on wasm32; worldgen tests
  added to `just test`/`test-full`.
- c301b40/760c32f — test_threads 4→3 + comment syncs.

Measured: diag_a_lockstep 186s→98s; offline nine-regions 94.6s→27.6s;
test-full 50/50 green, 14m39s wall at 3 threads.

**Key operational fact:** each slow client rig (reconcile_keepup/crossing/
harness_smoke — LocalServer + client 3×3 windows) peaks at **~6.7 GB RSS**
(measured /usr/bin/time -v, identical pre- and post-plan; the old "2.4 GB"
justfile note was always wrong). 4 threads OOMed the 31 GB box once faster
rigs stopped staggering — that is why `test_threads := "3"`. Raise only with
more RAM; an OOM SIGKILL in test-full looks like "signal: 9" on the test
binary.

**Why:** prevents re-planning landed work and re-diagnosing the OOM.
**How to apply:** future test-suite perf hunting should re-profile first
(remaining hot buckets after this plan: `ColliderBuilder::voxels`/`Voxels::new`
~14%, worldgen ~13% on cold cache, `game::lod::downsample2` ~10%, plus
reconcile_keepup's residual ~950s CPU). Also note cache-hit decode saved
little wall in dev profile — bincode decode of ~19 MB regions costs near
generation at opt-level 1.
