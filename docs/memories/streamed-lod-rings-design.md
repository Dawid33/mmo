---
name: streamed-lod-rings-design
description: "IMPLEMENTED (2026-07-11, develop 90c3fd8..781aa70) — all LOD levels (2/4/16/64/256) server-streamed; FarSnapshot wire path deleted; live GUI smoke pending; don't re-plan or re-implement"
metadata: 
  node_type: memory
  type: project
  originSessionId: 69597bda-3b30-46ff-a10f-6ab6aef8f1f5
---

IMPLEMENTED 2026-07-11 (develop, plan `docs/superpowers/plans/2026-07-10-streamed-lod-rings.md`,
all 12 tasks; final commits c389243 lod_rings renderer + 781aa70 factor-2
fold-in). The client-side worldgen LOD rings and the FarSnapshot far ring are
gone; one server-streamed pipeline serves factors 2/4/16 (per-region keys)
and 64/256 (per-tile keys) with ring table `[(2,2),(4,4),(8,16),(16,64),(32,256)]`.

Landed shape:
- `game::lod` (LodKey/downsamplers/PackedLodGrid RLE), `game::lod_builder`
  (InlineLodBuilder + server ThreadLodBuilder in `server/src/lod_workers.rs`),
  WorldManager cache/subscriptions/invalidation, `worldgen::coarse_tile`.
- Client: `instance.rs` `update_lod_window` diffs RequestLod/ReleaseLod;
  renderer `lod_rings.rs` decodes LodData into per-key roots and retires old
  roots only when a replacement is visibly meshed (or a frame deadline).
- Renames: FarSnapshot→TerrainSnapshot, RequestFarSnapshot→RequestTerrain,
  FarSnapshotChanged→TerrainChanged, far_cache→terrain_cache,
  FAR_SNAPSHOT_MIN_INTERVAL_TICKS→TERRAIN_HASH_INTERVAL_TICKS.

**Gotchas discovered:**
- The full ring table costs a headless client ~300 keys; through
  `InlineLodBuilder` (LocalServer/wasm) each is a synchronous full-region
  build (~70s+/test in debug). `GameInstanceManager.set_lod_table` exists as
  the hook; `SimHarness` defaults to an EMPTY table — LOD tests opt in via
  `SimHarness::with_lod_table(&[(2, 2)])`. The same synchronous join burst
  will hit the wasm `?offline` path at the production table — unresolved.
- End-to-end factor-2 coverage: `streamed_factor2_lod_arrives_for_ring_regions`
  in `crates/client/tests/harness_smoke.rs`.

**Pending:** live GUI smoke (fly to horizon: ring transitions, no holes;
edit a block, walk 3+ regions away, see it in the factor-4 ring after the
cadence interval).

**How to apply:** don't re-plan or re-implement; extend from the landed
modules. Supersedes the client-side parts of
[[voxel-rendering-phase1-landed]]'s LOD-ring description.
