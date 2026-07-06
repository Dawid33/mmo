Multi-region world follow-ups (milestone landed; spec 2026-07-05-multi-region-world):

- Client per-region clock tracking (single shared tick_rate today; 9 SyncClock streams fight over it).
- Durable region persistence (parking lot is in-memory only).
- Cross-region event relay groundwork (deferred with handoff).
- Stable client identity across connections: reconnects get a fresh ClientId, so the manager's no-duplicate-player reconnect path only fires for same-id sessions; the old player entity lingers in its home region.
- Cross-region interactions beyond collision (combat, pickup) — future spec.
- Ghost mirrors for parked-region persistence are TTL'd, not persisted.
- Seamless local-player crossing under latency — the local client's own boundary crossing
  converges bit-exact but has a transient rubber-band under active input (spec-accepted
  flip-tick divergence). Proper fix needs a protocol-level design (server-side input
  routing / predicted-authority-transfer); see docs/superpowers/2026-07-06-seamless-local-crossing-followup.md.
  - Now has a deterministic headless repro: `crates/client/tests/crossing.rs::walking_across_a_region_boundary_keeps_input_and_converges`
    (intentionally RED). The SimHarness bit-exact convergence check flags region (1,0) @tick 33
    diverging when input resumes right after the crossing. NOTE: the harness reports it as not
    self-healing within the test, sharper than the "transient" wording above — confirm whether
    it's the same spec-accepted transient case or a worse non-healing variant when the
    protocol-level fix is designed. Fix lives in the sim (apply_arrival / apply_local_transfers /
    handoff reconcile), not the harness.
