---
name: veloren-threading-research
description: "Veloren multi-threading deep research DONE 2026-07-10 — report in docs/superpowers/specs/; don't re-research"
metadata: 
  node_type: memory
  type: project
  originSessionId: fbbb934d-016c-45f1-8319-7a16d757a51b
---

Deep research (source clone + verified web pass) on Veloren's threading completed
2026-07-10; report at `docs/superpowers/specs/2026-07-10-veloren-multithreading-research.md`.

Key conclusions: Veloren has ONE shared-world sim parallelized via specs+rayon
`par_join` — incompatible with our rollback determinism (undo-log ordering); their
regions are interest-management only, so our region-per-thread design has no analogue
there and is ahead. Portable ideas: separate low-priority background pool with
string-named per-category thread caps (SlowJobPool), OS thread priorities (realtime
tick threads / Batch workers via `thread_priority`), cooperative `Arc<AtomicBool>`
cancellation inside worldgen, budgeted apply with fractional carry
(`dt * CHUNKS_PER_SECOND + overflow`), per-tick-phase CPU metrics; their measured
bottleneck was per-client sync fan-out (parallelize at our netcode edge, outside the
sim). Async results must enter GameData as tick-stamped logged RegionInput events,
never at arrival time. Related: [[client-prediction-runaway]],
[[first-tick-massprops-spike]], [[rendering-next-steps-research]].
