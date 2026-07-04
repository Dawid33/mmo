# Multi-Client Players: Server-Authoritative Creation + Reconcile Fixes

**Date:** 2026-07-04
**Status:** Approved design, pending implementation

## Problem

Running two clients against one server breaks in two ways:

1. **Both windows end up controlling the same player.** The server assigns
   distinct `ClientId`s per connection, but its outgoing channel broadcasts
   every `ServerPacket` to every connection — including the
   `PlayerRegion(id, client_id)` reply meant only for the requester.
   `GameInstanceManager::handle_server` unconditionally does
   `self.client_id = Some(client_id)` on receipt, so when client B connects,
   client A adopts B's id, re-sends `SetPlayer` to its renderer, and both
   clients drive the same player. Player creation is also client-initiated
   (each client sends `CreateClient` after loading its region), which produced
   duplicate `CreateClient` events during the observed failure.

2. **The reconcile path cannot represent other clients' events.** Server event
   ids form one global sequence, so every event originating from another
   client permanently shifts the id stream relative to the local client's
   predictions. `Region::reconcile` half-handles this (the bump-all-pending-ids
   walk), but the `TODO`s at `region.rs:79` and `region.rs:99` mark what is
   missing, and the removal-by-kind scan can silently delete a local pending
   event when a foreign event's kind matches (most obviously `Tick`), after
   which `event_log` and the undo stack diverge and later rollbacks restore
   the wrong depth.

## Design

### 1. Server networking (`crates/server/src/main.rs`)

- Key the connection map by the assigned `ClientId` (the `next_id` counter at
  accept time) instead of quinn's `stable_id`.
- Change the outgoing channel item from `ServerPacket` to
  `(Option<ClientId>, ServerPacket)`: `None` = broadcast to all connections,
  `Some(id)` = send only to that connection. The broadcast task looks up the
  target connection for directed packets; a directed packet to a
  no-longer-connected client is dropped silently.
- `PlayerRegion` and `Region` replies become directed to the requester.
  `GameEvent` and `SyncClock` remain broadcast.

### 2. Server-authoritative player creation

- New variant `ServerEvent::ClientConnected(ClientId)`, sent by the accept
  loop as soon as a connection is established. The accept task sends it on the
  same crossbeam channel that later carries the client's packets, and the
  client's first request can only arrive over the network afterwards, so
  ordering is guaranteed: the player exists before `RequestPlayerRegion` or
  `RequestRegionConnection` are served, and any region snapshot the new client
  receives already contains its player.
- Handling: if `world.find_player(&id)` is `None`, execute `CreateClient(id)`
  on region `(0,0,0)` exactly like other forwarded game events
  (`handle_region_event` + `forget_last_event`) and broadcast the resulting
  `GameEvent` so already-connected clients spawn the new player in their sims.
  If the player already exists, do nothing.

### 3. Client changes (`crates/client/src/main.rs`)

- Remove the `CreateClient` send after region load and drop `CreateClient`
  from the locally-originated event match arm — clients never originate
  player creation.
- With directed routing, `PlayerRegion` only ever reaches the client it is
  addressed to, so the unconditional `self.client_id = Some(client_id)` /
  `SetPlayer` behavior is now correct and stays as is.
- `new_region` passes `self.client_id` into `Region::new` (see 4a).

### 4. Reconcile path for multiple clients (`crates/game/src/region.rs`)

**4a. Region learns who it belongs to.** `Region` gains
`local_client_id: Option<ClientId>`: `Some(id)` on the client (the
`PlayerRegion` reply arrives before the `Region` snapshot, so the value is
available at construction), `None` on the server (the server never
reconciles). New helper `GameEventKind::origin_client() -> Option<ClientId>`
in `protocol.rs`: `Some(id)` for `PlayerInput(id, _)` and `CreateClient(id)`,
`None` for `Tick` and `Quit`.

**4b. Mismatch branch splits on origin.** When the popped server event and the
front local event share an id but differ:

- **Foreign event** (`origin_client() == Some(other)`,
  `other != local_client_id`): treat as an *insertion* into the local
  timeline, not a misprediction. Roll back the pending log, apply the server
  event, commit it (`forget`), bump **every** pending local event id by 1
  (no removal-by-kind, no "Client must be behind" log — that message becomes
  a genuine anomaly signal), and re-apply the pending log.
  `next_game_event_id` needs no special handling: the rollback restores it,
  the applied server event advances it by 1, and the re-applied local events
  advance it further, which is exactly what makes the +1 id bumps line up.
- **Own or server-shared event** (own inputs, own `CreateClient`, `Tick`):
  keep the existing rollback + removal logic, but restrict the removal scan to
  pending events whose `origin_client()` is `None` or equals
  `local_client_id`, so a foreign-origin lookalike can never be removed.

**4c. Stale-event guard at intake.** When a `Region` is constructed from a
snapshot, record `base_event_id = *next_game_event_id` from the snapshot.
`reconcile()` drops any incoming server event with `id < base_event_id` —
its effects are already baked into the snapshot. This handles the join flow
(the new client receiving the broadcast of its own `CreateClient`) and stops
the current leak where pre-snapshot events sit in `input_buffer` forever,
counting toward the 1000-event panic.

## Data flow after the change

1. Client connects → server assigns id N, sends `ClientConnected(N)` to the
   game loop → game loop creates player N, broadcasts the `CreateClient(N)`
   game event.
2. Client sends `RequestPlayerRegion` → directed `PlayerRegion(Some(region), N)`
   (player already exists).
3. Client sends `RequestRegionConnection` → directed `Region(id, snapshot)`;
   snapshot contains player N and a `next_game_event_id` past the
   `CreateClient(N)` event.
4. Client loads region with `local_client_id = Some(N)` and
   `base_event_id` from the snapshot; the buffered broadcast of its own
   `CreateClient(N)` is dropped by the stale-event guard.
5. Ongoing: server broadcasts all executed game events; each client reconciles
   — own events confirm or correct predictions, foreign events insert into the
   timeline via 4b.

## Out of scope

- Window-close `SendError` panic in `game::state` (client shutdown path).
- Server crash when clients disconnect (tokio unwraps in the broadcast task).
- The broader multi-region direction tracked in `TODO.md`.
- Tick-rate/latency tuning (`SyncClock` behavior unchanged).

## Testing

- Existing suites stay green: `cargo test -p game`, `cargo test -p client`.
- New `crates/game/tests/multi_client.rs`: a server region plus two client
  regions driven over simulated channels — client A predicts ticks and inputs,
  the server interleaves client B's events into the global id sequence, both
  clients reconcile. Assertions:
  1. once caught up, `hash(client region) == hash(server region)` after each
     reconcile round;
  2. a foreign `CreateClient`/`PlayerInput` never removes a local pending
     event;
  3. the bit-exact undo invariant (`hash(before) == hash(after undo)`) holds
     through foreign-event insertions;
  4. events with `id < base_event_id` are dropped and never applied.
- Manual verification: launch server + two clients; each window drives its own
  player, and the second player is visible and moving in the first window.
