# Collider Wireframe Debug View — Design

**Date:** 2026-07-08
**Status:** Approved

## Purpose

A runtime debug view that draws wireframes of the physics colliders — voxel
chunk colliders (block-scale + chisel-scale cuboids) and entity colliders
(player capsules etc.) — so collider bugs (misplaced, stale, missing, seam
artifacts) are visible in-game. Analogous to `bevy_rapier`'s
`RapierDebugRenderPlugin`, adapted to this codebase's split between the
Bevy-free sim (game thread) and the Bevy render world.

## Background: how bevy_rapier does it

Rapier itself ships the hard part: `DebugRenderPipeline` (behind the
`debug-render` cargo feature) walks `RigidBodySet`/`ColliderSet`/`NarrowPhase`
and emits colored line segments through a two-method `DebugRenderBackend`
trait (`draw_line`, plus a `filter_object` predicate). It knows how to
outline every shape type (cuboid, compound, capsule, trimesh, …).
`bevy_rapier`'s plugin is a thin backend impl that forwards lines to Bevy
`Gizmos` each frame, plus `DebugRenderMode` bitflags.

The vendored fork at `crates/rapier` already carries the full pipeline
(`src/pipeline/debug_render_pipeline/`); the `debug-render` feature is simply
not enabled in the workspace dependency. `DebugColor = [f32; 4]`.

We cannot use `bevy_rapier` because the physics sets live in `GameData`
(`PhysicsState` in `crates/game/src/state.rs`) on the blocking game thread,
and `game` must stay Bevy-free. What bevy_rapier gets for free — backend and
gizmos in the same world — we replace with the existing sim→render channel
pattern.

## Scope

- **In:** collider shape wireframes (`DebugRenderMode::COLLIDER_SHAPES`),
  all collider types, all loaded sim regions, radius-filtered around the
  viewer. Native + wasm.
- **Out:** contacts, collider AABBs, rigid-body axes, joints (rapier's mode
  bitflags make these easy to add later), far regions (no sim, no colliders),
  interpolation of debug lines.

## Architecture & data flow

Lines are generated on the game thread by rapier's own `DebugRenderPipeline`
and shipped to the render side as plain data:

```
game thread (per region, per tick, only while enabled)
  DebugRenderPipeline::render_colliders(&mut collector, bodies, colliders)
    → collector: Vec<DebugLine { a: [f32;3], b: [f32;3], color: [f32;4] }>  (region-local)
    → ClientUpdateEvent::DebugLines(RegionId, Vec<DebugLine>)   (replace-on-receipt)

bevy side
  bridge consumes event → latest buffer per region in a DebugColliderLines resource
  gizmo system (runs while the F3 overlay is visible) → for each region buffer:
      Gizmos::line(a + world_offset, b + world_offset, color)
```

Components:

- **Cargo wiring:** root `Cargo.toml` — add `"debug-render"` to the
  `rapier3d` workspace dependency features (render-only code, no effect on
  simulation math or determinism); add `"bevy_gizmos"` to the workspace Bevy
  features.
- **`game`: line collector.** A `DebugRenderBackend` impl that pushes
  segments into a `Vec<DebugLine>`. Engine-neutral; converts `Point<Real>` →
  `[f32; 3]` by unwrapping `OrderedFloat` with `.0` (same as `convert.rs`).
  Mode: `COLLIDER_SHAPES` only. Rapier's default `DebugRenderStyle` already
  colors fixed vs dynamic bodies differently, so chunk colliders and the
  player capsule are visually distinct with no extra styling.
- **`game`: `ClientUpdateEvent::DebugLines(RegionId, Vec<DebugLine>)`.**
  Replace-on-receipt semantics per region, like `NewFarRegion`. `DebugLine`
  is a plain struct in `game` (not part of rollback state, never serialized
  over the wire).
- **client `instance.rs`: generation site.** After each region tick, if the
  debug flag is set, run the pipeline over that region's
  `data.physics` sets and send the event. Not part of rollback/replay —
  runs after the tick, reads state only.
- **client `renderer/bridge.rs`: resource + consumption.** A
  `DebugColliderLines` resource (`HashMap<RegionId, Vec<DebugLine>>`).
  `DebugLines` replaces the region's entry; `RemoveRegion` teardown also
  removes it.
- **client renderer: gizmo draw system.** While the F3 overlay is visible,
  iterate buffers, add the region's `world_offset()`, draw with
  `Gizmos::line`. When hidden, draw nothing and clear the resource (drops
  stale buffers on toggle-off).

## Toggle path (F3 → game thread)

Debug state is not sim state — it must not touch rollback, `GameData`, or the
wire protocol. A shared flag struct (room to grow):

```rust
pub struct DebugFlags {
    pub collider_wireframes: Arc<AtomicBool>,
}
```

created at startup and handed to both the `GameInstanceManager` (game thread)
and the Bevy app (as a resource). The existing `toggle_debug` system in
`hud.rs` (F3) sets it alongside flipping overlay visibility, so the collider
view is part of the single F3 debug mode. While false, the game thread does
zero extra work and sends nothing — the feature costs nothing when off.
Works identically on wasm (inline sim driver, same process; atomics fine
single-threaded).

## Radius filter & tunables

```rust
/// Only colliders whose broad-phase AABB comes within this distance of the
/// viewer get debug wireframes.
pub const DEBUG_COLLIDER_RADIUS: f32 = 64.0;
```

Named constant in `game` (tunables convention). Implemented via rapier's
`filter_object` hook: draw a collider only if its AABB is within
`DEBUG_COLLIDER_RADIUS` of the viewer position.

Viewer position: for the home region, region-local straight from the player's
sim body (as `Instance::local_player_translation` does). For other loaded
regions, rebase the world-space viewer position into that region's local
frame using the two regions' `world_offset()`s. If the player body isn't
readable yet, skip generation for that tick (no lines beats wrong lines).

Worst case at r = 64 is roughly a 2-chunk-radius sphere of chisel-scale
colliders — thousands of segments, comfortably within Bevy gizmo budget.

## Frequency & staleness

Regenerate once per sim tick (50 Hz) per loaded region while enabled,
replacing that region's buffer — stateless, no dirty tracking. Static voxel
colliders are identical tick-to-tick; dynamic bodies' wireframes trail the
interpolated visuals by ≤ 1 tick, which is acceptable for a debug view
(bevy_rapier has the same tick-vs-frame mismatch). Reconcile replays don't
multiply sends: generation happens once per real tick advance, not per
re-simulated tick.

## Error handling

Channel send failures are ignored (receiver gone during shutdown), matching
every other `ClientUpdateEvent` send. Pipeline generation cannot fail; an
empty region produces an empty buffer.

## Testing

- **`game` unit tests:** collector backend — build a `PhysicsState` with a
  fixed cuboid and a dynamic ball, run `render_colliders`, assert non-empty
  segments for both; assert a collider placed beyond
  `DEBUG_COLLIDER_RADIUS` of the viewer produces no segments.
- **client headless tests (existing harness):** with the flag set, tick and
  assert a `DebugLines` event arrives with a plausible buffer; with the flag
  clear, assert no `DebugLines` events. Bridge test: resource populated on
  `DebugLines`, entry removed on `RemoveRegion`. (Gizmo draw output isn't
  assertable headlessly; the resource is the testable seam.)
- Tag any test measured over 100 ms with the `slow` ignore convention.
- No new build/run commands — `justfile` unchanged.
