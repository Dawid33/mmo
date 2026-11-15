---
name: client-prediction-runaway
description: Client reconciles forever after input — LIVE-CONFIRMED root cause (2026-07-08): client joins ~13 events BEHIND the server head and never gets ahead, so every input echo is displaced 13-60+ slots; each displaced echo/tick costs a FULL event-log re-simulation (~8-9ms/event); log depth diverges. NOT home disagreement, NOT orphans (both refuted by live log). Home-only prediction also wrong fix.
metadata:
  node_type: memory
  type: project
  originSessionId: 5191e9d6-591a-4df5-8035-13db82287c56
---

Symptom: client reconciles from the first player input and never converges;
event_log grows ("predicts very far"), reconcile passes take ~1s, drift climbs.

LIVE LOG CONFIRMATION (2026-07-08, user-captured run with the reconcile
tracing): the mechanism is the **displaced-input chase × O(L) replay**:

- At join, the client starts predicting from snapshot base (id 1) while the
  server head is already ~16 (region-genesis tick stalls of ~370ms + snapshot
  transfer). First input: predicted id=3, server slots it at id=16 —
  displacement 13. The client NEVER gets ahead of the server head.
- Each pending input sits at the event_log front and mispredicts against
  EVERY incoming server Tick: rollback + full re-simulation of the whole
  unconfirmed log (~8-9ms per replayed event — a real physics step), then
  shift +1. The dx=347 input took 13 full replays over 6.4s to confirm.
- Confirmations drain ~1/pass while predictions are produced at ~20/s → log
  depth L diverges (7→108 in 12s), passes grow 23ms→919ms, displacement of
  later inputs grows (63 slots) — compounding death spiral.
- **Drift metric lies**: SyncClock uses the PREDICTED tick, which the bug
  inflates, so drift read +3250ms ("ahead") and the pacing loop SLOWED the
  client (tick_rate 55ms) while the confirmed position fell seconds behind.
- REFUTED as live triggers: home disagreement (server "route to home" matched
  client "predicted in home" all session, incl. a simultaneous (0,0)→(0,-1)
  flip), input drops, orphans, content mismatch (echoes matched byte-exact).

Latent but real (headless repro in crates/client/tests/diag_reconcile.rs):
- Orphaned predictions never expire: one unconfirmable prediction ⇒ permanent
  full-log replay per server event (scenario E); a one-sided crossing would
  mass-produce them (scenario G). Not what fired live, but no defense exists.
- `forget()` always drops the OLDEST transaction; `with_data`
  (new_transaction+forget) while predictions pend shears the undo/id ledger ⇒
  permanent OUT-OF-ORDER wedge (caught by new ID-DESYNC invariant).

Fix directions (agreed priority): (1) join/pacing fix — start the client
timeline at server head + induced latency and pace by CONFIRMED position, not
predicted tick, so displacement ≈ 0-2 and L stays at the latency window;
(2) cap L with resnapshot fallback; (3) orphan eviction + forget/with_data
hardening as robustness work. Overload itself is an ACCEPTED failure mode:
the fix is optimizing the game/client, not reconcile logic.

Regression gates now in place (2026-07-08):
- `crates/client/tests/reconcile_keepup.rs` — permanent lockstep suite:
  convergence scenarios, keeps_up_under_moderate_input,
  overload_grows_event_log_unbounded (documents the failure signature),
  transient_stall_recovers, and `soak_realtime_keepup` (#[ignore], `just
  soak`, release: real cadence + 60/s input; release keeps up at steady
  depth 2).
- `crates/client/tests/keepup_budget.rs` (#[ignore], `just keepup-budget`,
  release): measures region tick cost, asserts the spiral inequality with a
  RATCHET factor `KEEPUP_SAFETY_FACTOR = 0.55` (baseline 2026-07-08: tick
  4.3ms → worst-case headroom 0.61x — raise the ratchet toward >= 1.0 as
  optimization lands).
- Runtime tripwire in `instance.rs::maybe_report_metrics`: warns when the
  home prediction-log depth rises across 3 metric windows (>= 24) or hits 64;
  metrics line now includes `evlog=`.
- `INDUCED_LATENCY` renamed `TEST_INDUCED_LATENCY_MS` — deliberate testing
  knob, 0 in normal operation, NOT a mistake.

Instrumentation in working tree (uncommitted): region.rs reconcile tracing
(RECV/COMPARE/MATCH+FORGET/MISPREDICT/OUT-OF-ORDER/DONE, ID-DESYNC,
REPLAY-ID-SKEW, INPUT-VS-INPUT diff), instance.rs prediction-send log +
world_mut() hook, server world_manager routing/echo logs, diag test.
`FAR_VIEW_RADIUS=0` is a user debugging change (its 2 test failures are fixed
as of 2026-07-08 — suite is green).
Latent: server RegionRunner never pops event_log (leak).

PERF-BUDGET ATTRIBUTION (2026-07-08, `[client-budget]` line in
instance.rs::maybe_report_metrics + `Region::take_tick_perf`): first measured
breakdown, the input to the next optimization task:
- Physics controller pass is ~99.8% of ALL sim time (ctrl ≈ 2.3s of each 5s
  release soak window; input step ~0.2ms and scan_boundaries ~3ms per window
  are noise). Optimizing anything but rapier's per-region step is pointless.
- Per-region tick execution ≈ 9.4-9.8ms in the live 9-region window (soak),
  ~2x the isolated keepup-budget number (4.8ms) — cache/window effects.
- FIRST tick of a freshly loaded region is a massive outlier: ~318ms/region
  release (2.6s for the 9-region join tick), ~911ms/region debug — physics
  warm-up. This spike alone puts a joining client ~50 ticks behind and likely
  seeds the join-behind displacement above.
- keepup-budget ratchet sits at exactly 0.55x on an idle machine and FAILS
  under any concurrent load — user-confirmed: client does not keep up,
  optimization is the current direction.

Related: [[debugging-follow-user-evidence]], [[testing-one-client]],
[[collider-perf-followup]].
