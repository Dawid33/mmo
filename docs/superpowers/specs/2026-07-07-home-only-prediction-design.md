# Home-Only Prediction (client reconciliation scope)

Date: 2026-07-07
Status: Design approved, pre-implementation

## Problem

The client runs away from the server under sustained play: client-vs-server
`drift` climbs without bound (observed 0 → 8300ms), the per-region prediction
backlog (`event_log`) grows, and rollback cost explodes, until the client can no
longer keep pace with the server's event stream.

Root cause, confirmed by instrumentation (see the `[client]` metrics line in
`crates/client/src/instance.rs::maybe_report_metrics`):

- `progress_world_one_tick` (`crates/game/src/lib.rs:165`) steps **every** loaded
  region each client tick. With a 3×3 near window that is 9 full physics sims per
  tick (~45ms measured, vs the server's ~5ms per region spread across one thread
  per region). That alone needs ~900ms of every wall-clock second.
- The client also **reconciles** the full 9-region server echo stream (~180
  events/s) on the same single thread, and reconcile is `O(event_log depth)` —
  it clones the whole `event_log` and replays `depth` ticks on a misprediction
  (`crates/game/src/region.rs:127`, `:178-180`).
- Combined, the single-threaded loop cannot ingest the stream in real time.
  Server packets pile up in the incoming channel (`recv_q` grew 282 → 4397
  monotonically), the client's view of the server tick lags, `drift`/`event_log`
  grow, each rollback gets deeper (`reconcile_ms` 0.04 → 129ms), fewer packets
  get pumped — a compounding death spiral.

Measured evidence (one run, moving):

```
recv_q:      282  →  1244  →  2233  →  3383  →  4397   (unpumped server packets)
evlog max:    12  →    81  →   108  →   170  →   216   (rollback depth, per region)
reconcile_ms: 0.04 → 46.6  →  92.6  →  94.6  →  129.0  (avg cost per server event)
tps act/tgt: 2.5/20 → 10.4/18 → 2.3/18 → 4.3/18 → 3.6/18
inbuf:        0/0 throughout                            (match logic is NOT the problem)
```

The server is healthy throughout (regions tick ~5ms, 0 overruns at steady
state); the bottleneck is entirely the client's per-tick workload.

## Decision

The client only needs rollback prediction for the region the local player acts
in — the **home** region. The other near regions are neighbors it merely needs
to display. So: **predict only the home region; simulate the neighbors purely
from authoritative server events (no local ticks, no prediction, no rollback).**

Dropping prediction + rollback for 8 of 9 regions removes the `O(depth)` replay
amplification for them: each neighbor event becomes a single bounded (~5ms) sim
step, self-paced to the server's arrival rate, so no per-neighbor backlog can
form. Only the home region keeps an `event_log`, and at ~20 predictions/s vs
~20 confirmations/s its depth stays shallow.

This needs **no protocol change, no new threads, and no render-pipeline change**
— neighbors still fully simulate, so their entities feed the existing
`GameDataUpdate → SimTarget → interpolate_transforms` render path unchanged.

An alternative (stop simulating neighbors entirely; drive them from periodic
server entity-state snapshots + interpolation) was considered. It removes even
the neighbor sim cost but requires a new protocol message and a client render
path for un-simulated entities. Rejected for now in favour of the smaller,
lower-risk change; revisit if neighbor sim cost proves too high.

## Design

### Two region modes

Every loaded region is either:

- **Predicted** — normally just the home region. Local tick clock advances it,
  `PlayerInput` is applied locally and logged to `event_log`, and server events
  are **reconciled** (rollback + replay). Exactly today's behaviour, scoped to
  one region.
- **Replicated** — every other near region. No local tick clock, no `event_log`,
  no reconcile. Each authoritative server `GameEvent` (`Tick`, `PlayerInput`,
  `EntityArrived`, `GhostUpdate`) is applied once via `handle_event` +
  `forget_last_event` — the same apply-and-forget policy the server's
  `RegionRunner` uses (`crates/game/src/region_runner.rs:106-122`). The region
  still fully simulates, so entity motion is authoritative and renders via the
  existing interpolation path. It self-paces to the server's send rate and can
  never accumulate a prediction backlog.

**Mode source of truth:** `GameInstanceManager` holds
`predicted_regions: BTreeSet<RegionCoords>`. Steady state it is `{home}`.
A region is reconciled iff it is in the set; otherwise it is applied-and-forgotten.

### Dispatch changes (all inside `GameInstanceManager`)

1. **Local tick advances only predicted regions.** `progress_world_one_tick`
   (`lib.rs:165`) currently ticks all regions; scope client tick generation to
   the regions in `predicted_regions` (add a home/subset-scoped variant rather
   than ticking the whole `World`). Neighbors get no locally-generated ticks.
2. **Server-event routing splits by mode** in `route_server_game_event`
   (`instance.rs`): if `event.region_id ∈ predicted_regions` → reconcile
   (today's path); else → `handle_event(kind)` + `forget_last_event()`.
3. **Clock/drift control keys off the home region only.** The `SyncClock`
   handler (`instance.rs:577`) currently retunes the single shared `tick_rate`
   from whichever region's SyncClock arrived last (9 regions fighting over one
   clock). Restrict tick-rate tuning and the `drift` metric to the home region;
   ignore replicated regions' SyncClocks for pacing.
4. **Input is unchanged** — already routes to `home` and is applied+logged there.
5. **Predicted transfers narrow to the local player.** `apply_local_transfers`
   (`instance.rs:272`) keeps only the local-player predicted crossing (needed
   for responsiveness during the flip); it no longer injects predicted
   `EntityArrived`/`GhostUpdate` into replicated neighbors — their authoritative
   `EntityArrived` from the server populates them.

### Boundary crossing (promotion / demotion)

A crossing transfers the predicted role from source region A to target B. A
predicted flip is itself a prediction that can be wrong, and replicated regions
cannot roll back, so A must stay predicted until the server confirms the flip.

- **On the predicted flip** (local player departs A→B, `apply_local_transfers`):
  set `home = B` and **promote B** — insert into `predicted_regions`, begin
  local ticks + reconcile for it. B's reconcile baseline is its current
  authoritative `next_game_event_id` (exactly how `Region::new` initializes a
  freshly-loaded region). **A stays predicted** through the RTT window, so both
  source and target are predicted+reconciled while the handoff resolves — the
  existing ghost/orphan machinery is unchanged. The transient two-predicted-
  region state lasts only ~RTT.
- **On the authoritative flip** (server `PlayerRegion(B, client)`,
  `instance.rs:612`): **demote every predicted region that is not the confirmed
  home** (normally just A) — roll its `event_log` back to the authoritative
  baseline with the existing exact-inverse rollback (pop + `data.rollback()` per
  event), clear the log, remove it from `predicted_regions`. By then the server
  has confirmed the player left A, so A's authoritative stream matches the
  rolled-back state — no double-render, no divergence. Thereafter A is
  replicated and its event ids stop mattering.
- A **mispredicted flip** (server says home is C, not B) falls out of the same
  rule: demote B, promote C.
- **Orphan inputs are unchanged** — inputs predicted in B during the window
  resolve via `evict_orphan_local_inputs` inside B's reconcile when the
  authoritative local `EntityArrived` lands (`region.rs:225`); that logic lives
  in the reconcile path B now runs.

The only genuinely new code is the promote/demote transitions and the
`predicted_regions` set. Rollback, reconcile, arrival detection, and orphan
eviction are all reused as-is.

### Deliberately unchanged

- **Protocol** — no new packets.
- **Render bridge + interpolation** — replicated regions still fully simulate
  and emit the same `GameDataUpdate`s; no visual pipeline change.
- **Threading / wasm** — still the single-threaded `GameInstanceManager`; the
  wasm `pump` path rides the same routing, so wasm is unaffected.
- **Reconcile/rollback machinery** (`region.rs`) — reused verbatim, scoped to
  `predicted_regions`.
- **Far ring, snapshots, subscriptions** — untouched.

## Verification

**Empirical (primary success criterion), via the metrics already added** — run
server + 1 client, move and cross boundaries ~30s, and confirm on the `[client]`
line: `recv_q` stays flat (not monotonic), `drift` stabilizes near target,
`evlog` sum/max stays small (only the home lead), `reconcile_ms` stays low,
`mispredicts/s` low. This directly proves the runaway is gone.

**Unit tests** (`cargo test -p client`, headless via `BevyInputHarness` /
`SimHarness`):

- A replicated region applies a server `Tick` with its `event_log` staying empty
  (`event_log_len == 0`).
- Only predicted regions advance on a local tick; a replicated region does not
  advance without a server event.
- A predicted flip adds the target to `predicted_regions`.
- An authoritative `PlayerRegion` demotes the old source; its `event_log` rolls
  back to empty and its state equals the authoritative baseline.

**Determinism/rollback suite** (`cargo test -p game`) must stay green — the
invariants are untouched, only which regions exercise them changes.

**Success bar:** movement stays responsive, neighbor entities render smoothly,
boundary crossings work with no rubber-band or double-render, and
`recv_q`/`drift`/`evlog` stay bounded under sustained movement.

## Notes / follow-ups

- `INDUCED_LATENCY` is currently `0` (`crates/game/src/lib.rs:45`), giving the
  home region ~one-way latency of lead. Out of scope here; revisit tuning after
  the scope fix lands, when the home region is the only clock being controlled.
- The temporary metrics instrumentation (tick/reconcile timing, backlog gauges,
  `recv_q`) added during diagnosis is useful for validating this change; decide
  whether to keep or trim it once verified.
