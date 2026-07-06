# Basic HUD: crosshair + F3 debug overlay — design

**Date:** 2026-07-06
**Status:** Design approved, pending implementation
**Crate:** `crates/client` (with a one-variant addition to `crates/game`)

## Goal

Introduce the first UI layer in the client: a centered Minecraft-style
crosshair that is always visible during play, plus a Minecraft-F3-style debug
overlay toggled by the `F3` key showing player position, region, connection
status, and FPS.

This is deliberately the smallest increment that stands up the *entire* Bevy UI
render path (Cargo features, UI camera, a UI plugin, one piece of cross-thread
status plumbing), so that later HUD work (hotbar, health, chat, menus) is pure
addition with no new infrastructure.

Bevy UI is retained-mode and ECS-native: a UI element is an entity carrying a
`Node` plus styling/content components. UI renders as a transparent overlay on
an existing 3D camera — no second camera and no extra render target. We do
**not** use `bevy_egui`.

## Scope

**In scope**
- Centered crosshair (simple procedural white cross).
- F3-toggled debug overlay: player XYZ, home/viewer region, connection status, FPS.
- Cargo feature enablement for `bevy_ui` and text.
- Marking the player camera as the default UI camera.
- One new `ClientUpdateEvent` variant to surface region + connection status to Bevy.

**Out of scope** (each a natural follow-up spec)
- Hotbar, inventory, health bar, chat, menus/settings screens.
- Textured or color-inversion-blend crosshair (the authentic Minecraft blend).
- Any interactive widgets (`bevy_ui_widgets`) — this HUD is display-only.

## Design decisions (settled during brainstorming)

- **Crosshair:** simple procedural white cross (two thin `Node` rectangles), no
  asset, no custom blend/material. Accepts reduced visibility on bright
  surfaces as a known tradeoff of "basic".
- **Debug overlay:** hidden by default, toggled by `F3`, top-left, translucent
  backing panel for legibility. Shows all four fields (position, region,
  connection, FPS).
- **Crosshair auto-hide:** hidden while the cursor is ungrabbed (free-cam),
  mirroring how Minecraft hides the crosshair when a menu is open.

## Architecture

### 1. Cargo features (enable the UI render path)

The client currently enables `bevy_pbr`/`bevy_render`/`bevy_winit`/etc. but
**no** UI sub-crates, so `Node`/`Text` and their layout/render plugins are not
compiled into `DefaultPlugins`. Add to the Bevy dependency feature list in both
the workspace `Cargo.toml` and `crates/client/Cargo.toml`:

- `bevy_ui` — the UI components + taffy layout.
- `bevy_ui_render` — the UI render pass (split out from `bevy_ui` in 0.17+).
- `bevy_text` — `Text`/`TextFont`/`TextColor`.
- `default_font` — embedded font, so the debug text needs **no** committed font
  asset. This matters for wasm: no extra asset fetch on the browser path.

> Implementation note: verify the exact 0.18 feature names against
> `bevy`'s `Cargo.toml` when wiring this up — the `bevy_ui` / `bevy_ui_render`
> split and feature spellings are version-sensitive. Confirm the client still
> builds for `wasm32-unknown-unknown` with the new features (they must not be
> gated behind a native-only cfg).

### 2. UI camera marker

There is no camera in `setup_scene`; the active `Camera3d` is attached to the
**local player's sim entity** by the bridge at two sites:

- Snapshot path: `crates/client/src/renderer/bridge.rs:158` (spawns
  `Camera3d::default()` when `Some(key) == local_camera_key`).
- Streaming path: `crates/client/src/renderer/bridge.rs:246`
  (`GameDataUpdateKind::AddCameraComponent`).

Add `bevy::render::camera::IsDefaultUiCamera` to the component tuple at both
sites. UI root nodes then overlay this camera. Consequence: the HUD is not
visible until the player camera exists (i.e. once you are in the world), which
is the desired behavior. The camera is removed on the streaming remove path
(`bridge.rs:259`); `IsDefaultUiCamera` is removed with it — acceptable, since a
player without a camera has no HUD to show.

### 3. New module `crates/client/src/renderer/hud.rs`

Systems registered inside `SimBridgePlugin::build` (`renderer/mod.rs:39`).
Register `bevy::diagnostic::FrameTimeDiagnosticsPlugin` (for FPS) and the
`HudStatus` resource there as well.

- **`setup_hud` (Startup)**
  - Crosshair: a root `Node` (`PositionType::Absolute`, full-viewport, centered
    via `justify_content`/`align_items: Center`) carrying two child `Node`
    rectangles — one ~`16px × 2px`, one ~`2px × 16px` — each with
    `BackgroundColor(Color::WHITE)`, forming a `+`. Tagged with a `Crosshair`
    marker component.
  - Debug overlay: a root `Node` (`PositionType::Absolute`, top-left, `padding`,
    translucent `BackgroundColor` e.g. `Color::srgba(0.0, 0.0, 0.0, 0.5)`),
    `Visibility::Hidden`, tagged `DebugOverlay`, with a single child `Text`
    entity (tagged `DebugText`) using `TextFont { font_size: 14.0, .. }` and
    `TextColor(Color::WHITE)`. The debug string is multi-line (`\n`-separated).

- **`toggle_debug` (Update)** — on `KeyCode::F3` just-pressed, flip the
  `DebugOverlay` root's `Visibility` between `Hidden` and `Visible`.

- **`update_debug_text` (Update)** — runs only when the overlay is visible
  (`run_if` on overlay visibility, or early-return). Reads the local player's
  `Transform` (via `LocalPlayer` + `SimEntityMap`), the `HudStatus` resource,
  and the FPS diagnostic from `DiagnosticsStore`; writes the `DebugText` `Text`
  by calling the pure `format_debug_text(..)` helper (below).

- **`update_crosshair_visibility` (Update)** — reads the `PrimaryWindow`
  `CursorOptions.grab_mode`; sets the `Crosshair` root `Visibility` to `Hidden`
  when the cursor is not locked (free-cam), `Visible` otherwise. `SetFreeCam`
  already toggles `CursorOptions` in `drain_region_updates`
  (`bridge.rs:304`), so this system just observes that state.

Pure helper (unit-testable, no Bevy types beyond primitives):

```rust
pub fn format_debug_text(
    pos: Vec3,
    home: RegionId,
    viewer: RegionId,
    conn: ConnectionState,
    fps: Option<f64>,
) -> String
```

Produces lines like:

```
XYZ: 12.34 / 65.00 / -8.10
Region: home (0, 0)  viewer (0, 1)
Status: Ready
FPS: 143
```

### 4. Status plumbing (region + connection)

Region and connection state live in `GameInstanceManager` on the game thread
(`main.rs`), never surfaced to Bevy. Add a single channel event that reuses the
existing `client_event_send` → `ClientUpdates` → `drain_client_updates` path.

- **`crates/game/src/lib.rs`** — extend `ClientUpdateEvent` (currently at
  `lib.rs:42`) with:

  ```rust
  HudStatus {
      home_region: RegionId,
      viewer_region: RegionId,
      connection: ConnectionState,
  }
  ```

  and a new small enum in the `game` crate:

  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum ConnectionState { Connecting, CatchingUp, Ready }
  ```

  `ConnectionState` is derived from the manager's existing `ready` /
  `is_caught_up` fields: not ready → `Connecting`; ready but not caught up →
  `CatchingUp`; ready and caught up → `Ready`.

- **`crates/client/src/main.rs`** — `GameInstanceManager` emits
  `ClientUpdateEvent::HudStatus { .. }` through `client_event_send` whenever the
  relevant state changes: on becoming ready, on catch-up transition, and on a
  region flip (where `home_region` / `viewer_region` update — see
  `update_window` around `main.rs:304`). Emit is edge-triggered (only on change)
  to avoid channel spam; a helper that compares against the last-sent status
  keeps this localized. This same path is exercised by the wasm `pump()` loop,
  so wasm gets status for free.

- **`crates/client/src/renderer/bridge.rs`** — `drain_client_updates`
  (`bridge.rs:66`) gains a match arm for `HudStatus { .. }` that writes a new
  Bevy resource:

  ```rust
  #[derive(Resource, Default)]
  pub struct HudStatus {
      pub home_region: Option<RegionId>,
      pub viewer_region: Option<RegionId>,
      pub connection: ConnectionState, // defaults to Connecting
  }
  ```

  `drain_client_updates` already takes several `ResMut`; add `ResMut<HudStatus>`
  to its signature. Register `HudStatus` in `SimBridgePlugin::build`.

The `local_server.rs` (offline/wasm embedded world) match on `ClientUpdateEvent`
must also handle the new variant — for the offline world it can emit a static
`Ready` status once, or simply ignore it (overlay shows `Connecting`/last
value). Ignoring is acceptable for a debug overlay; emitting `Ready` is nicer
and cheap.

## Data flow

```
game thread (GameInstanceManager)
  status change  ──►  ClientUpdateEvent::HudStatus  ──►  client_event_send
                                                            │
Bevy thread                                                 ▼
  drain_client_updates  ──►  writes HudStatus resource
                                                            │
  update_debug_text (when overlay visible) ◄── HudStatus, LocalPlayer→Transform,
                                                DiagnosticsStore(FPS)
                                                            │
                                                            ▼
                                               format_debug_text() → Text
```

Crosshair has no data flow — static entities whose only mutation is a
`Visibility` toggle driven by cursor grab state.

## Error / edge handling

- **No camera yet** (pre-spawn): HUD roots exist but nothing renders until a
  camera with `IsDefaultUiCamera` appears. Expected, not an error.
- **No local player Transform** (region not yet mapped): `update_debug_text`
  shows placeholder (`XYZ: --`) rather than panicking on a missing entity.
- **FPS diagnostic not yet populated** (first frames): `fps: None` →
  `FPS: --`.
- **Offline world**: `HudStatus` may never update past `Connecting` unless
  `local_server.rs` emits `Ready`; debug overlay degrades gracefully.

## Testing

Fits the existing headless `cargo test -p client` suite (`MinimalPlugins` /
`AssetPlugin`, no window/GPU). UI *rendering* cannot be asserted headless; be
explicit about that ceiling rather than implying render coverage.

- **`format_debug_text` unit tests** — formatting for each `ConnectionState`,
  `Some`/`None` FPS, and coordinate rounding. Pure function, no Bevy.
- **Event → resource mapping** — build an app with `drain_client_updates`, send
  a `ClientUpdateEvent::HudStatus`, assert the `HudStatus` resource reflects it.
  Follows the existing bridge test pattern (`bridge.rs:428` onward).
- **Connection-state derivation** — unit-test the `ready`/`is_caught_up` →
  `ConnectionState` mapping helper.
- **Spawn assertions (best-effort)** — under `MinimalPlugins` + UI plugins,
  assert `setup_hud` spawns the expected `Crosshair` / `DebugOverlay` /
  `DebugText` marker entities. Note in the test that this covers construction,
  not visual rendering.

## Files touched

- `Cargo.toml` (workspace) — Bevy UI/text features.
- `crates/client/Cargo.toml` — Bevy UI/text features.
- `crates/game/src/lib.rs` — `ClientUpdateEvent::HudStatus`, `ConnectionState`.
- `crates/client/src/renderer/hud.rs` — **new**: HUD components, systems,
  `format_debug_text`, `HudStatus` resource.
- `crates/client/src/renderer/mod.rs` — module decl, plugin registration,
  `FrameTimeDiagnosticsPlugin`, `HudStatus` resource.
- `crates/client/src/renderer/bridge.rs` — `IsDefaultUiCamera` at two camera
  sites; `HudStatus` arm in `drain_client_updates`.
- `crates/client/src/main.rs` — edge-triggered `HudStatus` emission from
  `GameInstanceManager`.
- `crates/client/src/local_server.rs` — handle the new variant (emit `Ready`
  or ignore).

## Known limitations / future work

- Crosshair is not visible on bright/white surfaces (no inversion blend) — a
  later spec can upgrade to a textured or blended crosshair.
- Debug overlay rebuilds its full string each visible frame; fine for a debug
  tool, not a pattern to copy for high-frequency HUD elements (prefer mutating
  only changed spans).
- No interactive UI yet; when menus arrive, adopt `bevy_ui_widgets` +
  observer (`On<Activate>`) rather than the legacy `Interaction` polling model.
