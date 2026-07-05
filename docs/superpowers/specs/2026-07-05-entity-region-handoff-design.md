# Entity/Region Handoff with Ghost Margins — Design

**Date:** 2026-07-05
**Status:** Approved (brainstormed with user; decisions below are theirs)
**Builds on:** `2026-07-05-multi-region-world-design.md` (regions as sim instances on threads,
hub-and-spoke manager, region-local coordinates, park/restore lifecycle)

## Goal

Entities move between region sims as a first-class mechanism. Players are entities with a
client attached — nothing about the transfer is player-specific; home rerouting is layered on
top. Near a boundary an entity is mirrored as a **ghost** into the adjacent region(s); when it
crosses the boundary (with hysteresis) **ownership flips**: the old region deterministically
extracts it during its own tick, and the new region receives it as an explicit arrival event.
All communication stays on the existing channel seams (`RegionInput`/`RegionOutput`, the
future network boundary); no region ever reads another's state, and no cross-region tick
synchronization is introduced.

## Decisions (locked with user)

1. **Fully entity-generic.** The boundary check and ghost margin apply to every owned entity,
   not just players. Rejected: player-only handoff (bakes "players are special" into the
   protocol, against the entities-are-entities direction); generic-mechanism-player-trigger
   (chosen scope is the full mechanism now).
2. **Arrivals wake cold regions.** An entity flipping into a non-running region triggers
   `ensure_running` (parked blob or worldgen); the normal grace period re-parks it if nobody
   subscribes. **Ghost updates never wake a region** — dropped if the neighbor isn't running,
   else a margin-walker would keep all neighbors alive permanently. Rejected: manager-side
   mailbox buffering (entities freeze in limbo, mailboxes need shutdown handling); injecting
   into the parked blob at the manager (manager stops being sim-blind).
3. **Fully predicted on the client.** The client's local sims run the same deterministic
   functions the server regions run: its predicted tick performs the extraction, and it
   synthesizes the same arrival/ghost events into sibling local regions that the server's
   manager relays. Normal per-region reconcile corrects both sides. Rejected:
   server-authoritative crossing with a small snap (user wants seamless).
4. **Ghost margins with ownership flip** (approach 3, event-mirrored — not shared state).
   Ghost poses lag their owner by ~1–2 ticks in-process (arrival-order application under
   independent timers), growing to network latency once regions leave the process. Exact
   simultaneous cross-boundary physics is a non-goal — that would require lockstep ticks,
   which the multi-region design explicitly forbids. Rejected: plain paired events without
   ghosts (no cross-boundary visibility/collision); manager-orchestrated two-phase RPC
   (the decision leaves every region's deterministic stream, so the client cannot predict it,
   and the entity is nowhere between the two messages).
5. **Staged implementation.** Stage 1: mirroring, flip, ghost *rendering* (ghosts have no
   colliders). Stage 2: ghost colliders (cross-boundary collision). One spec, two plan
   stages.

## Why independent timers are not a blocker

The multi-region spec rejected ghost zones as "cross-region tick coordination". That applies
to ghosts as *shared state* (region B reading A's bodies). Ghosts as *events* preserve every
invariant: B's state remains a pure function of B's own input event sequence, which happens
to include `GhostUpdate` events that A's ticks produced. No shared clock exists or is needed;
the only cost is that a ghost's pose is as old as the channel hop plus the tick phase
difference — bounded staleness, not lost correctness. The rollback bar holds because ghost
mutations are ordinary undo-tracked operations.

## Constants (in `game`)

- `GHOST_MARGIN: f32 = 32.0` — owned entities within 32 units of a region edge mirror into
  that neighbor; at a corner an entity mirrors into up to 3 neighbors.
- `FLIP_HYSTERESIS: f32 = 2.0` — ownership flips only when the pose is 2 units *past* the
  boundary, so flipping back requires 4 units of travel. No flip thrash.
- `GHOST_TTL_TICKS: usize = 25` — a ghost not refreshed for 25 host ticks (~0.5 s) is removed
  by the host region's tick. Covers the owner region parking, dying, or the entity leaving
  the margin.

## Protocol

### Transfer payloads

```rust
/// The unit of transfer. Assembled deterministically from region state at
/// the extraction tick; translation is region-local to the SOURCE until the
/// relay rebases it.
struct EntityBundle {
    kind: EntityKind,
    isometry: Isometry3<f32>,
    body: BodyState,        // body kind + linear/angular velocity + kinematic targets
    collider: ColliderSpec, // capsule dims for players; extensible per kind
    camera: Option<CameraState>,
    client: Option<ClientId>, // set for player entities; drives home rerouting
    source_region: RegionCoords,
    source_key: EntityKey,    // identity for ghost upgrade + idempotency
}

/// Lightweight per-tick mirror of a margin entity.
struct GhostData {
    source_region: RegionCoords,
    source_key: EntityKey,
    kind: EntityKind,
    isometry: Isometry3<f32>,
    velocity: Velocity,
}
```

### New event kinds (`GameEventKind`)

- `EntityArrived(EntityBundle)` — input to the receiving region; appears in its
  authoritative stream, so clients and the wasm `LocalServer` replay it for free.
- `GhostUpdate(GhostData)` — same, for mirrors.

There is **no `EntityDeparted` event** in the source region's stream: extraction happens
inside `Tick` processing as a deterministic, undo-tracked mutation. Any replay of that tick
reproduces the extraction bit-exactly, which is what lets the client predict it.

### New region outputs (`RegionOutput`)

Emitted by the runner after each tick, only when non-empty:

- `Departures(Vec<(EntityBundle, RegionCoords /* target */)>)`
- `GhostUpdates(Vec<(GhostData, RegionCoords /* target */)>)`

These ride the existing output channel next to `EventProcessed` — the future network seam.

### Manager relay (the only new manager logic; still sim-blind)

For each departure `(bundle, target)`:
1. Rebase `bundle.isometry.translation` by `source.world_offset() − target.world_offset()`
   (exact in f32; offsets are multiples of 256). The rebase helper lives in `game` so the
   client's predicted synthesis uses the identical code path.
2. `ensure_running(target)` (parked blob or worldgen — the wake decision).
3. `send_to_region(target, RegionInput::Event(GameEventKind::EntityArrived(bundle)))`.
4. If `bundle.client` is `Some(c)`: set `homes[c] = target`, push
   `ServerPacket::PlayerRegion(Some(target), c)` to that client, and refresh keep-alive on
   the old home region (it may have just lost its keep-alive reason).

For each ghost update `(data, target)`: rebase and forward **only if `target` is running**,
else drop silently.

### Input routing becomes manager-authoritative

`PlayerInput` events route to `homes[client]`, ignoring the region id the client stamped.
During the client's predicted-but-unconfirmed handoff window its inputs still land in the
server's current authoritative home region — exactly what the server sim needs; the client's
prediction reconciles per normal.

## Sim internals

### Region tick: boundary/margin scan

After the controllers step, the tick scans owned entities (ghosts excluded):

- Pose past a boundary by more than `FLIP_HYSTERESIS` → **extract**: assemble the
  `EntityBundle`, then remove the entity through undo-tracked operations only — collider
  (snapshot `change()`; no exact ColliderSet inverse exists), body (`revert_remove`, exact
  LIFO inverse), components (camera, kind, isometry), `player_entites` entry if a player,
  and finally the ECS slot, whose removal auto-emits `RemoveEntity` to the renderer.
- Pose within `GHOST_MARGIN` of an edge → produce `GhostData` per adjacent neighbor
  (up to 3 at a corner).

Departures and ghost data are buffered on the region; the runner drains the buffers after
`handle_event(Tick)` into the new `RegionOutput`s. The client's game loop drains the same
buffers from its *local* regions after predicting a tick.

### Injection: `EntityArrived`

Generalizes `create_player_safe`: allocate the ECS entity, insert body + collider from the
bundle via the same undo-safe insert paths, set kind/camera, and register in
`player_entites` when `client` is set.

**Upgrade-in-place:** if the receiving region already holds a ghost for
`(source_region, source_key)`, the arrival converts that entity — attach body/collider, flip
kind from Ghost, drop the ghost record — instead of remove+create. The `EntityKey` is
preserved, which is what makes the crossing visually seamless on every observing client.

**Idempotency:** if an owned entity for `(source_region, source_key)` already exists (e.g. a
replayed arrival after a respawn-resnapshot), the arrival applies as a pose/velocity
correction, not a duplicate create.

### Ghost lifecycle

`ghosts: BTreeMap<(RegionCoords, EntityKey), GhostEntry>` in region state (undo-tracked),
where `GhostEntry { entity: EntityKey, last_update_tick: Tick }`.
`GhostUpdate` upserts: create the ghost entity on first sight (stage 1: kind + pose only, no
collider; stage 2 adds the collider from `GhostData`), update pose/velocity subsequently,
stamp `last_update_tick`. The tick removes ghosts with `last_update_tick` older than
`GHOST_TTL_TICKS`. Ghosts are excluded from the boundary/margin scan (no ghost-of-ghost, no
ghost departures).

## Client

- **Predicted synthesis:** after predicting a tick in a loaded region, drain its
  departure/ghost buffers and synthesize `EntityArrived`/`GhostUpdate` into the sibling
  local regions (rebased with the shared helper) as predicted events on those regions'
  timelines. Authoritative streams correct them via existing per-region reconcile.
- **Predicted home flip:** on a predicted extraction of the local player, immediately
  re-point `home_region` and input routing to the target (safe — the server routes input by
  its authoritative `homes` regardless). `ServerPacket::PlayerRegion` confirms or corrects.
  Window pinning follows `home_region`; the old home unpins and releases when it leaves the
  3×3 window.
- **Ghost render dedupe:** a ghost is rendered only when its source region is not loaded
  locally. With both regions in the window the client renders the owned copy in A and hides
  A's ghost in B (sim state keeps the ghost either way — stage 2 colliders must match the
  server's sim exactly; only rendering dedupes).
- **Bridge continuity:** upgrade-in-place keeps the same `(region, key)` in the target
  region, so the bevy entity, `SimTarget`, and interpolation survive the flip untouched.
  The source region's `RemoveEntity` despawns the old copy as usual.
- **Wasm `LocalServer`:** inherits everything through the shared `WorldManager` core and
  `RegionRunner` — no wasm-specific handoff code.

## Error handling

- **Source region dies after emitting a departure:** the bundle is already in the manager's
  hands; the arrival proceeds. The source respawns from park/worldgen per existing
  machinery (that tick's edits lost — pre-persistence status quo).
- **Target region dies before consuming an arrival:** the event dies with the input channel;
  respawn re-snapshots subscribers and the entity is lost with the unconsumed event — logged
  loudly (`error!`). This is the existing parked-persistence gap, not a new one; durable
  persistence remains future work.
- **Ghost staleness/orphans:** handled structurally by `GHOST_TTL_TICKS` — owner parks,
  dies, or leaves the margin → ghosts expire deterministically in the host's tick.
- **Client predicted a flip the server never confirms** (or confirms at a different tick):
  undone by normal per-region reconcile; `PlayerRegion` remains the authoritative home
  signal. Ghost updates/arrivals for regions the client doesn't hold are dropped-and-logged
  at debug level like all unknown-region events.
- **Manager shutdown:** unchanged — `Shutdown` to all regions; in-flight departures drain
  through the manager loop before join (the loop processes region outputs until `Stopped`).

## Testing

1. **Headless two-region harness** (`game`, `WorldManager` + `InlineSpawner`): drive an
   entity across a boundary via input events — assert extraction in A, arrival in B, `homes`
   update + `PlayerRegion` push, cold-target wake, grace re-park with the transferred entity
   inside, hysteresis (oscillation at the line causes exactly one flip), corner case
   (3 ghost targets), ghost TTL expiry.
2. **Rollback bars:** `hash(before) == hash(after undo)` across a tick that extracts; across
   `EntityArrived` (fresh, upgrade-in-place, and idempotent-correction paths) and
   `GhostUpdate`; park/restore of a region holding ghosts is bit-exact.
3. **Determinism:** identical input streams on two separate runs produce identical departure
   buffers, bundles, and state hashes — the property client prediction depends on.
4. **Client headless suite:** predicted flip synthesizes the arrival locally and survives
   reconcile against a differing authoritative stream; ghost render-dedupe; upgrade-in-place
   preserves the bevy entity; input reroutes on predicted home change.
5. **Threaded integration** (`server`): scripted client walks across a boundary on real
   threads. Manual acceptance: two clients either side of a boundary see each other
   (stage 1), bump into each other (stage 2); one walks across with no visible hitch;
   walking in a large circle through four regions returns home with terrain edits intact.

## Out of scope

Durable persistence of in-flight bundles and parked regions; cross-region interactions
beyond collision (combat, item pickup across the line); ghost updates waking cold regions;
per-region adaptive tick rates; networked region servers (the seam is respected, not
implemented); NPC AI — the mechanism is generic but players are the only movers until NPCs
exist.

## Risks / notes

- **Ghost event volume:** per tick, one `GhostUpdate` per margin entity per adjacent running
  neighbor, through the manager. Trivial at current entity counts; if it ever isn't, batch
  per tick (the `Vec` output is already the batch) before considering topology changes.
- **Kinematic bodies and velocity:** the player body is kinematic-position-based, so rapier
  dynamics carry no useful velocity. Definition: bundle/ghost velocity = (pose@tick −
  pose@tick−1) / dt, computed in the source tick's scan. `BodyState` additionally carries the
  camera controller's pending kinematic targets so the receiving region continues motion
  without a hiccup.
- **EntityKey non-portability:** keys are per-region slotmap allocations; `source_key` is
  carried only as an identity token for ghost upgrade/idempotency, never dereferenced in the
  target's slotmap.
- **Two sources of `PlayerRegion`:** connect-time reply and handoff-time push. The client
  must treat it as "current home" idempotently, not as a connection-phase-only message.
- **Prediction divergence at the flip tick:** the client may extract at predicted tick T
  while the server extracts at T±k (input arrival differences). Reconcile handles it, but
  the interpolation carry-over should tolerate the owned entity briefly existing in both
  local regions (predicted B copy + not-yet-corrected A copy) — the render dedupe rule
  covers the visual; the sim copies converge on reconcile.
