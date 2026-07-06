# Basic HUD (Crosshair + F3 Debug Overlay) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the client's first Bevy UI layer — a centered white crosshair plus an F3-toggled Minecraft-style debug overlay (player XYZ, region, connection status, FPS).

**Architecture:** Bevy UI is retained-mode ECS. A new `renderer/hud.rs` owns HUD entities/systems and a pure `format_debug_text` helper. Region + connection status is pushed from `GameInstanceManager` (game thread) to Bevy via one new `ClientUpdateEvent::HudStatus` variant landing in a `HudStatus` resource, reusing the existing `client_event_send → drain_client_updates` channel. The player camera is marked `IsDefaultUiCamera` so UI overlays the 3D world with no second camera.

**Tech Stack:** Rust, Bevy 0.18 (`bevy_ui`/`bevy_ui_render`/`bevy_text`), crossbeam channels, existing `game` crate protocol types.

**Design spec:** `docs/superpowers/specs/2026-07-06-hud-crosshair-debug-design.md`

## Global Constraints

- Bevy is pinned to `0.18` (workspace `Cargo.toml`); do not bump it.
- `game` and `server` crates must stay Bevy-free — the `HudStatus` event carries only plain `game` types (`RegionCoords`, a new `ConnectionState` enum), never Bevy types.
- The client must keep building for `wasm32-unknown-unknown`; new Bevy features must not be gated behind a native-only cfg, and text must use `default_font` (no fetched font asset).
- `RegionId` is a type alias for `RegionCoords { x: i32, z: i32 }` (`crates/game/src/protocol.rs:160`).
- Client tests run headless via `MinimalPlugins`/`AssetPlugin` (no window, no GPU). UI *rendering* cannot be asserted headless; tests cover construction, formatting, and event→resource wiring only.
- Test commands: `cargo test -p client`, `cargo test -p game`. Build check: `cargo build -p client` and `cargo build -p client --target wasm32-unknown-unknown`.

---

### Task 1: Enable Bevy UI Cargo features

**Files:**
- Modify: `Cargo.toml` (workspace `bevy` dependency feature list, lines 19–34)
- Modify: `crates/client/src/renderer/mod.rs` (add a compile-time smoke test)

**Interfaces:**
- Produces: the `bevy::ui` (`Node`, `Val`, `PositionType`, `BackgroundColor`), `bevy::text` (`Text`, `TextFont`, `TextColor`), and `bevy::render::camera::IsDefaultUiCamera` types become available to the client. Later tasks depend on these compiling.

- [ ] **Step 1: Add the UI features to the workspace Bevy dependency**

In `Cargo.toml`, extend the `bevy` feature list (currently ends with `"x11"`, `"wayland"`) to include the UI/text features. The block becomes:

```toml
bevy = { version = "0.18", default-features = false, features = [
  "std",
  "bevy_winit",
  "bevy_window",
  "bevy_render",
  "bevy_core_pipeline",
  "bevy_pbr",
  "bevy_asset",
  "bevy_log",
  "bevy_ui",
  "bevy_ui_render",
  "bevy_text",
  "default_font",
  "multi_threaded",
  "tonemapping_luts",
  "ktx2",
  "zstd_rust",
  "x11",
  "wayland",
] }
```

> If `cargo build` reports any of these as an unknown feature, run `cargo tree -e features -p bevy` (or check `~/.cargo` registry `bevy-0.18.*/Cargo.toml`) to find the exact 0.18 spelling — the `bevy_ui` / `bevy_ui_render` split and `default_font` naming are version-sensitive. Adjust the names and continue; do not remove UI support to make it build.

- [ ] **Step 2: Add a compile-time smoke test proving the types exist**

Append to the `#[cfg(test)] mod tests` block in `crates/client/src/renderer/mod.rs` (it already exists at the bottom of the file):

```rust
#[test]
fn bevy_ui_features_enabled() {
    // Compiles only if bevy_ui / bevy_text features are on. Constructing the
    // types (not rendering) is enough to prove the feature gate.
    use bevy::prelude::*;
    let _node = Node::default();
    let _text = Text::new("hud");
    let _white = BackgroundColor(Color::WHITE);
    let _mark = bevy::render::camera::IsDefaultUiCamera;
}
```

- [ ] **Step 3: Verify the native build and test compile**

Run: `cargo build -p client && cargo test -p client bevy_ui_features_enabled`
Expected: build succeeds; the `bevy_ui_features_enabled` test passes.

- [ ] **Step 4: Verify the wasm build still compiles**

Run: `cargo build -p client --target wasm32-unknown-unknown`
Expected: build succeeds (the new features are shared, not native-gated). If the target is missing, run `rustup target add wasm32-unknown-unknown` first.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/client/src/renderer/mod.rs
git commit -m "build(client): enable bevy_ui/bevy_text features for HUD"
```

---

### Task 2: `ConnectionState` + `ClientUpdateEvent::HudStatus` in the game crate

**Files:**
- Modify: `crates/game/src/lib.rs` (the `ClientUpdateEvent` enum at line 42; add the enum nearby)

**Interfaces:**
- Produces: `game::ConnectionState { Connecting, CatchingUp, Ready }` (derives `Debug, Clone, Copy, PartialEq, Eq, Default`, default = `Connecting`) and a new variant `ClientUpdateEvent::HudStatus { home_region: RegionCoords, viewer_region: RegionCoords, connection: ConnectionState }`. Consumed by Tasks 3, 4, 5.

- [ ] **Step 1: Write the failing test**

Add to `crates/game/src/lib.rs` (create a `#[cfg(test)] mod hud_status_tests` at the end of the file, or add to an existing test module if one is present):

```rust
#[cfg(test)]
mod hud_status_tests {
    use super::*;

    #[test]
    fn connection_state_default_is_connecting() {
        assert_eq!(ConnectionState::default(), ConnectionState::Connecting);
    }

    #[test]
    fn hud_status_variant_constructs() {
        let ev = ClientUpdateEvent::HudStatus {
            home_region: RegionCoords::new(1, 2),
            viewer_region: RegionCoords::new(1, 3),
            connection: ConnectionState::Ready,
        };
        match ev {
            ClientUpdateEvent::HudStatus { connection, .. } => {
                assert_eq!(connection, ConnectionState::Ready);
            }
            _ => panic!("wrong variant"),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game hud_status_tests`
Expected: FAIL — `cannot find type ConnectionState` / no variant `HudStatus`.

- [ ] **Step 3: Add the enum and variant**

In `crates/game/src/lib.rs`, immediately above the `ClientUpdateEvent` enum (line 42), add:

```rust
/// Connection/sync state surfaced to the client HUD. Derived from the
/// manager's `ready`/`is_caught_up` flags; pure `game` type so the enum can
/// cross the Bevy-free boundary in `ClientUpdateEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionState {
    #[default]
    Connecting,
    CatchingUp,
    Ready,
}
```

Then add the variant inside the `ClientUpdateEvent` enum (after `SetPlayer(ClientId)`):

```rust
    /// Region + connection status for the debug HUD. Edge-triggered by the
    /// manager (only sent on change); consumed by the render bridge.
    HudStatus {
        home_region: RegionId,
        viewer_region: RegionId,
        connection: ConnectionState,
    },
```

(`RegionId` is already in scope in `lib.rs`; it is an alias for `RegionCoords`.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p game hud_status_tests`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/lib.rs
git commit -m "feat(game): add ConnectionState + ClientUpdateEvent::HudStatus"
```

---

### Task 3: `renderer/hud.rs` scaffold — `HudStatus` resource + `format_debug_text`

**Files:**
- Create: `crates/client/src/renderer/hud.rs`
- Modify: `crates/client/src/renderer/mod.rs` (add `mod hud;`)

**Interfaces:**
- Consumes: `game::ConnectionState`, `game::RegionCoords` (Task 2).
- Produces:
  - `pub struct HudStatus { pub home_region: Option<RegionCoords>, pub viewer_region: Option<RegionCoords>, pub connection: ConnectionState }` — a `#[derive(Resource, Default)]`. Consumed by Tasks 4 and 7.
  - `pub fn format_debug_text(pos: Vec3, home: Option<RegionCoords>, viewer: Option<RegionCoords>, conn: ConnectionState, fps: Option<f64>) -> String`. Consumed by Task 7.

- [ ] **Step 1: Write the failing test**

Create `crates/client/src/renderer/hud.rs` with only the test first (so it fails to compile against missing items) — actually write the module skeleton and test together; run it to see the test fail on assertions after it compiles. Put this at the bottom of the new file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::prelude::Vec3;
    use game::{ConnectionState, RegionCoords};

    #[test]
    fn format_debug_text_full() {
        let s = format_debug_text(
            Vec3::new(12.345, 65.0, -8.1),
            Some(RegionCoords::new(0, 0)),
            Some(RegionCoords::new(0, 1)),
            ConnectionState::Ready,
            Some(143.4),
        );
        assert!(s.contains("XYZ: 12.35 / 65.00 / -8.10"), "got: {s}");
        assert!(s.contains("home (0, 0)"), "got: {s}");
        assert!(s.contains("viewer (0, 1)"), "got: {s}");
        assert!(s.contains("Status: Ready"), "got: {s}");
        assert!(s.contains("FPS: 143"), "got: {s}");
    }

    #[test]
    fn format_debug_text_missing_fields() {
        let s = format_debug_text(
            Vec3::ZERO,
            None,
            None,
            ConnectionState::Connecting,
            None,
        );
        assert!(s.contains("Region: --"), "got: {s}");
        assert!(s.contains("Status: Connecting"), "got: {s}");
        assert!(s.contains("FPS: --"), "got: {s}");
    }
}
```

- [ ] **Step 2: Write the module body (resource + helper) above the test**

At the top of `crates/client/src/renderer/hud.rs`:

```rust
use bevy::prelude::*;
use game::{ConnectionState, RegionCoords};

/// Region + connection status mirrored from the game thread via
/// `ClientUpdateEvent::HudStatus`. Written by `drain_client_updates`, read by
/// the debug overlay. Options are `None` until the first status arrives.
#[derive(Resource, Default)]
pub struct HudStatus {
    pub home_region: Option<RegionCoords>,
    pub viewer_region: Option<RegionCoords>,
    pub connection: ConnectionState,
}

/// Render the F3 debug overlay text. Pure so it is unit-testable without Bevy
/// systems. Coordinates print with 2 decimals; FPS rounds to a whole number.
pub fn format_debug_text(
    pos: Vec3,
    home: Option<RegionCoords>,
    viewer: Option<RegionCoords>,
    conn: ConnectionState,
    fps: Option<f64>,
) -> String {
    let region_line = match (home, viewer) {
        (Some(h), Some(v)) => format!("home ({}, {})  viewer ({}, {})", h.x, h.z, v.x, v.z),
        _ => "--".to_string(),
    };
    let status = match conn {
        ConnectionState::Connecting => "Connecting",
        ConnectionState::CatchingUp => "Catching up",
        ConnectionState::Ready => "Ready",
    };
    let fps_line = match fps {
        Some(f) => format!("{}", f.round() as i64),
        None => "--".to_string(),
    };
    format!(
        "XYZ: {:.2} / {:.2} / {:.2}\nRegion: {}\nStatus: {}\nFPS: {}",
        pos.x, pos.y, pos.z, region_line, status, fps_line
    )
}
```

- [ ] **Step 3: Register the module**

In `crates/client/src/renderer/mod.rs`, add to the module declarations near the top (alongside `mod bridge;` etc.):

```rust
mod hud;
```

- [ ] **Step 4: Run the tests to verify they fail, then pass**

Run: `cargo test -p client format_debug_text`
Expected: PASS both `format_debug_text_full` and `format_debug_text_missing_fields`. (If a decimal-format assertion fails, note the exact rounding Rust produces and adjust the assertion — `{:.2}` on `12.345` yields `12.35` via round-half-to-even; keep the test matching real output.)

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/hud.rs crates/client/src/renderer/mod.rs
git commit -m "feat(client): add HudStatus resource + format_debug_text helper"
```

---

### Task 4: Wire `HudStatus` into `drain_client_updates`

**Files:**
- Modify: `crates/client/src/renderer/bridge.rs` (`drain_client_updates` signature + match, starting line 66)
- Modify: `crates/client/src/renderer/mod.rs` (`init_resource::<hud::HudStatus>()` in `SimBridgePlugin::build`)

**Interfaces:**
- Consumes: `hud::HudStatus` (Task 3), `ClientUpdateEvent::HudStatus` (Task 2).
- Produces: the `HudStatus` resource is kept current from the channel. Consumed by Task 7.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `crates/client/src/renderer/bridge.rs` (it already builds apps with `MinimalPlugins`; mirror the existing helpers around line 428):

```rust
#[test]
fn drain_client_updates_writes_hud_status() {
    use super::super::hud::HudStatus;
    use game::{ConnectionState, RegionCoords};

    let (client, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(ClientUpdates(client_recv))
        .init_resource::<LocalPlayer>()
        .init_resource::<Regions>()
        .init_resource::<RegionRoots>()
        .init_resource::<SimEntityMap>()
        .init_resource::<HudStatus>()
        .add_systems(Update, drain_client_updates);

    client
        .send(ClientUpdateEvent::HudStatus {
            home_region: RegionCoords::new(2, -1),
            viewer_region: RegionCoords::new(2, 0),
            connection: ConnectionState::Ready,
        })
        .unwrap();

    app.update();

    let status = app.world().resource::<HudStatus>();
    assert_eq!(status.home_region, Some(RegionCoords::new(2, -1)));
    assert_eq!(status.viewer_region, Some(RegionCoords::new(2, 0)));
    assert_eq!(status.connection, ConnectionState::Ready);
}
```

> If the existing test module already imports `App`, `MinimalPlugins`, the resources, and `crossbeam`, reuse those imports rather than duplicating them.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p client drain_client_updates_writes_hud_status`
Expected: FAIL — `HudStatus` not a param of `drain_client_updates`; non-exhaustive match on `ClientUpdateEvent`.

- [ ] **Step 3: Add the resource param and match arm**

In `crates/client/src/renderer/bridge.rs`, add `use super::hud::HudStatus;` to the imports (top of file, near `use super::{ClientUpdates, LocalPlayer};`). Extend `drain_client_updates`'s signature with the new resource:

```rust
pub fn drain_client_updates(
    mut commands: Commands,
    updates: Res<ClientUpdates>,
    mut player: ResMut<LocalPlayer>,
    mut regions: ResMut<Regions>,
    mut roots: ResMut<RegionRoots>,
    mut map: ResMut<SimEntityMap>,
    mut hud_status: ResMut<HudStatus>,
) {
```

Add the match arm alongside the others (after the `SetPlayer` arm):

```rust
            ClientUpdateEvent::HudStatus { home_region, viewer_region, connection } => {
                hud_status.home_region = Some(home_region);
                hud_status.viewer_region = Some(viewer_region);
                hud_status.connection = connection;
            }
```

- [ ] **Step 4: Register the resource in the plugin**

In `crates/client/src/renderer/mod.rs`, inside `SimBridgePlugin::build`, add to the resource init chain (after `.init_resource::<bridge::SimEntityMap>()`):

```rust
            .init_resource::<hud::HudStatus>()
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p client drain_client_updates`
Expected: PASS. Also run `cargo build -p client` to confirm the match is exhaustive.

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/renderer/bridge.rs crates/client/src/renderer/mod.rs
git commit -m "feat(client): route HudStatus event into HudStatus resource"
```

---

### Task 5: Emit `HudStatus` from `GameInstanceManager` (edge-triggered)

**Files:**
- Modify: `crates/client/src/main.rs` (struct field ~line 44, `new()` ~line 102, add methods, two call sites)

**Interfaces:**
- Consumes: `game::ConnectionState`, `ClientUpdateEvent::HudStatus` (Task 2); existing `client_event_send`, `home_region`, `viewer_region()`, `ready`, `is_caught_up`.
- Produces: `HudStatus` events on the client channel whenever status changes. Consumed at runtime by Task 4's drain system.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `crates/client/src/main.rs` (private fields are accessible — the tests live in the same module):

```rust
#[test]
fn connection_state_maps_ready_flags() {
    let (game_send, game_recv) = crossbeam::channel::unbounded();
    let (client_send, _client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
    let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
    let mut manager =
        GameInstanceManager::new(game_send, game_recv, client_send, dummy_addr);

    manager.ready = false;
    manager.is_caught_up = false;
    assert_eq!(manager.connection_state(), game::ConnectionState::Connecting);

    manager.ready = true;
    manager.is_caught_up = false;
    assert_eq!(manager.connection_state(), game::ConnectionState::CatchingUp);

    manager.ready = true;
    manager.is_caught_up = true;
    assert_eq!(manager.connection_state(), game::ConnectionState::Ready);
}

#[test]
fn emit_hud_status_noop_without_home() {
    let (game_send, game_recv) = crossbeam::channel::unbounded();
    let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
    let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
    let mut manager =
        GameInstanceManager::new(game_send, game_recv, client_send, dummy_addr);

    // No home_region / world yet: nothing to report, no event sent.
    manager.emit_hud_status();
    assert!(client_recv.try_recv().is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p client connection_state_maps_ready_flags emit_hud_status_noop_without_home`
Expected: FAIL — no method `connection_state` / `emit_hud_status`.

- [ ] **Step 3: Add the struct field**

In `crates/client/src/main.rs`, add a field to `GameInstanceManager` (after `results_buffer`):

```rust
    /// Last HUD status sent, for edge-triggered emission (avoid channel spam).
    last_hud_status: Option<(RegionCoords, RegionCoords, game::ConnectionState)>,
```

And initialize it in `new()` (after `results_buffer: BTreeMap::new(),`):

```rust
            last_hud_status: None,
```

- [ ] **Step 4: Add the methods**

Add to the `impl GameInstanceManager` block (near `viewer_region`/`update_window`):

```rust
    /// Current connection/sync state for the HUD, from the ready flags.
    fn connection_state(&self) -> game::ConnectionState {
        if !self.ready {
            game::ConnectionState::Connecting
        } else if !self.is_caught_up {
            game::ConnectionState::CatchingUp
        } else {
            game::ConnectionState::Ready
        }
    }

    /// Push region + connection status to the render bridge, but only when it
    /// changed since the last push (edge-triggered). No-op until we know our
    /// home region and can resolve the viewer region.
    fn emit_hud_status(&mut self) {
        let (Some(home), Some(viewer)) = (self.home_region, self.viewer_region()) else {
            return;
        };
        let conn = self.connection_state();
        let snapshot = (home, viewer, conn);
        if self.last_hud_status == Some(snapshot) {
            return;
        }
        self.last_hud_status = Some(snapshot);
        let _ = self.client_event_send.send(ClientUpdateEvent::HudStatus {
            home_region: home,
            viewer_region: viewer,
            connection: conn,
        });
    }
```

- [ ] **Step 5: Call `emit_hud_status` on both runtime paths**

In the native `connect_and_run` loop (`main.rs`, the `loop { select! { .. } }` around line 498), add a call after the `select!` block so it runs each iteration:

```rust
        loop {
            select! {
                recv(server_recv) -> server_msg => {
                    self.handle_server(server_msg)?;
                },
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
            self.emit_hud_status();
        }
```

In `pump()` (used by wasm and tests), add the call before the final `Ok(true)`:

```rust
    pub fn pump(&mut self, server_recv: &Receiver<ServerPacket>) -> Result<bool, GameError> {
        while let Ok(msg) = server_recv.try_recv() {
            self.handle_server(Ok(msg))?;
        }
        while let Ok(event) = self.game_event_recv.try_recv() {
            if !self.handle_game_event(event)? {
                return Ok(false);
            }
        }
        self.emit_hud_status();
        Ok(true)
    }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p client connection_state_maps_ready_flags emit_hud_status_noop_without_home`
Expected: PASS.

- [ ] **Step 7: Guard against dead-code warnings on wasm**

`connect_and_run` is native-only; `pump` is used by wasm/tests. Both call `emit_hud_status`, so it is used on every target. Run `cargo build -p client` and `cargo build -p client --target wasm32-unknown-unknown`; if either warns `method never used`, add `#[allow(dead_code)]` only on the unused path. Expected: no new warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/client/src/main.rs
git commit -m "feat(client): emit edge-triggered HudStatus from GameInstanceManager"
```

---

### Task 6: Mark the player camera as the default UI camera

**Files:**
- Modify: `crates/client/src/renderer/bridge.rs` (snapshot camera spawn ~line 158; streaming `AddCameraComponent` ~line 246)

**Interfaces:**
- Consumes: `bevy::render::camera::IsDefaultUiCamera` (Task 1).
- Produces: the local player's `Camera3d` also carries `IsDefaultUiCamera`, so HUD root nodes overlay it. Consumed visually by Task 7.

- [ ] **Step 1: Write the failing test**

The streaming path already has camera tests around `bridge.rs:441` (asserting `Camera3d` present). Add an assertion in the same style. Locate the test that inserts `AddCameraComponent` for the local player and asserts `contains::<Camera3d>()`, and add alongside it:

```rust
    assert!(
        app.world().entity(e).contains::<bevy::render::camera::IsDefaultUiCamera>(),
        "local player's camera must be the default UI camera"
    );
```

(If unsure which test, it is the one near line 441 that asserts `app.world().entity(e).contains::<Camera3d>()` after sending `GameDataUpdateKind::AddCameraComponent(k, 0, ..)` for the local player.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p client` (run the camera test; e.g. the test name containing `camera`)
Expected: FAIL — entity does not contain `IsDefaultUiCamera`.

- [ ] **Step 3: Add the marker at the snapshot spawn site**

In `crates/client/src/renderer/bridge.rs` at the snapshot path (~line 158), add the marker to the inserted tuple:

```rust
                        e.insert((
                            Camera3d::default(),
                            Projection::Perspective(perspective_to_projection(&cam.proj_matrix)),
                            tf,
                            SimTarget::camera(tf.translation, tf.rotation),
                            bevy::render::camera::IsDefaultUiCamera,
                        ));
```

- [ ] **Step 4: Add the marker at the streaming spawn site**

At the streaming `AddCameraComponent` local-player branch (~line 246):

```rust
                        commands.entity(e).insert((
                            Camera3d::default(),
                            Projection::Perspective(perspective_to_projection(&proj)),
                            tf,
                            SimTarget::camera(tf.translation, tf.rotation),
                            bevy::render::camera::IsDefaultUiCamera,
                        ));
```

(Do not add it to the "other players' camera" `else` branch — only the local render camera should own UI.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p client`
Expected: the camera tests pass, including the new `IsDefaultUiCamera` assertion.

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/renderer/bridge.rs
git commit -m "feat(client): mark local player camera as default UI camera"
```

---

### Task 7: Crosshair + debug overlay entities and systems

**Files:**
- Modify: `crates/client/src/renderer/hud.rs` (components, systems, plugin registration entrypoint)
- Modify: `crates/client/src/renderer/mod.rs` (`FrameTimeDiagnosticsPlugin`, register HUD systems)

**Interfaces:**
- Consumes: `HudStatus`, `format_debug_text` (Task 3); `LocalPlayer`, `SimEntityMap` (`bridge.rs`); `IsDefaultUiCamera` camera (Task 6).
- Produces: HUD entities (`Crosshair`, `DebugOverlay`, `DebugText` markers) and the systems that drive them. Terminal deliverable.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `crates/client/src/renderer/hud.rs`:

```rust
    use bevy::prelude::*;

    #[test]
    fn setup_hud_spawns_markers() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_systems(Startup, setup_hud);
        app.update();

        let w = app.world_mut();
        assert_eq!(w.query_filtered::<(), With<Crosshair>>().iter(w).count(), 1);
        assert_eq!(w.query_filtered::<(), With<DebugOverlay>>().iter(w).count(), 1);
        assert_eq!(w.query_filtered::<(), With<DebugText>>().iter(w).count(), 1);
    }

    #[test]
    fn toggle_debug_flips_visibility() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .add_systems(Startup, setup_hud)
            .add_systems(Update, toggle_debug);
        app.update(); // runs setup_hud

        // Overlay starts Hidden.
        {
            let w = app.world_mut();
            let vis = w.query_filtered::<&Visibility, With<DebugOverlay>>().single(w).unwrap();
            assert_eq!(*vis, Visibility::Hidden);
        }

        // Press F3.
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::F3);
        app.update();
        {
            let w = app.world_mut();
            let vis = w.query_filtered::<&Visibility, With<DebugOverlay>>().single(w).unwrap();
            assert_eq!(*vis, Visibility::Visible);
        }
    }
```

> `ButtonInput`'s `press` state persists across frames until released; that is fine here because `toggle_debug` uses `just_pressed`, which is true only on the frame the press is first observed. If the second `app.update()` does not register the press, clear-and-re-press: call `.clear()` then `.press(KeyCode::F3)` before that update.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p client setup_hud_spawns_markers toggle_debug_flips_visibility`
Expected: FAIL — `Crosshair`/`DebugOverlay`/`DebugText`/`setup_hud`/`toggle_debug` not found.

- [ ] **Step 3: Add components and systems to `hud.rs`**

Add above the test module in `crates/client/src/renderer/hud.rs` (the `use bevy::prelude::*;` and `HudStatus`/`format_debug_text` from Task 3 are already at the top):

```rust
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use super::LocalPlayer;
use super::bridge::SimEntityMap;

/// Marker: root of the centered crosshair.
#[derive(Component)]
pub struct Crosshair;

/// Marker: root panel of the F3 debug overlay (toggled visibility).
#[derive(Component)]
pub struct DebugOverlay;

/// Marker: the `Text` entity inside the debug overlay.
#[derive(Component)]
pub struct DebugText;

/// Spawn the crosshair and (hidden) debug overlay. Runs once at startup; the
/// entities exist before any camera does and simply do not render until the
/// local player's `IsDefaultUiCamera` appears.
pub fn setup_hud(mut commands: Commands) {
    // Crosshair: full-viewport centering container with two thin white bars.
    commands
        .spawn((
            Crosshair,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            // The container must not eat clicks meant for the world.
            Pickable::IGNORE,
        ))
        .with_children(|p| {
            // Horizontal bar.
            p.spawn((
                Node { width: Val::Px(16.0), height: Val::Px(2.0), position_type: PositionType::Absolute, ..default() },
                BackgroundColor(Color::WHITE),
            ));
            // Vertical bar.
            p.spawn((
                Node { width: Val::Px(2.0), height: Val::Px(16.0), position_type: PositionType::Absolute, ..default() },
                BackgroundColor(Color::WHITE),
            ));
        });

    // Debug overlay: top-left translucent panel, hidden until F3.
    commands
        .spawn((
            DebugOverlay,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(4.0),
                left: Val::Px(4.0),
                padding: UiRect::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.5)),
            Visibility::Hidden,
            Pickable::IGNORE,
        ))
        .with_children(|p| {
            p.spawn((
                DebugText,
                Text::new(""),
                TextFont { font_size: 14.0, ..default() },
                TextColor(Color::WHITE),
            ));
        });
}

/// F3 toggles the debug overlay's visibility.
pub fn toggle_debug(
    keys: Res<ButtonInput<KeyCode>>,
    mut overlay: Query<&mut Visibility, With<DebugOverlay>>,
) {
    if !keys.just_pressed(KeyCode::F3) {
        return;
    }
    for mut vis in &mut overlay {
        *vis = match *vis {
            Visibility::Hidden => Visibility::Visible,
            _ => Visibility::Hidden,
        };
    }
}

/// While the overlay is visible, rebuild its text from live state.
pub fn update_debug_text(
    overlay: Query<&Visibility, With<DebugOverlay>>,
    mut text: Query<&mut Text, With<DebugText>>,
    status: Res<HudStatus>,
    local: Res<LocalPlayer>,
    map: Res<SimEntityMap>,
    transforms: Query<&Transform>,
    diagnostics: Res<DiagnosticsStore>,
) {
    // Cheap early-out when hidden.
    if overlay.iter().all(|v| *v == Visibility::Hidden) {
        return;
    }

    // Local player world position: find its bevy entity via the sim map.
    let pos = local
        .0
        .and_then(|id| {
            map.0
                .iter()
                .find(|((_r, _k), _e)| status.home_region.is_some() && Some(id) == local.0)
                .map(|(_, e)| *e)
        })
        .and_then(|e| transforms.get(e).ok())
        .map(|t| t.translation)
        .unwrap_or(Vec3::ZERO);

    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed());

    let s = format_debug_text(pos, status.home_region, status.viewer_region, status.connection, fps);
    for mut t in &mut text {
        *t = Text::new(s.clone());
    }
}

/// Hide the crosshair whenever the cursor is ungrabbed (free-cam / menus).
pub fn update_crosshair_visibility(
    windows: Query<&CursorOptions, With<PrimaryWindow>>,
    mut crosshair: Query<&mut Visibility, With<Crosshair>>,
) {
    let grabbed = windows
        .iter()
        .next()
        .map(|c| c.grab_mode != CursorGrabMode::None)
        .unwrap_or(false);
    for mut vis in &mut crosshair {
        *vis = if grabbed { Visibility::Visible } else { Visibility::Hidden };
    }
}
```

> The `pos` lookup above is intentionally conservative. If `SimEntityMap`
> already exposes a direct "local player entity" accessor, prefer it. The key
> requirement: resolve the local player's bevy `Entity` and read its
> `Transform.translation`; fall back to `Vec3::ZERO` when not yet mapped so the
> overlay shows `XYZ: 0.00 / 0.00 / 0.00` rather than panicking. Confirm the
> exact `SimEntityMap` key shape (`(RegionId, EntityKey)`) and, if a cleaner
> local-player resolver exists, simplify this to use it during implementation.

- [ ] **Step 4: Register the plugin, diagnostics, and systems in `mod.rs`**

In `crates/client/src/renderer/mod.rs`, add the diagnostics plugin and HUD systems in `SimBridgePlugin::build`. Add to the `add_plugins` call:

```rust
        app.add_plugins((
            MaterialPlugin::<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>::default(),
            bevy::diagnostic::FrameTimeDiagnosticsPlugin::default(),
        ))
```

(Replace the existing single `add_plugins(MaterialPlugin::...)` call with the tuple form above.)

Add `hud::setup_hud` to the `Startup` systems (alongside `setup_scene`):

```rust
            .add_systems(Startup, (setup_scene, hud::setup_hud))
```

Add the HUD update systems to the `Update` set (extend the existing tuple):

```rust
            .add_systems(
                Update,
                (
                    meshing::queue_meshing,
                    meshing::apply_meshed_chunks,
                    interpolate::interpolate_transforms,
                    avatar::attach_avatars,
                    hud::toggle_debug,
                    hud::update_debug_text,
                    hud::update_crosshair_visibility,
                ),
            );
```

- [ ] **Step 5: Run the HUD tests and the full client suite**

Run: `cargo test -p client`
Expected: `setup_hud_spawns_markers` and `toggle_debug_flips_visibility` pass; all pre-existing client tests still pass.

- [ ] **Step 6: Verify the wasm build**

Run: `cargo build -p client --target wasm32-unknown-unknown`
Expected: succeeds.

- [ ] **Step 7: Manual verification (native)**

Start the server and client (`scripts/run.sh` or the cranelift `cargo-clif run` commands from CLAUDE.md). Confirm: a white crosshair sits at screen center during play; pressing `F3` toggles a top-left overlay showing XYZ / Region / Status / FPS; the crosshair disappears when the cursor is released (free-cam) and returns when grabbed. This is the render coverage the headless tests cannot provide.

- [ ] **Step 8: Commit**

```bash
git add crates/client/src/renderer/hud.rs crates/client/src/renderer/mod.rs
git commit -m "feat(client): crosshair + F3 debug overlay HUD"
```

---

## Self-Review

**Spec coverage:**
- Cargo features (spec §1) → Task 1.
- UI camera marker (spec §2) → Task 6.
- `renderer/hud.rs` crosshair + overlay + systems (spec §3) → Tasks 3 & 7.
- Status plumbing: `ConnectionState` + `HudStatus` variant (spec §4) → Task 2; manager emission → Task 5; `HudStatus` resource + drain arm → Tasks 3 & 4.
- FPS via `FrameTimeDiagnosticsPlugin` (spec §3) → Task 7.
- Testing (spec §Testing): pure formatter → Task 3; event→resource → Task 4; connection-state derivation → Task 5; spawn assertions → Task 7. All covered.
- Edge handling — missing camera / missing player transform / missing FPS / offline (spec §Error handling): default `Vec3::ZERO` and `None`→`--` in Task 3/7; offline uses the same manager path so `HudStatus` flows via Task 5, and `local_server.rs`'s existing `_ => {}` catch-all (verified) already tolerates the new variant — no change needed there.

**Placeholder scan:** No TBD/TODO. The one soft spot (local-player entity resolution in `update_debug_text`) has explicit fallback behavior and a concrete requirement; not a blocking placeholder.

**Type consistency:** `format_debug_text(pos, home, viewer, conn, fps)` signature identical in Task 3 definition and Task 7 call. `HudStatus { home_region, viewer_region, connection }` fields consistent across Tasks 3/4/5. `ConnectionState { Connecting, CatchingUp, Ready }` consistent Tasks 2/3/5. `ClientUpdateEvent::HudStatus { home_region, viewer_region, connection }` consistent Tasks 2/4/5.

**Scope:** Single cohesive feature (one HUD), single implementation plan. No decomposition needed.
