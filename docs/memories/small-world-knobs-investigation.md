---
name: small-world-knobs-investigation
description: "2026-09-19 — world-size knobs + join robustness IMPLEMENTED (develop c1c497a); perf data-collection handoff written (8bd9ac8); step 3 (pre-existing worlds) deferred; live iGPU run still unverified"
metadata: 
  node_type: memory
  type: project
  originSessionId: ab94b011-65cd-4a21-8156-09a0c14d3c11
  modified: 2026-09-19T13:17:44.645Z
---

Investigated 2026-09-19, then steps 1+2 IMPLEMENTED the same day (develop c1c497a): `VOXEL_WORLD=tiny|small|full` via `crates/client/src/world_scale.rs` + justfile `world=`, bench gates follow the scale, connect retry, paced RequestLod, pristine terrain_cache LRU, `LOD_BUILD_THREADS` env. Next work is the perf data-collection handoff at `docs/superpowers/plans/2026-09-19-client-performance-data-handoff.md` (8bd9ac8). Step 3 (pre-existing worlds) is deferred by the user. Unverified: client never launched after the change; new client unit tests compile-checked only. The findings below are the pre-change state.

**Findings**
- No env var/flag controls world size in client or server binaries. `GameInstanceManager` defaults to `near_radius = 1` + full `LOD_RING_TABLE` (instance.rs:252-253); `set_near_radius`/`set_lod_table` exist but only tests/harnesses call them (see [[headless-harness-world-size]]).
- Server is demand-driven by client subscriptions, so a client-side knob shrinks both sides. One join = 9 sim regions (9216 chunk entities) + ~570 `RequestLod` keys in one unthrottled burst; factor 2/4/16 keys on pristine regions each cost a full `generate_region` (~280) and land in `terrain_cache`, which is never evicted (world_manager.rs:138, :613).
- Chunk dim 32³ is baked into const generics, u64 mesher masks, 6-bit quad records and WGSL — not a knob. `REGION_SIZE` is assumed =1024 by lod.rs. Shrink the window, not the region/chunk.
- Render bench hard-requires 9 near regions + every LOD factor, stable for 120 *frames* inside a 120 s *wall-clock* `LOAD_TIMEOUT` (render_bench.rs:46-48) — unsatisfiable at very low fps; any size knob must feed these gates.
- Timeout candidates: `just run` starts client before server and netcode.rs:50 `.unwrap()`s a single connect attempt (quinn 30 s idle → `TimedOut` panic); server drops (not retries) any packet that can't send in 2 s (server/lib.rs:149-157); debug builds are over the client keep-up budget (see [[client-prediction-runaway]]).
- Persistence: parking lot is in-memory only; the only on-disk world data is `worldgen::cache` (columns, `target/worldgen-cache/<key>/`, debug-on/release-off, `WORLDGEN_CACHE=0|1`). No way to inject a `SerializedRegion` into `parked` today; blobs are unversioned bincode embedding full rapier state. `ensure_running` calls the generator even on a parked hit (world_manager.rs:453).

**How to apply:** don't re-run this sweep or re-plan the knobs; continue from the handoff doc. If step 3 is revived: `preload_parked` hook + fixture blobs first, disk-backed parking (needs version header + source fingerprint) last. Verify file:line refs before relying on them. See [[smoke-tests-not-full-suites]] and [[cap-build-test-parallelism]] for how to verify here.
