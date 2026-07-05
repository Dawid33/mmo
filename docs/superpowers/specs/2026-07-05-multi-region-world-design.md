# Multi-Region World (Sim Instances on Threads) — Design

**Date:** 2026-07-05
**Status:** Approved (brainstormed with user; decisions below are theirs)

## Goal

Multiple regions, each a self-contained sim instance running on its own thread, cooperate as
one "world". A game management thread (the evolved `WorldIngress`, here `WorldManager`) routes
between clients and regions and cycles regions in and out as players move, so a client only
ever loads the relevant portion of the world — a 3×3 window of regions around the viewer —
and receives events from those regions to stay up to date.

All cross-thread communication is via channels, never shared memory: the thread boundary
around a region is the seam that later becomes a network boundary (region servers behind the
world ingress). The purpose of the design is that nothing built now has to be rebuilt when
that seam moves out of process.

## Decisions (locked with user)

1. **Thread scope: server only.** The server spawns one thread per running region. The client
   stays a client: its game thread keeps a local multi-region `World` for prediction/rollback,
   and the wasm build keeps its single-threaded pump.
2. **Coordinates: region-local sim, offset at the boundary.** Regions simulate near the origin
   exactly as today (uniform f32 precision everywhere, translation-invariant regions, hashes
   independent of world position). The world offset exists only in the client render bridge
   (region root `Transform`) and, later, in handoff rebasing.
3. **Entity handoff: deferred.** This milestone roams via free-cam; the player entity stays in
   its home region. Handoff gets its own spec on top of this substrate.
4. **Tick pacing: fully independent timers.** Each region thread paces itself; the manager only
   routes. Clocks between regions do not need to be in sync — correctness never depends on it
   (each region is its own deterministic event stream), and per-region `SyncClock`
   (tick ↔ wall-time) is the future basis for cross-region time mapping. Tick *rates* should
   stay equal (`TICK_RATE`); note that per-region adaptive rates would mean visible time
   dilation between regions once things cross boundaries.
5. **World-level responsibilities:** client→region routing, region subscription management,
   and region lifecycle (spawn/generate on demand, park when unused). A general cross-region
   relay is deferred with handoff.
6. **World extent: unbounded, 3×3 active window.** Regions are generated on demand as the
   window moves and parked when left behind; region coordinates are signed and unbounded.
7. **Topology: hub-and-spoke.** All traffic flows client ↔ manager ↔ region. Regions expose
   exactly one input stream and one output stream and never learn about subscribers or the
   network. Rejected: split control/data plane (regions publish straight to the network —
   fewer hops but replicated routing state, subscribe/unsubscribe races, and the fan-out gets
   rebuilt anyway when regions leave the process); regions as tokio tasks (blocking rapier
   steps abuse an async executor; sim stays sync by convention).

## Architecture

```
                  ┌────────────────────────────────────────────┐
                  │ netcode thread (tokio + quinn, as today)    │
                  │ accept, ClientId alloc, (de)serialize       │
                  └───────▲───────────────────────┬────────────┘
   (Option<ClientId>,     │                       │ ServerEvent
    ServerPacket)         │                       ▼
                  ┌───────┴────────────────────────────────────┐
                  │ manager thread — WorldManager               │
                  │ sessions, subscriptions, region registry,   │
                  │ lifecycle + parking lot, routing            │
                  └──▲──────┬─────────▲──────┬─────────▲───┬───┘
     (RegionCoords,   │      │RegionInput     │          │   │
      RegionOutput)   │      ▼                │          │   ▼
                  ┌───┴──────────┐  ┌─────────┴────┐  ┌──┴──────────┐
                  │ region thread│  │ region thread│  │ region thread│ …
                  │ (-1,0): own  │  │ (0,0): own   │  │ (1,0): own   │
                  │ timer + sim  │  │ timer + sim  │  │ timer + sim  │
                  └──────────────┘  └──────────────┘  └──────────────┘
```

- **Region thread loop:** owns one `Region` (existing type, sim behavior untouched) and its
  own tick timer: `recv_deadline(next_tick)` — handle `RegionInput` when a message arrives,
  tick when the deadline fires. This absorbs today's separate tick-generator thread, and
  backpressure is inherent: a slow region ticks late instead of growing a queue.
- **Channel graph (all crossbeam):** netcode→manager `ServerEvent` and the manager→netcode
  `(Option<ClientId>, ServerPacket)` broadcast channel stay as today. New: one
  `Sender<RegionInput>` per region, and one shared `Sender<(RegionCoords, RegionOutput)>`
  cloned into every region thread — the manager selects over two receivers regardless of
  region count.
- **Code placement:** `WorldManager` is a **threadless core** (state + `handle_server_event`
  / `handle_region_output` methods) in the `game` crate — Bevy-free, engine-agnostic,
  headlessly testable. `crates/server` keeps only the shells: thread spawning, region-thread
  timer loop, quinn. The wasm `LocalServer` wraps the same core (see Client section).
- **Client process:** thread structure unchanged (Bevy main / game thread / tick thread /
  netcode thread). `GameInstanceManager`'s local `World` gains and loses regions as the
  window moves; per-region reconcile is unchanged.

## Region coordinates & world layout

- **`RegionCoords { x: i32, z: i32 }` becomes `RegionId`.** Regions tile the horizontal
  plane; vertical space lives within a region (chunks already stack in y). This ends the
  `RegionId = ChunkCoords` conflation — `ChunkCoords` (unsigned) remains chunk indices
  *within* a region. Wire format changes (bincode); no compatibility to preserve.
- **Region size is fixed: 8×8 chunks × 32 voxels = 256×256 units** in x,z (today's
  `World::basic` layout becomes the per-region layout). A region's world offset is
  `(x * 256, 0, z * 256)` — exactly representable in f32, so render offsets and future
  handoff rebases are lossless. Region ownership of a world point is floor-division by 256.
- **The sim never sees world offsets.** Rapier poses, chunks, and all `GameData` stay
  region-local, identical to today.
- **Worldgen becomes real (minimally):** the stub `crates/worldgen` gains
  `generate_region(coords: RegionCoords) -> …`, a pure deterministic function of coords
  producing the region's chunks. `World::basic()`'s flat-floor construction moves behind it.
  Floor height varies by region parity (height 8 on even `x+z`, 12 on odd — a checkerboard)
  so boundaries are visible as steps while roaming — no new voxel types or materials. Deterministic generation
  is what makes "cycle out = park, restore or regenerate on return" safe.

## Region actor protocol

This message pair is the future network seam:

```rust
enum RegionInput {
    Event(GameEvent),          // client events routed by region_id + manager-authoritative
                               // ones (CreateClient)
    RequestSnapshot(ClientId), // new subscriber needs a ServerPacket::Region payload
    Shutdown,                  // graceful stop; region replies Stopped(final state)
}

enum RegionOutput {            // always sent as (RegionCoords, RegionOutput)
    EventProcessed(GameEvent),            // authoritative event (tick result or applied
                                          // client event) → fan out to subscribers
    Snapshot(ClientId, SerializedRegion), // reply to RequestSnapshot → ServerPacket::Region
    SyncClock { tick_rate: u64, tick: Tick }, // every ~10 ticks, self-reported; manager wraps
                                          // into ServerPacket::SyncClock for subscribers
    Stopped(SerializedRegion),            // reply to Shutdown; manager parks it
}
```

`SerializedRegion` is the bincode-serialized region snapshot — the same payload
`ServerPacket::Region` carries today.

Regions never see subscriber lists or network types; the manager owns all fan-out.

## Lifecycle (the "cycling")

**Manager state:** per-client session (home region, subscribed set); region registry
(`RegionCoords → RegionHandle { input sender, join handle, subscribers,
last_subscriber_gone_at }`); parking lot (`RegionCoords → SerializedRegion`).

- **Client connects:** manager picks the spawn region ((0,0) for now), ensures it is running,
  sends `CreateClient` into it, replies `PlayerRegion`. The client then requests its 3×3
  window. Subscriptions are **client-driven**: `RequestRegionConnection(rc)` (exists) plus a
  new `ReleaseRegionConnection(rc)`.
- **Subscribe to a non-running region:** spawn the thread — restore from the parking lot if
  present, else generate via `worldgen::generate_region` — then `RequestSnapshot`, forward
  the `Snapshot` to the client as `ServerPacket::Region`, add the client to the fan-out set.
  Restore reuses the same snapshot path the client already uses for `ServerPacket::Region`.
- **Cycle out:** a region stays alive while it has subscribers **or hosts any connected
  client's player entity** (the window follows the free-cam, which can roam away from the
  player's home region). When neither holds, a grace period (constant, default 5 s) absorbs
  boundary thrash, then `Shutdown` → `Stopped(state)` → park in memory → join the thread.
  Parking preserves terrain edits across roaming; durable persistence is out of scope.
- **Event routing:** `ClientPacket::GameEvent` → manager verifies the region is running and
  the sender is subscribed → forward as `RegionInput::Event`; otherwise drop and log.

## Client changes

- **Window tracking (game thread):** `GameInstanceManager.player_chunk` becomes
  `home_region: Option<RegionCoords>` + `subscribed: BTreeSet<RegionCoords>`. Each tick the
  game thread computes the viewer's world position as home-region offset + the camera's
  region-local isometry (free-cam local coords simply run past region bounds; nothing clamps
  them). Current region = floor-division by 256; desired window = 3×3 around it; the diff
  against `subscribed` emits `RequestRegionConnection` / `ReleaseRegionConnection`. Requests
  only fire when the derived region changes.
- **Region departure:** on release, drop the local `Region` and emit a new
  `ClientUpdateEvent::RemoveRegion(rc)` (arrival already works via `NewRegion`).
- **Render bridge:** (1) `NewRegion` sets the region root `Transform` to the region's world
  offset instead of `IDENTITY` (bridge.rs:74 today). (2) `RemoveRegion` despawns the root
  recursively, removes the region's entries from `Regions` / `RegionRoots` / `SimEntityMap`,
  and drops its update receiver. In-flight async mesh tasks for a removed region are
  tolerated — `apply_meshed_chunks` guards on the target entity still existing.
- **Prediction/reconcile:** structurally unchanged; reconcile is already keyed by
  `region_id`. Neighbour regions are effectively spectated — server `EventProcessed` events
  apply with nothing to roll back. Player input keeps routing to the home region only.
- **Wasm `LocalServer`:** rewritten to wrap the threadless `WorldManager` core plus inline
  region pumping — `pump()` feeds `ClientPacket`s through the core and drains region channels
  synchronously; `tick()` advances every loaded region at `TICK_RATE` off the driver's clock
  (timer independence is a server-deployment property; one clock in the single-threaded build
  is fine). The browser build gets region cycling for free and stops hand-mirroring server
  logic.

## Error handling

- **Region thread death (panic):** detected when the manager's next send into that region's
  input channel fails (plus the stored `JoinHandle`). Log, remove the registry entry, respawn
  — from the parking lot if ever parked, else fresh from worldgen (that tick's edits are
  lost; acceptable pre-persistence) — then push a fresh `ServerPacket::Region` to every
  subscriber. Client-side rule making this work: receiving `Region` for an already-loaded
  region **replaces** it (swap the local `Region`; bridge does `RemoveRegion` + `NewRegion`).
  Replace-on-re-receipt also makes reconnect/resubscribe idempotent.
- **Unsubscribe races:** events for a just-released region, releases for a just-closed
  region, or a snapshot arriving after the client moved on are steady-state noise — both ends
  tolerate "unknown region" by dropping and logging at debug level; the client immediately
  releases a snapshot that lands outside its desired window.
- **Parked-state corruption:** if deserializing a parked region fails on restore, log,
  discard the blob, regenerate deterministically from worldgen.
- **Channels:** unbounded, as today. Region-side backpressure is tick lateness. Server
  shutdown sends `Shutdown` to all regions and joins them.
- **Sim errors:** per-event `Result<GameEvent, GameError>` keeps today's behavior; a
  `GameError` in one region never affects another region's thread.

## Testing

1. **`WorldManager` core tests (headless, in `game`):** drive the threadless core with
   hand-fed `ServerEvent`s and inline-pumped regions — deterministic, no threads. Covers
   connect flow, subscribe → snapshot delivery, window movement driving spawn/park/restore,
   keep-alive rules (home region survives the window leaving; grace period), unknown-region
   tolerance, crash-respawn (simulated by dropping a region's channels).
2. **Park/restore roundtrip holds the rollback bar:** `hash(before park) == hash(after
   restore)`, bit-exact, using the existing crc32 state hashing.
3. **`worldgen` tests:** `generate_region` purity (same coords → hash-equal output); parity
   height variation.
4. **Client headless tests (extend the existing suite):** window-diff logic (camera world
   position → expected subscribe/release stream), `NewRegion` root transform equals the
   region offset, `RemoveRegion` empties all three bridge maps and despawns the root,
   replace-on-re-receipt, in-flight mesh tasks on a removed region don't panic.
5. **Threaded integration smoke test (in `server`):** real region + manager threads, a
   scripted client speaking over channels (bypassing quinn), roaming across several
   boundaries; asserts the running-region set tracks the window and event streams stay
   consistent. Existing sim/rollback suites pass unchanged.
6. **Manual acceptance:** server + one client, free-cam roam across boundaries — floor-height
   steps make crossings visible; regions cycle at the window edge. Wasm build/run check.

## Out of scope

Entity/player handoff between regions, cross-region event relay, durable region persistence
(parking is in-memory only), per-region adaptive tick rates, subscription validation
(clients are trusted about their window), networked region servers, multi-ingress.

## Risks / notes

- **WebTransport spec compatibility:** the 2026-07-05 WebTransport design assumes transports
  feed one `ServerEvent` channel consumed by the main loop. That still holds — the consumer
  becomes the manager thread; transports are unaffected.
- **Manager as serialization point:** every event crosses two channels. At 50 Hz × 9 regions
  this is noise; if it ever isn't, the accepted escalation path is moving fan-out into
  regions (approach 2), not shared memory.
- **`Select` vs shared output channel:** the shared region→manager sender means a dead region
  cannot be detected by receiver disconnect; detection is via failed input-send/JoinHandle
  (see error handling). This is deliberate — it keeps the manager loop at two receivers.
- **`ChunkCoords` audit:** replacing `RegionId = ChunkCoords` touches protocol types,
  `World`'s region map keys, and every call site currently passing `ChunkCoords::new(0,0,0)`
  as a region id (server main loop, client, local server). The type split makes the compiler
  find them all, but expect a wide mechanical diff.
- **Free-cam far-roam precision:** the camera's region-local coords grow as the free-cam
  travels while the home region stays fixed; precision degrades only for the *camera pose*,
  not world state, and handoff (later) resets it. Acceptable for this milestone.
