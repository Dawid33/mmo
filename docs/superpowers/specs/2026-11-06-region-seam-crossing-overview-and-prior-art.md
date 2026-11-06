# Region seam crossing: how it works today, prior art, and a seam-transparent layer

**Date:** 2026-11-06
**Status:** Proposal accepted 2026-11-06 ("go with your recommendations"). The [DECIDE] items are resolved in `plans/2026-11-06-seam-transparent-regions.md` § Decisions. Nothing is implemented yet.
**Related:**
- `specs/2026-07-05-entity-region-handoff-design.md`: the original handoff design.
- `2026-07-06-seamless-local-crossing-followup.md`: why the local player's own crossing was deferred.
- `plans/2026-10-29-entity-seam-crossing.md` and `…-handoff.md`: generic entity crossing, 0 of 38 steps done. The handoff doc is partly stale: it says ghosts have no body, but they do now.

The user's hypothesis is that seam crossing will be a lasting source of bugs. The better design is a sub-layer under which gameplay code is written as ordinary code that never mentions regions. This document:
1. records how crossing actually works in the code today (the code was read; the earlier docs were not trusted);
2. surveys prior art and the bugs other systems ran into;
3. proposes the interface.

---

## Part 1: How crossing works today

### 1.1 The model in one paragraph

**Regions and their state.**
- The world is tiled into `REGION_SIZE` squares (`RegionCoords`, `protocol.rs:116`).
- Each region is one deterministic, rollback-logged `GameData` (`state.rs:228-318`). That covers its own ECS, its own rapier world, its own **tick counter starting at 0 at genesis**, its own slotmap `EntityKey`s, ghosts and arrivals.
- On the server each region is an OS thread with its own deadline timer. There is **no lockstep and no shared tick numbering** (`server/src/region_threads.rs:118-162`). On the client each predicted region runs inline or on its own thread, driven by one shared broadcast `Tick` (`client/src/regions/`).

**What is world-level.** Only `WorldManager` state (`world_manager.rs:390-494`):
- `homes` (client → region), used to route input;
- region links, parked blobs, the moves in flight, the `MapExtent`;
- the history.

There is **no world-level entity id**. Across a seam an entity is identified by `(source_region, source_key)`.

**Coordinates.** Everything gameplay touches is region-local:
- f32 voxel units: 16 per block (`BLOCK_VOXELS`), 1 block = 1 m;
- `BlockPos` / `TilePos` integers in `0..REGION_BLOCKS`.

The world exists only at two points:
- the manager's `rebase_isometry` (`protocol.rs:220-227`; its comment "exact multiples of 256" is stale);
- the renderer's per-region roots at `world_offset()`.

### 1.2 Server pipeline

1. **Detect.** At the very end of `Region::handle_event(Tick)` (`region.rs:754`), after every gameplay system and physics, `scan_boundaries` (`state.rs:1623-1692`) does the following in key order:
   - It clamps bodies that are past an off-map side back onto the seam.
   - It tests each body's **translation point** with `departure_offset` (`protocol.rs:189-203`). Ownership flips `FLIP_HYSTERESIS` = 2 units (1/8 block) past the edge.
   - An entity within `GHOST_MARGIN` = 32 units (2 m) of an edge emits a `GhostData` to each of 1-3 neighbours.
   - Leavers are removed by `extract_entity_safe` (`state.rs:1551-1614`), which is undo-logged, into a hand-written `EntityBundle` (`protocol.rs:236-266`). The bundle holds kind, pose, linvel, a `CapsuleY` collider and the optional components (camera, motion, slime, animator, walker, client).
2. **Emit.** `RegionRunner::tick` produces `RegionOutput::Departures` and `GhostUpdates` (`region_runner.rs:354-420`).
3. **Relay departures.** The manager (`world_manager.rs:1153-1208`) handles each departure:
   - An off-map target is sent back to the source.
   - A `Person` going into a region that is not running is **absorbed** into the abstract history.
   - Anything else is rebased, wakes the target (`ensure_running`; "arrivals ALWAYS wake"), and is sent as `EntityArrived`.
   - For a player, `homes` flips, `PlayerRegion` is pushed to the client, and move/park races are re-issued here.
4. **Relay ghosts.** Ghosts go only to running regions and never wake one (`:1209-1222`).
5. **Arrive.** `EntityArrived` is an ordinary queued event, applied **whenever it lands, before the target's next tick, whatever that region's tick number is**. `apply_arrival` (`state.rs:1338-1447`) takes one of three paths:
   - **Replay** (identity already present): it corrects the **pose only**.
   - **Ghost present:** the ghost is **upgraded in place**, keeping the same key so the renderer sees no pop.
   - **Otherwise:** a fresh entity is spawned.

   It then attaches the components. For players it calls `Client::reset_region_local()`, which clears mining, the break cooldown and the open chest. Finally it re-plans the walker (`resume_walker_safe`).
6. **Ghosts.** A ghost is a kinematic body plus collider, teleported on every source tick while the entity is in the margin. It expires after `GHOST_TTL_TICKS` = 25 host ticks without an update, which is the only signal that its owner left, parked or died.
7. **Lifecycle edges:**
   - Events for a stopping region queue in `pending_events`.
   - `PlayerInput` to a stopping region is dropped.
   - An arrival into a thread that dies is lost; the spec accepts this.

**Input routing.**
- `PlayerInput` / `Order` / `UiAction` are routed by `homes[client]`, not by the client's region stamp (`world_manager.rs:990-1023`).
- The tick stamp (`EventAfterTick`) is honoured only when the stamped region *is* the current home. Otherwise the input is applied whenever it lands.

### 1.3 Client pipeline

**Prediction.**
- Predicted regions run the same `scan_boundaries`.
- `route_transfers` (`instance.rs:1137-1169`) rebases each departure and sends `RegionCmd::Arrival` to the target if that region is loaded; otherwise the departure is dropped.
- It **flips the client's `home_region` at once** when the bundle carries the local player.
- The authoritative `PlayerRegion` overwrites the flip later.

**Reconcile.**
- Transfer events are matched by identity (`matches_prediction`, `protocol.rs:632-650`).
- When the authoritative local `EntityArrived` lands in the new home, `evict_orphan_local_inputs` (`region.rs:583-625`) drops every pending local input there. These are inputs the server routed to the *old* home during the RTT window.
- The follow-up doc *proves* that no region-local rule can do better, so a crossing under input rubber-bands for about one RTT.

**Misrouted input.** An input predicted into a region whose body has just left becomes `RegionReport::Misrouted` and is re-sent to the new home (`runner.rs:169-173`, `instance.rs:1121-1128`).

**Clock.**
- Only the home region steers the shared rate (`regions/clock.rs`).
- The stale-`SyncClock` crossing stall is fixed.
- "9 SyncClock streams fight over one rate" is still open (TODO.md:19).

**Rendering.**
- Ghosts are hidden while their source region is loaded (`bridge.rs:1298-1311`).
- Visual continuity at a flip depends on the upgrade-in-place.

**Host divergence.** The inline host (harnesses, wasm) drains transfers with a `TakeTransfers` broadcast that also catches replay leftovers. The threaded host relays per `Ticked` report. So the harness does not exercise exactly what native play does.

### 1.4 Rollback

- A departure is part of the source region's `Tick` transaction: undoing the tick restores the entity.
- An arrival is a separate undoable event in the *target's* log.
- **No transaction spans the two logs.** A crossing the client predicts but the server never makes has no cross-region correction path. The predicted flip of `home_region` stays until a `PlayerRegion` arrives, and the server sends that only when *its* home changes. No test covers this.
- An identity match depends on the client's predicted slotmap `source_key` equalling the server's. A test forces this (`instance.rs:2676`).
- `Tick == Tick` compares the event, not its outcome. A divergent extraction is caught only by the state hash every 100 ticks, which logs `DESYNC` and does not repair.
- Predicted ghost updates carry different poses from the server's, which costs about one mispredict per tick per neighbour (memory `input-slot-rubber-banding`).

### 1.5 Gameplay systems and their seam special cases

Every system is region-bounded by default. Each one that needed it got its own patch:

| System | Seam behaviour today | Where |
|---|---|---|
| Walker / MoveTo | goals ≤ 1 region away; path to `edge_toward` tile, step one block past, re-plan from scratch on arrival | `adventure.rs:274, 395-441, 244` |
| People wander | goals clipped to the region; `walk_out_safe` for departures | `people.rs:150-217` |
| Pathfinding (A*) | region tiles only; `in_region` rejects the rest | `pathfind.rs:46-58` |
| Tree felling | picks a tip direction that lands `TIP_EDGE_MARGIN_BLOCKS` inside the region ("interim seam rule") | `trees.rs:33-35, 234-266` |
| Voxel bodies | no `EntityKind`: never transferred or ghosted; settled as soon as the centre of mass leaves the region | `voxel_body.rs:36-39, 624-651` |
| Item piles | not entities; cells clamped into the region; pickup sees only its own region | `item_piles.rs:217-219`, TODO.md:65 |
| Block edits / raycast | "a ray that leaves the region finds nothing"; outer ring not diggable below the surface | `edit.rs:18-32, 103-117`, `orders.rs:162` |
| Implied ground | only materialised inside the region; the renderer treats beyond-edge space as unknown | `edit.rs:21-24`, `bricks.rs:255` |
| Slimes | leashed 4 blocks from edges (beyond `GHOST_MARGIN`) so arrivals don't wake regions | `slime.rs:67-70, 141-193` |
| Chests | reach region-local; open view reset on arrival; `inventory_open` survives (bug) | `containers.rs:33-91`, TODO.md:68 |
| Mining | target + cooldown are region-local / region-tick; reset on arrival | `state.rs:155-166` |
| Physics vs terrain | no neighbour terrain: a body dips at the edge and arrives sunk → `depenetration_lift` | `movement.rs:262-300` |
| Physics vs entities | only via ghosts: kinematic, one-way, 1-2 ticks late, 2 m deep; dynamic bodies get no push | `state.rs:1454-1526` |
| History people | crossing into a non-running region = absorbed into the abstract history | `history/handover.rs:482-505` |
| Water (prototype) | its own seam flux protocol | `water-proto` commits |

### 1.6 Known bugs and limits

- **Client crossing convergence test red since 8e90481** (`client/tests/crossing.rs`). After a crossing the new home is bit-inexact on all 40 idle ticks, permanently. TODO.md suspects:
  - the 1.5-tick client lead puts the predicted arrival on a different tick than the server's;
  - the replay branch corrects pose only.

  The late-scheduling twin is red too.
- **Held input across a seam** oscillates the home region and never settles (TODO.md:48-51).
- **Region-tick-stamped state crosses unconverted** (latent; verified this session): `extract_entity_safe` copies `Motion` and `Slime` verbatim. `Motion.last_jump_tap` (`movement.rs:103`) and `Slime.next_turn_tick` / `next_hop_tick` (`slime.rs:81-83`) are absolute ticks of the *source* region's clock, and region clocks start independently. Mining patches the same class of bug with `reset_region_local`.
- **The bundle is maintained by hand in two places** (`extract_entity_safe` and `apply_arrival`). It has no angular velocity, mass, sleep state, CCD flag or non-capsule shape (`collider_spec_of` falls back to the player capsule), and a person's wander state is dropped.
- **Input to the source in flight during extraction** is applied to a region where `clients.get_mut` silently misses (`region.rs:764`). A released key can therefore be lost. This is inferred from the code and not tested.
- **The netcode dropped-packet gap** (memory): a packet dropped after the 2 s send deadline leaves a hole in the stream, and the client panics "event input buffer too big".
- **Orphan predictions never expire**, which causes permanent full-log replay (memory `client-prediction-runaway`).
- **Deferred items in TODO.md:**
  - cross-region interactions beyond collision (combat, pickup);
  - piles crossing seams;
  - goals more than one region away;
  - following a non-player across a seam;
  - the hotbar flickering mid-handoff.

### 1.7 Fragile points

1. **Region-local state leaks through hand-written bundles.** Every new component needs someone to remember to ask "is this region-local, tick-stamped, or a key?". Nothing enforces the answer.
2. **No common clock.** The arrival tick is "whenever the message lands". The client cannot predict it, and that is the root of the red convergence test and of the input-stamp ambiguity.
3. **Ownership by a point with 1/8-block hysteresis.** Corners, fast bodies and large bodies all hinge on the centre. The source has no terrain past the edge.
4. **Three ownership tables can disagree**: the server's `homes`, the client's `home_region`, and the region that actually holds the body.
5. **Every spatial query stops at the edge.** Each new feature (combat, AoE, sound, vision, fluids, hauling) starts out broken at seams.
6. **Ghosts are degraded copies with no behaviour.** They expire by TTL and each costs constant mispredicts.
7. **Lifecycle coupling.** Arrivals wake regions, so slimes are leashed and people absorbed. Stopping regions queue events. Dead threads lose entities.
8. **Rollback across two independent logs** with no spanning transaction.

---

## Part 2: Prior art

### 2.1 Systems

**BigWorld (cells, real and ghost entities)** is the canonical seamless-world server design ([server overview ch.4](https://howarduong.github.io/github.io/doc/html/server_overview/ch04.html)).
- Each CellApp owns the entities inside its cell. Entities within the ghost distance (= AoI distance) of a boundary have read-only **ghosts** on the neighbour, updated by the real entity.
- Method calls on a ghost are **forwarded to the real**. Scripts are told never to assume a referenced entity is real ([programming guide ch.27](https://howarduong.github.io/github.io/doc/html/server_programming_guide/ch27.html); [Geco notes](https://kiddinglife.github.io/2016/05/02/geco-engine-server-component)).
- Its documented hazards are all **state that does not travel**: entity "extras" are "lost when the entity changes cells", `DOMAIN_REAL` controllers must not run on a ghost, and portal controllers misbehave when moved.
- A reimplementation's verdict is that ghosting, AoI, property sync and migration windows "must be designed together" ([theseed](https://cuihairu.github.io/theseed/design/2-replication-and-space/02-ghost-and-witness.html)).

**Folia (PaperMC regionised Minecraft)** is the closest modern analogue, and its main idea is to **avoid crossings**.
- Ticking regions are dynamic, padded with a buffer of chunks, never adjacent while ticking, and **merge** when players approach. A walking entity therefore never steps into another thread's region mid-tick ([overview](https://docs.papermc.io/folia/reference/overview), [region logic](https://docs.papermc.io/folia/reference/region-logic)).
- Only teleports cross. They are an explicit async two-step, and the entity does not tick while its teleport is pending.
- What broke: synchronous `Entity#teleport` was removed for good, the scoreboard API is "considered broken", and "every single plugin" needs changes ([README](https://github.com/PaperMC/Folia)).
- The fix was **location-addressed scheduling**: `RegionScheduler` / `EntityScheduler` run code on whichever region owns a position or entity, and `isOwnedByCurrentRegion` asserts ownership ([plugin compatibility](https://github.com/PaperMC/Folia/wiki/Plugin-Compatibility)).

**Second Life / OpenSim** is the cautionary tale. It uses a fixed 256 m grid like ours, and an object lives in exactly one sim ([Nagle, regioncrossing.md](https://github.com/John-Nagle/lslutils/blob/master/regioncrossing.md)). Documented failures:
- **Handoff freeze** of 0.5-10 s while the viewer extrapolates, producing rollovers and sinking.
- **"Potholes"**: the body falls off the sim edge, then is depenetrated on arrival. This is our `depenetration_lift`.
- **Half-unsit**: the vehicle crosses but a rider does not.
- **Double crossings** near four-sim corners at speed. `tc netem` delay reproduces them on demand.

A 2020 server fix improved things from "almost impossible to complete more than two or three crossings" to about 25 in a row ([Modem World](https://modemworld.me/2020/08/13/second-life-region-crossings-update/)), and flags were still being lost on crossing in 2024 ([release notes](https://releasenotes.secondlife.com/simulator/2024-09-06.10740986115.html)). Physics over sim borders is a standing feature request ([feedback](https://feedback.secondlife.com/feature-requests/p/make-physics-work-over-sim-borders)).

**EVE Online** makes crossings **rare, discrete and rate-limited**.
- Crossings are gate or dock "session changes" behind a session timer, which CCP calls a "legacy technical" consistency guard ([CCP](https://www.eveonline.com/news/view/death-to-the-session-change-timer)).
- Overload is handled by **Time Dilation**: a system's clock slows (down to 10%) instead of being split ([TiDi](https://eveonline.com/article/introducing-time-dilation-tidi)).
- Node assignment is static, and a fight crashed a node that chance had shared with a staging system ([X47L-Q](https://forums.eveonline.com/t/regarding-the-fleet-fight-in-x47l-q/398872)).

**SpatialOS (Improbable)**:
- It used per-component single-writer authority that moves as entities move ([primer](https://www.sto.nato.int/publications/STO%20Meeting%20Proceedings/STO-MP-MSG-175/MP-MSG-175-01P.pdf)).
- The API churned around handover: `AuthorityLossImminent` was removed, the `Handover` property was deprecated, and cross-server RPCs came to need a **sender actor as the ordering anchor** ([Unity GDK](https://www.github.com/spatialos/gdk-for-unity/blob/master/UPGRADE_GUIDE.md), [Unreal GDK](https://www.github.com/spatialos/UnrealGDK/releases)).
- Worlds Adrift and Mavericks died. Cost and difficulty were cited, but there is no technical postmortem ([PCGI](https://www.pcgamesinsider.biz/news/69671/bossa-doesnt-blame-improbable-for-worlds-adrifts-cancellation/)).

**Star Citizen server meshing** keeps single-writer authority behind a replication layer. Containers (a ship and everything on it) are the authority unit ([Q&A](https://api.star-citizen.wiki/comm-links/18397)). Static meshing shipped in late 2024 with transit desyncs ([4.0 notes](https://starcitizen.tools/Update%3AStar_Citizen_Alpha_4.0.0)). In 2026, about a decade in, it is "quasi-dynamic" and territories are still not subdivided.

**Others:**
- **New World** gives each hub two non-adjacent cells, so a hub never borders itself ([AWS](https://aws.amazon.com/blogs/gametech/the-unique-architecture-behind-amazon-games-seamless-mmo-new-world)).
- **WoW sharding**: mobs cannot path across shard boundaries, players pop in and out at borders, and a layer is "sharding with a lot of sticky tape" ([PC Gamer](https://pcgamer.com/the-server-tech-that-saved-wow-10-years-ago-is-causing-problems-for-classics-season-of-discovery-dev-reveals-a-layer-is-actually-just-sharding-with-a-lot-of-sticky-tape)).
- **Vanilla Minecraft** duplicates and loses entities at a mere *save-unit* border ([MC-145467](https://bugs.mojang.com/browse/MC-145467), [MC-150170](https://bugs-legacy.mojang.com/browse/MC-150170), [MC-16009](https://bugs-legacy.mojang.com/browse/MC-16009)).
- **Factorio**: shared RNGs, ID allocators and statistics are why surfaces won't be threaded, and a threading determinism bug survived from 2017 ([FFF #415](https://devtrackers.gg/factorio/p/3f33c62d-friday-facts-415-fix-improve-optimize)).
- **Bevy `big_space`** warns that cell recentring is a discontinuity camera controllers don't expect ([docs](https://docs.rs/crate/big_space/0.11.0/source/README.md)).

### 2.2 Theory

- **Colyseus** (NSDI'06): one primary copy plus read-only replicas found by range queries ([paper](https://www.usenix.org/legacy/event/nsdi06/tech/full_papers/bharambe/bharambe_html/main.html)). These are our ghosts, generalised.
- **Orleans virtual actors**: stable ids, a directory, and forwarding of misrouted messages. The single-activation guarantee still admits transient duplicates under failure ([docs](https://learn.microsoft.com/dotnet/orleans/deployment/multi-cluster-support/global-single-instance)).
- **Parallel discrete-event simulation (PDES)** is the precise theory for "deterministic partitions with their own clocks that exchange time-stamped messages":
  - **Time Warp** (Jefferson 1985, [TWOS](https://lasr.cs.ucla.edu/fmg-members/reiher/Time_Warp.html)) is optimistic. A straggler message from the past forces a rollback, plus **anti-messages** that cancel what the undone timeline sent, and those can cascade.
  - **Conservative** synchronisation (Chandy-Misra-Bryant) processes only messages that are provably final. It relies on **lookahead**: a guarantee that a partition's output for time T+L depends only on its own state up to T.

  "Cross-region effects take effect at tick+1" is exactly lookahead 1.
- **Croquet**: a reflector timestamps every external event, so replicas never roll back.
- **No shipped game** was found that does rollback prediction across several interacting deterministic partitions. Our design is past the published state of the art. PDES is the literature to borrow from.

### 2.3 Recurring bug classes, and which we already have

| # | Class | Seen in | Here |
|---|---|---|---|
| 1 | Duplication or loss at handoff | Minecraft, Orleans, SL | arrival lost to a dead thread; replay path; history absorption |
| 2 | Component state that doesn't travel or is wrong after travel | BigWorld extras, SL flags | tick-stamped `Motion`/`Slime`, `inventory_open`, hand-written bundle |
| 3 | Composite entities split across the seam | SL half-unsit, OpenSim attachments | none yet (no mounts or carried bodies), but voxel bodies and piles are already refused |
| 4 | Re-crossing before the handoff completes; corners | SL double crossings | held-input home oscillation; corner walk tests |
| 5 | Interactions and queries straddling the seam | WoW, SC | raycast, A*, pickup, chest reach, felling, edit ring |
| 6 | Physics contacts across seams | SL potholes | no neighbour terrain, `depenetration_lift`, one-way ghosts |
| 7 | Timing / clock discontinuity at the crossing | SL freeze, `big_space` | untimed arrival, convergence test red |
| 8 | Cross-partition message ordering | SpatialOS sender actor | inputs in flight to the old home, misrouted input |
| 9 | Code that assumed one space and synchronous access | Folia plugins, Factorio globals | every gameplay system (§1.5) |
| 10 | Rollback across partitions | (PDES theory only) | no spanning transaction; unrecoverable predicted flip |

---

## Part 3: Proposal — a seam-transparent simulation layer

### 3.1 Goal and non-goals

**Goal.** Gameplay code (movement, AI, orders, mining, piles, felling, combat, hauling, fluids later) is written against a **world** API and never sees region keys, region-local coordinates, region ticks, ghosts or handoff. A seam may cost at most a **uniform, documented latency**: the same latency everywhere, not only at seams. Correctness at seams is then a property of the substrate, tested once.

**Non-goals.**
- Not one physics world, and not a global lock: regions stay the threading, rollback and persistence unit.
- Not Folia-style dynamic region merging. The region stays the fixed determinism unit. Merging would mean re-partitioning the rollback state, which the deterministic undo log makes very expensive. The halo plus intents below give the same effect for gameplay code.

### 3.2 Design pillars

Each pillar addresses specific bug classes from §2.3.

**P1. A shared tick numbering, not a shared tick execution.** (Fixes classes 7 and 10, and part of 2 and 8.)

What it means:
- Tick number N means the same moment of game time in every region: the world epoch + N × `TICK_RATE`. It replaces the per-region genesis counter.
- A region woken at wall time "now" starts at `N_now`, whether it is generated or restored.
  - A restored region jumps from its parked tick P to `N_now`.
  - Absolute deadlines that fell in between expire at once, meaning "time passed while unloaded". No per-field conversion is needed.

What it does **not** mean:
- No region waits for any other. Each region thread keeps its own deadline timer, paced against the shared wall clock.
- A region that misses deadlines **catches up**: it runs the late ticks back to back, up to a cap. Today it skips them and its counter drifts from wall time.
- A region that stays overloaded falls behind and simply runs at tick `N_now − lag`: local time dilation, like EVE's TiDi, for that region only.
- Nobody else is held back. Any cross-region message that reaches a region late is handled by P4's stamping rule, never by waiting.

What it buys:
- Every tick-stamped field (`last_jump_tap`, the slime timers, mining cooldowns) is meaningful in any region without conversion.
- An input stamp means the same tick everywhere.
- The client's regions all tick the same numbers against one clock. That removes "9 `SyncClock` streams fight over one rate" (TODO.md:19), because there is one server clock to track, not nine.

**P2. Stable entity identity and a location directory.** (Fixes classes 1, 8 and 9.)
- Every crossing-capable entity gets an `EntityId` that is unique for the world's life. The id is allocated deterministically, e.g. `(birth region, birth world tick, per-region counter)`, so client prediction allocates the same id as the server and the identity match no longer depends on slotmap keys agreeing.
- Region-local `EntityKey`s stay inside the substrate.
- The manager keeps `EntityId → (region, since tick)` with a short history window (Orleans' directory).
- Input and intents are addressed **by `EntityId` and world tick**, never by "the client's home". The server routes an input stamped T to whoever owned the entity at T, and forwards it if the entity has since left. This is the "server-side input stamp-routing" the July follow-up said the local-crossing problem needs.

**P3. Migration policy declared per component and enforced by the macro.** (Fixes class 2.)
- Every rollback ECS component must carry `#[migrate(carry | reset | local)]`. The `#[rollback]` expansion refuses a component without one.
  - `carry`: copied as is. Safe for tick fields once P1 lands.
  - `reset`: set to `Default` on arrival, e.g. `open_container` or a mining target in region cells.
  - `local`: never travels; the substrate rebuilds it, e.g. the rapier handle.
- The bundle becomes a generated component bag, which replaces the two hand-maintained lists. Body state (linvel, angvel, sleep, mass props, shape) is one `BodyState` that carries every collider shape.
- This absorbs part A of `plans/2026-10-29-entity-seam-crossing.md`.

**P4. Transfers land at T+1 when they can, and the receiver stamps when they actually land.** (Fixes classes 1, 4, 7 and 10.)

**Rule.** Something sent from region A during tick T (a departure, a halo delta or an intent) is applied by region B at the start of tick `max(T+1, B's next tick)`. B **records** that tick as an ordinary event in its own authoritative stream.

The two cases:
- **B is at or behind T.** This is the normal case: regions paced by one clock are within a tick of each other.
  - The message is a future event. B queues it and applies it at exactly T+1.
  - The client predicted T+1, so the prediction is exact and nothing mispredicts.
- **B is already past T+1.** This happens only when A is lagging (overloaded, or catching up). The message is in B's past.
  - The server never rolls back, so B applies it at its next tick, and the stamp in B's stream says which tick that was.
  - The entity has effectively been in flight for the length of A's lag: one hitch, confined to things leaving the slow region.
  - The client predicted T+1. It learns the real tick from B's stream and rolls back and replays, which is what rollback netcode is for.

**There is no barrier.**
- No region ever waits for a neighbour, so lag does not propagate. An overloaded region dilates alone, and its neighbours only see late messages from it.
- **Determinism comes from each region's event stream, not from timing.** Every outside effect on B enters B's stream at one definite tick, and the client replays that stream. This is the server-authoritative counterpart of PDES's two options:
  - *conservative* (wait for neighbours): rejected, because waiting is transitive and becomes world-wide lockstep;
  - *optimistic* (roll back on a late message, Time Warp): rejected on the server, because it needs anti-messages and cascading rollbacks between regions.

  The cost is moved to the client instead: an occasional misprediction at a crossing, and only while some region lags.

**The rest of P4:**
- In-flight state (`Departed at T`, not yet applied) lives in the directory. The conservation invariant "every `EntityId` is owned by exactly one region, or in flight" becomes assertable at every manager step.
- In-flight state (`Departed at T`, not yet applied) lives in the directory. The conservation invariant "every `EntityId` is owned by exactly one region, or in flight with a known arrival tick" becomes assertable at every manager step.
- Arrivals are idempotent by `(EntityId, transfer seq)`. A replay overwrites the **whole** bundle, not only the pose.
- A dead target thread re-receives its in-flight arrivals from the directory instead of losing them.
- **Corners and re-crossing.** Ownership is decided once per tick from the position at the end of the tick, so an entity is in exactly one region per tick and a diagonal move goes A→D directly. Keep a small dwell or hysteresis to avoid flip-flopping, measured in ticks, not 1/8 blocks.

**P5. The halo: read-only neighbour state of a fixed width.** (Fixes classes 5 and 6.)
- Each running region holds a read-only mirror of its neighbours within `HALO_BLOCKS` of each edge. This is a generalisation of ghosts:
  - **terrain chunks**: deferred part B of the October plan;
  - **entity snapshots**: today's ghosts, now keyed by `EntityId` with full kind and shape;
  - **piles and containers**.
- Halo deltas follow P4's rule. In the normal case the halo at tick T+1 is exactly the neighbour's state at the end of T. When the neighbour lags, the halo is as stale as that lag, and the stamp records it. Either way it is part of the receiver's event stream, so the client replays it exactly. It needs no TTL: a delete is an explicit delta.
- **Width rule** (BigWorld's ghost distance = AoI): `HALO_BLOCKS ≥ max interaction reach + max speed × 1 tick + largest body half-extent`. Today that is about reach 6 + a sprint step + about 1 = **8 blocks**, which costs about 13% extra area per 256 m region. Re-measure memory before committing.
- Physics steps against halo terrain as fixed colliders and halo entities as kinematic bodies. A body walking across the seam therefore has a floor, and `depenetration_lift`'s seam use goes away.
- **Static guard:** an interaction whose reach exceeds the halo panics in debug. Large effects (explosions, long rays) must go through P6 as region-spanning intents.

**P6. Every write to another entity or to the world is an intent.** (Fixes classes 3, 5 and 9.)
- Gameplay reads anything in its region ∪ halo through one `WorldView` (below).
- **Mutations of anything the caller does not own** (another entity's health, a pile, a block, a chest) are `Intent`s addressed by `EntityId` or `WorldBlockPos`.
- **Recommended:** apply every intent at the start of T+1 (P4's rule), **including within one region**. Behaviour is then *bit-identical* whether or not a seam lies between actor and target, which is what makes "write it like normal code" true rather than mostly true. The cost is one tick (50 ms) of reaction latency on entity-to-entity effects.
- **[DECIDE]** The alternative applies same-region intents immediately and cross-region ones at T+1. It is snappier but reintroduces seam-only behaviour.
- **Self-writes stay immediate.** An entity moving, changing its own inventory or its own animation is not an intent.
- **Conflicts** (two intents taking the same pile item at T+1) resolve deterministically at the owner, ordered by `(tick, source EntityId, seq)`, which is SpatialOS's ordering-anchor lesson. The loser receives a deterministic refusal.
- **Composite entities** (a future mount, carried items, a felled tree's body) migrate as **one bundle**. A group crosses when its root crosses, and its members are not independently ownable. This prevents SL's half-unsit.

**P7. World coordinates in gameplay APIs.**
- Gameplay uses `WorldBlockPos` (i64 blocks) and `WorldPos` (region + local f32, or an i64 fixed-point), similar to `big_space`'s cell + offset.
- The substrate converts at its edge.
- Region-local types stay for the substrate and the renderer.

### 3.3 The interface (sketch)

```rust
/// What gameplay systems get. No RegionCoords, EntityKey, ghost or local tick.
pub trait WorldView {
    fn tick(&self) -> WorldTick;
    fn block(&self, p: WorldBlockPos) -> Block;              // own region ∪ halo; implied ground included
    fn entity(&self, id: EntityId) -> Option<EntityRef<'_>>; // owned or halo snapshot, read-only
    fn entities_in(&self, aabb: WorldAabb) -> impl Iterator<Item = EntityRef<'_>>;
    fn raycast(&self, ray: WorldRay, max: f32) -> Option<Hit>; // panics in debug if max > halo
    fn path(&self, from: WorldBlockPos, to: WorldBlockPos) -> PathPlan; // see below
    fn rng(&self, subject: EntityId, salt: u64) -> u64;      // stateless, world-tick keyed
}

/// Mutation of the caller's own entity: immediate, undo-logged as today.
pub struct Me<'w> { /* &mut to the owned entity's components */ }

/// Everything else: deferred to T+1 at the owner, deterministic order.
pub trait Effects {
    fn send(&mut self, target: Target, intent: Intent); // Target::Entity(EntityId) | Target::Block(WorldBlockPos)
    fn spawn(&mut self, at: WorldPos, bundle: SpawnBundle) -> EntityId; // id allocated now, entity exists at T+1
}
```

**Pathfinding.** Paths longer than the halo become a two-level plan:
- a coarse route over the region graph from `WorldMap`/`ColumnGround` recipes, which every region can evaluate identically;
- fine A* inside the region ∪ halo, re-planned per region as the walker does today.

`PathPlan` hides that split from the caller. The current `edge_toward` / `resume_walker_safe` logic becomes its implementation, not walker code.

**Enforcement.** Gameplay systems take `(&dyn WorldView, Me, &mut dyn Effects)` and **cannot** name `GameData` internals. Module visibility enforces this: the substrate is a module whose internals are `pub(crate)` only to the substrate.

### 3.4 What changes for the existing special cases

| Today | After |
|---|---|
| `reset_region_local`, tick-stamped bundle fields | `#[migrate]` per component + world tick |
| hand-written `EntityBundle` (capsule only) | generated component bag + `BodyState` (all shapes) |
| `evict_orphan_local_inputs`, `Misrouted`, `home_region` flip | inputs addressed by (`EntityId`, tick), routed by the directory's history |
| ghosts with TTL, one-way, 2 m | halo deltas at T+1, explicit deletes, width from the reach rule |
| `depenetration_lift` at seams, potholes | halo terrain colliders |
| edge ring not diggable | the block intent goes to the owning region, which materialises its own implied ground |
| pile clamp, region-only pickup | piles keyed by `WorldBlockPos`; pickup = intent; piles live in the owning region |
| tree tip margin, voxel bodies settle at edge | voxel bodies become migratable entities (generated bundle, compound shape) |
| slime leash (arrivals wake regions) | a wake policy in the substrate: **[DECIDE]** wake, hold at the seam, or absorb, per kind |
| walker `edge_toward` / re-plan | `PathPlan` |
| water prototype seam flux | rides the same boundary outbox (fluid deltas are halo deltas) |

### 3.5 Testing: seam translation invariance

Pillars P1, P4 and P6 make a strong test oracle possible. **Any gameplay scenario, translated so that it straddles a seam or a four-region corner, must end in the same world-coordinate state, bit for bit, as when it sits in the middle of a region.** It is one harness, applied to every gameplay test, rather than a seam test per feature:

- `scenario.run_at(center_offset)` vs `scenario.run_at(seam_offset)` vs `scenario.run_at(corner_offset)`; compare a world-space state hash.
- Run it under `InlineSchedule` jitter and on real threads (`just test-threads`).
- Add a fault layer in the spirit of SL's `tc netem`:
  - delayed or dropped boundary outbox;
  - region park, stop or thread death mid-transfer;
  - double crossings at corners;
  - save during a transfer (the Minecraft class).
- Assert the conservation invariant (P4) on every manager step.

The oracle is bit-exact only when no region lags. That holds on the lockstep `InlineSchedule` and on real threads that keep up. Under jitter, P4's stamping can delay a cross-region effect by a region's lag. There, assert the weaker property instead: same set of entities and items, and no duplication or loss. Bit-exactness is not expected because the delay changes timing.

Under today's model even the lockstep version fails by design, because arrival timing and region-bounded queries make seam behaviour differ. That makes it a good acceptance test for the migration.

### 3.6 Costs and risks

- **Late cross-region messages (P4).** There is no barrier, so a lagging region costs a hitch to whatever leaves it, and client mispredictions at those crossings. Nothing propagates. Measure how often messages arrive late (count `stamped > T+1` per region) with `just fly-probe` / `shuttle-probe` and an artificially slowed region.
- **Halo memory and traffic (P5).** About 13% more chunk state per region at 8 blocks, plus delta traffic for changes near seams. The client already holds the 3×3 neighbourhood, so on the client the halo is mostly a *view* of regions it already predicts.
- **One-tick intent latency (P6).** 50 ms for hits and pickups. That is fine for DF-style play and noticeable for Minecraft-style melee; **[DECIDE]**.
- **Migration effort.**
  - P1 + P3 are mechanical, and P1 alone fixes the tick-stamped-field bug class.
  - P2 + P4 replace the crossing core. They are the riskiest step, and the one that should turn `client/tests/crossing.rs` green.
  - P5 and P6 are incremental: one system at a time can move onto `WorldView`.
- **Determinism and wasm.** New ids and orderings must be integer and ordered-container only. `just determinism-wasm` after each step.

### 3.7 Suggested order

1. **P1 world tick.** Small, removes a latent bug class, and is a prerequisite for the rest.
2. **P3 `#[migrate]` + generated bundle.** Folds in the October plan's part A.
3. **P2 + P4: ids, directory, deterministic T+1 transfer, (`EntityId`, tick) input routing.** Delete eviction, `Misrouted` and the predicted home flip. Acceptance: crossing convergence green, held-input oscillation gone, conservation invariant asserted.
4. **Seam-translation harness** (§3.5) on the existing movement and walker tests.
5. **P5 halo:** terrain first (physics, raycast, edits, x-ray), then entities (replacing ghosts).
6. **P6 `WorldView` / `Effects`:** migrate systems one by one (piles, chests, mining, felling, people, slimes), deleting each row of §1.5's table.

### 3.8 Open decisions

1. **[DECIDE]** Intents at T+1 everywhere (bit-identical seam behaviour, +50 ms) vs only across seams.
2. **[DECIDE]** The catch-up cap for a region that missed deadlines: how many late ticks to run back to back before it accepts running behind wall time (time dilation).
3. **[DECIDE]** Wake policy per entity kind when crossing into a non-running region: wake (players, today), hold at the seam, or absorb (people today, abstract history).
4. **[DECIDE]** Halo width: fixed 8 blocks vs derived from the largest registered reach.
5. **[DECIDE]** Whether this supersedes `plans/2026-10-29-entity-seam-crossing.md`. The recommendation is yes: its part A becomes step 2, its part B becomes step 5.
