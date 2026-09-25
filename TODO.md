Multi-region world follow-ups (milestone landed; spec 2026-07-05-multi-region-world):
- Island regions + cell loading (spec 2026-09-25-island-regions-cell-loading-design): phase 1 + 1b DONE (cell mode runs live behind `just region_chunks=128 cells=16 world=tiny run`); limits: world=tiny only, region crossings unsupported in cell mode (client lacks the target snapshot at the flip), cell regions never reclaimed; phase 1c (centre spawn, home-region LOD masked under active cells) and phase 2 (large regions: lazy per-cell worldgen, sizes up to 32768 chunks in cell mode) DONE; LOD for large regions is a placeholder (user to revisit); spec phase 2 (cheaper activation) PARTLY DONE: persistent `GameData.chunk_index` replaces the whole-region chunk scans (activation, release, cell hash, every block edit) and the terrain fingerprint is incremental (`GameData.cell_edits`); OPEN: genesis-only timing (`cell_profile --time`, user runs benchmarks), all-water collapse deferred to phases 3/6 (no water in the current world; water is solid in the sim), compact `Voxels` waits on the timing; phase 3 island worldgen DONE (quadtree islands in open sea, spawn island + dry coastal spawn via IslandDirectory, `just island=<cells>`); deferred: island-sized regions keyed by IslandId (with phase 6), ridge wavelength scaling, all-water chunk collapse (ocean cells now exist — measure with `cell_profile --centre`); next: phase 4 wire (zstd, framed snapshot, catch-up).

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
  - **REGRESSED — FAILING as of 2026-09-20** (was PASSING): `crates/client/tests/crossing.rs::walking_across_a_region_boundary_keeps_input_and_converges`.
    Red since 8e90481 (client tick lead), which no run of this slow client test covered. Two layers:
    (1) harness — `assert_converged` assumed the client TRAILS the server and panicked "no matching
    server-authoritative tick"; FIXED: `SimHarness::catch_up` freezes the client, ticks the server up
    to it with `SyncClock` withheld, compares live state at the same tick, then restores the lead.
    (2) real, OPEN — under that strict check the pre-crossing baseline matches on all 9 regions for
    40 ticks, and after the crossing 8 of 9 still match (old home with its ghost mirror, fresh
    neighbours), but the NEW HOME region is bit-inexact on every one of 40 idle ticks: permanent,
    not the ~1-tick flip transient described below. Suspects: the 1.5-tick lead putting the client's
    predicted arrival at a different tick than the server's with state the correction path never
    fixes (`apply_arrival`'s replay branch only corrects the pose — not `Motion`, yaw/pitch,
    `Client` input state; `cross_boundary` flies, so `Motion`/`last_jump_tap` are live), or
    `align_region` stepping the flipped home. Repro: `just test-one client "--test crossing walking_across"`.
    Next step: diff the two `GameData`s field by field at the aligned tick (hash per `#[rollback]` field).
  - Previous status, kept for the root-cause notes: deterministic headless coverage in the same test. Root-caused via SimHarness: the flip-tick divergence at the just-crossed home
    region is genuinely TRANSIENT — it reconciles bit-exact within ~1 tick once input stops
    (measured: diverged at the flip tick, converged the next tick, stable 240+ ticks after).
    The earlier "not self-healing" reading was the harness asserting one tick too early
    (`assert_converged` now lets the documented bounded transient heal before its strict check).
    STILL DEFERRED (this ticket): input held INDEFINITELY across a seam oscillates the home
    region and never settles in the tight lockstep — the real protocol-level case above. Needs
    the server-side input stamp-routing / predicted-authority-transfer design, not a harness or
    client-side patch.
  - Validation tool for that design, when it lands: `game::InlineSchedule` (seeded region-scheduling
    jitter on the `InlineSpawner`; `SimHarness::set_schedule`). Lockstep is the one schedule the
    threaded server never runs, so judge a candidate protocol by `crossing.rs` convergence across
    `InlineSchedule::late` seeds, not by the lockstep run alone. `jittery` (missed ticks) needs the
    client per-region clock tracking item above first — the harness aligns client and server by tick.

In-engine UI (spec docs/superpowers/specs/2026-07-07-in-engine-ui-design.md,
plan docs/superpowers/plans/2026-07-07-in-engine-ui.md): inventory/hotbar landed
(engine-owned data → client-side widget projection → bevy_ui). Follow-ups: real
item icons/textures, drag-drop UX, additional screens, and the deferred diegetic
world-space panel case (needs in-engine taffy + deterministic text measurement).
