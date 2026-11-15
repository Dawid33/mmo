---
name: region-size-dial
description: "INVESTIGATED + IMPLEMENTED 2026-09-20/22 (uncommitted): build-time `just region_chunks=64|128` (VOXEL_REGION_CHUNKS, legal 32|64|128); per-region cost ∝ area (×3.6–4.0 measured); real win needs a distance-based near window (not built); don't re-investigate"
metadata: 
  node_type: memory
  type: project
  originSessionId: 429c107e-acbf-40d4-a9fe-0b42f2992c62
  modified: 2026-09-20T19:10:57.837Z
---

Region size investigation, 2026-09-20. Report: `docs/superpowers/specs/2026-09-20-region-size-investigation.md`.

**Facts:** a region is 1024 units = only 64 m (`UNITS_PER_METER` = 16); sprint-fly crosses one in ~3 s. The region edge is NOT baked into packed records/shaders/wire types (only the 32³ chunk edge is) — the old CLAUDE.md claim was too strong. What pinned it: `CULL_BFS_MAX_CHEBYSHEV = 64` (exactly enough for 3×3 at 32 chunks, `Node::pos` u8), `LOD_RING_TABLE` bounds in regions, the LOD tile contract (`32*factor == span*REGION_SIZE`, region = whole grids at factor 32 → legal set {32, 64}; 16 and 128 blocked, 128 also by the 256 MiB packet cap), `PRISTINE_TERRAIN_CACHE_MAX`, test fixtures.

**128 added 2026-09-22 (user: "double region size again"):** the sweep's "LOD tiling blocks 128" was WRONG — `tile_regions` truncation correctly makes factor 64 per-region (2×2 grids, like 16/32 at default) and the builder branches on `region_span()==1`; real blockers were the packet cap (now 256 MiB × area ratio, ≤2 GiB) and fixtures. Ring table at 128: `[(2,8),(3,16),(6,32),(8,64)]`. Measured 128: 49 982 chunks/region, start 3.9 s (9 regions 5.7–7.4 s, 6.9 GB RSS), snapshot 578 MiB raw / 33.5 MiB wire, decode 736 ms. Same test set green at 128. 128 = practical ceiling; only sane with the distance window.

**Dial:** `game::REGION_CHUNKS` = `option_env!("VOXEL_REGION_CHUNKS")` parsed by a const fn, const-asserted; default in `.cargo/config.toml` `[env]` so raw cargo / rust-analyzer / `just` share one fingerprint (unset vs "32" would rebuild `game` on every alternation); `just region_chunks=64 …`. Ring table at 64: `[(2,4),(3,8),(6,16),(13,32),(16,64)]` (same px budget + horizon, ~1080 keys). No connect handshake → mismatched client/server is not refused.

**Measured (release, 130 W adapter):** 32 → 64 chunks: region start 368 → 1309 ms, 9-region server RSS 471 → 1837 MB, snapshot 40.3 → 161.3 MiB (wire 1.15 → 5.46 MiB), decode 44 → 183 ms, 2671 → 10527 chunks. Same land starts ~3× slower as one big region than four small ones in parallel. Verified at 64: handoff suites, seeded-schedule walks, real-thread `threaded_world`, lod_streaming, worldgen lib, client type-check. NOT run: client on a GPU, client test binaries, wasm.

**Why:** user wants bigger regions for the thread-per-region architecture and smoother fast traversal. Finding: threads per isolated player stay 9 at any size (window is `(2r+1)²` regions); sim-critical lead margin improves (~2.4 → ~4.0 s at sprint-fly) and flips halve, but visible fill gets relatively worse (load ×4, lead ×2).

**How to apply:** the recommended next step is a DISTANCE-based near window (regions within D=1024 units → always exactly 4 regions at 2048-unit regions: 4 threads/~42k chunks vs 9/~95k) — a subscription-model design change, not built; needs a `(1,2)` ring entry + cull bound following the window. First thing to watch when running 64: server tick overruns from the every-25-ticks `terrain_fingerprint` (∝ chunks). Never write the region edge as a literal. Experiment builds used `CARGO_TARGET_DIR=target/region64` (8.4 GB, deletable). Related: [[small-world-knobs-investigation]], [[multi-instance-k8s-postgres-research]], [[region-threading-tests]], [[perf-capture-tooling-landed]].
