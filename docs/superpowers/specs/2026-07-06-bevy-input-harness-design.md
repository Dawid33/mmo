# Bevy-glue input harness + "E never grabs cursor" bug

**Date:** 2026-07-06
**Branch:** `bevy-input-harness` (off `develop`)
**Companion handoff:** `docs/handoffs/2026-07-06-bevy-input-harness-handoff.md`

## Problem

Running the native client, pressing **E** (the fps-cam toggle) never grabs/locks
the cursor, making mouse-look unusable. The sim-side E path is verified working
in `SimHarness` (headless, no Bevy). The fault is therefore in the **Bevy client
glue that `SimHarness` bypasses** — the layer between OS input and the sim
(`forward_input` → `GameInstanceManager` gates) and the layer that turns a
`SetFreeCam` delta into a real cursor grab (`drain_region_updates`). That glue
has **no test coverage** today.

The three prime suspects (from the handoff):

- **(a)** `SetFreeCam(client_id, mode)` reaches the bridge but
  `local_player.0 != Some(client_id)`, so the grab branch is skipped
  (`bridge.rs:315`). The sim emits `client_id` from the `data.clients` key;
  `LocalPlayer` is set from the `SetPlayer(client_id)` derived from
  `PlayerRegion` — the harness must prove these are the same id.
- **(b)** The E `PlayerInput` is dropped inside `GameInstanceManager` by the
  `!is_caught_up` gate (`instance.rs:175`) or the `home_region`
  unset/not-loaded gate (`instance.rs:184-193`), so no `SetFreeCam` is ever
  produced.
- **(c)** `LocalPlayer` is never set live, so `forward_input` early-returns
  (`input.rs:44`) and no input is ever forwarded at all.

## Goal

A **headless Bevy-App test harness** that drives the *real* client input
pipeline end to end (`ButtonInput<KeyCode>` → `forward_input` → game channel →
`GameInstanceManager` gates → sim tick → `SetFreeCam` → `drain_region_updates`
→ `CursorGrabMode::Locked`), reproduces the bug as a failing test, and — via
per-boundary inspectors — pinpoints *which* seam breaks. Then root-cause, fix,
and confirm the harness plus `cargo test -p client` are green.

## Architecture

New module **`crates/client/src/bevy_harness.rs`** exposing
`BevyInputHarness` (sibling to `harness.rs`'s `SimHarness`; `pub mod`, not
`cfg(test)`, so integration tests can use it). Exercised by a new integration
test **`crates/client/tests/bevy_input.rs`** (mirrors `crossing.rs` /
`harness_smoke.rs`).

`BevyInputHarness` owns two halves wired over the *same* channels the live
client uses:

### 1. Sim core (identical to `SimHarness`)

A real `LocalServer` (`WorldManager<InlineSpawner>`) + `GameInstanceManager`,
driven in deterministic lockstep — no threads, no wall clock. Reuses the exact
channel setup from `SimHarness::new`:

- `(game_event_send, game_event_recv)` — the manager's input channel.
- `(bridge_send, bridge_recv)` — the manager's `ClientUpdateEvent` output.
- client↔server via `client.client_packet_recv()` + a `server_to_client`
  channel; `LocalServer::new` bridges them.

`game::set_hash_verification(true)` stays on, as in `SimHarness`, so the driven
path still enforces `hash(before) == hash(after undo)`.

### 2. Bevy front/back (headless `App`)

`App::new().add_plugins((MinimalPlugins, AssetPlugin::default()))` — **not** the
full `SimBridgePlugin`, which pulls `MaterialPlugin` + `FrameTimeDiagnostics` +
`setup_scene` (texture/GPU work the headless client tests deliberately avoid).
Instead, register by hand exactly the systems and resources the pipeline needs:

**Systems**, with the **same PreUpdate ordering `SimBridgePlugin` uses** (this
ordering — `forward_input` *before* the drain chain — is itself under test):

```
PreUpdate: (forward_input, (drain_client_updates, drain_region_updates).chain()).chain()
```

`dedupe_ghosts` is irrelevant to the grab path and omitted.

**Resources** (all init'd by hand since `MinimalPlugins` provides none of the
input/window ones):

- `ButtonInput<KeyCode>`, `ButtonInput<MouseButton>` — `init_resource`
- `AccumulatedMouseMotion` — `init_resource` (read by `forward_input`)
- `LocalPlayer`, `Regions`, `RegionRoots`, `SimEntityMap`, `HudStatus` — init
- `ClientUpdates(bridge_recv)` — the manager's bridge output
- `GameEvents(tap_send)` — **a separate tap channel, not the manager's**
  (see the tee below)

**Stub window:** spawn one entity carrying `(Window::default(),
CursorOptions::default(), PrimaryWindow)`. Bevy 0.18 split `CursorOptions` out
of `Window` into its own component; `forward_input` queries `&Window` and
`drain_region_updates` queries `&mut CursorOptions`, both `With<PrimaryWindow>`,
so the single window entity must carry all three. Without it,
`windows.single_mut()` in the grab branch returns `Err` and silently no-ops
(exactly why today's bridge tests never touch the grab path).

### The game-channel tee (clean sniff point)

`forward_input` sends into `GameEvents.0`. Rather than clone the manager's
sender (which would make the sniff-vs-consume a race on one MPMC channel), the
Bevy app gets a **dedicated** `(tap_send, tap_recv)` channel. Each step, after
the front `app.update()`, the harness drains `tap_recv`, records what it saw
(e.g. `PlayerInput` carrying `Key::KeyE`), and **relays every event into the
manager** via `manager.push_game_event(ev)`. The harness thus observes every
input event `forward_input` produces while still feeding the real manager.
(`forward_input` only ever emits `PlayerInput` here: `Resized` needs a size
change, `MouseMotion` needs nonzero `AccumulatedMouseMotion` — neither occurs
with a static stub window and default motion.)

## Step model

`MinimalPlugins` has **no `InputPlugin`**, so `ButtonInput` is never
auto-cleared. Leaving a press edge uncleared makes `get_just_pressed()` re-fire
every frame — toggling fps-cam every tick. So each `step()` is a sandwich:

```rust
fn step(&mut self) -> Result<(), GameError> {
    self.app.update();          // #1 forward_input: E edge -> tap channel; drains PRIOR tick's updates
    self.drain_tap();           //    record + relay each input event into the manager
    self.clear_input_edges();   //    ButtonInput::clear() consumes just_pressed/just_released (keeps `pressed`)

    self.client.send_tick();    // sim lockstep == SimHarness::step
    self.client.pump(&self.server_to_client)?;   // PlayerInput(E) then Tick -> toggle -> SetFreeCam emitted
    self.server.pump()?; self.server.tick();
    self.client.pump(&self.server_to_client)?;

    self.app.update();          // #2 drain_region_updates: THIS tick's SetFreeCam -> CursorGrabMode
    self.clear_input_edges();   //    (no new edges; keep ButtonInput clean)
    Ok(())
}
```

FIFO ordering guarantees correctness: the tee relays `PlayerInput(E, pressed)`
into `game_event_recv` *before* `send_tick` enqueues `Tick`, so `client.pump`
registers the key press, then the `Tick` runs the sim toggle (`region.rs:243`)
in the same drain. The grab lands on the client's **predicted** timeline
(`region.rs` Tick emits `SetFreeCam` on the client's own `Region`) without
waiting on the server — matching live instant feedback.

`connect()` mirrors `SimHarness::connect` (call `client.start()`, then step a
handful of times) so the `PlayerRegion → SetPlayer → Region` handshake settles
and `is_caught_up` becomes true before input is driven.

Input entry points: `press_key(KeyCode)` / `release_key(KeyCode)` set
`ButtonInput` state (`press`/`release`); convenience `press_e()` / `release_e()`.
One press edge = one toggle; a second toggle needs release-then-press.

## Inspectors (pinpoint the broken seam)

Each maps to a suspect so a red bar names the boundary:

- `local_player() -> Option<ClientId>` — is `LocalPlayer` set? → suspect **(c)**
- `e_forwarded_count() -> usize` — count of `PlayerInput(_, Key::KeyE, pressed)`
  seen on the tap. Did the press clear the `LocalPlayer` gate in
  `forward_input`? → separates (c)/front from the rest
- `sim_fps_cam_mode() -> Option<bool>` — the local player's `Client.fps_cam_mode`
  read from `world_ref().data(home)`. Did the press survive the manager gates
  **and** toggle the sim? → suspect **(b)**. (If not already reachable from the
  client crate, add a small read-only hook on `GameInstanceManager`, mirroring
  the existing `local_player_translation`.)
- `cursor_grab_mode() -> CursorGrabMode` — the stub window's live grab mode.
  Did `SetFreeCam → grab` land? → suspect **(a)** + the end assertion.

Diagnostic logic: `sim_fps_cam_mode` flipped but grab did not ⇒ bug is the
bridge gate (a) / window / region channel. `sim_fps_cam_mode` did not flip ⇒
the E press never reached the sim (b/c); consult `e_forwarded_count` and
`local_player` to say which.

## Tests (`crates/client/tests/bevy_input.rs`)

- `pressing_e_grabs_and_releases_cursor` — `connect()`, settle, `press_e()`,
  step ⇒ `CursorGrabMode::Locked`; `release_e()` + `press_e()` again ⇒
  `CursorGrabMode::None`. **The reproducer — must fail today, pass after fix.**
- `local_player_is_set_after_connect` — asserts `local_player().is_some()`
  after `connect()`. Guards suspect (c).
- `e_press_forwards_and_toggles_sim` — after `connect()` + `press_e()` + step,
  `e_forwarded_count() >= 1` and `sim_fps_cam_mode() == Some(true)`. Guards
  suspects (b)/(c); isolates the sim boundary from the grab boundary.

These decompose the pipeline so the failing test set localizes the fault before
any fix is written.

## Constraints

- `crates/game` and `crates/server` stay Bevy-free and windowing-free; the
  harness lives entirely in `crates/client`.
- Determinism bar unchanged; `set_hash_verification(true)` stays on.
- Headless only — `MinimalPlugins`/`AssetPlugin`, no window, no GPU.
- Build/test via the Cranelift backend; if a timing-sensitive test flakes under
  cranelift, confirm with stock `cargo test` (known flake, per handoff).
- Keep the `justfile` in sync if a new one-word test command is worth adding.

## Out of scope

- Fixing the live "~12s connect delay" (a wall-clock/threading artifact; the
  deterministic harness models the *logical* gate — N ticks of
  `!is_caught_up`/no-home — not seconds).
- Any change to `SimHarness` or the sim-side E path (already verified).
- Refactoring `SimBridgePlugin` registration (the harness re-declares the
  subset it needs rather than reusing the plugin).
