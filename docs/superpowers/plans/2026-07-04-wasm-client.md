# WASM Client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Bevy client compiles to `wasm32-unknown-unknown` and runs in a browser as an offline single-player build, with the native QUIC path untouched.

**Architecture:** The browser has no threads, no UDP, and no tokio, so the three `std::thread::spawn` sites in `main.rs` and the quinn/tokio netcode are cfg-gated to native. The blocking `GameInstanceManager` loop is refactored into a non-blocking `pump()` that a Bevy `Update` system drives once per frame on wasm. Instead of a network server, wasm embeds a `LocalServer` — a ~80-line mirror of the server's "dumb router" loop (`crates/server/src/main.rs`) running `World::basic()` behind the exact same crossbeam-channel interface `netcode::ServerConnection` uses. Because the transport seam is "channels of `ClientPacket`/`ServerPacket`", a future WebTransport/WebSocket transport can replace `LocalServer` without touching the manager again.

**Tech Stack:** Bevy 0.18 (`webgl2` feature on wasm), `wasm-server-runner` for the dev loop, existing crossbeam channel plumbing, existing deterministic `game` crate (already wasm-friendly: libm-forced math, `oorandom`, no `std::net`/`Instant`).

## Global Constraints

- Bevy is pinned to `0.18` — do not bump it (CLAUDE.md).
- `game` and `server` crates must stay Bevy-free and windowing-free; all new wasm code lives in `crates/client`.
- Vendored forks (`nalgebra`, `simba`, `parry`, `rapier`, `approx`, `ordered-float`, `slotmapd`, `block-mesh`) must not be switched to crates.io versions or have their math touched (determinism).
- Native client behavior must be unchanged: same threads, same quinn netcode, same channel wiring. All wasm divergence is behind `#[cfg(target_arch = "wasm32")]` / `#[cfg(not(target_arch = "wasm32"))]`.
- Native test suites must keep passing: `cargo test -p game` and `cargo test -p client`.
- Workspace uses resolver = "2", so target-specific `[target.'cfg(...)'.dependencies]` features do not leak across targets — rely on this rather than feature flags where possible.
- Commit after every task with a conventional-commit message.

## Non-Goals (explicitly out of scope)

- Real multiplayer networking from the browser (WebTransport or a WebSocket↔QUIC bridge). The `LocalServer` seam is designed so this can be a follow-up plan; the server crate is untouched here.
- wasm threads / shared-memory atomics builds; Bevy's task pools run single-threaded on wasm and that is fine at current world sizes.
- WebGPU backend, release packaging (trunk, CDN, wasm-opt). Dev-loop via `wasm-server-runner` only.

---

### Task 1: WASM toolchain + target-gated dependencies

`quinn` and `tokio` (`rt-multi-thread`) do not build for `wasm32-unknown-unknown`; they must become native-only dependencies. Bevy needs the `webgl2` feature on wasm to select the WebGL2 wgpu backend (workspace sets `default-features = false`, so it is not on by default). The custom voxel shader (`assets/shaders/voxel_texture.wgsl`) only uses a `texture_2d_array` + sampler — WebGL2-compatible, no storage buffers — so no shader work is needed.

**Files:**
- Modify: `crates/client/Cargo.toml`
- Create: `.cargo/config.toml` (repo root)

**Interfaces:**
- Produces: a dep graph where `cargo tree --target wasm32-unknown-unknown -p client` contains no `quinn`/`tokio`, and `cargo run --target wasm32-unknown-unknown` is wired to `wasm-server-runner`. Later tasks assume `bevy` has `webgl2` on wasm and `console_error_panic_hook` is available on wasm.

- [ ] **Step 1: Install the target and the dev-server runner**

Run:
```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-server-runner
```
Expected: both succeed (`wasm-server-runner` may take a few minutes to compile).

- [ ] **Step 2: Move native-only deps to a target table in `crates/client/Cargo.toml`**

Replace the `[dependencies]` entries for `quinn` and `tokio` with target tables. The file becomes:

```toml
[package]
name = "client"
version = "0.1.0"
edition = "2021"

[features]
pyroscope = ["dep:pyroscope", "dep:pyroscope_pprofrs"]

[dependencies]
game = { workspace = true }
slotmapd = { workspace = true }
log = { workspace = true }
block-mesh = { workspace = true }
crossbeam = { workspace = true }
bincode = { workspace = true }
pyroscope = { workspace = true, optional = true }
pyroscope_pprofrs = { workspace = true, optional = true }
bevy = { workspace = true }
image = { workspace = true }
ordered-float = { workspace = true }

[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
quinn = { workspace = true }
tokio = { workspace = true }

[target.'cfg(target_arch = "wasm32")'.dependencies]
bevy = { workspace = true, features = ["webgl2"] }
console_error_panic_hook = "0.1"
```

(The `x11`/`wayland`/`multi_threaded` workspace features are winit/task-pool features that are no-ops on wasm — leave the workspace `bevy` entry alone.)

- [ ] **Step 3: Create `.cargo/config.toml` at the repo root**

```toml
[target.wasm32-unknown-unknown]
runner = "wasm-server-runner"
```

- [ ] **Step 4: Verify the wasm dep graph**

Run:
```bash
cargo tree -p client --target wasm32-unknown-unknown -e normal | grep -E '^\S*(quinn|tokio)' ; echo "exit: $?"
```
Expected: no matches, `exit: 1`.

Run:
```bash
cargo tree -p client --target wasm32-unknown-unknown -i getrandom
```
Two possible outcomes:
- "nothing depends on getrandom" style error → nothing to do.
- If `getrandom v0.2.x` appears → add to `crates/client/Cargo.toml` under the wasm target table: `getrandom = { version = "0.2", features = ["js"] }`.
- If `getrandom v0.3.x` appears → add under the wasm target table: `getrandom = { version = "0.3", features = ["wasm_js"] }` **and** add to `.cargo/config.toml`:
  ```toml
  [target.wasm32-unknown-unknown]
  runner = "wasm-server-runner"
  rustflags = ["--cfg", 'getrandom_backend="wasm_js"']
  ```

- [ ] **Step 5: Smoke-check the sim crate for wasm**

Run: `cargo check -p game --target wasm32-unknown-unknown`
Expected: PASS. The `game` crate has no netcode/thread/Instant usage and its math stack is libm-forced. If a vendored fork fails here (e.g. an `instant`/`web-time` gap in parry/rapier), stop and report — that is a scope change, not something to patch ad hoc in a vendored fork.

Note: `cargo check -p client --target wasm32-unknown-unknown` is EXPECTED to fail at this point (`netcode.rs` still imports quinn unconditionally). Task 4 fixes that.

- [ ] **Step 6: Verify native build still resolves**

Run: `cargo check -p client`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/client/Cargo.toml .cargo/config.toml
git commit -m "build(client): gate quinn/tokio to native, add wasm target config"
```

---

### Task 2: Extract a non-blocking `pump()` from `GameInstanceManager`

`connect_and_run` (`crates/client/src/main.rs:98-173`) is a blocking `select!` loop that owns a local `results_buffer` and inlines game-event handling. On wasm there is no thread to block, so the event handling must be callable one non-blocking drain at a time. This task is a pure refactor: native behavior stays identical, and the new `pump()` gets a native unit test.

**Files:**
- Modify: `crates/client/src/main.rs`
- Test: new `#[cfg(test)]` module at the bottom of `crates/client/src/main.rs`

**Interfaces:**
- Consumes: existing `GameInstanceManager` fields; `game::World::{basic, build_region_server_packet}`; `game::ServerPacket`, `game::ClientPacket`, `game::GameEventKind`.
- Produces (used by Tasks 4–5):
  - `GameInstanceManager::pump(&mut self, server_recv: &Receiver<ServerPacket>) -> Result<bool, GameError>` — drains both channels without blocking; `Ok(false)` means Quit was consumed.
  - `GameInstanceManager::start(&mut self)` — sends the initial `ClientPacket::RequestPlayerRegion`.
  - `GameInstanceManager::send_tick(&self)` — pushes `GameEventKind::Tick` into `game_event_send`.
  - `GameInstanceManager::tick_rate_ms(&self) -> u64` — current adaptive tick rate.
  - `GameInstanceManager::client_packet_recv(&self) -> Receiver<ClientPacket>` — clone of the channel `netcode::ServerConnection` normally consumes.
  - New field `results_buffer: BTreeMap<RegionId, Result<GameEvent, GameError>>` (moved from the local in `connect_and_run`).

- [ ] **Step 1: Write the failing test**

Append to `crates/client/src/main.rs`:

```rust
#[cfg(test)]
mod manager_tests {
    use super::*;
    use game::ClientUpdateEvent;

    /// pump() must: load a region from a ServerPacket, then advance the sim
    /// on a Tick game event — all without blocking.
    #[test]
    fn pump_loads_region_and_ticks() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager =
            GameInstanceManager::new(game_send.clone(), game_recv, client_send, dummy_addr);

        // Fake the server side using the same code the real server uses.
        let world = game::World::basic();
        let region_id = ChunkCoords::new(0, 0, 0);
        server_send
            .send(ServerPacket::PlayerRegion(Some(region_id), 0))
            .unwrap();
        server_send
            .send(world.build_region_server_packet(&region_id))
            .unwrap();

        assert!(manager.pump(&server_recv).unwrap());
        let tick_before = manager.world.as_ref().unwrap().current_tick(&region_id);

        manager.send_tick();
        assert!(manager.pump(&server_recv).unwrap());
        let tick_after = manager.world.as_ref().unwrap().current_tick(&region_id);
        assert_eq!(tick_after, tick_before + 1);

        // The render bridge must have been told about the new region + player.
        let mut saw_region = false;
        let mut saw_player = false;
        while let Ok(ev) = client_recv.try_recv() {
            match ev {
                ClientUpdateEvent::NewRegion(..) => saw_region = true,
                ClientUpdateEvent::SetPlayer(..) => saw_player = true,
                _ => {}
            }
        }
        assert!(saw_region && saw_player);

        // Quit terminates the pump.
        game_send.send(GameEventKind::Quit).unwrap();
        assert!(!manager.pump(&server_recv).unwrap());
    }
}
```

(If `ClientUpdateEvent` has other variants the `_ => {}` arm covers them; if `NewRegion`'s receiver field makes the match arm fussy, match with `ClientUpdateEvent::NewRegion(_, _, _)`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p client pump_loads_region_and_ticks`
Expected: FAIL to compile — `pump`, `send_tick` not found.

- [ ] **Step 3: Refactor `GameInstanceManager`**

In `crates/client/src/main.rs`:

3a. Add the field to the struct and its initializer in `new()`:

```rust
    player_chunk: Option<ChunkCoords>,
    results_buffer: BTreeMap<RegionId, Result<GameEvent, GameError>>,
```
```rust
            player_chunk: None,
            results_buffer: BTreeMap::new(),
```

3b. Add the small public helpers (right after `new()`):

```rust
    /// Kick off the connection handshake: ask the server which region the
    /// player belongs to. Both the native thread loop and the wasm frame
    /// driver call this exactly once before their first pump/select.
    pub fn start(&mut self) {
        self.server_game_send
            .send(ClientPacket::RequestPlayerRegion)
            .unwrap();
    }

    /// Inject a client-side tick (wasm frame driver replaces the native
    /// tick-generator thread with this).
    pub fn send_tick(&self) {
        let _ = self.game_event_send.send(GameEventKind::Tick);
    }

    /// Current adaptive tick interval in milliseconds.
    pub fn tick_rate_ms(&self) -> u64 {
        self.tick_rate.load(Ordering::SeqCst)
    }

    /// The channel the transport (quinn on native, LocalServer on wasm)
    /// reads outgoing ClientPackets from.
    pub fn client_packet_recv(&self) -> Receiver<ClientPacket> {
        self.server_game_recv.clone()
    }
```

3c. Extract the game-event arm of the `select!` into a method. Behavior notes preserved verbatim: events are ignored entirely while `self.world` is `None` (including Quit — that matches today's code), and player input is dropped until the sim is ready:

```rust
    /// Handle one client-side game event. Returns Ok(false) if the game
    /// should quit. Events arriving before the first region loads are
    /// dropped, matching the pre-refactor select! loop.
    fn handle_game_event(&mut self, event: GameEventKind) -> Result<bool, GameError> {
        if self.world.is_none() {
            return Ok(true);
        }
        match event {
            GameEventKind::Quit => return Ok(false),
            GameEventKind::Tick => {
                self.world
                    .as_mut()
                    .unwrap()
                    .progress_world_one_tick(&mut self.results_buffer);
            }
            GameEventKind::PlayerInput(_, _) if !self.ready && self.is_caught_up => {
                // don't handle player events until sim has caught up with server.
            }
            e @ GameEventKind::PlayerInput(_, _) => {
                if let Some(chunk) = self.player_chunk {
                    let event = self.world.as_mut().unwrap().handle_region_event(e, chunk)?;
                    self.server_game_send
                        .send(game::ClientPacket::GameEvent(event))
                        .unwrap();
                }
            }
            GameEventKind::CreateClient(_) => {
                // Players are created by the server on connection.
                warn!("ignoring locally-originated CreateClient");
            }
        }
        Ok(true)
    }

    /// Drain all pending server packets, then all pending game events,
    /// without blocking. Returns Ok(false) once Quit has been consumed.
    pub fn pump(&mut self, server_recv: &Receiver<ServerPacket>) -> Result<bool, GameError> {
        while let Ok(msg) = server_recv.try_recv() {
            self.handle_server(Ok(msg))?;
        }
        while let Ok(event) = self.game_event_recv.try_recv() {
            if !self.handle_game_event(event)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
```

3d. Rewrite the tail of `connect_and_run` to delegate (thread spawns at the top stay exactly as they are; delete the old inline match and the local `results_buffer`):

```rust
        self.start();

        loop {
            select! {
                // Recieve and handle server packets.
                recv(server_recv) -> server_msg => {
                    self.handle_server(server_msg)?;
                },
                // Recieve client game events from either the player or from
                // client-side game tick timer.
                recv(self.game_event_recv) -> game_event => {
                    match game_event {
                        Ok(event) => {
                            if !self.handle_game_event(event)? {
                                return Ok(());
                            }
                        }
                        Err(e) => panic!("{}", e),
                    }
                }
            }
        }
```

Also update the one remaining use of the deleted local: `handle_game_event` now uses `self.results_buffer`.

Original quirk check (do NOT "fix" these silently, they are load-bearing for identical native behavior):
- old code ran `progress_world_one_tick` only when `world` is `Some` → preserved by the early `is_none()` return.
- old code's inner `GameEventKind::Quit | GameEventKind::PlayerInput` arm was unreachable for Quit (already returned) → collapsing to `PlayerInput`-only arms is behavior-identical.

- [ ] **Step 4: Run the test and the full native suites**

Run: `cargo test -p client pump_loads_region_and_ticks`
Expected: PASS.

Run: `cargo test -p client && cargo test -p game`
Expected: PASS (16 client tests + game suite).

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/main.rs
git commit -m "refactor(client): non-blocking GameInstanceManager::pump for wasm frame driving"
```

---

### Task 3: `LocalServer` — embedded single-player loopback

The browser build has no server to talk to, so wasm embeds the server's "dumb router" event loop (mirroring `crates/server/src/main.rs:185-271`) behind the same channel pair the quinn transport uses. It is plain Rust over `game` APIs — no Bevy, no wasm — so it is written and tested natively.

**Files:**
- Create: `crates/client/src/local_server.rs`
- Modify: `crates/client/src/main.rs` (module declaration)
- Test: `#[cfg(test)]` module inside `crates/client/src/local_server.rs`

**Interfaces:**
- Consumes: `GameInstanceManager::{pump, start, send_tick, client_packet_recv}` from Task 2; `game::World::{basic, find_player, build_region_server_packet, handle_region_event, forget_last_event, progress_world_one_tick, current_tick}`.
- Produces (used by Task 4):
  - `LocalServer::new(recv: Receiver<ClientPacket>, send: Sender<ServerPacket>) -> Result<LocalServer, GameError>` — builds `World::basic()`, creates the local player (`CreateClient`), broadcasts that event.
  - `LocalServer::pump(&mut self) -> Result<(), GameError>` — non-blocking drain of client packets.
  - `LocalServer::tick(&mut self)` — advance the authoritative sim one tick, broadcast results + periodic `SyncClock`.
  - `pub const LOCAL_CLIENT_ID: ClientId = 0;`

- [ ] **Step 1: Write the failing test**

Create `crates/client/src/local_server.rs` with just the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::GameInstanceManager;
    use game::{ClientUpdateEvent, GameEventKind, ServerPacket};
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

    /// Full offline handshake: manager + LocalServer wired over channels,
    /// pumped in lockstep like the wasm frame driver will.
    #[test]
    fn offline_handshake_loads_region() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded::<ServerPacket>();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager =
            GameInstanceManager::new(game_send.clone(), game_recv, client_send, dummy_addr);

        let mut server = LocalServer::new(manager.client_packet_recv(), server_send).unwrap();
        manager.start();

        // A few frames of the wasm drive loop: server pump -> client pump.
        for _ in 0..4 {
            server.pump().unwrap();
            assert!(manager.pump(&server_recv).unwrap());
        }

        // Client received region + player identity.
        let mut saw_region = false;
        let mut saw_player = false;
        while let Ok(ev) = client_recv.try_recv() {
            match ev {
                ClientUpdateEvent::NewRegion(..) => saw_region = true,
                ClientUpdateEvent::SetPlayer(id) => {
                    saw_player = true;
                    assert_eq!(id, LOCAL_CLIENT_ID);
                }
                _ => {}
            }
        }
        assert!(saw_region, "client never received the region snapshot");
        assert!(saw_player, "client never learned its ClientId");

        // Server ticks advance the authoritative world and reach the client.
        server.tick();
        manager.pump(&server_recv).unwrap();

        // Client ticks advance the local prediction.
        manager.send_tick();
        assert!(manager.pump(&server_recv).unwrap());
    }
}
```

And in `crates/client/src/main.rs`, next to `mod netcode;`:

```rust
#[cfg(any(target_arch = "wasm32", test))]
mod local_server;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p client offline_handshake_loads_region`
Expected: FAIL to compile — `LocalServer` not defined.

- [ ] **Step 3: Implement `LocalServer`**

Prepend to `crates/client/src/local_server.rs` (above the test module):

```rust
//! Embedded single-player "server" for the wasm build: mirrors the dumb-router
//! event loop in crates/server/src/main.rs against a local World, behind the
//! same channel interface netcode::ServerConnection provides on native. A
//! future WebTransport/WebSocket transport replaces this without touching
//! GameInstanceManager.
use std::collections::BTreeMap;
use std::time::Duration;

use crossbeam::channel::{Receiver, Sender};
use game::{
    ChunkCoords, ClientId, ClientPacket, GameError, GameEvent, GameEventKind, RegionId,
    ServerPacket, World, TICK_RATE,
};

/// The only client in an offline world.
pub const LOCAL_CLIENT_ID: ClientId = 0;

pub struct LocalServer {
    world: World,
    recv: Receiver<ClientPacket>,
    send: Sender<ServerPacket>,
    results_buffer: BTreeMap<RegionId, Result<GameEvent, GameError>>,
}

impl LocalServer {
    pub fn new(
        recv: Receiver<ClientPacket>,
        send: Sender<ServerPacket>,
    ) -> Result<Self, GameError> {
        let mut world = World::basic();
        // Server-authoritative player creation, as on ClientConnected.
        let region_id = ChunkCoords::new(0, 0, 0);
        let event = world.handle_region_event(GameEventKind::CreateClient(LOCAL_CLIENT_ID), region_id)?;
        world.forget_last_event(&region_id);
        send.send(ServerPacket::GameEvent(event)).unwrap();
        Ok(Self {
            world,
            recv,
            send,
            results_buffer: BTreeMap::new(),
        })
    }

    /// Drain pending client packets without blocking.
    pub fn pump(&mut self) -> Result<(), GameError> {
        while let Ok(packet) = self.recv.try_recv() {
            match packet {
                ClientPacket::RequestPlayerRegion => {
                    let id = self.world.find_player(&LOCAL_CLIENT_ID);
                    self.send
                        .send(ServerPacket::PlayerRegion(id, LOCAL_CLIENT_ID))
                        .unwrap();
                }
                ClientPacket::RequestRegionConnection(id) => {
                    self.send
                        .send(self.world.build_region_server_packet(&id))
                        .unwrap();
                }
                ClientPacket::GameEvent(game_event) => match game_event.kind {
                    GameEventKind::Tick => (),
                    GameEventKind::Quit => (),
                    kind => {
                        let event = self
                            .world
                            .handle_region_event(kind, game_event.region_id)?;
                        self.world.forget_last_event(&game_event.region_id);
                        self.send.send(ServerPacket::GameEvent(event)).unwrap();
                    }
                },
            }
        }
        Ok(())
    }

    /// Advance the authoritative sim one tick and broadcast results,
    /// mirroring ServerEvent::ServerTickTimer handling on the real server
    /// (including the every-10-ticks SyncClock, with zero RTT).
    pub fn tick(&mut self) {
        self.world.progress_world_one_tick(&mut self.results_buffer);
        for (id, result) in &self.results_buffer {
            self.send
                .send(ServerPacket::GameEvent(result.as_ref().unwrap().clone()))
                .unwrap();
            if self.world.current_tick(id) % 10 == 0 {
                self.send
                    .send(ServerPacket::SyncClock(
                        *id,
                        TICK_RATE,
                        self.world.current_tick(id),
                        Duration::ZERO,
                    ))
                    .unwrap();
            }
        }
    }
}
```

Signature checks against the real code (verify while implementing, adjust the call only if the compiler disagrees):
- `ServerPacket::SyncClock(RegionId, u64, usize, Duration)` — see `crates/server/src/main.rs:259-264`.
- `GameEvent { kind, region_id, .. }` — see `crates/game/src/protocol.rs:59-63`. If `GameEvent` fields don't destructure as written, match the server's usage at `crates/server/src/main.rs:219-227` exactly.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p client offline_handshake_loads_region`
Expected: PASS.

Run: `cargo test -p client`
Expected: PASS (all tests, including Task 2's).

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/local_server.rs crates/client/src/main.rs
git commit -m "feat(client): LocalServer loopback for offline/wasm single-player"
```

---

### Task 4: cfg-gate native code, add the wasm frame driver

Make the client compile for wasm: gate `netcode`, the three `std::thread::spawn` sites, and pyroscope to native; add a `SimDriver` Bevy resource + `drive_sim` system that replaces the tick thread, the tokio thread, and the blocking game thread on wasm.

**Files:**
- Modify: `crates/client/src/main.rs`
- Create: `crates/client/src/sim_driver.rs` (wasm-only module)

**Interfaces:**
- Consumes: `GameInstanceManager::{new, start, pump, send_tick, tick_rate_ms, client_packet_recv}` (Task 2); `LocalServer::{new, pump, tick}` (Task 3); `game::TICK_RATE`.
- Produces: `sim_driver::SimDriver` (Bevy `Resource`), `sim_driver::drive_sim` (system), `sim_driver::start_wasm_sim() -> (SimDriver, Sender<GameEventKind>, Receiver<ClientUpdateEvent>)`.

- [ ] **Step 1: Gate the native-only module and imports in `main.rs`**

```rust
#[cfg(not(target_arch = "wasm32"))]
mod netcode;
#[cfg(any(target_arch = "wasm32", test))]
mod local_server;
#[cfg(target_arch = "wasm32")]
mod sim_driver;
mod renderer;
```

Gate `connect_and_run` and `start_game_thread` (both spawn threads and reference `netcode`):

```rust
#[cfg(not(target_arch = "wasm32"))]
impl GameInstanceManager {
    pub fn connect_and_run(&mut self) -> Result<(), GameError> {
        // ... unchanged from Task 2 ...
    }
}
```

(i.e. move `connect_and_run` out of the main `impl` block into a second, cfg-gated `impl GameInstanceManager` block; `new`/`start`/`pump`/`handle_server`/helpers stay in the ungated block.)

```rust
#[cfg(not(target_arch = "wasm32"))]
fn start_game_thread() -> Sender<Command> { /* unchanged */ }
```

Gate `Command` too (only the native path uses it):

```rust
#[cfg(not(target_arch = "wasm32"))]
pub enum Command { /* unchanged */ }
```

- [ ] **Step 2: Create `crates/client/src/sim_driver.rs`**

```rust
//! Drives the sim from the Bevy schedule on wasm, replacing the native
//! tick-generator thread, tokio/netcode thread, and blocking game thread.
use bevy::prelude::*;
use crossbeam::channel::{Receiver, Sender};
use game::{ClientUpdateEvent, GameEventKind, ServerPacket, TICK_RATE};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use crate::local_server::LocalServer;
use crate::GameInstanceManager;

#[derive(Resource)]
pub struct SimDriver {
    manager: GameInstanceManager,
    server_recv: Receiver<ServerPacket>,
    local_server: LocalServer,
    client_tick_ms: f64,
    server_tick_ms: f64,
}

pub fn start_wasm_sim() -> (
    SimDriver,
    Sender<GameEventKind>,
    Receiver<ClientUpdateEvent>,
) {
    let (game_send, game_recv) = crossbeam::channel::unbounded();
    let (client_send, client_recv) = crossbeam::channel::unbounded();
    let (server_send, server_recv) = crossbeam::channel::unbounded();
    // The addr is unused on wasm; GameInstanceManager::new keeps one
    // signature on both targets.
    let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
    let mut manager =
        GameInstanceManager::new(game_send.clone(), game_recv, client_send, dummy_addr);
    let local_server = LocalServer::new(manager.client_packet_recv(), server_send)
        .expect("failed to build offline world");
    manager.start();
    (
        SimDriver {
            manager,
            server_recv,
            local_server,
            client_tick_ms: 0.0,
            server_tick_ms: 0.0,
        },
        game_send,
        client_recv,
    )
}

/// Once per frame: run due server ticks, run due client ticks, then drain
/// both directions of traffic. Mirrors what the native threads do with
/// sleeps and blocking select.
pub fn drive_sim(time: Res<Time>, mut driver: ResMut<SimDriver>) {
    let SimDriver {
        manager,
        server_recv,
        local_server,
        client_tick_ms,
        server_tick_ms,
    } = &mut *driver;

    let dt_ms = time.delta_secs_f64() * 1000.0;

    // Authoritative sim at fixed TICK_RATE (ms per tick), like the server's
    // tick thread. Cap catch-up to avoid a spiral after a background tab.
    *server_tick_ms = (*server_tick_ms + dt_ms).min(10.0 * TICK_RATE as f64);
    while *server_tick_ms >= TICK_RATE as f64 {
        *server_tick_ms -= TICK_RATE as f64;
        local_server.tick();
    }

    // Predicted client sim at the adaptive rate (SyncClock adjusts it),
    // like the native tick-generator thread.
    let rate = manager.tick_rate_ms().max(1) as f64;
    *client_tick_ms = (*client_tick_ms + dt_ms).min(10.0 * rate);
    while *client_tick_ms >= rate {
        *client_tick_ms -= rate;
        manager.send_tick();
    }

    local_server.pump().expect("offline server crashed");
    if !manager.pump(server_recv).expect("game sim crashed") {
        info!("sim received Quit");
    }
}
```

- [ ] **Step 3: Split `main()` by target**

Replace the pre-`App` setup and post-`run` shutdown in `main()`:

```rust
fn main() {
    #[cfg(feature = "pyroscope")]
    let agent_running = /* unchanged pyroscope block */;

    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();

    #[cfg(not(target_arch = "wasm32"))]
    let (command_send, game_send, client_recv) = {
        let command_send = start_game_thread();
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        command_send
            .send(Command::ConnectToServerAndScene(
                game_send.clone(),
                game_recv,
                client_send,
                SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 6466)),
            ))
            .unwrap();
        (command_send, game_send, client_recv)
    };

    #[cfg(target_arch = "wasm32")]
    let (sim, game_send, client_recv) = sim_driver::start_wasm_sim();

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(bevy::window::WindowPlugin { /* unchanged */ })
            .set(bevy::log::LogPlugin { /* unchanged */ })
            // Native: repo-root assets dir relative to crates/client (see
            // existing comment). Wasm: assets are fetched over HTTP relative
            // to the served root, and wasm-server-runner serves ./assets
            // from the working directory.
            .set(bevy::asset::AssetPlugin {
                #[cfg(not(target_arch = "wasm32"))]
                file_path: "../../assets".into(),
                #[cfg(target_arch = "wasm32")]
                file_path: "assets".into(),
                ..Default::default()
            }),
    )
    .add_plugins(renderer::SimBridgePlugin {
        client_recv,
        game_send: game_send.clone(),
    });

    #[cfg(target_arch = "wasm32")]
    app.insert_resource(sim)
        .add_systems(Update, sim_driver::drive_sim);

    app.run();

    // Window closed: shut the sim and game threads down.
    let _ = game_send.send(GameEventKind::Quit);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = command_send.send(Command::Quit);

    #[cfg(feature = "pyroscope")]
    /* unchanged pyroscope shutdown */
}
```

(If the attribute-inside-struct-literal form for `file_path` doesn't parse, use a `const`: `#[cfg(target_arch = "wasm32")] let asset_path = "assets"; #[cfg(not(target_arch = "wasm32"))] let asset_path = "../../assets";` then `file_path: asset_path.into()`.)

- [ ] **Step 4: Check both targets**

Run: `cargo check -p client --target wasm32-unknown-unknown`
Expected: PASS. Known contingency: if `SimDriver` fails the `Resource: Send + Sync` bound (something inside `GameData` is `!Sync`, e.g. a `Cell` in an undo wrapper), wrap it:

```rust
use bevy::utils::synccell::SyncCell;

#[derive(Resource)]
pub struct SimDriverRes(pub SyncCell<SimDriver>);
```

and in `drive_sim` take `mut driver: ResMut<SimDriverRes>` and start with `let driver = driver.0.get();`. (`SyncCell<T>` is `Sync` for any `T: Send`, and `GameData` is provably `Send` — the native client already sends it across a crossbeam channel in `handle_server`.)

**Amendment (2026-07-04, hit during execution):** `SimDriver` was not `Send` — not because of `GameData`, but because `game::Controller` (`crates/game/src/lib.rs:50`) had no `Send` supertrait, making `Vec<Box<dyn Controller>>` inside `Region` (and hence `World`) `!Send`. The `World` never crosses threads natively (it is created *inside* the game thread), so nothing had ever forced this. Resolution: add the supertrait — `pub trait Controller: Send { ... }` — as a separate commit touching `crates/game`. Both implementors (`PhysicsController` wrapping rapier's `PhysicsPipeline`, and the zero-sized `CameraController`) are `Send`, so this compiles without further changes, keeps `game` Bevy-free, and does not affect determinism or native behavior.

Run: `cargo test -p client && cargo test -p game && cargo check -p client && cargo check -p server`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/main.rs crates/client/src/sim_driver.rs
git commit -m "feat(client): wasm target support — frame-driven sim, native code cfg-gated"
```

---

### Task 5: Run in the browser and verify end-to-end

**Files:**
- No planned source changes; this task is build + manual verification, with fixups applied where the browser disagrees.

**Interfaces:**
- Consumes: everything above.
- Produces: a documented, repeatable dev loop.

- [ ] **Step 1: Build the wasm binary**

Run (from the repo root):
```bash
cargo build -p client --target wasm32-unknown-unknown
```
Expected: PASS. This is the first full wasm *build* (Tasks 1–4 only ran `check`), so codegen issues in dependencies would surface here.

- [ ] **Step 2: Serve and open**

Run (from the repo root, so `./assets` is served):
```bash
cargo run -p client --target wasm32-unknown-unknown
```
Expected output: `wasm-server-runner` prints a local URL (default `http://127.0.0.1:1334`). Open it in a Chromium or Firefox browser on the machine (if this is a headless session, report the URL and ask the user to open it — do not claim visual success without observing it).

Known fixups if this step fails:
- `wasm-bindgen` schema-version mismatch error → `cargo install wasm-server-runner --force` to get a runner built against the current wasm-bindgen.
- 404s on `assets/...` in the browser console → confirm the command was run from the repo root; `wasm-server-runner` serves files relative to the working directory.

- [ ] **Step 3: Verify in the browser (manual checklist)**

- Page loads, no panic in the devtools console (panics are readable thanks to `console_error_panic_hook`).
- The voxel floor renders (the 2d-array texture material works under WebGL2).
- WASD/mouse input moves the player: input flows renderer → `game_send` → `pump` → `LocalServer` → reconcile, so movement proves the whole loop.
- Leave it running ~60s: no unbounded memory growth in the task manager, no console errors from the tick loops.

- [ ] **Step 4: Verify native is untouched**

Run: `cargo run --bin server` and `cargo run --bin client` (or the cargo-clif variants in `scripts/run.sh`), connect, move around.
Expected: identical behavior to `develop`.

- [ ] **Step 5: Document the dev loop**

Add to `CLAUDE.md` under "Building and Running":

```markdown
### WASM build (offline single-player)

The client also targets `wasm32-unknown-unknown` (native QUIC netcode is
replaced by an embedded `LocalServer`; see `crates/client/src/local_server.rs`):

```sh
# From the repo root (so ./assets is served):
cargo run -p client --target wasm32-unknown-unknown   # opens via wasm-server-runner
```

Requires `rustup target add wasm32-unknown-unknown` and
`cargo install wasm-server-runner`.
```

- [ ] **Step 6: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: wasm client dev loop"
```

---

**Amendment (2026-07-04, found during Task 5's headless run):** block-texture discovery (`crates/client/src/renderer/mod.rs:setup_scene`) enumerates `assets/blocks/*.png` via `std::fs::read_dir` — impossible on wasm (no filesystem; HTTP cannot list directories), so browser voxels rendered untextured. Resolution (Task 6): on wasm, embed the textures at compile time — a `#[cfg(any(target_arch = "wasm32", test))]` constant `EMBEDDED_BLOCK_TEXTURES: &[(&str, &[u8])]` built from `include_bytes!`, consumed by a `#[cfg(target_arch = "wasm32")]` branch in `setup_scene` via `image::load_from_memory`; the native `read_dir` path is untouched. A native unit test asserts the embedded name list exactly matches the `.png` files present in `assets/blocks/`, so the list cannot silently drift when new textures are added.

## Follow-up (separate plan, not part of this one)

Real browser multiplayer: browsers cannot open raw QUIC/UDP sockets, so the options are (a) WebTransport — closest to the existing quinn stack; the server adds a WebTransport endpoint (e.g. `wtransport` or `web-transport-quinn` sharing the QUIC listener) and the client gets a `web-sys` WebTransport implementation of the same `Sender<ServerPacket>`/`Receiver<ClientPacket>` seam that `LocalServer` and `netcode::ServerConnection` implement today, or (b) a WebSocket↔QUIC bridge process. Decision deferred until this plan lands.
