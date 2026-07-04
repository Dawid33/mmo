# Multi-Client Players Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two clients can play on one server, each driving their own player: the server creates players on connection, routes directed packets to the right client, and the rollback reconcile path correctly inserts other clients' events into the local timeline.

**Architecture:** Server assigns a `ClientId` per QUIC connection and injects `CreateClient` into the game loop itself (clients never originate it). The server's outgoing channel gains addressing (`Option<ClientId>`: `None` = broadcast). `Region` learns which client it belongs to plus the snapshot's base event id, and `reconcile()` distinguishes foreign-event *insertions* (rollback, apply, bump all pending ids, re-apply) from own-event mispredictions (existing removal logic, restricted to local-origin events).

**Tech Stack:** Rust workspace; `game` crate is engine-neutral (no Bevy). Build with `~/Software/rustc_codegen_cranelift/dist/cargo-clif build --workspace --bins` (plain `cargo` also works for `check`/`test`). Tests: `cargo test -p game`, `cargo test -p client`.

**Spec:** `docs/superpowers/specs/2026-07-04-multi-client-players-design.md`

## Global Constraints

- `game` and `server` crates must stay Bevy-free and windowing-free.
- Rollback correctness bar: `hash(before) == hash(after undo)`, bit-exact (enforced at rollback time by the generated machinery).
- Wire format unchanged: the QUIC payload stays a bincoded `ServerPacket`; only the server-internal channel gains addressing.
- Do not touch vendored forks (`crates/nalgebra`, `crates/rapier`, `crates/slotmapd`, ...).
- Existing test suites must stay green: `cargo test -p game`, `cargo test -p client`.

---

### Task 1: `GameEventKind::origin_client()`

**Files:**
- Modify: `crates/game/src/protocol.rs` (append `impl GameEventKind` after the enum at the end of the file)
- Test: `crates/game/tests/multi_client.rs` (new file)

**Interfaces:**
- Produces: `GameEventKind::origin_client(&self) -> Option<ClientId>` — `Some(id)` for `PlayerInput(id, _)` and `CreateClient(id)`, `None` for `Tick`/`Quit`. Used by Task 4's reconcile split.

- [ ] **Step 1: Write the failing test**

Create `crates/game/tests/multi_client.rs`:

```rust
//! Multi-client reconcile: foreign events are insertions into the local
//! timeline, not mispredictions. See
//! docs/superpowers/specs/2026-07-04-multi-client-players-design.md
use game::{GameEventKind, InputEvent, Key};

#[test]
fn origin_client_classifies_event_kinds() {
    let input = InputEvent::Key { key: Key::KeyW, pressed: true };
    assert_eq!(GameEventKind::PlayerInput(7, input).origin_client(), Some(7));
    assert_eq!(GameEventKind::CreateClient(3).origin_client(), Some(3));
    assert_eq!(GameEventKind::Tick.origin_client(), None);
    assert_eq!(GameEventKind::Quit.origin_client(), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test multi_client -- origin_client_classifies_event_kinds`
Expected: FAIL to compile with "no method named `origin_client`"

- [ ] **Step 3: Implement**

Append to `crates/game/src/protocol.rs`:

```rust
impl GameEventKind {
    /// The client this event originated from. `Tick` and `Quit` are
    /// server/shared events with no originating client.
    pub fn origin_client(&self) -> Option<ClientId> {
        match self {
            GameEventKind::PlayerInput(id, _) | GameEventKind::CreateClient(id) => Some(*id),
            GameEventKind::Tick | GameEventKind::Quit => None,
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p game --test multi_client`
Expected: PASS (1 test)

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/protocol.rs crates/game/tests/multi_client.rs
git commit -m "feat(game): GameEventKind::origin_client for reconcile origin split"
```

---

### Task 2: Region identity — `local_client_id` + `base_event_id`

**Files:**
- Modify: `crates/game/src/region.rs` (struct + `new()` + accessors)
- Modify: `crates/game/src/lib.rs:67` (`World::basic` caller)
- Modify: `crates/client/src/main.rs:178` (`new_region` caller)

**Interfaces:**
- Consumes: nothing new.
- Produces: `Region::new(data: Rollback, game_update_send: Option<Sender<GameDataUpdate>>, id: ChunkCoords, local_client_id: Option<ClientId>) -> Self`; test accessor `Region::pending_event_ids(&self) -> Vec<usize>`. Fields `local_client_id: Option<ClientId>`, `base_event_id: usize` (private, used by Tasks 3–4).

- [ ] **Step 1: Extend the struct and constructor**

In `crates/game/src/region.rs`, add two fields to `Region`:

```rust
pub struct Region {
    event_log: VecDeque<GameEvent>,
    data: Rollback,
    id: RegionId,
    input_buffer: BinaryHeap<Reverse<GameEvent>>,
    controllers: Vec<Box<dyn Controller>>,
    synchronized: bool,
    /// `Some` on a client (which client's predictions live in `event_log`);
    /// `None` on the server, which never reconciles.
    local_client_id: Option<ClientId>,
    /// `next_game_event_id` of the snapshot this region was built from.
    /// Server events below this id are already baked into the state.
    base_event_id: usize,
}
```

Update `new()` (the `base_event_id` read must come *after* `reinitialize`):

```rust
    pub fn new(
        mut data: Rollback,
        game_update_send: Option<Sender<GameDataUpdate>>,
        id: ChunkCoords,
        local_client_id: Option<ClientId>,
    ) -> Self {
        data.reinitialize(game_update_send);
        let base_event_id = *data.next_game_event_id;
        Self {
            data,
            event_log: VecDeque::new(),
            input_buffer: BinaryHeap::new(),
            controllers: Vec::from([CameraController::new(), PhysicsController::new()]),
            id,
            synchronized: false,
            local_client_id,
            base_event_id,
        }
    }
```

Add the test accessor next to `current_tick()`:

```rust
    /// Ids of locally-predicted events awaiting server confirmation.
    /// Exposed for tests.
    pub fn pending_event_ids(&self) -> Vec<usize> {
        self.event_log.iter().map(|e| e.id).collect()
    }
```

- [ ] **Step 2: Update the two callers**

`crates/game/src/lib.rs:67` (server-side region, no local client):

```rust
        let mut data = Region::new(Rollback::new(None), None, one, None);
```

`crates/client/src/main.rs:178` (inside the `new_region` closure — `self.client_id` is already set by the time a `Region` packet arrives, because `PlayerRegion` precedes it):

```rust
                let data = Region::new(raw_game_data.clone(), Some(send), id, self.client_id);
```

- [ ] **Step 3: Verify everything still compiles and passes**

Run: `cargo check --workspace && cargo test -p game`
Expected: clean check; all existing game tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/game/src/region.rs crates/game/src/lib.rs crates/client/src/main.rs
git commit -m "feat(game): Region carries local_client_id and snapshot base_event_id"
```

---

### Task 3: Stale-event guard at reconcile intake

**Files:**
- Modify: `crates/game/src/region.rs` (top of `reconcile()`)
- Test: `crates/game/tests/multi_client.rs`

**Interfaces:**
- Consumes: `base_event_id` from Task 2.
- Produces: `reconcile()` drops events with `id < base_event_id`. Join flow becomes: snapshot already contains the player; the broadcast of the joiner's own `CreateClient` is discarded.

- [ ] **Step 1: Write the failing test**

Append to `crates/game/tests/multi_client.rs`:

```rust
use std::collections::BTreeMap;
use std::hash::Hash;

use game::{ChunkCoords, Region, World};

fn state_hash(r: &game::Rollback) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    r.hash(&mut hasher);
    hasher.finalize()
}

const R0: ChunkCoords = ChunkCoords { x: 0, y: 0, z: 0 };

/// Server world with `n` players created on connection (the new join flow),
/// plus the broadcast CreateClient events it produced.
fn server_with_players(n: usize) -> (World, Vec<game::GameEvent>) {
    let mut server = World::basic();
    let mut events = Vec::new();
    for client_id in 0..n {
        let ev = server
            .handle_region_event(GameEventKind::CreateClient(client_id), R0)
            .unwrap();
        server.forget_last_event(&R0);
        events.push(ev);
    }
    (server, events)
}

/// Client world joined from a server snapshot, as `handle_server` does.
fn join_client(server: &World, client_id: usize) -> World {
    let snapshot = server.get_region_data(&R0);
    let mut world = World::new();
    world.load(&R0, Region::new(snapshot, None, R0, Some(client_id)));
    world
}

/// Run one lockstep tick: client predicts, server executes, client reconciles.
fn lockstep_tick(server: &mut World, client: &mut World) {
    let mut client_results = BTreeMap::new();
    let mut server_results = BTreeMap::new();
    client.progress_world_one_tick(&mut client_results);
    server.progress_world_one_tick(&mut server_results);
    let ev = server_results.get(&R0).unwrap().as_ref().unwrap().clone();
    client.reconcile_event(ev).unwrap();
}

#[test]
fn join_snapshot_already_contains_player_and_stale_create_is_dropped() {
    let (mut server, events) = server_with_players(1);
    let mut client = join_client(&server, 0);

    // Snapshot already contains our player.
    assert!(client.data(&R0).player_entites.contains_key(&0));
    let h = state_hash(client.data(&R0));

    // The broadcast of our own CreateClient arrives after the snapshot:
    // it must be dropped, not applied a second time.
    client.reconcile_event(events[0].clone()).unwrap();
    assert_eq!(h, state_hash(client.data(&R0)));

    // And it must not linger in the input buffer poisoning later
    // reconciles: the next server tick must confirm the next prediction.
    lockstep_tick(&mut server, &mut client);
    assert_eq!(
        client.regions.get(&R0).unwrap().pending_event_ids(),
        Vec::<usize>::new()
    );
    assert_eq!(state_hash(client.data(&R0)), state_hash(server.data(&R0)));
}
```

Note: `crc32fast` is already a dev-dependency of `game` (used by `tests/simple.rs`); if the import fails, add it under `[dev-dependencies]` in `crates/game/Cargo.toml` matching the version used there.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test multi_client -- join_snapshot`
Expected: FAIL on the `pending_event_ids` assertion — without the guard the stale event parks in `input_buffer` (smallest id), so the later server tick gets flagged "out of order" and never confirms the prediction, leaving one pending event.

- [ ] **Step 3: Implement the guard**

At the very top of `Region::reconcile`, before the `input_buffer.push`:

```rust
    pub fn reconcile(&mut self, server_event: GameEvent) -> Result<(), GameError> {
        // Events older than the snapshot this region was constructed from
        // are already baked into its state.
        if server_event.id < self.base_event_id {
            return Ok(());
        }
        self.input_buffer.push(Reverse(server_event.clone()));
        ...
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p game`
Expected: all PASS, including `join_snapshot_already_contains_player_and_stale_create_is_dropped`.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/region.rs crates/game/tests/multi_client.rs
git commit -m "feat(game): drop pre-snapshot server events at reconcile intake"
```

---

### Task 4: Origin-aware mismatch handling in `reconcile()`

**Files:**
- Modify: `crates/game/src/region.rs:60-117` (the mismatch branch)
- Test: `crates/game/tests/multi_client.rs`

**Interfaces:**
- Consumes: `origin_client()` (Task 1), `local_client_id` (Task 2).
- Produces: foreign events insert into the timeline (all pending ids +1, nothing removed); own/shared events keep the removal logic but the scan skips foreign-origin pending events.

- [ ] **Step 1: Write the failing tests**

Append to `crates/game/tests/multi_client.rs`:

```rust
#[test]
fn foreign_create_client_inserts_into_predicted_timeline() {
    let (mut server, _) = server_with_players(1);
    let mut client_a = join_client(&server, 0);

    // A predicts a tick (id = N) before hearing that B joined.
    let mut client_results = BTreeMap::new();
    client_a.progress_world_one_tick(&mut client_results);
    let predicted_ids = client_a.regions.get(&R0).unwrap().pending_event_ids();
    assert_eq!(predicted_ids.len(), 1);
    let n = predicted_ids[0];

    // Server creates B's player at id N, then ticks at id N+1.
    let ev_create_b = server
        .handle_region_event(GameEventKind::CreateClient(1), R0)
        .unwrap();
    server.forget_last_event(&R0);
    assert_eq!(ev_create_b.id, n);
    let mut server_results = BTreeMap::new();
    server.progress_world_one_tick(&mut server_results);
    let ev_tick = server_results.get(&R0).unwrap().as_ref().unwrap().clone();

    // A reconciles the foreign CreateClient: it is an insertion — the
    // pending tick must survive, bumped to id N+1.
    client_a.reconcile_event(ev_create_b).unwrap();
    assert_eq!(
        client_a.regions.get(&R0).unwrap().pending_event_ids(),
        vec![n + 1]
    );
    assert!(client_a.data(&R0).player_entites.contains_key(&1));

    // The server tick then confirms the bumped prediction exactly.
    client_a.reconcile_event(ev_tick).unwrap();
    assert_eq!(
        client_a.regions.get(&R0).unwrap().pending_event_ids(),
        Vec::<usize>::new()
    );
    assert_eq!(state_hash(client_a.data(&R0)), state_hash(server.data(&R0)));
}

#[test]
fn foreign_player_input_converges_and_undo_stays_bit_exact() {
    let (mut server, mut events) = server_with_players(2);
    let mut client_a = join_client(&server, 0);
    // A joined after both players existed: both CreateClient broadcasts are
    // stale for A and must be dropped.
    for ev in events.drain(..) {
        client_a.reconcile_event(ev).unwrap();
    }
    assert_eq!(state_hash(client_a.data(&R0)), state_hash(server.data(&R0)));

    // A predicts two ticks ahead.
    let mut client_results = BTreeMap::new();
    client_a.progress_world_one_tick(&mut client_results);
    client_a.progress_world_one_tick(&mut client_results);

    // Server interleaves B's input into the same id range.
    let input = InputEvent::Key { key: Key::KeyW, pressed: true };
    let ev_b_input = server
        .handle_region_event(GameEventKind::PlayerInput(1, input), R0)
        .unwrap();
    server.forget_last_event(&R0);
    let mut server_results = BTreeMap::new();
    server.progress_world_one_tick(&mut server_results);
    let ev_t1 = server_results.get(&R0).unwrap().as_ref().unwrap().clone();
    server.progress_world_one_tick(&mut server_results);
    let ev_t2 = server_results.get(&R0).unwrap().as_ref().unwrap().clone();

    // Foreign input inserted mid-log (exercises rollback + re-apply of the
    // whole pending log; the rollback machinery enforces bit-exact hashes).
    client_a.reconcile_event(ev_b_input).unwrap();
    // Both pending ticks survived, ids bumped by one.
    assert_eq!(
        client_a.regions.get(&R0).unwrap().pending_event_ids().len(),
        2
    );
    client_a.reconcile_event(ev_t1).unwrap();
    client_a.reconcile_event(ev_t2).unwrap();
    assert_eq!(
        client_a.regions.get(&R0).unwrap().pending_event_ids(),
        Vec::<usize>::new()
    );
    assert_eq!(state_hash(client_a.data(&R0)), state_hash(server.data(&R0)));
}
```

`client_a.regions` is the public `BTreeMap` on `World`, so `pending_event_ids` is reachable without new `World` API.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p game --test multi_client`
Expected: the two new tests FAIL (the current removal-by-kind walk logs "Client must be behind" and produces wrong pending ids / diverging hashes). `origin_client_classifies_event_kinds` and the join test still PASS.

- [ ] **Step 3: Rewrite the mismatch branch**

Replace the body of the `if event != server_event.0 { ... }` mismatch arm in `Region::reconcile` (currently `region.rs:64-103`, including both `TODO` comments, which this resolves) with:

```rust
                        if event != server_event.0 {
                            self.event_log.push_front(event);
                            let mut temp_log = self.event_log.clone();

                            // rollback whole event log
                            while let Some(_e) = self.event_log.pop_back() {
                                self.data.rollback();
                            }

                            // apply server event that was different from expected.
                            self.handle_event(server_event.clone().0.kind)?;
                            self.data.forget();

                            let foreign = match server_event.0.kind.origin_client() {
                                Some(origin) => Some(origin) != self.local_client_id,
                                None => false,
                            };

                            if foreign {
                                // Another client's event is an *insertion* into
                                // our timeline: keep every prediction, shift
                                // them all one id later.
                                for event in &mut temp_log {
                                    event.id += 1;
                                }
                            } else {
                                // Our own event came back at a different id
                                // than predicted: remove our matching
                                // prediction, shifting everything before it.
                                // Only locally-originated predictions are
                                // candidates for removal.
                                let len = temp_log.len();
                                let mut i = 0;
                                while i < temp_log.len() {
                                    let e = &mut temp_log[i];
                                    let local_origin = match e.kind.origin_client() {
                                        Some(o) => Some(o) == self.local_client_id,
                                        None => true,
                                    };
                                    if local_origin && e.kind == server_event.0.kind {
                                        temp_log.remove(i);
                                        break;
                                    }
                                    e.id += 1;
                                    i += 1;
                                }
                                if len == temp_log.len() {
                                    info!(
                                        "Client didn't have event recieved from server. Client must be behind."
                                    );
                                }
                            }

                            for event in &mut temp_log {
                                let _ = self.handle_event(event.clone().kind);
                            }
                            self.event_log = temp_log;
                        } else {
```

(The `else { self.data.forget(); }` confirmation arm and the out-of-order arm are unchanged.)

- [ ] **Step 4: Run the full game suite**

Run: `cargo test -p game`
Expected: all PASS — the two new tests plus `log_model`, `simple`, `random_ops`, `hash_restore` untouched.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/region.rs crates/game/tests/multi_client.rs
git commit -m "feat(game): origin-aware reconcile - foreign events insert instead of consuming predictions"
```

---

### Task 5: Server — create player on connection, directed packets

**Files:**
- Modify: `crates/server/src/main.rs`

**Interfaces:**
- Consumes: nothing new from other tasks (server never reconciles).
- Produces: `ServerEvent::ClientConnected(ClientId)`; internal channel type `(Option<ClientId>, ServerPacket)` — `None` broadcast, `Some(id)` directed. Wire format to clients unchanged.

- [ ] **Step 1: Add the event variant and addressed channel**

In `crates/server/src/main.rs`:

`ServerEvent` gains a variant:

```rust
#[derive(Debug)]
/// Internal event that is handled by the server.
pub enum ServerEvent {
    /// Recieved a packet from a client that needs to be handled.
    ClientPacket(ClientPacket, ClientId),
    /// A new client finished connecting and needs a player.
    ClientConnected(ClientId),
    /// Internal timer for generated game ticks.
    ServerTickTimer,
}
```

`listen()` signature changes to the addressed channel:

```rust
    pub async fn listen(
        &mut self,
        send: Sender<ServerEvent>,
        server_recv: Receiver<(Option<ClientId>, ServerPacket)>,
    ) {
```

Connection map keyed by `ClientId`, and the sender task routes:

```rust
        let connections: Arc<DashMap<ClientId, Connection>> = Arc::new(DashMap::new());
        let conns = connections.clone();
        tokio::spawn(async move {
            while let Ok((target, event)) = server_recv.recv() {
                let packet = bincode::serialize(&event).unwrap();
                match target {
                    // Directed packet: if the client is gone, drop it.
                    Some(id) => {
                        let conn = conns.get(&id).map(|e| e.value().clone());
                        if let Some(conn) = conn {
                            let mut stream = conn.open_uni().await.unwrap();
                            stream.write_all(&packet).await.unwrap();
                            stream.finish().unwrap();
                            tokio::spawn(async move {
                                stream.stopped().await.unwrap();
                            });
                        }
                    }
                    None => {
                        for entry in conns.iter() {
                            let mut stream = entry.value().open_uni().await.unwrap();
                            stream.write_all(&packet).await.unwrap();
                            stream.finish().unwrap();
                            tokio::spawn(async move {
                                stream.stopped().await.unwrap();
                            });
                        }
                    }
                }
            }
        });
```

Accept loop registers by `ClientId`, announces the connection to the game loop *before* any of the client's packets can be read, and unregisters on close:

```rust
        let mut next_id = 0;
        while let Some(conn) = endpoint.accept().await {
            let send = send.clone();
            let conns = connections.clone();
            let id = next_id;
            next_id += 1;
            tokio::spawn(async move {
                info!("accepting connection");
                let connection = conn.await.unwrap();
                conns.insert(id, connection.clone());
                send.send(ServerEvent::ClientConnected(id)).unwrap();

                let fut = handle_connection(connection, send, id);
                tokio::spawn(async move {
                    if let Err(e) = fut.await {
                        error!("connection failed: {reason}", reason = e.to_string())
                    }
                    conns.remove(&id);
                });
            });
        }
```

- [ ] **Step 2: Handle `ClientConnected` and address every send in the game loop**

In `main()`'s event loop, add the new arm (before/after the existing arms, order irrelevant):

```rust
            ServerEvent::ClientConnected(client_id) => {
                // Server-authoritative player creation. Reconnects (player
                // already exists) create nothing.
                if world.find_player(&client_id).is_none() {
                    let region_id = ChunkCoords::new(0, 0, 0);
                    let event = match world
                        .handle_region_event(game::GameEventKind::CreateClient(client_id), region_id)
                    {
                        Ok(e) => {
                            world.forget_last_event(&region_id);
                            e
                        }
                        Err(e) => panic!("Server crashed {:?}", e),
                    };
                    server_send.send((None, ServerPacket::GameEvent(event))).unwrap();
                }
            }
```

Update the existing sends with addressing. `RequestPlayerRegion` arm (directed both ways):

```rust
                ClientPacket::RequestPlayerRegion => {
                    if let Some(id) = world.find_player(&client_id) {
                        server_send
                            .send((Some(client_id), ServerPacket::PlayerRegion(Some(id), client_id)))
                            .unwrap();
                    } else {
                        server_send
                            .send((Some(client_id), ServerPacket::PlayerRegion(None, client_id)))
                            .unwrap();
                    };
                }
```

`RequestRegionConnection` arm (directed):

```rust
                ClientPacket::RequestRegionConnection(id) => {
                    server_send
                        .send((Some(client_id), world.build_region_server_packet(&id)))
                        .unwrap();
                }
```

`GameEvent` arm (broadcast): `server_send.send((None, ServerPacket::GameEvent(event))).unwrap();`

`ServerTickTimer` arm (both sends broadcast):

```rust
            ServerEvent::ServerTickTimer => {
                world.progress_world_one_tick(&mut results_buffer);
                for (id, result) in &results_buffer {
                    server_send
                        .send((None, ServerPacket::GameEvent(result.as_ref().unwrap().clone())))
                        .unwrap();
                    if world.current_tick(&ChunkCoords::new(0, 0, 0)) % 10 == 0 {
                        server_send
                            .send((
                                None,
                                ServerPacket::SyncClock(
                                    *id,
                                    tick_rate.load(Ordering::SeqCst),
                                    world.current_tick(&id),
                                    Duration::new(0, 0),
                                ),
                            ))
                            .unwrap();
                    }
                }
            }
```

- [ ] **Step 3: Verify the workspace compiles**

Run: `cargo check --workspace`
Expected: clean. (The channel type is inferred at `crossbeam::channel::unbounded()`; the `listen` signature change propagates it.)

- [ ] **Step 4: Commit**

```bash
git add crates/server/src/main.rs
git commit -m "feat(server): create player on connection; direct PlayerRegion/Region replies to requester"
```

---

### Task 6: Client — stop originating `CreateClient`

**Files:**
- Modify: `crates/client/src/main.rs`

**Interfaces:**
- Consumes: server-authoritative creation (Task 5); `Region::new` already receives `self.client_id` (Task 2).
- Produces: client never sends `CreateClient`; a `CreateClient` appearing on the local `game_event` channel is ignored with a warning.

- [ ] **Step 1: Remove the CreateClient origination**

In `handle_server`, shrink the `Region` arm (currently `main.rs:258-267`) to:

```rust
            game::ServerPacket::Region(id, raw_game_data) => {
                new_region(id, raw_game_data, &mut self.world);
            }
```

In `connect_and_run`'s local event match (currently `main.rs:149-159`), split `CreateClient` out of the forwarded arm:

```rust
                                        match e {
                                            GameEventKind::Tick => {
                                                world.progress_world_one_tick(&mut results_buffer);
                                            },
                                            GameEventKind::Quit | GameEventKind::PlayerInput(_, _) => {
                                                if let Some(chunk) = self.player_chunk {
                                                    let event = world.handle_region_event(game_event.unwrap(), chunk)?;
                                                    self.server_game_send.send(game::ClientPacket::GameEvent(event)).unwrap();
                                                }
                                            },
                                            GameEventKind::CreateClient(_) => {
                                                // Players are created by the server on connection.
                                                warn!("ignoring locally-originated CreateClient");
                                            },
                                        }
```

- [ ] **Step 2: Verify build and client tests**

Run: `cargo check --workspace && cargo test -p client`
Expected: clean check, all 16 client tests PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/client/src/main.rs
git commit -m "feat(client): players are created by the server on connection"
```

---

### Task 7: End-to-end verification

**Files:** none (verification only).

- [ ] **Step 1: Full build and full test run**

```bash
~/Software/rustc_codegen_cranelift/dist/cargo-clif build --workspace --bins
cargo test -p game
cargo test -p client
```
Expected: build succeeds; every test PASSES.

- [ ] **Step 2: Manual two-client run**

```bash
./target/debug/server > /tmp/mmo-server.log 2>&1 &
./target/debug/client > /tmp/mmo-client1.log 2>&1 &
sleep 5
./target/debug/client > /tmp/mmo-client2.log 2>&1 &
```

Verify, then kill all three:
- Client 1's log contains `PlayerRegion`/`SetPlayer` for id 0 only — it must NOT re-log "Region recieved and loaded!" when client 2 joins, and must not adopt id 1.
- Each log shows its own `CreateClient` arriving as a server broadcast; no client sends one.
- The per-tick "Client must be behind." spam is gone (isolated occurrences at join are acceptable; continuous per-tick repetition is a failure).
- On screen: two windows, each driving its own player; the second player is visible in the first window.

- [ ] **Step 3: Verify skill / final review**

Run the superpowers:verification-before-completion flow: confirm every command above was actually run with passing output before claiming completion.
