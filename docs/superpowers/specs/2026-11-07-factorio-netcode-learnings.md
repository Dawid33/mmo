# Factorio learnings: cheap ticks and lockstep networking

Date: 2026-11-07
Status: research, nothing implemented

**Question:** what can we take from Factorio to keep tick cost low and the networking robust?

**Sources:** Factorio Friday Facts (FFF), wiki.factorio.com, lua-api.factorio.com and Wube's
`server-settings.example.json`. Every claim below carries its source. Forum posts could not be fetched (HTTP 403);
claims from them are marked *(forum snippet)*.

## 0. How close is Factorio's design to ours?

The designs are close, but not identical. The difference decides most of what transfers.

| | Factorio | This engine |
|---|---|---|
| Simulation | Deterministic. Every peer simulates the **whole** map. | Deterministic. Server regions are authoritative; each client simulates its subscribed regions (home + `fortress_radius`, up to 3×3). |
| Wire | Input actions only. The server merges each tick's actions into one "tick closure" and relays it to everyone ([FFF #147](https://factorio.com/blog/post/fff-147), [#302](https://factorio.com/blog/post/fff-302)). | Tick-stamped inputs up; per-region event streams, halo, transfers and LOD down. |
| Tick rate | 60 UPS, a 16.67 ms budget ([wiki: Time](https://wiki.factorio.com/Time)). | 20 Hz (`TICK_RATE` = 50 ms). |
| Overload | The **game slows down**; ticks are never skipped ([wiki: Time](https://wiki.factorio.com/Time)). | Shared `WorldClock`; a late region catches up in bursts (`MAX_CATCH_UP_BURST`), never skipping a tick number. |
| Prediction | A **latency state**: a small, non-authoritative copy of the predicted subset. It is reset from the confirmed state every tick, then the unconfirmed local actions are replayed onto it ([FFF #83](https://factorio.com/blog/post/fff-83), [#302](https://factorio.com/blog/post/fff-302)). The real game state is **never rolled back**. | Full-region rollback. The undo log reverts the whole region (physics journal included), then predicted ticks are replayed. Cost is depth × a full region tick. |
| Not predicted | Shooting, combat, most entity interaction, and car/train driving (until 2.0's car prediction) ([FFF #83](https://factorio.com/blog/post/fff-83), [wiki](https://wiki.factorio.com/Multiplayer)). | Everything in a predicted region. |
| Desync check | A CRC per crcTick; on mismatch the client re-downloads the map ([2016 log](https://gist.github.com/johnathanludwig/680347fa5fa19a3a848b1791475fc6e7)). | A whole-state hash every 100 ticks. A mismatch is logged; there is no recovery. |

**Why Factorio affords 60 UPS:** the authoritative state on a client only moves forward, once per confirmed tick, so a tick
is paid for once. Our client pays for a tick again on every misprediction replay. That makes our tick cost the product
of per-tick cost and replay depth, which is why 20 Hz and home-only prediction
(`2026-07-07-home-only-prediction-design.md`) were needed.

## 1. Making a tick cheap

Factorio's main lesson: **a tick should cost in proportion to what is changing, not to what exists.**

1. **Active lists and sleeping.** Only active entities are visited. The active set is an intrusive linked list, so an
   entity is added or removed in O(1) during iteration ([FFF #204](https://factorio.com/blog/post/fff-204);
   *(forum snippet)* p=303572). A blocked machine registers with whatever blocks it and is woken when that clears
   *(forum snippet)* p=506079. The 0.17 fluid rewrite paid off mostly because pipes no longer kept drills and assemblers
   awake every tick ([FFF #271](https://factorio.com/blog/post/fff-271)). Roboports with no job turned off went from
   about 1 → 0.025 ms per tick ([FFF #421](https://cf-www.factorio.com/blog/post/fff-421)).
   *For us:* audit what `Region::tick` walks every tick whether or not anything changed. Likely candidates: the pile
   resettle/despawn scans, the slime spawner's surface scan, animator re-posing of idle rigs, halo band scans and
   people. Each should be event-driven or put to sleep.
2. **Due-tick buckets instead of `tick % N` polling.** Pollution was spread over 60 ticks by having every chunk test
   `tick % 60 == hash % 60` each tick. Moving each chunk into the bucket for its next due tick gave about 7 % on big
   factories ([FFF #161](https://cf-www.factorio.com/blog/post/fff-161)). Map reveal works the same way in 2.0 (3.6 %).
   *For us:* `History` already keys its queue by `(tick, seq)`. Give `Region` the same kind of rollback-safe timer
   queue for pile despawn, `REST_TICKS`, slime rest timers, re-plans, `WATER_STEP_TICKS` and future DF jobs.
   Undoing it is just pop/push.
3. **Replace general simulation with an algorithm where the domain allows.** Belts became "transport lines": items
   stored as gaps, and an unblocked line updates two integers instead of moving every item. Item movement got
   50–100× faster ([FFF #176](https://cf-www.factorio.com/blog/post/fff-176)). Fluids became segments
   ([FFF #416](https://cf-www.factorio.com/blog/post/fff-416)). Robots store a movement intention and are simulated at
   most every 20 ticks while moving and 60 while stationary, with rendering interpolating between updates (10–25 %)
   ([FFF #421](https://cf-www.factorio.com/blog/post/fff-421)).
   *For us, the biggest lever.* Measured earlier: physics is about 99.8 % of client sim time
   (memory `client-prediction-runaway`), and every person, slime and walker is a rapier body moved through the KCC every
   tick. For the Dwarf Fortress direction (hundreds of dwarves and creatures on tiles), a walking creature can be a
   **tile-path follower**: position is a pure function of (path, start tick, speed), with no rapier step while it
   walks. Only first-person players, falling or felled voxel bodies, and contact situations need the physics step.
   This is Factorio's robot trick and also DF's own model. It also shrinks rollback: a walker's state is a few integers.
4. **Memory latency dominates.** Factorio measured UPS as memory-latency bound. Prefetching the next active entity gave
   9–13 % ([FFF #204](https://factorio.com/blog/post/fff-204)); regrouping electric network data by prototype gave more
   than 2× ([FFF #209](https://factorio.com/blog/post/fff-209)). Parallelising inside the update made it *slower*
   (cache-line invalidation, [FFF #215](https://cf-www.factorio.com/blog/post/fff-215)). Threading the electric network
   cut it from 0.5 to 0.39 ms but raised CPU use from 0.5 % to 15 %, so it was rejected
   ([FFF #421](https://cf-www.factorio.com/blog/post/fff-421)).
   *For us:* this confirms "the region is the threading unit, nothing inside a region is parallel". Spend effort on the
   data touched per tick (smaller components, hot/cold splits for `Ecs`, fewer `BTreeMap` walks per tick), not on
   threads inside a tick.
5. **Overload policy.** Factorio slows the whole game; that only works because there is one simulation. Our shared
   world clock with catch-up bursts is the right MMO equivalent. Keep `lag_ticks` visible; it is our "UPS" metric.

## 2. Tick rate

Factorio's tick rate follows from lockstep: one tick of input delay is 16 ms, and a tick runs once. With rollback, a
higher rate means more replayed ticks per round trip (RTT / tick period ticks per misprediction). **Keep 20 Hz** until
replay cost is no longer full-region physics (see §3). After that, a higher rate only adds input responsiveness, which
the commander camera does not need. Walker interpolation (§1.3) gives smooth motion at any tick rate, so visual
smoothness is not a reason to raise it.

## 3. Prediction: a Factorio-style latency state

Factorio rejected state correction because its state is too big ([FFF #83](https://factorio.com/blog/post/fff-83)). It
also rejected rolling back the real state. Instead:

```
per client tick (FFF #302):
  apply server actions for the tick to the game state        (authoritative, forward only)
  drop those actions from the latency queue
  reset latency state := game state (only the predicted subset)
  re-apply remaining queued local actions to the latency state
  render from both
```

Mispredictions correct themselves because the overlay is rebuilt every tick.

*For us:* the home region's full rollback could become "confirmed region + latency overlay". The confirmed copy
advances only on server events and is never undone; the overlay holds:
- the local body (rapier query against the confirmed world's colliders, read-only);
- pending designations/orders, item moves, the cursor stack;
- the mining progress of the block being broken.

Factorio predicts building, mining, movement, cursor and GUI the same way, and leaves out combat and cascades (rule:
"the more state an action touches, the harder it is to predict"). For Fortress mode almost everything is an order whose
effect is a designation, which is trivially predictable; worker outcomes need not be predicted at all.

**Trade-offs:**
- This removes the per-misprediction replay of whole region ticks, and makes `HASH_MODE`'s per-entry undo checks and
  the region undo log unnecessary on the client.
- It loses prediction of second-order effects (a felled tree landing, pushing a slime). Those then appear one RTT late,
  as in Factorio.
- It is a big change: the undo-log rollback is the core of `game`. Treat it as an option to weigh against the current
  model when replay cost next bites (TODO: the halo-from-non-predicted-sources rollbacks), not a quick win.
- Factorio's 2.0 car prediction shows the cost ([FFF #412](https://direct.factorio.com/blog/post/fff-412)): every place
  the overlay and the real state disagree shows up as a bug: rounding from a different damage order, sounds and
  particles fired twice. They needed `LatencyOneTimeEffects` to deduplicate one-shot effects. We would need the same
  for item-pickup sounds and break particles.

## 4. Networking robustness

1. **Late input: delay it, never drop it, never wait for it.** The server never waits. A late action goes into the next
   tick with a notice to the client, and a `StopMovementInTheNextTick` is injected so a character does not keep running
   through the gap ([FFF #302](https://factorio.com/blog/post/fff-302)). We already re-slot via `Forward` and stamped
   holds. Check that a held key across a gap of missed inputs cannot run on. That is the analogue of their injected stop.
2. **Per-client latency, renegotiated.** The server estimates each client's RTT and renegotiates that client's input
   latency every 5 s ([FFF #302](https://factorio.com/blog/post/fff-302)). Ours is a fixed `CLIENT_LEAD_MS` of 1.5
   ticks ±1 tick for every client. A lead sized from measured RTT jitter would cut mispredictions for jittery (browser,
   Wi-Fi) clients and keep good clients' latency low.
3. **Lost data is re-requested, never skipped.** A client re-requests a missing tick closure
   ([FFF #147](https://factorio.com/blog/post/fff-147)). Our server writer **drops** a packet after `SEND_DEADLINE`
   (2 s), leaving a hole in the stream that ends in the `"Server event input buffer too big"` panic (memory
   `netcode-dropped-packet-gap`, still open). Factorio's answer: never drop; if a peer cannot keep up, resync it (§4.5)
   or kick it.
4. **Bound what a catch-up can emit (the megapacket post-mortem).** A client that caught up 400 ticks at once emitted
   one selection action per tick, then sent a single 400-action packet. The server cannot drop actions, so it relayed
   that to about 200 clients and saturated its upload. It took 3 weeks to find
   ([FFF #302](https://factorio.com/blog/post/fff-302)). Delta-compressed, order-dependent actions made it worse.
   *For us:* make sure a client catch-up burst (`PredictedClock` bursts, region catch-up) coalesces held-state inputs
   instead of emitting one per caught-up tick. Prefer state-style inputs (`MoveKeys`) over deltas, so any one can be
   dropped or merged safely.
5. **Desync recovery: resync, don't panic.** On a CRC mismatch the Factorio client goes to `DesyncedWaitingForMap` and
   re-downloads the map; players are kicked after 3 desyncs ([wiki](https://wiki.factorio.com/Multiplayer)). We detect
   desyncs (`[state-hash …] DESYNC`) but keep running diverged. The per-region analogue is cheap: on a mismatch
   (or a stream hole), the client re-requests that region's snapshot and rebuilds it. Our region snapshots are already
   the join path.
6. **Join while others play.** The map uploads in the background, buffered actions are sent, and the joiner runs as
   fast as it can to catch up ([FFF #149](https://www.factorio.com/blog/post/fff-149)). 2.0 added "pause while joining"
   because catching up on big maps took minutes ([FFF #415](https://direct.factorio.com/blog/post/fff-415)). Our
   per-region snapshot plus event stream is the same design at region granularity, and keeps catch-up short. Worth
   keeping regions small for this reason too.
7. **Bundle sends at a heartbeat cap.** `max_heartbeats_per_second` (default 60, range 6–240) bundles updates once more
   are due than that. Long server messages are segmented over several ticks, with segment size scaled by peer count
   (`server-settings.example.json`). Typical traffic was quoted at about 5 kB/s per player, with more than that called
   suspicious *(forum snippet, ~2015)*. That is a useful budget to compare our `[client] by_kinds` traffic against
   (halo deltas, a voxel body re-sending its compound collider every tick — TODO).

## 5. Determinism hygiene and tooling

1. **Heavy mode.** Save and reload the game every tick and compare the CRC before and after. It runs after every
   commit, about 400–600 s on 10–18 cores ([FFF #315](https://direct.factorio.com/blog/post/fff-315)). It catches
   state that does not survive serialization (our parking, autosave and join snapshots).
   *For us:* a seeded scenario test that, every tick, serializes the region (`SerializedRegion` / the save format),
   restores it, and asserts `state_hash` is equal. That would also pin `restore_saved`'s exclusions. Slow-tagged and
   run in `test-full`.
2. **First-bad-tick reports.** Debug mode CRCs the whole map every tick and writes a tagged save per tick. A replay
   finds the first bad tick, and a binary diff of the two saves usually names one variable
   ([FFF #47](https://cf-www.factorio.com/blog/post/fff-47)). Desync reports bundle the client save, the server's
   reference save and the log ([wiki: Desynchronization](https://wiki.factorio.com/Desynchronization)).
   *For us:* on `DESYNC` (with an opt-in env knob), the server sends its serialized region for that tick, and the client
   writes both blobs plus a field-by-field `StateHash` walk diff to `target/desync/`. The `StateHash` derive already
   visits every field in order, so a "hash path" variant can name the first field that differs.
3. **Thread-count determinism.** A desync that depended on core count, from multithreaded chunk generation whenever a
   mod edited tiles, lived from 2017 until 2.0. It was found by emulating fewer cores
   ([FFF #415](https://direct.factorio.com/blog/post/fff-415)). We already assert the forge is bit-identical at any
   `RAYON_NUM_THREADS`. Extend that habit to anything pooled that feeds sim state (LOD workers do not; worldgen region
   generation does). Run generate_region under 1 vs N threads in a test.
4. **Fixed-point positions.** Factorio's `MapPosition` is a 32-bit integer with 8 fractional bits
   ([lua-api](https://lua-api.factorio.com/latest/concepts/MapPosition.html)); it builds with `/fp:strict` and its float
   issues came from per-platform math libraries (dev AMA). We use f32 + `libm` + rapier `enhanced-determinism`,
   which is equivalent. Tile-path walkers (§1.3) would get integer positions for free.
5. **Deterministic modding.** Lua's `math.random` is one global generator seeded from the map; `pairs` runs in
   insertion order; `io`/`os` are removed; `on_load` may not change state
   ([lua-api](https://lua-api.factorio.com/latest/auxiliary/libraries.html),
   [data-lifecycle](https://lua-api.factorio.com/latest/auxiliary/data-lifecycle.html)). This matches our stateless
   `roll(tick, subject, salt)` and the planned wasm mods (memory `modding-wasm-research`): the host owns state, and
   guests get no clock, no unordered iteration and no RNG state.

## 6. Recommendations, ranked by expected value

| # | Change | Why (Factorio precedent) | Size |
|---|---|---|---|
| 1 | Tile-path walkers for people/creatures: position from (path, start tick, speed), physics only when needed | Robots on intention, transport lines (FFF 421, 176); physics ≈ all sim time | Large; do it before DF jobs bring hundreds of dwarves |
| 2 | Rollback-safe due-tick timer queue in `Region`; audit per-tick scans → sleep or event-driven | Active lists, due-tick buckets (FFF 204, 161, 421) | Medium |
| 3 | Never drop sends; on a stream hole or a `DESYNC`, re-request the region snapshot instead of panicking or running diverged | Re-request, desync → re-download map (FFF 147, wiki) | Medium; fixes the open `netcode-dropped-packet-gap` |
| 4 | Heavy-mode test: serialize, restore and hash every tick in seeded scenarios | FFF 315 | Small |
| 5 | Desync report dump + first-differing-field `StateHash` path | FFF 47, wiki | Small–medium |
| 6 | Per-client input lead from measured RTT jitter | FFF 302 | Small |
| 7 | Coalesce inputs during catch-up bursts; keep inputs state-style | Megapacket post-mortem (FFF 302) | Small (audit) |
| 8 | Latency-state overlay instead of full-region client rollback | FFF 83 / 302 | Very large; revisit only if replay cost still dominates after #1 |
| 9 | Keep 20 Hz; don't raise the tick rate while replays re-run physics | §2 | none |

**Not found in primary sources:** what Factorio's "heuristic" CRC covers and how often it runs; current per-player
bandwidth; packet layout; its internal RNG layout.
