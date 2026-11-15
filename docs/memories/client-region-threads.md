---
name: client-region-threads
description: Client predicted regions run like the server's (crates/client/src/regions/, RegionHost inline|threaded) — IMPLEMENTED 2026-11-02, uncommitted; live A/B pending
metadata:
  type: project
---

IMPLEMENTED 2026-11-02 (uncommitted on develop; plan docs/superpowers/plans/2026-11-02-client-region-threads.md, ledger .superpowers/sdd/2026-11-02-client-region-threads/progress.md). Cause it addresses: client predicted the fortress 3×3 (9 regions at 256 m) serially on one thread ≈ 37–50 ms sim per 50 ms tick → rollback loop.
Design: GameInstanceManager only sends RegionCmd / routes RegionReport; RegionHost::Inline (harnesses, wasm, VOXEL_CLIENT_REGION_THREADS=0 — bit-identical to the old loop, verified by baseline A/B) or ::Threaded (native default, region-x,z threads). Shared clock (Tick broadcast, never waits).
Known pre-existing reds (baseline-confirmed byte-identical): crossing::walking_across_*, crossing::crossing_converges_under_late_region_scheduling, session_reset::second_session_starts_clean.
Not done: in-region amplifiers (one rollback per batch, hash check folded in, Tick coalescing), per-region clocks, backlog metric, fortress region's own 13–17 ms tick.

**Why:** user asked for the client "multithreaded like server" and chose option B.
**How to apply:** don't re-investigate; next is the live A/B (`PERF_LOG=1 just run` vs `VOXEL_CLIENT_REGION_THREADS=0`). Related: [[client-prediction-runaway]], [[region-threading-tests]].
