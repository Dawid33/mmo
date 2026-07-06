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
  - Deterministic headless coverage: `crates/client/tests/crossing.rs::walking_across_a_region_boundary_keeps_input_and_converges`
    (PASSING). Root-caused via SimHarness: the flip-tick divergence at the just-crossed home
    region is genuinely TRANSIENT — it reconciles bit-exact within ~1 tick once input stops
    (measured: diverged at the flip tick, converged the next tick, stable 240+ ticks after).
    The earlier "not self-healing" reading was the harness asserting one tick too early
    (`assert_converged` now lets the documented bounded transient heal before its strict check).
    STILL DEFERRED (this ticket): input held INDEFINITELY across a seam oscillates the home
    region and never settles in the tight lockstep — the real protocol-level case above. Needs
    the server-side input stamp-routing / predicted-authority-transfer design, not a harness or
    client-side patch.
