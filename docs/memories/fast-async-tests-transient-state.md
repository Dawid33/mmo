---
name: fast-async-tests-transient-state
description: "Headless Bevy pipeline tests must assert end-states, not transient scheduling states — the fast mesher completes spawn→apply within one update nondeterministically"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: eea0547e-6e65-4961-acd6-eb570d849702
---

Three meshing-queue tests flaked 50-100% under `--include-ignored` load by asserting transient states (`MeshingTask` present, entity still in `RemeshQueue.dirty`) one `app.update()` after triggering work. With chained `spawn_mesh_tasks → apply_meshed_chunks` systems and a ~µs mesher, a task can spawn, complete, AND apply within a single update — the transient is legitimately gone. Fixed in develop (b45c34f, bf6f2e3, a751b2e): assert guaranteed outcomes (task-or-mesh present, new mesh handle landed, wall geometry re-grown) and use deadline-based `wait_until` helpers (queue.rs tests) instead of fixed-iteration polling.

**Why:** the first diagnosis ("load starvation, raise the budget") was wrong and only masked it; the real cause was asserting scheduling intermediates.
**How to apply:** when writing headless async-pipeline tests, ask "can this state be skipped entirely on a fast machine?" — assert the end-state the feature exists for; reuse `wait_until`/`wait_until_settled` in `meshing/queue.rs` tests. Related: [[subagent-derails-on-bg-notification]].
