---
name: region-threading-tests
description: "State of sim multithreading (per-region OS threads, nothing parallel inside a region) and the seeded-schedule + real-thread crossing tests added 2026-09-20"
metadata: 
  node_type: memory
  type: project
  originSessionId: e97ac6de-a2f0-4a07-9c1a-1901dcb04a72
  modified: 2026-09-20T18:49:16.276Z
---

INVESTIGATED + IMPLEMENTED 2026-09-20 (uncommitted on develop); don't re-investigate.

**State:** the threading unit is the REGION, never the chunk — one OS thread per running region (`crates/server/src/region_threads.rs`), manager routes over crossbeam. Nothing inside a region is parallel (dead optional `rayon` dep removed from `crates/game/Cargo.toml`; rapier `parallel` stays off — incompatible with rollback, see [[veloren-threading-research]]). Client sim + wasm `LocalServer` are single-threaded `InlineSpawner`.

**Key insight:** `InlineSpawner` lockstep is ONE schedule (all regions tick once per round, every output lands before the neighbour's next tick) — the one schedule the threaded server never runs. A constant phase offset is equivalent to lockstep; only JITTER matters.

**Added:**
- `game::InlineSchedule` (`world_manager.rs`): seeded jitter on the inline spawner — `shuffled` (tick order), `late` (a region's input queue untouched until after its next tick), `jittery` (+ missed ticks, like `region_thread_loop`'s overrun skip). `None` = unchanged behaviour. Hooks: `InlineSpawner::with_schedule/set_schedule`, `LocalServer::set_schedule`, `SimHarness::set_schedule`.
- `crates/game/tests/handoff_schedule.rs`: walk across a seam AND diagonally through a corner, there and back, 12 seeds × 3 modes, ownership invariant every round, per-seed reproducibility, non-vacuity guard. All pass.
- `crates/server/tests/threaded_world.rs`: walk test now crosses GROUNDED (seam-spanning floor) and asserts the arrived bundle's state; new corner-crossing and cold-region wake-and-return (production `ThreadRegionSpawner::with_generator` path) tests. 4× repeat, no flakes.
- `just test-threads` (~12 s warm) runs both layers; `just smoke` now type-checks server integration tests (a new `ClientPacket::GameEventAfterTick` variant had silently broken `webtransport_handshake.rs` — fixed).
- `crates/client/tests/crossing.rs::crossing_converges_under_late_region_scheduling` (slow client opt-in) — currently FAILS, see below.
- `SimHarness::catch_up` (harness.rs): `assert_converged` assumed the client trails the server; since 8e90481 the client LEADS (~1.5 ticks). Now: freeze client, tick server up to it with `SyncClock` withheld, compare live state at the same tick, then let the client re-predict to restore its lead. Recorded first-prediction hashes do NOT work (per-tick ghost updates rewrite every predicted tick).

**OPEN BUG found 2026-09-20 (not fixed, user's netcode domain):** `crossing.rs::walking_across_a_region_boundary_keeps_input_and_converges` (lockstep, NOT ignored) is red since 8e90481. Under the strict check: baseline 9/9 regions bit-exact for 40 ticks; after a crossing 8/9 match but the NEW HOME region is bit-inexact on all 40 idle ticks (permanent, not the ~1-tick flip transient). Suspects: predicted vs server arrival tick under the lead + `apply_arrival` replay branch correcting pose only (not Motion/yaw/Client), or `align_region` on the flipped home. Details in TODO.md "REGRESSED". Next step: per-field hash diff of the two GameDatas at the aligned tick.

**Why:** the only real-thread walk test asserted events not state, crossed while falling, and was `#[ignore]`d out of every routine check.

**How to apply:** scheduling-sensitive correctness goes in the seeded layer (replayable seed), real threads only for plumbing. `SimHarness` can only use `late`/`shuffled` — `jittery` desyncs region tick counts from the client, which needs the TODO "client per-region clock tracking" first. Use the `late` seed sweep to judge any future fix for the held-input seam oscillation ([[input-slot-rubber-banding]], TODO.md).
