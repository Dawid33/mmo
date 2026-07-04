# Bevy Renderer Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the client's hand-rolled winit/wgpu renderer with Bevy's renderer, used strictly as a retained scene graph fed one-way by the existing `GameDataUpdate` channel from the deterministic sim.

**Architecture:** The sim (rollback ECS, own thread) stays authoritative and untouched except for one refactor: engine-neutral input types replace winit types in the wire format. A `SimBridgePlugin` in the client drains `ClientUpdateEvent`/`GameDataUpdate` channels into Bevy's main world (entities with `Transform`, `Mesh3d`, `Camera3d`); Bevy's own extraction handles main-world→render-world. Data flows sim→bevy only; input events flow bevy→sim through the existing `GameEventKind` channel. Bevy's main world is write-only from the bridge's perspective — no gameplay systems.

**Tech Stack:** bevy 0.18 (default-features off, curated feature list), existing crossbeam channels, existing `block-mesh` greedy meshing, `image` crate for texture-array assembly.

## Global Constraints

- **One-way bridge:** nothing in the Bevy world may mutate sim state. The only bevy→sim path is `GameEventKind::PlayerInput` over the existing channel.
- **No bevy outside the client:** `game`, `rollback`, `server`, and the vendored forks must not gain a bevy dependency.
- **Vendored forks untouched:** never modify `crates/nalgebra`, `crates/rapier`, `crates/parry`, `crates/simba`, `crates/ordered-float`, `crates/slotmapd`, `crates/approx`, `crates/block-mesh`.
- **Wire format changes are fine** (client and server are always built from the same tree), but `cargo build --workspace --bins` must pass at the end of every task.
- **Rollback correctness bar unchanged:** `cargo test -p rollback` must pass at the end of every task.
- **Bevy version:** pin `bevy = "0.18"` (released 2026-01-13). This plan's Bevy API usage was written against the 0.16→0.18 API line; 0.19 (2026-06-19) exists but is NOT the target. If a bevy item in this plan fails to compile (bevy renames things between minors — e.g. buffered events are `Message`/`MessageReader` since 0.17, cursor options may live on a `CursorOptions` component rather than `Window`), check docs.rs for bevy 0.18 and keep the semantics of the step; do not downgrade bevy.
- **Build command:** plain `cargo build -p <crate>` / `cargo test -p <crate>` (stable) is the verification gate. The cranelift wrapper is a dev convenience, not part of this plan.
- **Commit after every task** with the message given in the task.

## File Structure

```
crates/rollback/src/common.rs      # MODIFY: engine-neutral Key/MouseButton/InputEvent, GameEventKind::PlayerInput
crates/rollback/src/input.rs       # MODIFY: InputState (renamed WinitInput), keyed by rollback Key
crates/game/src/camera.rs          # MODIFY: rollback Key instead of winit KeyCode; aspect fix
crates/game/src/region.rs          # MODIFY: rollback Key; PlayerInput rename; drop parley helper (task 10)
crates/client/src/main.rs          # MODIFY: bevy App entry; GameInstanceManager unchanged
crates/client/src/renderer/mod.rs      # CREATE: SimBridgePlugin, resources, schedule wiring
crates/client/src/renderer/convert.rs  # CREATE: nalgebra/OrderedFloat → glam conversions
crates/client/src/renderer/bridge.rs   # CREATE: drain systems, SimEntityMap, SimTarget, camera arms
crates/client/src/renderer/interpolate.rs # CREATE: SimTarget → Transform smoothing
crates/client/src/renderer/meshing.rs  # CREATE: voxels → bevy Mesh (sync task 7, async task 10)
crates/client/src/renderer/input.rs    # CREATE: bevy input → InputEvent forwarding
crates/client/src/renderer/voxel_material.rs # CREATE (task 9): recovered texture-array material
assets/shaders/voxel_texture.wgsl      # CREATE (task 9): recovered shader
crates/client/src/{window,state,render_world,layout,text,bevy}.rs # DELETE (task 2)
```

---

### Task 1: Engine-neutral input types in `rollback`

Removes winit from the wire format and from `rollback`/`game`, so the client can later be the only crate that knows about a windowing library. The current winit renderer keeps working after this task — `window.rs` maps winit events to the new types at the edge.

**Files:**
- Modify: `crates/rollback/src/common.rs` (winit imports at top; `WindowEvent`/`DeviceEvent`/`WinitEvent` at lines ~98-180)
- Modify: `crates/rollback/src/input.rs` (whole file)
- Modify: `crates/rollback/Cargo.toml` (remove `winit`)
- Modify: `crates/game/src/camera.rs:32-83` (key constants, resize handling)
- Modify: `crates/game/src/region.rs:160-180` (KeyE toggle, `PlayerWinitEvent` arm)
- Modify: `crates/game/src/data.rs` (drop `winit::keyboard::KeyCode` import if unused)
- Modify: `crates/client/src/window.rs`, `crates/client/src/main.rs` (map winit → `InputEvent`)
- Test: inline `#[cfg(test)]` module in `crates/rollback/src/input.rs`

**Interfaces:**
- Produces (used by every later task):
  - `rollback::common::Key` — `enum Key { KeyW, KeyA, KeyS, KeyD, KeyE, Space, ControlLeft, ShiftLeft, Escape }`
  - `rollback::common::MouseButton` — `enum MouseButton { Left, Right, Middle, Other(u16) }`
  - `rollback::common::InputEvent` — see step 1
  - `rollback::common::GameEventKind::PlayerInput(ClientId, InputEvent)` (replaces `PlayerWinitEvent`)
  - `rollback::input::InputState` (replaces `WinitInput`) with `update(&mut self, event: InputEvent) -> Option<Box<dyn Fn(&mut InputState) + Send + Sync>>`, `step()`, `mouse_diff() -> (f32, f32)`, `window_resized() -> &Option<(u32, u32)>`, `key_pressed(&Key) -> bool`, `key_held(&Key) -> bool`

- [ ] **Step 1: Replace the winit event types in `common.rs`**

Delete the `use winit::{...}` block at the top of `crates/rollback/src/common.rs`, the `WindowEvent`, `DeviceEvent`, and `WinitEvent` enums, and replace with:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Key {
    KeyW,
    KeyA,
    KeyS,
    KeyD,
    KeyE,
    Space,
    ControlLeft,
    ShiftLeft,
    Escape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u16),
}

/// Engine-neutral input event. This is the wire format for player input —
/// it must never contain types from a windowing library.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum InputEvent {
    Resized { width: u32, height: u32 },
    Key { key: Key, pressed: bool },
    MouseButton { button: MouseButton, pressed: bool },
    /// Line-based wheel scrolling, in lines.
    MouseWheel { x: f32, y: f32 },
    /// Accumulated raw mouse motion for one frame.
    MouseMotion { dx: f32, dy: f32 },
    Focused(bool),
}
```

Change `GameEventKind`:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum GameEventKind {
    Tick,
    PlayerInput(ClientId, InputEvent),
    CreateClient(ClientId),
    Quit,
}
```

The dropped variants (`CursorMoved`, `ScaleFactorChanged`, `DroppedFile`, `CloseRequested`, `Destroyed`, `DeviceEvent::{Added,Removed,Motion,Button,Key}`, `NewEvents`, `AboutToWait`) are consumed nowhere in the sim (`input.rs` matched them all to `None`) — YAGNI, delete their client-side senders in step 4 rather than porting them.

- [ ] **Step 2: Rewrite `input.rs` around the new types**

Replace the whole file body (keep the `KeyState` enum and undo-closure structure exactly as-is — it is part of the rollback contract):

```rust
use ordered_float::OrderedFloat;
use parry3d::math::Real;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::common::{InputEvent, Key};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
enum KeyState {
    Released,
    Pressed,
    Held,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, Hash)]
pub struct InputState {
    window_resized: Option<(u32, u32)>,
    keyboard_state: BTreeMap<Key, KeyState>,
    mouse_diff: Option<(Real, Real)>,
}
```

`update` keeps the same three-way state machine, but matches on the flat `InputEvent`:

```rust
impl InputState {
    pub fn update(
        &mut self,
        event: InputEvent,
    ) -> Option<Box<dyn Fn(&mut InputState) + 'static + Send + Sync>> {
        match event {
            InputEvent::Resized { width, height } => {
                let old = self.window_resized.take();
                self.window_resized = Some((width, height));
                Some(Box::new(move |s| s.window_resized = old))
            }
            InputEvent::Key { key, pressed } => {
                if let Some(k) = self.keyboard_state.get_mut(&key) {
                    match k {
                        KeyState::Released => {
                            if !pressed {
                                None
                            } else {
                                *k = KeyState::Pressed;
                                Some(Box::new(move |s| {
                                    *s.keyboard_state.get_mut(&key).unwrap() = KeyState::Released;
                                }))
                            }
                        }
                        KeyState::Held | KeyState::Pressed => {
                            if pressed {
                                None
                            } else {
                                let old = k.clone();
                                *k = KeyState::Released;
                                Some(Box::new(move |s| {
                                    *s.keyboard_state.get_mut(&key).unwrap() = old.clone();
                                }))
                            }
                        }
                    }
                } else {
                    let state = if pressed { KeyState::Pressed } else { KeyState::Released };
                    self.keyboard_state.insert(key, state);
                    Some(Box::new(move |s| {
                        s.keyboard_state.remove(&key);
                    }))
                }
            }
            InputEvent::MouseMotion { dx, dy } => {
                let old = self.mouse_diff;
                if let Some(m) = &mut self.mouse_diff {
                    m.0 += dx;
                    m.1 += dy;
                } else {
                    self.mouse_diff = Some((OrderedFloat(dx), OrderedFloat(dy)));
                }
                Some(Box::new(move |s: &mut Self| s.mouse_diff = old))
            }
            InputEvent::MouseButton { .. } | InputEvent::MouseWheel { .. } | InputEvent::Focused(_) => None,
        }
    }
}
```

`step`, `mouse_diff`, `key_pressed`, `key_held` are unchanged except `winit::keyboard::KeyCode` → `Key`. `window_resized` becomes:

```rust
    pub fn window_resized(&self) -> &Option<(u32, u32)> {
        &self.window_resized
    }
```

Remove `winit = { workspace = true }` from `crates/rollback/Cargo.toml`. Rename the type alias/export site: wherever `rollback` re-exports `WinitInput` (grep `WinitInput` in `crates/rollback/src/`), export `InputState` instead; keep `pub use` paths otherwise identical.

- [ ] **Step 3: Add the failing state-machine test**

At the bottom of `crates/rollback/src/input.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_step_release_with_undo() {
        let mut input = InputState::default();

        let undo_press = input.update(InputEvent::Key { key: Key::KeyE, pressed: true }).unwrap();
        assert!(input.key_pressed(&Key::KeyE));
        assert!(input.key_held(&Key::KeyE));

        let undo_step = input.step().unwrap();
        assert!(!input.key_pressed(&Key::KeyE), "step demotes Pressed to Held");
        assert!(input.key_held(&Key::KeyE));

        undo_step(&mut input);
        assert!(input.key_pressed(&Key::KeyE), "undoing step restores Pressed");

        undo_press(&mut input);
        assert!(!input.key_held(&Key::KeyE), "undoing press removes the key");
    }

    #[test]
    fn mouse_motion_accumulates_and_steps_away() {
        let mut input = InputState::default();
        input.update(InputEvent::MouseMotion { dx: 1.5, dy: -2.0 });
        input.update(InputEvent::MouseMotion { dx: 0.5, dy: 1.0 });
        assert_eq!(input.mouse_diff(), (2.0, -1.0));
        input.step();
        assert_eq!(input.mouse_diff(), (0.0, 0.0));
    }
}
```

Run: `cargo test -p rollback input` — expect PASS (implementation was written in step 2; if it fails, fix `input.rs`, not the test).

- [ ] **Step 4: Update the `game` crate**

`crates/game/src/camera.rs`: replace all seven `client.input.key_held(&winit::keyboard::KeyCode::X)` / `key_pressed` calls with `rollback::Key::X` (W, S, A, D, Space, ControlLeft). Replace the resize block (the `window_resized()` consumer) — note this **fixes an existing integer-division bug** (`resolution.width / resolution.height` divided two `u32`s):

```rust
            if let Some((width, height)) = *client.input.window_resized() {
                let old = ecs.camera.get(e_id).proj_matrix.clone();
                // ... (emit_on_undo / undo_scope block unchanged) ...
                scope
                    .get_mut(e_id)
                    .proj_matrix
                    .set_aspect(OrderedFloat(width as f32 / height as f32));
```

`crates/game/src/region.rs`: `winit::keyboard::KeyCode::KeyE` → `rollback::Key::KeyE` (line ~160); `GameEventKind::PlayerWinitEvent(client_id, player_event)` → `GameEventKind::PlayerInput(client_id, player_event)` (line ~176). Remove the now-unused `use winit::keyboard::{KeyCode, SmolStr};` import. In `crates/game/src/data.rs` remove the `winit::keyboard::KeyCode` import if nothing else uses it.

- [ ] **Step 5: Map winit → `InputEvent` in the client (current renderer stays)**

In `crates/client/src/window.rs`, add at the bottom:

```rust
fn map_key(code: winit::keyboard::KeyCode) -> Option<game::Key> {
    use game::Key;
    use winit::keyboard::KeyCode as K;
    Some(match code {
        K::KeyW => Key::KeyW,
        K::KeyA => Key::KeyA,
        K::KeyS => Key::KeyS,
        K::KeyD => Key::KeyD,
        K::KeyE => Key::KeyE,
        K::Space => Key::Space,
        K::ControlLeft => Key::ControlLeft,
        K::ShiftLeft => Key::ShiftLeft,
        K::Escape => Key::Escape,
        _ => return None,
    })
}

fn map_mouse_button(b: winit::event::MouseButton) -> game::MouseButton {
    use winit::event::MouseButton as M;
    match b {
        M::Left => game::MouseButton::Left,
        M::Right => game::MouseButton::Right,
        M::Middle => game::MouseButton::Middle,
        M::Back => game::MouseButton::Other(3),
        M::Forward => game::MouseButton::Other(4),
        M::Other(n) => game::MouseButton::Other(n),
    }
}
```

(`game` re-exports `rollback::common::*`, so `game::Key`/`game::MouseButton`/`game::InputEvent` resolve; if not, add them to the `pub use rollback::...` list in `crates/game/src/lib.rs:48`.)

Then rewrite the event senders: every `GameEventKind::PlayerWinitEvent(player, WinitEvent::...)` becomes `GameEventKind::PlayerInput(player, InputEvent::...)`:
- `Resized(size)` → `InputEvent::Resized { width: size.width, height: size.height }`
- `KeyboardInput` → `if let PhysicalKey::Code(code) = event.physical_key { if let Some(key) = map_key(code) { send(InputEvent::Key { key, pressed: event.state.is_pressed() }) } }` (keep the `!event.repeat` guard)
- `MouseInput` → `InputEvent::MouseButton { button: map_mouse_button(button), pressed: button_state.is_pressed() }`
- `MouseWheel` → `match delta { MouseScrollDelta::LineDelta(x, y) => InputEvent::MouseWheel { x, y }, MouseScrollDelta::PixelDelta(p) => InputEvent::MouseWheel { x: p.x as f32 / 20.0, y: p.y as f32 / 20.0 } }`
- `Focused(f)` → `InputEvent::Focused(f)`
- mouse-motion buffer flush (RedrawRequested arm) → `InputEvent::MouseMotion { dx: buf.0 as f32, dy: buf.1 as f32 }`
- Delete the `DroppedFile`, `ScaleFactorChanged`, and `WindowEvent::Destroyed` sender arms and the `device_event` arms for `Added`/`Removed`/`MouseWheel` (nothing consumed them).

In `crates/client/src/main.rs:155`, the gate `if let GameEventKind::PlayerWinitEvent(_,_) = e` → `if let GameEventKind::PlayerInput(_,_) = e`, and the match arm at line 165 `GameEventKind::PlayerWinitEvent(_, _)` → `GameEventKind::PlayerInput(_, _)`.

- [ ] **Step 6: Verify workspace**

Run: `cargo build --workspace --bins` — expect success.
Run: `cargo test -p rollback` — expect all tests PASS (including the existing log_model/simple/random_ops/hash_restore suites).
Smoke: run `cargo run --bin server` and `cargo run --bin client` (or `scripts/run.sh`); WASD/mouse must still move the camera, E must still toggle freecam.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor: engine-neutral input types; winit removed from rollback wire format"
```

---

### Task 2: Bevy dependency and app scaffold; delete the old renderer

After this task the client is a Bevy app: window opens, connects to the server, logs region arrival. Nothing renders yet.

**Files:**
- Modify: `Cargo.toml` (workspace: add bevy)
- Modify: `crates/client/Cargo.toml`
- Modify: `crates/client/src/main.rs` (`fn main`, module decls; `GameInstanceManager` untouched)
- Create: `crates/client/src/renderer/mod.rs`
- Delete: `crates/client/src/{window.rs,state.rs,render_world.rs,layout.rs,text.rs,bevy.rs,shader.wgsl}`

**Interfaces:**
- Produces (used by tasks 3-10):
  - `renderer::SimBridgePlugin { client_recv: Receiver<ClientUpdateEvent>, game_send: Sender<GameEventKind> }`
  - `renderer::ClientUpdates(pub Receiver<ClientUpdateEvent>)` — `Resource`
  - `renderer::GameEvents(pub Sender<GameEventKind>)` — `Resource`
  - `renderer::LocalPlayer(pub Option<ClientId>)` — `Resource`
- Consumes: `map_key`/`map_mouse_button` from task 1 are deleted along with `window.rs` — task 8 recreates the mapping from *bevy* key codes.

- [ ] **Step 1: Add bevy to the workspace**

In root `Cargo.toml` `[workspace.dependencies]`:

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
    "multi_threaded",
    "tonemapping_luts",
    "ktx2",
    "zstd_rust",
    "x11",
    "wayland",
] }
```

In `crates/client/Cargo.toml`: add `bevy = { workspace = true }`; remove `wgpu`, `pollster`, `winit`, `parley`, `simplelog`, `bytemuck`, `ndshape`, `derive_more`, `weak-table`, `futures-lite`. Keep `image`, `block-mesh`, `crossbeam`, `log`, `rand`, `slotmapd`, `bincode`, `crc32fast`, `quinn`, `tokio`, `rollback`, `game`, `rapier3d`.

Run: `cargo tree -p client -i winit` afterwards to confirm exactly one winit version (bevy's). If bevy 0.18 pins a different winit minor than 0.30, that is fine — nothing outside bevy uses winit anymore after this task.

- [ ] **Step 2: Delete the old renderer files**

```bash
git rm crates/client/src/window.rs crates/client/src/state.rs crates/client/src/render_world.rs \
       crates/client/src/layout.rs crates/client/src/text.rs crates/client/src/bevy.rs \
       crates/client/src/shader.wgsl
```

(If `shader.wgsl` lives elsewhere, find it: `ls crates/client/src/*.wgsl`.)

- [ ] **Step 3: New `renderer/mod.rs` with plugin + drain stub**

```rust
use bevy::prelude::*;
use crossbeam::channel::{Receiver, Sender};
use game::{ClientId, ClientUpdateEvent, GameEventKind};

mod convert;

#[derive(Resource)]
pub struct ClientUpdates(pub Receiver<ClientUpdateEvent>);

#[derive(Resource)]
pub struct GameEvents(pub Sender<GameEventKind>);

#[derive(Resource, Default)]
pub struct LocalPlayer(pub Option<ClientId>);

pub struct SimBridgePlugin {
    pub client_recv: Receiver<ClientUpdateEvent>,
    pub game_send: Sender<GameEventKind>,
}

impl Plugin for SimBridgePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClientUpdates(self.client_recv.clone()))
            .insert_resource(GameEvents(self.game_send.clone()))
            .init_resource::<LocalPlayer>()
            .add_systems(PreUpdate, drain_client_updates);
    }
}

fn drain_client_updates(updates: Res<ClientUpdates>, mut player: ResMut<LocalPlayer>) {
    while let Ok(event) = updates.0.try_recv() {
        match event {
            ClientUpdateEvent::NewRegion(id, _data, _receiver) => {
                info!("bridge: new region {:?}", id);
            }
            ClientUpdateEvent::SetPlayer(client_id) => {
                info!("bridge: local player {:?}", client_id);
                player.0 = Some(client_id);
            }
            ClientUpdateEvent::GameCrash(e) => {
                error!("bridge: game thread crashed: {:?}", e);
            }
        }
    }
}
```

(`convert` module is created in task 3; add `mod convert;` there instead if you prefer a compiling stub now — either way this task must end compiling, so create an empty `convert.rs` if the decl stays.)

- [ ] **Step 4: Rewrite `fn main` in `crates/client/src/main.rs`**

Replace module decls (`mod layout; mod netcode; mod render_world; mod state; mod text; mod window;`) with `mod netcode; mod renderer;`. Remove the `simplelog` logger setup and `FORMAT` const, all `winit::` imports, and the `EventLoop`/`App::new(sender)` block. New main:

```rust
fn main() {
    #[cfg(feature = "pyroscope")]
    let agent_running = /* unchanged pyroscope block */;

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

    App::new()
        .add_plugins(
            DefaultPlugins
                .set(bevy::window::WindowPlugin {
                    primary_window: Some(bevy::window::Window {
                        title: "Labour of Love".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .set(bevy::log::LogPlugin {
                    filter: "wgpu=error,naga=warn".into(),
                    ..Default::default()
                }),
        )
        .add_plugins(renderer::SimBridgePlugin {
            client_recv,
            game_send: game_send.clone(),
        })
        .run();

    // Window closed: shut the sim and game threads down.
    let _ = game_send.send(GameEventKind::Quit);
    let _ = command_send.send(Command::Quit);

    #[cfg(feature = "pyroscope")]
    /* unchanged pyroscope stop block */
}
```

Add `use bevy::prelude::*;` and keep the existing `use` lines that `GameInstanceManager` needs. Remove the crate-level `#![allow(unused)]` and fix (delete) whatever dead imports it was hiding in this file.

- [ ] **Step 5: Verify**

Run: `cargo build --workspace --bins` — expect success.
Smoke: start `cargo run --bin server`, then `cargo run --bin client`. Expect: a window with bevy's default clear color, and client logs containing `bridge: local player` and `bridge: new region`. Ctrl-C cleanly.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(client): bevy app scaffold with sim-bridge stub; old wgpu renderer removed"
```

---

### Task 3: Math conversion module

**Files:**
- Create: `crates/client/src/renderer/convert.rs`
- Test: inline `#[cfg(test)]` module in the same file

**Interfaces:**
- Consumes: `rollback::IsometryReal` (nalgebra `Isometry3<OrderedFloat<f32>>`), `game::na::Perspective3<Real>`
- Produces: `convert::iso_to_transform(&IsometryReal) -> Transform`, `convert::perspective_to_projection(&Perspective3<Real>) -> PerspectiveProjection`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use game::na::{Perspective3, Translation3, UnitQuaternion, Vector3};
    use game::parry::math::Real;
    use ordered_float::OrderedFloat;

    #[test]
    fn identity_iso_is_identity_transform() {
        let iso = rollback::IsometryReal::identity();
        let t = iso_to_transform(&iso);
        assert_eq!(t, Transform::IDENTITY);
    }

    #[test]
    fn translation_maps_componentwise() {
        let iso = rollback::IsometryReal::from_parts(
            Translation3::new(OrderedFloat(1.0), OrderedFloat(2.0), OrderedFloat(-3.0)),
            UnitQuaternion::identity(),
        );
        let t = iso_to_transform(&iso);
        assert_eq!(t.translation, Vec3::new(1.0, 2.0, -3.0));
    }

    #[test]
    fn rotation_maps_to_equivalent_quat() {
        let rot = UnitQuaternion::from_axis_angle(&Vector3::y_axis(), Real::from(1.0));
        let iso = rollback::IsometryReal::from_parts(Translation3::identity(), rot);
        let t = iso_to_transform(&iso);
        let expected = Quat::from_axis_angle(Vec3::Y, 1.0);
        assert!(t.rotation.angle_between(expected) < 1e-6);
    }

    #[test]
    fn perspective_fields_carry_over() {
        let p = Perspective3::new(
            Real::from(16.0 / 9.0),
            Real::from(1.2),
            Real::from(0.1),
            Real::from(100.0),
        );
        let proj = perspective_to_projection(&p);
        assert!((proj.fov - 1.2).abs() < 1e-6);
        assert!((proj.near - 0.1).abs() < 1e-6);
        assert!((proj.far - 100.0).abs() < 1e-6);
        assert!((proj.aspect_ratio - 16.0 / 9.0).abs() < 1e-6);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p client convert` — expect FAIL: functions not found.

- [ ] **Step 3: Implement**

```rust
use bevy::math::{Quat, Vec3};
use bevy::prelude::{PerspectiveProjection, Transform};
use game::na::Perspective3;
use game::parry::math::Real;
use rollback::IsometryReal;

/// Sim isometry (right-handed, Y-up — same convention as bevy) → bevy Transform.
pub fn iso_to_transform(iso: &IsometryReal) -> Transform {
    Transform {
        translation: Vec3::new(
            iso.translation.vector.x.into_inner(),
            iso.translation.vector.y.into_inner(),
            iso.translation.vector.z.into_inner(),
        ),
        rotation: Quat::from_xyzw(
            iso.rotation.i.into_inner(),
            iso.rotation.j.into_inner(),
            iso.rotation.k.into_inner(),
            iso.rotation.w.into_inner(),
        ),
        scale: Vec3::ONE,
    }
}

pub fn perspective_to_projection(p: &Perspective3<Real>) -> PerspectiveProjection {
    PerspectiveProjection {
        fov: p.fovy().into_inner(),
        aspect_ratio: p.aspect().into_inner(),
        near: p.znear().into_inner(),
        far: p.zfar().into_inner(),
    }
}
```

(If `IsometryReal` is exported under a different path, check `crates/rollback/src/common.rs` for the alias — the current renderer imports it as `rollback::IsometryReal`.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p client convert` — expect 4 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/convert.rs crates/client/src/renderer/mod.rs
git commit -m "feat(client): nalgebra/ordered-float to glam conversion with tests"
```

---

### Task 4: Entity lifecycle bridge

The core of the migration: `GameDataUpdate` events become entity spawns/despawns/target-pose writes in Bevy's main world.

**Files:**
- Create: `crates/client/src/renderer/bridge.rs`
- Modify: `crates/client/src/renderer/mod.rs` (register resources/systems, replace stub drain)
- Test: inline `#[cfg(test)]` module in `bridge.rs`

**Interfaces:**
- Consumes: `convert::iso_to_transform` (task 3); `ClientUpdates`, `LocalPlayer` (task 2)
- Produces (used by tasks 5-10):
  - `Regions(pub BTreeMap<RegionId, Receiver<GameDataUpdate>>)` — `Resource`
  - `RegionRoots(pub BTreeMap<RegionId, Entity>)` — `Resource`
  - `SimEntityMap(pub BTreeMap<(RegionId, EntityKey), Entity>)` — `Resource`
  - `SimEntity { pub region: RegionId, pub key: EntityKey }` — `Component`
  - `SimTarget { pub pos: Vec3, pub rot: Quat, pub smoothing: f32, pub pos_snap: f32, pub rot_snap: f32 }` — `Component`, with constructors `SimTarget::body(pos, rot)` (smoothing 0.5, pos_snap 0.1, rot_snap 0.1) and `SimTarget::camera(pos, rot)` (smoothing 0.1, pos_snap 0.0005, rot_snap 0.001) matching the old renderer's constants
  - `VoxelData(pub Vec<Voxel>)` — `Component` (consumed by task 7)
  - `fn drain_client_updates`, `fn drain_region_updates` — `PreUpdate`, chained in that order

- [ ] **Step 1: Write the failing tests**

At the bottom of `bridge.rs` (headless bevy: no render plugins needed — these are plain components):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{ClientUpdates, GameEvents, LocalPlayer};
    use game::{ChunkCoords, ClientUpdateEvent, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Rollback};
    use rollback::EntityKey;
    use slotmapd::KeyData;

    fn test_app() -> (App, crossbeam::channel::Sender<ClientUpdateEvent>, crossbeam::channel::Sender<GameDataUpdate>, game::RegionId) {
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        let (update_send, update_recv) = crossbeam::channel::unbounded();
        let (game_send, _game_recv) = crossbeam::channel::unbounded();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(ClientUpdates(client_recv))
            .insert_resource(GameEvents(game_send))
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<SimEntityMap>()
            .add_systems(PreUpdate, (drain_client_updates, drain_region_updates).chain());

        let region_id = ChunkCoords::new(0, 0, 0);
        let rb = Rollback::new(None);
        client_send
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv))
            .unwrap();
        (app, client_send, update_send, region_id)
    }

    fn key(n: u64) -> EntityKey {
        EntityKey::from(KeyData::from_ffi((1 << 32) | n))
    }

    #[test]
    fn new_region_spawns_root() {
        let (mut app, _c, _u, region_id) = test_app();
        app.update();
        let roots = app.world().resource::<RegionRoots>();
        assert!(roots.0.contains_key(&region_id));
    }

    #[test]
    fn create_set_position_remove() {
        let (mut app, _c, updates, region_id) = test_app();
        app.update();

        let k = key(7);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetEntityPosition(k, rollback::IsometryReal::identity()))).unwrap();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).expect("entity mapped");
        assert!(app.world().entity(e).contains::<SimTarget>());

        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::RemoveEntity(k))).unwrap();
        app.update();
        assert!(app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).is_none());
        assert!(app.world().get_entity(e).is_err(), "despawned");
    }

    #[test]
    fn unknown_key_is_tolerated() {
        let (mut app, _c, updates, _region_id) = test_app();
        app.update();
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetEntityPosition(key(99), rollback::IsometryReal::identity()))).unwrap();
        app.update(); // must not panic
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p client bridge` — expect FAIL: types not defined.

- [ ] **Step 3: Implement `bridge.rs`**

```rust
use std::collections::BTreeMap;

use bevy::prelude::*;
use crossbeam::channel::Receiver;
use game::{ClientUpdateEvent, GameData, GameDataUpdate, GameDataUpdateKind, RegionId};
use rollback::{EntityKey, Voxel};

use super::convert::iso_to_transform;
use super::{ClientUpdates, LocalPlayer};

#[derive(Resource, Default)]
pub struct Regions(pub BTreeMap<RegionId, Receiver<GameDataUpdate>>);

#[derive(Resource, Default)]
pub struct RegionRoots(pub BTreeMap<RegionId, Entity>);

#[derive(Resource, Default)]
pub struct SimEntityMap(pub BTreeMap<(RegionId, EntityKey), Entity>);

#[derive(Component)]
pub struct SimEntity {
    pub region: RegionId,
    pub key: EntityKey,
}

/// Where the sim says this entity should be. `Transform` chases it (task 5).
/// Written by the bridge on every Do *and* Undo event: target writes are
/// last-write-wins, so a rollback/reapply burst drained in one frame
/// collapses to the final pose. That idempotence is the invariant that
/// makes the one-way bridge safe — do not switch to incremental deltas.
#[derive(Component)]
pub struct SimTarget {
    pub pos: Vec3,
    pub rot: Quat,
    pub smoothing: f32,
    pub pos_snap: f32,
    pub rot_snap: f32,
}

impl SimTarget {
    pub fn body(pos: Vec3, rot: Quat) -> Self {
        Self { pos, rot, smoothing: 0.5, pos_snap: 0.1, rot_snap: 0.1 }
    }
    pub fn camera(pos: Vec3, rot: Quat) -> Self {
        Self { pos, rot, smoothing: 0.1, pos_snap: 0.0005, rot_snap: 0.001 }
    }
}

#[derive(Component)]
pub struct VoxelData(pub Vec<Voxel>);

pub fn drain_client_updates(
    mut commands: Commands,
    updates: Res<ClientUpdates>,
    mut player: ResMut<LocalPlayer>,
    mut regions: ResMut<Regions>,
    mut roots: ResMut<RegionRoots>,
    mut map: ResMut<SimEntityMap>,
) {
    while let Ok(event) = updates.0.try_recv() {
        match event {
            ClientUpdateEvent::NewRegion(id, data, receiver) => {
                let root = commands
                    .spawn((Transform::IDENTITY, Visibility::default(), Name::new(format!("region {:?}", id))))
                    .id();
                roots.0.insert(id, root);
                regions.0.insert(id, receiver);
                spawn_region_snapshot(&mut commands, root, id, &data, &mut map);
                info!("bridge: region {:?} loaded", id);
            }
            ClientUpdateEvent::SetPlayer(client_id) => player.0 = Some(client_id),
            ClientUpdateEvent::GameCrash(e) => error!("bridge: game thread crashed: {:?}", e),
        }
    }
}

/// Port of the old `TrueWorld::new` snapshot walk.
fn spawn_region_snapshot(
    commands: &mut Commands,
    root: Entity,
    region: RegionId,
    data: &GameData,
    map: &mut SimEntityMap,
) {
    for (key, _) in data.ecs.entities.iter() {
        let mut e = commands.spawn((
            SimEntity { region, key },
            Transform::IDENTITY,
            Visibility::default(),
            ChildOf(root),
        ));
        if let Some(handle) = data.ecs.rigidbody.try_get(key) {
            if let Some(body) = data.physics.bodies.get(*handle) {
                let tf = iso_to_transform(body.position());
                e.insert((tf, SimTarget::body(tf.translation, tf.rotation)));
            }
        }
        if let Some(chunk) = data.ecs.chunk.try_get(key) {
            e.insert(VoxelData(chunk.voxels.clone()));
        }
        // camera components: added in task 6
        map.0.insert((region, key), e.id());
    }
}

pub fn drain_region_updates(
    mut commands: Commands,
    regions: Res<Regions>,
    roots: Res<RegionRoots>,
    mut map: ResMut<SimEntityMap>,
    mut targets: Query<&mut SimTarget>,
) {
    for (&region, receiver) in regions.0.iter() {
        let root = roots.0[&region];
        while let Ok(update) = receiver.try_recv() {
            // Do and Undo both applied: see SimTarget doc comment.
            match update.update_kind {
                GameDataUpdateKind::CreateEntity(key) => {
                    let e = commands
                        .spawn((
                            SimEntity { region, key },
                            Transform::IDENTITY,
                            Visibility::default(),
                            SimTarget::body(Vec3::ZERO, Quat::IDENTITY),
                            ChildOf(root),
                        ))
                        .id();
                    map.0.insert((region, key), e);
                }
                GameDataUpdateKind::RemoveEntity(key) => {
                    if let Some(e) = map.0.remove(&(region, key)) {
                        commands.entity(e).despawn();
                    }
                }
                GameDataUpdateKind::SetEntityPosition(key, iso) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetEntityPosition for unmapped {:?}", key);
                        continue;
                    };
                    let tf = iso_to_transform(&iso);
                    if let Ok(mut target) = targets.get_mut(e) {
                        target.pos = tf.translation;
                        target.rot = tf.rotation;
                    } else {
                        // Entity spawned via Commands earlier this same drain —
                        // not yet queryable. Insert (overwrites on apply).
                        commands.entity(e).insert(SimTarget::body(tf.translation, tf.rotation));
                    }
                }
                GameDataUpdateKind::SetVoxelComponent(key, Some(voxels)) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).insert(VoxelData(voxels));
                    }
                }
                GameDataUpdateKind::SetVoxelComponent(key, None) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).remove::<VoxelData>();
                    }
                }
                // Camera arms: task 6. Freecam: task 8.
                GameDataUpdateKind::AddCameraComponent(..)
                | GameDataUpdateKind::RemoveCameraComponent(..)
                | GameDataUpdateKind::UpdateCameraViewProj(..)
                | GameDataUpdateKind::UpdateCameraViewMatrix(..)
                | GameDataUpdateKind::SetFreeCam(..) => {}
            }
        }
    }
}
```

In `renderer/mod.rs`: `mod bridge; pub use bridge::*;`, add `.init_resource::<Regions>() .init_resource::<RegionRoots>() .init_resource::<SimEntityMap>()` to the plugin, replace the stub drain with `.add_systems(PreUpdate, (bridge::drain_client_updates, bridge::drain_region_updates).chain())`, and delete the stub `drain_client_updates` from `mod.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p client bridge` — expect 3 PASS. If `chunk.voxels` or `try_get` signatures differ, check `crates/rollback/src/rollback.rs` (the old `TrueWorld::new` at the pre-task-2 revision of `render_world.rs:143-181` is the reference — `git show HEAD~2:crates/client/src/render_world.rs`).

- [ ] **Step 5: Smoke and commit**

Smoke: server + client; logs show `region ... loaded`, no panics.

```bash
git add -A
git commit -m "feat(client): sim-to-bevy entity lifecycle bridge with headless tests"
```

---

### Task 5: Interpolation system

**Files:**
- Create: `crates/client/src/renderer/interpolate.rs`
- Modify: `crates/client/src/renderer/mod.rs` (register in `Update`)
- Test: inline `#[cfg(test)]` module

**Interfaces:**
- Consumes: `SimTarget` (task 4)
- Produces: `fn interpolate_transforms` in `Update`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::bridge::SimTarget;

    fn app_with(target: SimTarget, start: Transform) -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_systems(Update, interpolate_transforms);
        let e = app.world_mut().spawn((start, target)).id();
        (app, e)
    }

    #[test]
    fn converges_and_snaps_exactly() {
        let target = SimTarget::camera(Vec3::new(1.0, 0.0, 0.0), Quat::IDENTITY);
        let (mut app, e) = app_with(target, Transform::IDENTITY);
        for _ in 0..200 {
            app.update();
        }
        let t = app.world().entity(e).get::<Transform>().unwrap();
        assert_eq!(t.translation, Vec3::new(1.0, 0.0, 0.0), "must snap bit-exact, not hover nearby");
    }

    #[test]
    fn first_step_moves_partway() {
        let target = SimTarget::body(Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY);
        let (mut app, e) = app_with(target, Transform::IDENTITY);
        app.update();
        let t = app.world().entity(e).get::<Transform>().unwrap();
        assert!(t.translation.x > 0.0 && t.translation.x < 10.0);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p client interpolate` — expect FAIL.

- [ ] **Step 3: Implement**

```rust
use bevy::prelude::*;

use super::bridge::SimTarget;

/// Frame-rate-dependent exponential smoothing toward the sim pose —
/// deliberate parity with the old renderer's lerp constants. If tick
/// timestamps are ever added to SetEntityPosition, replace with
/// two-snapshot interpolation.
pub fn interpolate_transforms(mut query: Query<(&mut Transform, &SimTarget)>) {
    for (mut tf, target) in &mut query {
        let close = tf.translation.distance(target.pos) <= target.pos_snap
            && tf.rotation.angle_between(target.rot) <= target.rot_snap;
        if close {
            tf.translation = target.pos;
            tf.rotation = target.rot;
        } else {
            tf.translation = tf.translation.lerp(target.pos, target.smoothing);
            tf.rotation = tf.rotation.slerp(target.rot, target.smoothing);
        }
    }
}
```

Register in `renderer/mod.rs`: `mod interpolate;` and `.add_systems(Update, interpolate::interpolate_transforms)`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p client interpolate` — expect 2 PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(client): transform interpolation toward sim targets"
```

---

### Task 6: Camera bridge

**Files:**
- Modify: `crates/client/src/renderer/bridge.rs` (camera match arms + snapshot walk camera block)
- Test: extend `#[cfg(test)]` in `bridge.rs`

**Interfaces:**
- Consumes: `convert::perspective_to_projection`, `SimTarget::camera`
- Produces: camera entities carry `(Camera3d, Projection, Transform, SimTarget)`

- [ ] **Step 1: Write the failing test**

Add to `bridge.rs` tests:

```rust
    #[test]
    fn camera_add_update_remove() {
        let (mut app, _c, updates, region_id) = test_app();
        app.update();
        let k = key(3);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        let proj = game::na::Perspective3::new(
            game::parry::math::Real::from(1.5),
            game::parry::math::Real::from(1.2),
            game::parry::math::Real::from(0.1),
            game::parry::math::Real::from(100.0),
        );
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddCameraComponent(k, proj.clone(), rollback::IsometryReal::identity()),
        )).unwrap();
        app.update();
        // second frame: AddCameraComponent on a same-drain-spawned entity goes through Commands
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        assert!(app.world().entity(e).contains::<Camera3d>());
        let Projection::Perspective(p) = app.world().entity(e).get::<Projection>().unwrap() else {
            panic!("expected perspective projection");
        };
        assert!((p.fov - 1.2).abs() < 1e-6);

        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::RemoveCameraComponent(k))).unwrap();
        app.update();
        assert!(!app.world().entity(e).contains::<Camera3d>());
    }
```

Run: `cargo test -p client camera_add` — expect FAIL (arms are no-ops).

- [ ] **Step 2: Implement the camera arms**

Replace the empty camera arms in `drain_region_updates`:

```rust
                GameDataUpdateKind::AddCameraComponent(key, proj, iso) => {
                    let Some(&e) = map.0.get(&(region, key)) else { continue };
                    let tf = iso_to_transform(&iso);
                    commands.entity(e).insert((
                        Camera3d::default(),
                        Projection::Perspective(perspective_to_projection(&proj)),
                        tf,
                        SimTarget::camera(tf.translation, tf.rotation),
                    ));
                }
                GameDataUpdateKind::RemoveCameraComponent(key) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).remove::<(Camera3d, Projection)>();
                    }
                }
                GameDataUpdateKind::UpdateCameraViewProj(key, proj) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).insert(Projection::Perspective(perspective_to_projection(&proj)));
                    }
                }
                GameDataUpdateKind::UpdateCameraViewMatrix(key, iso) => {
                    let Some(&e) = map.0.get(&(region, key)) else { continue };
                    let tf = iso_to_transform(&iso);
                    if let Ok(mut target) = targets.get_mut(e) {
                        target.pos = tf.translation;
                        target.rot = tf.rotation;
                    } else {
                        commands.entity(e).insert(SimTarget::camera(tf.translation, tf.rotation));
                    }
                }
```

Add `use super::convert::perspective_to_projection;` and the needed `bevy::prelude` items. In `spawn_region_snapshot`, after the chunk block, port the old `TrueWorld::new` camera walk **without its unwraps**:

```rust
        if let Some(cam) = data.ecs.camera.try_get(key) {
            if let Some(handle) = cam.view_matrix {
                if let Some(body) = data.physics.bodies.get(handle) {
                    let tf = iso_to_transform(body.position());
                    e.insert((
                        Camera3d::default(),
                        Projection::Perspective(perspective_to_projection(&cam.proj_matrix)),
                        tf,
                        SimTarget::camera(tf.translation, tf.rotation),
                    ));
                }
            }
        }
```

(Check the `cam.view_matrix` type in `crates/rollback/src/rollback.rs` — the old code used `cam.view_matrix.unwrap()`, so it is an `Option<RigidBodyHandle>`.)

- [ ] **Step 3: Run tests**

Run: `cargo test -p client bridge` — expect all PASS (including the new one).

Note: bevy's own systems auto-update `PerspectiveProjection::aspect_ratio` on window resize; the sim's `UpdateCameraViewProj` also carries an aspect. Both write the same value one frame apart — acceptable; do not fight it.

- [ ] **Step 4: Smoke and commit**

Smoke: server + client. Expect no visible geometry yet, but log-quiet frames and a camera entity (add a temporary `info!` if needed, remove before commit).

```bash
git add -A
git commit -m "feat(client): camera events bridged to Camera3d/Projection"
```

---

### Task 7: Voxel meshing (synchronous) — first pixels

**Files:**
- Create: `crates/client/src/renderer/meshing.rs`
- Modify: `crates/client/src/renderer/mod.rs` (register system + startup lights/material)
- Test: inline `#[cfg(test)]` module in `meshing.rs`

**Interfaces:**
- Consumes: `VoxelData` (task 4)
- Produces: `meshing::build_chunk_mesh(&[Voxel]) -> Option<Mesh>`; `ChunkMaterial(pub Handle<StandardMaterial>)` — `Resource`; `fn mesh_chunks` in `Update` (before `interpolate_transforms` is fine, order irrelevant)

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use game::ChunkShape;
    use block_mesh::ndshape::ConstShape; // ndshape itself is no longer a direct dep (task 2)
    use rollback::{Voxel, VoxelType, CHUNK_VOXEL_COUNT};

    #[test]
    fn empty_chunk_yields_no_mesh() {
        let voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        assert!(build_chunk_mesh(&voxels).is_none());
    }

    #[test]
    fn single_voxel_yields_cube() {
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        let idx = ChunkShape::linearize([5, 5, 5]) as usize;
        voxels[idx] = Voxel::new(VoxelType::Black);
        let mesh = build_chunk_mesh(&voxels).expect("mesh");
        assert_eq!(mesh.count_vertices(), 24, "6 faces x 4 verts");
        assert_eq!(mesh.indices().unwrap().len(), 36, "6 faces x 2 tris x 3");
    }
}
```

Run: `cargo test -p client meshing` — expect FAIL.

- [ ] **Step 2: Implement `build_chunk_mesh`**

Port of the old `ChunkMesh::new` (`git show HEAD~5:crates/client/src/render_world.rs`, lines 69-115), emitting a bevy `Mesh`. Bounds come from `ChunkShape`, not hardcoded:

```rust
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use block_mesh::{greedy_quads, GreedyQuadsBuffer, RIGHT_HANDED_Y_UP_CONFIG};
use game::ChunkShape;
use rollback::Voxel;

pub fn build_chunk_mesh(voxels: &[Voxel]) -> Option<Mesh> {
    let mut buffer = GreedyQuadsBuffer::new(voxels.len());
    greedy_quads(voxels, &ChunkShape {}, [0; 3], [31; 3], &RIGHT_HANDED_Y_UP_CONFIG.faces, &mut buffer);
    if buffer.quads.num_quads() == 0 {
        return None;
    }
    let num_vertices = buffer.quads.num_quads() * 4;
    let mut indices = Vec::with_capacity(buffer.quads.num_quads() * 6);
    let mut positions = Vec::with_capacity(num_vertices);
    let mut normals = Vec::with_capacity(num_vertices);
    let mut uvs = Vec::with_capacity(num_vertices);
    let quad_uv = [[0.0f32, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];
    for (group, face) in buffer.quads.groups.iter().zip(RIGHT_HANDED_Y_UP_CONFIG.faces.into_iter()) {
        for quad in group.iter() {
            indices.extend_from_slice(&face.quad_mesh_indices(positions.len() as u32));
            positions.extend_from_slice(&face.quad_mesh_positions(quad, 1.0));
            normals.extend_from_slice(&face.quad_mesh_normals());
            for uv in quad_uv {
                uvs.push([uv[0] * quad.width as f32, uv[1] * quad.height as f32]);
            }
        }
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, VertexAttributeValues::Float32x3(positions));
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, VertexAttributeValues::Float32x3(normals));
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, VertexAttributeValues::Float32x2(uvs));
    mesh.insert_indices(Indices::U32(indices));
    Some(mesh)
}

#[derive(Resource)]
pub struct ChunkMaterial(pub Handle<StandardMaterial>);

pub fn mesh_chunks(
    mut commands: Commands,
    changed: Query<(Entity, &super::bridge::VoxelData), Changed<super::bridge::VoxelData>>,
    mut removed: RemovedComponents<super::bridge::VoxelData>,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Res<ChunkMaterial>,
) {
    for (e, voxels) in &changed {
        match build_chunk_mesh(&voxels.0) {
            Some(mesh) => {
                commands.entity(e).insert((Mesh3d(meshes.add(mesh)), MeshMaterial3d(material.0.clone())));
            }
            None => {
                commands.entity(e).remove::<Mesh3d>();
            }
        }
    }
    for e in removed.read() {
        if let Ok(mut ec) = commands.get_entity(e) {
            ec.remove::<Mesh3d>();
        }
    }
}
```

- [ ] **Step 3: Startup scene (material + lights)**

In `renderer/mod.rs` add a `Startup` system (ported from the old `bevy.rs` `setup`):

```rust
fn setup_scene(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(meshing::ChunkMaterial(materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.9,
        ..Default::default()
    })));
    commands.spawn((
        DirectionalLight { color: Color::srgb(0.98, 0.95, 0.82), shadows_enabled: true, ..Default::default() },
        Transform::default().looking_at(Vec3::new(-0.15, -0.1, 0.15), Vec3::Y),
    ));
    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.98, 0.95, 0.82),
        brightness: 100.0,
        ..Default::default()
    });
}
```

Register: `.add_systems(Startup, setup_scene)` and `.add_systems(Update, (meshing::mesh_chunks, interpolate::interpolate_transforms))`.

- [ ] **Step 4: Run tests, then visual smoke**

Run: `cargo test -p client` — expect all PASS.
Smoke: server + client — **the chunk must be visible** (lit gray/white blocks). This is the milestone: if nothing shows, check camera forward-axis sign first (temporarily spawn a `Camera3d` at `Transform::from_xyz(40., 40., 40.).looking_at(Vec3::ZERO, Vec3::Y)` to isolate whether meshing or the camera bridge is at fault).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(client): greedy-meshed chunks render via bevy pbr"
```

---

### Task 8: Input bridge and freecam cursor

**Files:**
- Create: `crates/client/src/renderer/input.rs`
- Modify: `crates/client/src/renderer/bridge.rs` (fill the `SetFreeCam` arm)
- Modify: `crates/client/src/renderer/mod.rs` (register)

**Interfaces:**
- Consumes: `GameEvents`, `LocalPlayer` (task 2); `rollback::{Key, MouseButton, InputEvent}` (task 1)
- Produces: `fn forward_input` in `PreUpdate` (before the drains — input first, then state application)

- [ ] **Step 1: Implement `input.rs`**

Poll `ButtonInput` resources rather than event readers (stable API, no Message/Event naming risk):

```rust
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use game::{GameEventKind, InputEvent};

use super::{GameEvents, LocalPlayer};

fn map_key(code: KeyCode) -> Option<game::Key> {
    use game::Key as K;
    Some(match code {
        KeyCode::KeyW => K::KeyW,
        KeyCode::KeyA => K::KeyA,
        KeyCode::KeyS => K::KeyS,
        KeyCode::KeyD => K::KeyD,
        KeyCode::KeyE => K::KeyE,
        KeyCode::Space => K::Space,
        KeyCode::ControlLeft => K::ControlLeft,
        KeyCode::ShiftLeft => K::ShiftLeft,
        KeyCode::Escape => K::Escape,
        _ => return None,
    })
}

fn map_button(b: MouseButton) -> game::MouseButton {
    match b {
        MouseButton::Left => game::MouseButton::Left,
        MouseButton::Right => game::MouseButton::Right,
        MouseButton::Middle => game::MouseButton::Middle,
        MouseButton::Back => game::MouseButton::Other(3),
        MouseButton::Forward => game::MouseButton::Other(4),
        MouseButton::Other(n) => game::MouseButton::Other(n),
    }
}

pub fn forward_input(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut last_size: Local<Option<(u32, u32)>>,
    player: Res<LocalPlayer>,
    game: Res<GameEvents>,
) {
    let Some(player) = player.0 else { return };
    let mut send = |ev: InputEvent| {
        let _ = game.0.send(GameEventKind::PlayerInput(player, ev));
    };

    for code in keys.get_just_pressed() {
        if let Some(key) = map_key(*code) {
            send(InputEvent::Key { key, pressed: true });
        }
    }
    for code in keys.get_just_released() {
        if let Some(key) = map_key(*code) {
            send(InputEvent::Key { key, pressed: false });
        }
    }
    for b in buttons.get_just_pressed() {
        send(InputEvent::MouseButton { button: map_button(*b), pressed: true });
    }
    for b in buttons.get_just_released() {
        send(InputEvent::MouseButton { button: map_button(*b), pressed: false });
    }
    if motion.delta != Vec2::ZERO {
        send(InputEvent::MouseMotion { dx: motion.delta.x, dy: motion.delta.y });
    }
    if let Ok(w) = window.single() {
        let size = (w.physical_width(), w.physical_height());
        if *last_size != Some(size) {
            if last_size.is_some() {
                send(InputEvent::Resized { width: size.0, height: size.1 });
            }
            *last_size = Some(size);
        }
    }
}
```

Register in the plugin: `.add_systems(PreUpdate, input::forward_input.before(bridge::drain_client_updates))`.

- [ ] **Step 2: Fill the `SetFreeCam` arm in `bridge.rs`**

Add `local_player: Res<LocalPlayer>` and `mut windows: Query<&mut Window, With<PrimaryWindow>>` params to `drain_region_updates`, then:

```rust
                GameDataUpdateKind::SetFreeCam(client_id, enabled) => {
                    // Only the local player's toggle may grab this window's cursor.
                    if local_player.0 != Some(client_id) {
                        continue;
                    }
                    if let Ok(mut window) = windows.single_mut() {
                        if enabled {
                            window.cursor_options.grab_mode = bevy::window::CursorGrabMode::Locked;
                            window.cursor_options.visible = false;
                        } else {
                            window.cursor_options.grab_mode = bevy::window::CursorGrabMode::None;
                            window.cursor_options.visible = true;
                        }
                    }
                }
```

(API-drift note per Global Constraints: if bevy 0.18 has moved cursor options off `Window`, query `&mut bevy::window::CursorOptions` on the primary window entity instead — same field names.)

Update the existing headless tests: `drain_region_updates` gained params — `Query<&mut Window>` and `Res<LocalPlayer>` resolve fine in a `MinimalPlugins` app (the window query is just empty), so tests need no changes beyond compiling.

- [ ] **Step 3: End-to-end smoke**

Run server + client:
- E toggles freecam: cursor locks/hides, mouse look works, WASD/Space/Ctrl fly the camera.
- Resize the window: no panic; camera aspect follows.
- Close the window: server keeps running (client's Quit is local), client process exits cleanly.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(client): bevy input forwarded to sim; freecam cursor grab gated to local player"
```

---

### Task 9: Texture-array voxel material

Recovers the pre-removal texture-array material and adapts it. The sim currently has only `VoxelType::{Black, Air}`, so the visible payoff is small until more voxel types exist — keep this task mechanical.

**Files:**
- Create: `crates/client/src/renderer/voxel_material.rs` (recovered)
- Create: `assets/shaders/voxel_texture.wgsl` (recovered)
- Modify: `crates/client/src/renderer/meshing.rs` (tex-index vertex attribute)
- Modify: `crates/client/src/renderer/mod.rs` (material plugin, texture-array build in `setup_scene`)

**Interfaces:**
- Consumes: `assets/blocks/*.png` (existing), `VoxelType`
- Produces: `StandardVoxelMaterial` (a `MaterialExtension` on `StandardMaterial`), `ATTRIBUTE_TEX_INDEX` (`Uint32x3`, shader location 8), `VoxelTypeLayers` — `Resource` mapping `VoxelType -> u32` layer

- [ ] **Step 1: Recover the old material and shader**

```bash
git show 6ad7f6c:crates/client/src/voxel/voxel_material.rs > crates/client/src/renderer/voxel_material.rs
mkdir -p assets/shaders
git show 6ad7f6c:crates/client/src/voxel/shaders/voxel_texture.wgsl > assets/shaders/voxel_texture.wgsl
```

- [ ] **Step 2: Adapt `voxel_material.rs`**

Edits to the recovered file:
- Delete `LoadingTexture`, `TextureLayers`, and the `VOXEL_TEXTURE_SHADER_HANDLE` weak-handle const (weak-handle API churned across bevy versions; load by path instead).
- In the `MaterialExtension` impl, point both shader fns at the asset path:

```rust
impl MaterialExtension for StandardVoxelMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/voxel_texture.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "shaders/voxel_texture.wgsl".into()
    }
    // keep the recovered specialize() that injects the vertex layout with ATTRIBUTE_TEX_INDEX
}
```
- Keep `ATTRIBUTE_TEX_INDEX` and `vertex_layout()` as recovered; delete the commented-out attribute lines and any `bevy_voxel_world` references.
- Prune `vertex_layout()` to the attributes the mesher actually emits: POSITION(0), NORMAL(1), UV_0(2), TEX_INDEX(8) — drop the two COLOR entries unless the recovered WGSL requires them (check the shader's vertex inputs; if it declares color, emit `Mesh::ATTRIBUTE_COLOR` as `vec![[1.0; 4]; num_vertices]` in the mesher instead of pruning).

- [ ] **Step 3: Build the texture array and layer map in `setup_scene`**

Replace the plain `StandardMaterial` chunk material with the extended one. In `renderer/mod.rs`:

```rust
#[derive(Resource, Default)]
pub struct VoxelTypeLayers(pub std::collections::BTreeMap<rollback::VoxelType, u32>);
```

In `setup_scene` (now also taking `mut images: ResMut<Assets<Image>>`, `mut voxel_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>>`), port the `assets/blocks` PNG scan from the old `state.rs` (`git show HEAD~8:crates/client/src/state.rs`, lines 115-196), but stack the decoded RGBA8 images into one array texture instead of separate bindings:

```rust
    let mut layers: Vec<image::RgbaImage> = Vec::new();
    let mut layer_names: Vec<String> = Vec::new();
    // read_dir("assets/blocks"), decode each .png exactly as the old code did,
    // push into `layers`/`layer_names` sorted by filename (BTreeMap iteration order).
    let (w, h) = (layers[0].width(), layers[0].height());
    assert!(layers.iter().all(|l| l.dimensions() == (w, h)), "all block textures must share dimensions");
    let data: Vec<u8> = layers.iter().flat_map(|l| l.as_raw().clone()).collect();
    let mut array_image = Image::new(
        bevy::render::render_resource::Extent3d { width: w, height: h, depth_or_array_layers: layers.len() as u32 },
        bevy::render::render_resource::TextureDimension::D2,
        data,
        bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    array_image.sampler = bevy::image::ImageSampler::nearest();
    let handle = images.add(array_image);
```

Map `VoxelType::Black` to the layer index of `black.png` if present, else 0, into `VoxelTypeLayers`. Create the material:

```rust
    commands.insert_resource(meshing::ChunkMaterial(voxel_materials.add(ExtendedMaterial {
        base: StandardMaterial { perceptual_roughness: 0.9, ..Default::default() },
        extension: StandardVoxelMaterial { voxels_texture: handle },
    })));
```

Change `ChunkMaterial` to hold `Handle<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>` and `mesh_chunks` to insert `MeshMaterial3d` of that type. Register the material plugin on the app: `.add_plugins(MaterialPlugin::<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>::default())`.

- [ ] **Step 4: Emit `ATTRIBUTE_TEX_INDEX` in the mesher**

`build_chunk_mesh` gains a parameter: `build_chunk_mesh(voxels: &[Voxel], layers: &VoxelTypeLayers) -> Option<Mesh>`. Inside the quad loop, look up the quad's voxel exactly as the old bevy meshing code did (`git show 6ad7f6c:crates/client/src/bevy.rs`, the `quad.minimum` linearize block):

```rust
            let voxel_index = ChunkShape::linearize(quad.minimum) as usize;
            let layer = layers.0.get(&voxels[voxel_index].kind).copied().unwrap_or(0);
            tex_indices.extend(std::iter::repeat([layer, layer, layer]).take(4));
```

and `mesh.insert_attribute(voxel_material::ATTRIBUTE_TEX_INDEX, VertexAttributeValues::Uint32x3(tex_indices));`. Update `mesh_chunks` to pass `Res<VoxelTypeLayers>`, and update the two meshing unit tests to pass `&VoxelTypeLayers::default()` (assertions unchanged — vertex/index counts don't depend on the attribute).

- [ ] **Step 5: Verify**

Run: `cargo test -p client` — all PASS.
Smoke: server + client — chunk renders with the block texture (tiled per the UV `width/height` scaling). If the pipeline fails to specialize, the error names the missing vertex attribute — cross-check `vertex_layout()` locations against the WGSL `@location` declarations.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(client): texture-array voxel material recovered from pre-removal bevy renderer"
```

---

### Task 10: Async meshing + dependency cleanup

**Files:**
- Modify: `crates/client/src/renderer/meshing.rs`
- Modify: `crates/game/src/region.rs` (delete `text_layout`), `crates/game/Cargo.toml` (drop `parley`, `winit`)
- Modify: `crates/client/Cargo.toml`, `CLAUDE.md`

**Interfaces:**
- Consumes: `build_chunk_mesh` (tasks 7/9)
- Produces: `MeshingTask` — `Component`; `mesh_chunks` split into `queue_meshing` + `apply_meshed_chunks`

- [ ] **Step 1: Write the failing test**

Meshing must complete across frames without blocking. Add to `meshing.rs` tests:

```rust
    #[test]
    fn async_meshing_attaches_mesh_eventually() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<crate::renderer::VoxelTypeLayers>()
            .add_systems(Update, (queue_meshing, apply_meshed_chunks));
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        voxels[ChunkShape::linearize([5, 5, 5]) as usize] = Voxel::new(VoxelType::Black);
        let e = app.world_mut().spawn(crate::renderer::bridge::VoxelData(voxels)).id();
        for _ in 0..100 {
            app.update();
            if app.world().entity(e).contains::<Mesh3d>() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("mesh never attached");
    }
```

Run: `cargo test -p client async_meshing` — expect FAIL (systems don't exist).

- [ ] **Step 2: Split `mesh_chunks` into queue + apply**

```rust
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};

#[derive(Component)]
pub struct MeshingTask(Task<Option<Mesh>>);

pub fn queue_meshing(
    mut commands: Commands,
    changed: Query<(Entity, &super::bridge::VoxelData), Changed<super::bridge::VoxelData>>,
    layers: Res<super::VoxelTypeLayers>,
) {
    let pool = AsyncComputeTaskPool::get();
    for (e, voxels) in &changed {
        let voxels = voxels.0.clone();
        let layers = layers.0.clone();
        let task = pool.spawn(async move {
            build_chunk_mesh(&voxels, &super::VoxelTypeLayers(layers))
        });
        commands.entity(e).insert(MeshingTask(task)); // replaces any in-flight task; stale results are dropped with it
    }
}

pub fn apply_meshed_chunks(
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut MeshingTask)>,
    mut removed: RemovedComponents<super::bridge::VoxelData>,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Option<Res<ChunkMaterial>>,
) {
    for (e, mut task) in &mut tasks {
        let Some(result) = block_on(future::poll_once(&mut task.0)) else { continue };
        match result {
            Some(mesh) => {
                let mut ec = commands.entity(e);
                ec.insert(Mesh3d(meshes.add(mesh)));
                if let Some(material) = &material {
                    ec.insert(MeshMaterial3d(material.0.clone()));
                }
                ec.remove::<MeshingTask>();
            }
            None => {
                commands.entity(e).remove::<(Mesh3d, MeshingTask)>();
            }
        }
    }
    for e in removed.read() {
        if let Ok(mut ec) = commands.get_entity(e) {
            ec.remove::<(Mesh3d, MeshingTask)>();
        }
    }
}
```

Replace the `mesh_chunks` registration with `(queue_meshing, apply_meshed_chunks)` in `Update`. Delete `mesh_chunks`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p client` — all PASS.

- [ ] **Step 4: Dependency and dead-code sweep**

- `crates/game/src/region.rs`: delete `text_layout` and the `parley` imports; `crates/game/src/camera.rs`: delete the `parley::swash` import. Remove `parley` and `winit` from `crates/game/Cargo.toml`. If anything else in `game` still references winit, it was missed in task 1 — fix it the same way.
- `crates/client/Cargo.toml`: confirm the task-2 removals stuck; additionally drop anything `cargo machete`-style unused (`rand`, `crc32fast` if nothing references them: `grep -rn "rand::\|crc32fast" crates/client/src/`).
- Remove `#![allow(unused)]` from `crates/game/src/lib.rs` and fix the fallout (delete dead imports; genuinely-pending items get `#[allow(dead_code)]` at item level with a one-line reason).
- Update `CLAUDE.md`: in the workspace-layout section, change the `crates/client` line to say the client is a Bevy app (`renderer/` bridge modules, networking in `netcode.rs`, game loop coordination in `main.rs`), and note that `game`/`rollback`/`server` must stay bevy-free and windowing-library-free.

- [ ] **Step 5: Final gates**

Run: `cargo build --workspace --bins` — success.
Run: `cargo test -p rollback -p client` — all PASS.
Run: `cargo tree -p game -i winit` — expect "nothing depends on winit" style error (winit gone from sim side).
Smoke: full server + client session — chunk textured, WASD+mouse freecam, E toggle, resize, clean exit.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(client): async chunk meshing; sim crates free of winit/parley; docs updated"
```

---

## Self-Review Notes

- **Spec coverage:** one-way bridge (tasks 4-6), SimBridgePlugin (task 2), engine-neutral input (task 1), render-state update via drain systems + bevy-internal extraction (tasks 4-6), interpolation parity (task 5), async meshing (task 10), recovered texture-array material (task 9), multiplayer freecam gating fix (task 8), integer-division aspect fix (task 1). ✓
- **Known deferred items (deliberate, not gaps):** two-snapshot interpolation (needs tick timestamps in events — noted in task 5 doc comment); per-region world offsets for the multi-region grid (region roots exist at identity; offsetting them is a one-line change when TODO.md's grid work lands); `GameCrash` still only logs (matches current behavior).
- **Type consistency check:** `SimTarget` fields/constructors identical across tasks 4/5/6; `build_chunk_mesh` signature changes once (task 9 adds the layers param) and task 10's code uses the two-arg form; `ChunkMaterial` changes handle type in task 9 and task 10's `apply_meshed_chunks` uses it opaquely via `material.0.clone()`. `InputState` naming consistent (task 1 defines, no later task references the old `WinitInput`). ✓
