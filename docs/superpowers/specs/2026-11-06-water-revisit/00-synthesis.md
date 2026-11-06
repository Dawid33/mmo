# Water, revisited for the engine as it is in November: synthesis

**Date:** 2026-11-06 · **Status:** research only, nothing implemented
**Ask:** research how to implement water in this voxel world, including the performance
implications of rollback and of communication between regions.

## Inputs

| File | Contents |
|---|---|
| [01-new-prior-art.md](01-new-prior-art.md) | Online research filling the September survey's gaps: Factorio fluid segments, Dwarf Fortress beyond pressure, colony sims, fluids under lockstep or rollback, mass-conserving exchange across partitions, 2025–26 work. |
| [02-engine-today.md](02-engine-today.md) | Code audit with file:line references: water's representation today, the rollback cost model, the cross-seam plumbing, the determinism analogues. |
| [../2026-09-22-voxel-water-efficiency/00-synthesis.md](../2026-09-22-voxel-water-efficiency/00-synthesis.md) | The September design and **the user's decisions, which still stand**: water is deterministic and in-sim on the server and every client (never server-solved), and finite everywhere. |
| [../2026-10-20-dwarf-fortress-research/08-fluids-and-environment.md](../2026-10-20-dwarf-fortress-research/08-fluids-and-environment.md) | DF fluid mechanics for the fortress direction. |

The September model still holds:
- **Water bodies** hold pooled lakes. Each has one level and one volume.
- A **per-cell cellular automaton (CA) at the frontier only** handles moving water.
- **Seam outflow and arrival events** carry water between regions.

What has changed is the engine underneath it: 256 m regions, 16³ chunks, predicted regions
running on their own threads, and the DF direction. Those changes make several September
"optimisations" **mandatory**, and expose two new hazards.

## TL;DR

1. **Water must leave chunk block data.**
   - Today water is block 5, a **solid** block. It has colliders, all-water chunks collapse to
     one cuboid, the character controller stands on the sea, and there is no swimming.
   - At 16³ chunks, one water cell change in block data costs:
     - a copy-on-write clone of a collider shape of up to 4096 keys;
     - a re-merge of the whole chunk's mesh (up to 512 bricks) plus its distance-LOD copy;
     - on the server, a terrain fingerprint change, which triggers LOD rebuilds.
   - A flow step touching N chunks pays that N times, and the client pays it again on every
     replay.
   - Water therefore becomes its own `GameData` layer. Chunks keep only rock, air and ice;
     ice stays a block.
2. **Most water costs zero bytes because it is recipe water.**
   - The sea, forged lakes and forged rivers are already a pure function of the worldgen
     recipe: `SEA_TOP_BLOCK`, plus the per-column `water_top` in `Patch`.
   - Store them the way implied ground is stored. A body is just `{ level, volume }`, and its
     membership is read from the recipe until an edit disturbs it.
   - A per-cell map would be roughly 2×10⁶ cells, about 100 MB, for a 30 m-deep sea under one
     256 m region. That rules per-cell storage out.
3. **Rollback cost is bounded by a frontier budget plus a step journal, not by the solver.**
   - The client has no fixed replay depth: it replays the whole unconfirmed log, L events.
     At seams, L was measured at 12–16 in September (02 §2).
   - Re-simulation costs `mispredicts/s × L/4 × (step + revert)` per region thread at the
     5 Hz water cadence.
   - Three rules keep that cheap:
     - **(a)** a hard cap on frontier cells per step, counted in cells, never in time;
     - **(b)** frontier cells in dense 8³ `u16` bricks (~50× faster than map entries,
       measured in September);
     - **(c)** undo as a **first-touch brick journal** per water step, like rapier's
       `StepJournal`, instead of one `UndoMap` entry per cell. Each per-cell entry clones a
       value, takes the global log mutex and is as large as the largest event variant.
   - Worked budget: 4096 cells/step ≈ 40 µs. A 16-tick replay holds 4 water steps, so about
     0.2 ms per region thread. Through per-cell map entries the same replay would cost ~10 ms.
4. **Crossing a seam needs one owner per face, a rate cap, and no client-predicted arrivals.**
   - No published work covers fluids under rollback (01 §4). The rule we use comes from
     conservative flux exchange in CFD domain decomposition. Factorio's pumps (01 §1) are the
     game precedent.
   - The upstream region computes the flux from its own state and subtracts it at once. It
     sends the flux as a `WaterArrival` event along the existing transfer path.
   - Mass is then conserved by construction: every unit is in one region, or in exactly one
     event in flight.
   - Server region threads are **not tick-aligned**. That is harmless here, because an
     arrival applies at whatever tick the target is on, like `EntityArrived`.
   - **Client:** do **not** relay predicted water arrivals between sibling region threads.
     The audit found relayed transfers are never retracted when the source rolls back. An
     unmatched one becomes a permanent orphan that re-applies on every rollback. The
     destination waits for the server's arrival event, which costs about one round trip of
     latency at a seam.
5. **Our finite ledger plays the role of DF's map edge.**
   - In DF, rivers and oceans at the map edge are infinite sources and sinks. Thin 1/7 water
     evaporates, and aquifers seep (01 §2). Every one of those breaks conservation, and every
     one is what lets DF water settle.
   - Proposal: route them all through the **world water ledger**, the sea's finite volume from
     September:
     - map-edge river outlets drain into the ledger, and inlets draw from it;
     - films below the minimum depth evaporate into it;
     - aquifers and springs draw from it.
   - That gives living rivers and a water cycle while the world total stays exactly
     constant. It answers September's open question of how a forged river can flow without
     being infinite.

## The model, adapted to today's engine

```
GameData.water            (rollback state; hashed by the 100-tick whole-state hash)
  recipe water            implied: sea / forged lake / river column tops from worldgen,
                          zero storage until disturbed (like ColumnGround)
  bodies: BTreeMap<BodyId, Body { level, volume, span overrides }>
                          one per lake/pool inside ONE region (hard size bound = the region,
                          Factorio's lesson: unbounded segments needed a cap)
  frontier: BTreeMap<BrickKey, Box<[u16; 512]>> + awake bitset + sleep counters
                          the only per-cell water; budgeted per step
  seam links: per (region side, body) last-reported neighbour level, rate-capped flux
world ledger (manager)    sea volume + evaporation / edge outlets / springs; level changes
                          broadcast as rare deterministic events
```

- **Cadence.** The water step runs every 4 ticks (5 Hz) as a protocol constant, reading
  edits from tick N − D as September proposed (02-algorithms §7.3). Most rollbacks then re-run
  no water step at all.
- **Undo.** A water step opens one journal: the first touch of a brick or body copies it
  (1 KiB per brick), and revert copies those back. This is the same pattern as the physics
  `StepJournal`. It goes through **one** undo entry per step, which keeps water out of the
  per-mutation log mutex.
- **Hashing.** Bodies and the awake frontier bricks go into `StateHash`. Recipe water is not
  hashed, because it is a function of the world file. With per-entry `HASH_MODE` off by
  default, the only steady cost is the 100-tick whole-state hash, roughly bytes of frontier
  plus bodies.
- **Interaction with chunks.** Breaking a block next to water wakes the frontier, or opens
  an outlet on a body. Placing a block into water displaces volume into the body, an O(1)
  change. Neither touches colliders. Water has no collider. Swimming and buoyancy are read in
  `movement::plan` by querying the water layer.
- **Rendering.** Water gets its own record stream and blended pass, as in 03 §B of the
  September research. It is kept **out of** the terrain fingerprint and the chunk mesh, so a
  draining lake changes a level uniform, not a remesh. LOD gets a per-key water-surface
  layer.
- **Templates in the code.**
  - **Item piles** are the closest analogue: rollback state keyed by cell, one undo-safe write
    funnel (`item_piles.rs:431`), event-driven settling, a cap with eviction, and an expiry
    index that becomes the awake set.
  - **Voxel bodies** show how activity goes back to sleep.
- **Gameplay depth.** Water stays at `u16` resolution internally. The UI and rules show DF's
  1–7 depth. The genre wants floodgates, drains, wells, cisterns, moats, drowning, freezing
  and droughts (01 §3), not fidelity. Timberborn's warning applies: re-planning paths every
  time water changes caused lag spikes. Paths must react to body level changes in batches,
  not per cell.

## Flow models

Four families were considered. The recommendation is three of them, one per kind of water,
joined by one edge rule.

| Model | Used by | Verdict |
|---|---|---|
| Level propagation: a source plus a distance 1–7 | Minecraft | **Rejected.** Water is not conserved, and lakes can't drain. That breaks the finite-water decision. |
| Volume CA: an integer amount per cell; fall, then spread | DF, Terraria, the July plan | **Frontier only.** The time to settle grows with the square of the width, so one block taken from a 40 m lake wakes 1,600 cells for about a minute, and replays pay that again. It is right for small amounts of moving water. |
| Heightfield / virtual pipes | Timberborn, From Dust, Cities: Skylines | **Not the core.** It allows one water surface per column, but a dug fortress has rooms under lakes and flooded tunnels under rivers. Usually floats, and it needs damping. |
| Pooled bodies: one level plus one volume per connected water | Factorio 2.0 segments, Fill-Spill-Merge | **Core.** Settled water costs zero, edits are O(1), and U-tubes and communicating vessels come out exact. No waves (a shader fakes them). Bodies need a size bound, and the region is that bound. |

**Which model covers which water**

| Water | Model |
|---|---|
| Lakes, pools, flooded rooms, the sea, player cisterns and moats | **Bodies** |
| Spills, waterfalls, sheets, a breach pouring out | **Frontier CA**, near disturbances only, with a cell budget per step. It visits in **checkerboard (red-black) order**, not in an ordered sweep, so flow has no bias in the scan direction. |
| Rivers | **A chain of reach bodies** (the forge already stores river reaches), each pair joined by a spill link whose flow depends on the head. Current, for swimming and moving items, is read from the flow through the link. |

**One edge rule for every connection.**
- **Where it applies:** body↔body through a gate, a pump, a reach-to-reach spill, and a
  region seam.
- **Flow:** `q = clamp(k × head, cap)` for every link, computed from the state at the
  start of the step (Jacobi).
- **Scaling:** if a source's total outflow exceeds what it holds, every outflow is scaled
  down proportionally.
- **What it gives:**
  - the result doesn't depend on update order, so there is no Factorio-1.1-style "first
    updated wins";
  - volume is conserved exactly in integers;
  - a seam is this same rule with one step of lag;
  - pressure is free: a U-tube is one body.

**Thin films** below the evaporation depth evaporate into the world ledger after a quiet
period. This is DF's 1/7 evaporation done without breaking conservation. It is gated on
decision 1 below; until then the prototype carries it switched off. Players see DF's 1–7
depth, shown over `u16` internally.

## What players can do with water

Players make their own water. A body is not a worldgen object: the simulation creates one
whenever loose water settles. A pond poured or flooded by a player is a body exactly like a
forged lake. Every player action reduces to one of three primitives: **add or remove volume
at a cell**, **open or close a cell** (dig or build), and **a link** (a gate or pump with a
set rate).

| Player does | What happens |
|---|---|
| Pours a bucket into a dug pit | It falls and spreads as frontier water, then settles into a new body. More water raises its level. |
| Scoops from a lake or well | The body's volume drops by one bucket: an O(1) change. |
| Digs a channel from a lake into a basin | The opening is an outlet, with flow set by the head. The water falls, pools and forms a second body. The two merge when level. |
| Breaches a dam, or digs into a river bank | Same as a channel. A river reach is a body too. |
| Builds a wall or dam across flow | The opening closes. Water behind it becomes a body and rises until it spills or finds another way. |
| Builds a floodgate or door | A link that is open or closed. A sluice is a link with a set rate. |
| Builds a pump | A powered link with a fixed rate that ignores head, so it can lift water (DF pumps work this way). |
| Builds a cistern, well or moat | It fills and drains like any body. A moat fed from a river is a body joined to a reach. |
| Builds pressure plumbing | Connected water is one body and rises to the source's level. There is no DF "one level short" quirk. |
| Floods invaders, or its own fortress by mistake | An outlet fills the space cell by cell. Opening flow is limited, so a flood takes time to arrive. |
| Irrigates | Done as moisture derived from nearby water (the Timberborn approach), not as water consumed. |
| Places blocks into water | The block displaces its volume into the body, so the level rises. |

Players see their own edit react within one water step (~0.2–0.4 s), because the client
simulates it locally. This is what in-sim water buys over a server-solved model.

**Limits players will notice:**
- no momentum or waves: water only flows downhill or toward a lower level;
- a standing body has one flat level, though it still drains at the rate its outlets allow;
- very large floods run slower than real time, because of the cell budget (the remedy is a
  bigger budget or more steps per second while a flood is active);
- across a region seam, flow is rate-capped and arrives about one round trip late on clients;
- a body draining below a sill splits into two pools, which is correct but one of the parts
  the prototype must prove.

The prototype's scenarios are these actions:
- pour into a pit;
- dam breach;
- gated flood (open, then close);
- pumped cistern;
- scoop;
- pressure plumbing (the U-tube);
- a lake joined across a seam.

## Communication between regions

| Case | Handling |
|---|---|
| Waterfall or stream leaving the region | The source region's step computes outflow at the seam face. It subtracts the water and emits `WaterArrival{seam, cells, amount}` as a transfer. The manager routes it like `EntityArrived`. The destination adds it to its frontier, or to a body. |
| One lake spanning a seam | One body per region, joined by a **seam link**. Each side broadcasts its level, like a ghost, and the higher side pushes flux, rate-capped, from the last reported level. With a one-tick lag, any overshoot is bounded by cap × lag (01 §5). Pick the cap so the overshoot is under one level unit. |
| Destination full | It bounces the remainder back as an arrival to the source. The water is never dropped. |
| Destination not running (parked or unloaded) | A **pending-volume ledger per region** at the manager, like people being absorbed by the history. It applies as recipe-level arrivals when the region wakes. Waking a 256 m region per flux would cost seconds. |
| Outer map seam | A sea edge feeds or drains the world ledger. A land edge is closed, or is an outlet to the ledger (decision 1 below). |
| Client | Predicted regions run the same step. Seam arrivals come **only** from server events, never from relays between sibling threads. An arrival the client had not predicted causes an ordinary rollback in the destination, like a remote player's input. |

**Prerequisite:** the audit found an open problem in September's capture: roughly one
mispredict per tick per neighbour region that carries relayed ghosts, at replay depth 12–16.
If it still happens with region threads at 256 m, every water step at a seam would re-run
about 20 × L times per second. Measure it first with `PERF_LOG=1` on a 3×3 embark. The audit
lists it as undetermined. Fix it before seam water. The bit-inexact home region after a
crossing (TODO.md:25-42) is in the same code path.

## What to build, in order

1. **Measure** with one `PERF_LOG=1` embarked run at 256 m:
   - mispredicts/s and log length per region thread;
   - snapshot size;
   - state-hash cost;
   - how many water cells a typical embark holds.

   This sizes every budget below.
2. **Phase 0: a liquid kind.**
   - Add a block kind `Air | Solid | Liquid` to the registry.
   - Water stops being solid. It loses its collider and stops counting as ground.
   - Today ~77 sites compare against `AIR` (20 of them in `lod.rs`), and 6 hard-code
     `BlockId(5)` (02 §1).
   - Ice stays solid.
3. **Static water layer.**
   - Recipe water comes from worldgen, and chunks stop storing water blocks.
   - Swimming in `movement::plan`.
   - A water render pass plus the LOD water layer.
   - Nothing flows yet. This is the visible win, and it fixes walking on the sea.
4. **Offline prototype.** `docs/superpowers/plans/2026-09-22-water-prototype.md`, amended
   2026-11-06. Tasks 6–11 add:
   - checkerboard frontier order;
   - links, the shared edge rule: gates and pumps;
   - brick-journal undo;
   - evaporation to the ledger, off by default;
   - the two-world seam with lagged, capped flux;
   - the player scenarios.

   Task 12 measures them and applies the decision gate.
5. **Flow in one region,** with seams closed and the ledger in place.
6. **Seam flux,** after the mispredict measurement and fix.

## Decisions for you

1. **Is the ledger the water cycle?** Map-edge rivers, evaporation and springs run through
   the world ledger, so rivers can flow forever while total water stays constant.
   - Recommended.
   - The alternative is September's "forged rivers asleep until disturbed", which stays
     possible: it is the same design with an edge flow of zero.
2. **Should the client predict water arriving across a seam?** Not predicting
   (recommended) costs about one round trip of delay where water crosses a seam, and avoids
   the orphan-transfer bug class.
3. **Depth resolution shown to players.** DF's 1–7 (recommended) is shown over `u16`
   internally. Or show a finer gradation.
4. **Go for step 1 (measure) and step 2 (Phase 0 liquid kind)?** Both are useful on their own,
   independent of the flow model.
