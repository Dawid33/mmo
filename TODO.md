Direction (2026-10-20): Dwarf Fortress–style game on the multi-region engine, MVP = fortress
mode. No phased plan (rejected 2026-10-20): implement features piece by piece, roughly
modelling DF; reference research in docs/superpowers/specs/2026-10-20-dwarf-fortress-research/
(00-synthesis.md). The commander
camera (renderer/commander/) is the default view; live feel pass on this branch pending.
Fortress selection/designation UI landed 2026-10-29 (game::orders, commander level + designate
tools, cutaway, overlay, Slint tool bar; live check pending). Next step: workers/jobs that consume
designations (dwarves pathing to and digging/building them, replacing the F3 "Complete all
orders" stand-in, incl. top-down deconstruction scheduling); then stockpiles and zones.

Multi-region world follow-ups (milestone landed; spec 2026-07-05-multi-region-world):
- Cell mode removed 2026-10-30 (plans/2026-10-30-remove-cell-mode.md): whole regions + seams; a client never simulates more than its own window. Play regions are 512 m since 2026-10-30 (one-region window, single factor-16 LOD ring); live run at 512 pending.

- World map + Play follow-ups: embark (choosing a site/party, not just a tile); adventure-mode walking between tiles (the player's body crossing into the next tile's region); real biomes on the map tiles (today a summary of height/water/geology); `just clippy` is red on toolchain 1.99.1 in macros / bbmodel / slotmapd / block-mesh; the live check of the world map (Esc menu, M, tile panel, Play, camera landing, old region parking) is still pending.
- Chunk-16 follow-up (needs Dawid's go-ahead to measure): client brick landing per frame (a whole chunk is ~200-300 bricks/frame vs the cap of 160), client replay churn when a predicted break materialises ground (RemoveEntity + AddTerrainChunk re-spawns bricks per replay), and a solid chunk's first-edit full collider rebuild; then fix what the numbers show.
- `just pull-smoke` is red: since 9aed311 `prepare_chunk_quads` needs `OverlayTexture`, which only `FortressOverlayPlugin` creates; the probe doesn't add it (production unaffected). Fix: `VoxelPullPlugin` falls back to a 1x1x1 placeholder overlay texture.

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
