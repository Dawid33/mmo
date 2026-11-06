# Voxel water efficiency: synthesis

**Date:** 2026-09-22 · **Status:** research only, nothing implemented  
**Ask:** research voxel-world water implementations, focusing on efficiency, and aim to beat
Minecraft.  
**Inputs:**

| File | Contents |
|---|---|
| [01-game-implementations.md](01-game-implementations.md) | About 20 shipped games and engines, from source code, slides and wikis. Unverified claims are flagged. |
| [02-algorithms-and-data-structures.md](02-algorithms-and-data-structures.md) | Models, costs, and throwaway micro-benchmarks, kept in the session's temp directory (`$CLAUDE_JOB_DIR/tmp/wb/`), not in the repo. |
| [03-networking-rendering.md](03-networking-rendering.md) | Sync options, bandwidth, render pass, LOD, update rate. |
| [../2026-09-22-rapier-water-physics-investigation.md](../2026-09-22-rapier-water-physics-investigation.md) | Earlier today: Rapier and salva can't be the sim. |

Baseline for everything below: `docs/2026-07-15-water-implementation-plan.md` (proposed, not
built). It uses a `u16` amount per 1 m cell, an ordered-edge CA every 2 ticks, sleep masks,
and runs inside the predicted rollback sim.

## User decisions, 2026-09-22 (these override the sections below)

1. **Not server-solved.** Water flow is deterministic and runs inside the rollback sim on
   the server and every client, like physics. The solver is bit-exact integer code with
   canonical ordering, identical on native and wasm32. TL;DR point 2, "Resolving the one
   disagreement", and build step 4 (`WaterStep` plumbing) are **superseded**. What replaces
   them:
   - **Water bodies are what keep re-simulation affordable.**
     - A step costs ~µs per active region, and a rollback replays at most 10 steps
       (02 §7.1).
     - Membership recomputes are bounded by a *cell* budget, never a time budget.
   - **Cadence.**
     - The frontier CA runs every 4 ticks (5 Hz), as a protocol constant.
     - Delayed-input water is held in reserve. The water step at tick N reads edits from
       N − D, so most rollbacks re-run no water (02 §7.3).
   - **Cross-region flow** reuses the entity-handoff pattern.
     - Outflow at a seam is decided by the source region from its own state and leaves as
       a stamped event.
     - The neighbour applies it as an arrival, as `EntityArrived` does.
     - A full destination bounces it back as an event. Mass is conserved, because every
       unit is always in exactly one region or one in-flight event.
     - This is the part to prototype hardest after the body↔CA interface.
2. **Water is finite everywhere.** There are no infinite bodies:
   - **The sea** is a finite world-level body with a volume ledger. Its area is ~10⁸ m² on a
     16 km forged world, and one level unit is 1/4096 m. Its level therefore moves only
     after about 24,000 m³ of net change, so in practice it is a constant that still
     conserves. The ledger lives at the manager, and a level change would be a
     deterministic broadcast event.
   - **Forged rivers** can't flow forever without a supply. They stay **asleep**, frozen
     where the forge put them, until an edit disturbs them locally. A living river needs a
     water cycle (sea evaporation → springs/rain), which is a later design question.
   - **Buckets** move volume; they never create it.
   - **Thin sheets.** A spill on flat ground spreads until it reaches a minimum film depth
     (a tunable, e.g. 1/16 m), then stops and stays as a puddle. Finite water can't vanish.

**Prototype results (2026-11-06):** see [../2026-11-06-water-revisit/03-prototype-results.md](../2026-11-06-water-revisit/03-prototype-results.md). Gate: PASS except (c) borderline. Bodies are 30–80× cheaper per step; the brick undo lost to per-cell.

### How water moves (not static like Minecraft)

Water flows to fit its container, conserving volume. The two layers split the work:

- **Moving water** (pours, waterfalls, sheets, streams): the per-cell CA moves integer
  amounts down first, then sideways toward lower neighbours. Water spreads, falls and
  pools, and fills whatever shape the terrain gives it.
- **Standing water** (a pool, lake or flooded cave): a quiet, flat patch of CA water becomes
  a body. Inside one body the level is uniform, so filling or draining updates one number.
  The simplification is that a lake levels instantly, with no sloshing across it; waves are
  shader-only.
- **Between containers:** a gap next to a body at or below its level is an *outlet*. Each
  step it emits water into the CA at a rate set by the head (level above the opening). The
  CA carries it downhill until it lands in another container, which grows as a new or
  existing body.

Worked example: you break the wall between a full lake and an empty basin below it.

1. The hole becomes an outlet.
2. Water streams out through the CA, falls, spreads across the basin floor and pools.
3. The pool settles into body B, which absorbs the continuing stream.
4. Body A's level falls as B's rises.
5. When B's level reaches the hole, the head is zero and the flow stops.
6. A and B merge into one body at a common level, like communicating vessels.

Pouring a bucket into a pit follows the same path in miniature. The water falls, spreads,
fills the pit and settles into a small body.

## TL;DR

Minecraft's water is cheap only because it is *fake*. Sources are infinite, flow is bounded
to 7 blocks, and a lake can't drain. Its real costs are the side effects of every block
change: lighting, neighbour updates, remeshing, and a packet per block. We can beat it on
cost and believability at once with four decisions.

1. **Water bodies, not water cells.** A settled pool, lake, sea or flooded cave is one object:
   integer level, volume, per-layer area, and membership stored as column spans.
   - Only the *frontier* is a per-cell CA: streams, waterfalls, spreading sheets, and outlets.
   - Draining a lake costs O(openings), not O(surface), and the surface stays flat.
   - The sea and rivers are *infinite* bodies with a fixed level: Minecraft's source trick,
     done properly.
   - Sources: Fill–Spill–Merge hydrology, the Enshrouded column representation, Terraria's
     `QuickWater` settle, and DF pressure.
2. **~~Solve on the server, apply everywhere.~~ SUPERSEDED: deterministic in-sim, see decisions above.** Water *state* stays in rollback state: hashed,
   snapshotted, parked, and read by predicted swimming. The *flow solver* runs only on the
   server region thread.
   - Each solver step becomes a `WaterStep` event of absolute values, stamped to apply
     6 ticks ahead and sent immediately.
   - Clients slot it in before they reach that tick, so water causes no rollbacks and replay
     never re-solves water.
   - Every shipped multiplayer water system surveyed (Terraria, Starbound, Minecraft,
     Hytale, Enshrouded) solves water only on the server; none has clients predict it.
3. **Dense bricks for whatever is per-cell.** Measured: the plan's 8-cell `WaterChunk` in a
   `BTreeMap` costs ~460 ns per cell step, against ~9 ns for a dense `u16` array. That is
   **~50×**, and it is layout, not algorithm. Bitset masks (64 cells per `u64`) handle the
   active and connectivity work at ~0.02 ns per cell.
4. **Batch every side effect per step.**
   - One remesh mark per touched tile, and a per-body level uniform, so a draining lake is
     not a remesh storm.
   - One event per step, not per cell.
   - Water is kept **out of the terrain LOD fingerprint**. Otherwise a drain rebuilds and
     resends 4–5 LOD payloads per viewer every 1.25 s.

## Where we beat Minecraft

| | Minecraft | July plan | **Recommended** |
|---|---|---|---|
| Volume | infinite sources, not conserved | finite, conserved | finite and conserved; sea and rivers explicitly infinite |
| Lake drain | impossible (sources) | ~1 ms per step for ~20 min as stored (dense: ~20 µs); surface slopes toward the outlet | **~2–20 µs per step**, surface flat, ~65 µs once per metre of drop |
| Small disturbance next to a 40 m lake | ~1–5 µs, then static | whole 1,600-cell surface re-levels for minutes (O(L²)) | **< 1 µs**; the lake is one number |
| Settled world | 0 (scheduled ticks) | 0 (sleep) | 0 (nothing to visit) |
| State per 64 m region | ~130 KiB of nibbles or block states | ~1.5–2 MiB | **~20–50 KiB**; the sea is implicit, ~0 |
| U-tubes, communicating vessels | no | only with compression hacks | exact (a body is connected) |
| Client CPU | none (server-solved) | solver × 25 predicted regions × rollback replays | **apply only, µs** |
| Wire, 32 m lake draining | per-block packets | ~0 (deterministic) | **~1–2 KB/s** with body-level records (12–15 KB/s as raw cell deltas) |
| Wire, 100 m dam break | ~140–200 KB/s | ~0 | ~50–75 KB/s cell deltas, less with bodies; a settle-and-snapshot valve above budget |
| Solver freedom | n/a | bit-exact integer code forever, native = wasm | **any code**: floats, SIMD, momentum, threads, later GPU; only *application* is deterministic |

Cost figures are estimates from 02 §6 and 03 §A.3. The ns-per-cell figures come from
micro-benchmarks on a throttling laptop, so read them as ratios.

## ~~Resolving the one disagreement: predicted vs server-solved~~ (SUPERSEDED: the user chose predicted, deterministic)

- **02 recommends predicting.** Bodies make re-simulation cheap, and swimming must read the
  same water state as predicted movement.
- **03 and 01 recommend a server solver.**

The lead-scheduled event design satisfies both constraints:

- Water state is still in `GameData`, so predicted swimming reads exactly what the server
  reads, with no position mispredictions. This is 02's concern, met.
- The step's *content* comes from the server before the client's predicted tick needs it:
  `LEAD > RTT + 75 ms` means 6 ticks cover 200 ms RTT. A late packet costs an ordinary
  rollback that re-*applies* a memcpy-sized event.
- It removes costs that bodies alone don't:
  - 25 predicted regions per client, where replay is already the client's bottleneck (see
    *client prediction runaway*);
  - the bit-exactness lock on the solver;
  - Phase 3 cross-region flux as a *client-determinism* problem. It becomes server plumbing:
    strip exchange between region threads, where lag costs a step of seam delay and can't
    desync anyone.
- **Price:** water reacts to your own edit 300–500 ms late. That is invisible for water, and
  Minecraft already flows on a 250 ms clock. An optional render-only local preview can hide
  it; that is polish, not v1.

With bodies, a `WaterStep` is small:
- `BodyLevel { id, level }` per changed body;
- span diffs for membership changes, only on layer crossings, splits and merges;
- absolute `u16` values for changed frontier cells, packed per 8-cell chunk with a mask.

Application stays dumb: set the values, never re-derive them. So application is the only
deterministic contract.

## Architecture

```
server region thread                         every client (and the server's own region sim)
────────────────────                         ─────────────────────────────────────────────
WaterSolver (every 4 ticks = 5 Hz,           GameData.water   (rollback state, hashed)
  2 Hz far / over budget)                      bodies:  UndoMap<BodyId, Body>
  reads region water + pending steps           spans:   per-column membership
  bodies: outlets emit, inflow absorbs,        bricks:  8³ u16 frontier cells, active bitsets
    level from volume, split/merge by        ▲
    bounded bitset flood (cell budget)       │ apply WaterStep at tick T+LEAD (before inputs)
  frontier CA in dense 8³ bricks,            │ idempotent absolute values
    checkerboard order, integer or float     │
  quiet flat CA patch → coalesce to body     │
  over budget → quick-settle + snapshot      │
        │ WaterStep{seq, apply_tick, bodies, spans, cells}
        └──► event log + early send ─────────┘
terrain edits (predicted, in-sim): remove solid → wake/open outlet; place solid → displace
into body volume (O(1)); sea/river = infinite bodies, never simulated
```

- **Movement.** Swimming, buoyancy and currents are read in `movement::plan_with`, as in the
  July plan and the Rapier doc. For a body the water test is `y < body.level` plus a span
  lookup, O(1).
- **Rendering** (03 §B):
  - 12-byte water records in separate per-tile (32×32 m) buffers, drawn after opaque in one
    blended layer with depth write on. No sorting, no OIT.
  - Absorption from floor depth baked into each quad, which works on WebGL2. Normal-map waves
    plus flow maps for motion.
  - A body's surface level is a uniform, and a per-quad previous→new lerp hides the 5 Hz
    steps.
  - LOD gets a per-key water-surface layer. The sea at 1024 units greedy-merges to a few
    quads per key.
- **Persistence.** Regions already park bit-exact, so water persists for free. Enshrouded's
  "only save in-base water" is available later if save size or flood exploits bite.

## What to build, in order

1. **Offline prototype**, before touching the engine. This is the risk. It's a headless
   `game` example or test on a 64³ fixture that runs:
   - the plan's CA in dense bricks;
   - bodies plus the frontier CA;
   - three scenarios: settled, small spill, big drain.

   It measures ns per step, steps to sleep, undo and event bytes per step, exact mass
   conservation, and surface flatness. **Riskiest pieces:**
   - outlet → CA → absorb → coalesce, without mass drift or member/CA flicker (needs
     hysteresis);
   - a split at a sill during a drain.

   **Fallback** if bodies prove fiddly: CA in dense bricks plus Terraria-style *analytic
   settle* of quiet patches. That still beats the plan by a wide margin.
2. **Phase 0** of the July plan: the registry `Air | Solid | Liquid` kind replaces ~18
   `id != AIR` sites. Water loses its collider and stops counting as ground.
3. **Static water:**
   - bodies generated from worldgen: the implicit sea, forged lakes as finite bodies at spill
     level, forged rivers as infinite bodies with a per-column surface;
   - swimming;
   - the water render pass and the LOD water layer.

   Nothing flows yet.
4. **~~`WaterStep` plumbing~~** (SUPERSEDED; replaced by an in-sim solver, cadence constant, and seam outflow/arrival events):
   - the server solver hook in `region_threads`;
   - the lead-stamped event;
   - the client scheduled-event queue, matched through `matches_prediction` like
     `GhostUpdate`;
   - water kept out of `TerrainChanged`.
5. **Cross-region:**
   - server strip exchange for frontier CA;
   - manager-owned bodies for finite bodies spanning a seam: one Δvolume per body per step;
   - closed seams until then, with a counter.

## Decisions for you (1 and 2 decided above; 3 and 4 still open)

1. **Server-solved flow** (recommended) vs **predicted flow** (the July plan, made cheap by
   bodies). Server-solved is what every shipped multiplayer game does; the price is a
   300–500 ms reaction to your own edit.
2. **Is water a finite resource?** Can you bucket it, and can a player empty a lake into a
   cave? The body model supports either. It decides whether the sea is truly infinite and
   whether springs exist.
3. **Wild-water persistence:** persist everything (free with parking) vs Enshrouded-style
   reset outside claimed areas.
4. **Prototype first?** Yes is recommended. It's cheap, offline, and settles the model before
   any engine change.

## Superseded or amended

- **July plan:**
  - The *representation* changes: bodies + spans + brick frontier, replacing the `WaterChunk`
    map as the iteration structure. The 8-cell chunk can remain the undo and wire unit for
    frontier cells.
  - *Solver placement* moves to the server.
  - Phase 0, swimming, edit semantics, hashing and material classes are unchanged.
- **Rapier doc §4.1** (river sources): answered. Rivers are infinite bodies with a baked
  per-column surface, static until an edit opens an outlet.
