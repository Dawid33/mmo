# Chunk/Region Debug Wireframe (F4) — Design

**Date:** 2026-07-10
**Status:** Approved for planning

## Purpose

A Minecraft-F3+G-style debug view that draws chunk and region boundaries as
colored wireframes in the client, toggled with a single key (F4). It answers
"where am I relative to the chunk grid / region tiling" while playtesting and
debugging streaming, meshing, and handoff behavior.

## Scope

- Client-only. No sim, protocol, or server changes. `game`/`server` stay
  untouched.
- Immediate-mode rendering via Bevy `Gizmos` (the pattern already used by
  `crates/client/src/renderer/collider_debug.rs`). No retained meshes,
  materials, or entities.
- Native client. Nothing here is platform-specific, so wasm gets it for free,
  but no wasm-specific work is in scope.

## Toggle

- New module `crates/client/src/renderer/grid_debug.rs`, registered in
  `SimBridgePlugin` (`renderer/mod.rs`) alongside `collider_debug`.
- Resource `GridDebug { enabled: bool }`, default off.
- System `toggle_grid_debug`: flips `enabled` on `KeyCode::F4`
  `just_pressed`, reading `Res<ButtonInput<KeyCode>>` directly (client-only
  concern; never forwarded to the sim input path), mirroring
  `hud::toggle_debug`.
- Fully independent of F3: F4 works with the HUD off; F3 does not affect the
  grid view.

## What is drawn

All drawing happens in one system, `draw_grid_debug`, which early-outs when
`enabled` is false. Camera position comes from the camera's
`GlobalTransform`.

### Chunk borders — yellow, current chunk red

- Compute the camera's integer chunk-grid cell as `floor(world_pos / 32.0)`
  per axis (identical keying to `meshing::queue::ChunkGrid`; must use
  floor-division semantics, not truncation, so negative coordinates align).
- Draw a 32-unit wireframe cube (12 edges) for every grid cell within
  `CHUNK_GRID_RADIUS_XZ` horizontally and `CHUNK_GRID_RADIUS_Y` vertically of
  the camera's cell.
- Cells are drawn from pure geometry, not gated on load state — the grid is a
  coordinate visualization and stays continuous through air and unloaded
  space.
- The cell containing the camera is drawn in `CHUNK_CURRENT_COLOR` (red); all
  others in `CHUNK_GRID_COLOR` (yellow).
- Worst case at defaults: 5×5×5 = 125 cubes ≈ 1,500 line segments.

### Region walls — cyan (near) / purple (far)

- For each region key in `bridge::RegionRoots` draw in `REGION_NEAR_COLOR`
  (cyan); for each key in `bridge::FarRegionRoots` draw in `REGION_FAR_COLOR`
  (purple).
- Each region is a `REGION_SIZE` (currently 1024.0) square tile whose world
  origin is `RegionCoords::world_offset()`.
- Along each of the four boundary edges, draw a vertical "wall band" centered
  on the camera's height:
  - vertical lines every `REGION_WALL_INTERVAL` (32.0) units along the edge,
  - horizontal rails at the band's top, bottom, and at camera height,
  - band spans `camera_y ± REGION_WALL_HALF_HEIGHT` (64.0).
- Shared edges between adjacent loaded regions will be drawn twice (once per
  region); this is accepted — line counts are small and overdraw of identical
  lines is invisible.
- Expected: up to ~9 near + ~25 far regions (`FAR_VIEW_RADIUS = 2` → 5×5
  window); each region edge is ~32 verticals + 3 rails, so worst case is
  roughly 25 × 4 × 35 ≈ 3.5k segments — still well within gizmo budget.

## Tunables

Named `const`s at the top of `grid_debug.rs` (no UI, per project preference):

| Constant | Default | Meaning |
|---|---|---|
| `CHUNK_GRID_RADIUS_XZ` | 2 | chunk cells drawn each side of camera, horizontally |
| `CHUNK_GRID_RADIUS_Y` | 2 | chunk cells drawn above/below camera |
| `REGION_WALL_HALF_HEIGHT` | 64.0 | half-height of region wall band around camera y |
| `REGION_WALL_INTERVAL` | 32.0 | spacing of vertical lines along region edges |
| `CHUNK_GRID_COLOR` | yellow | chunk cube edges |
| `CHUNK_CURRENT_COLOR` | red | the chunk cell containing the camera |
| `REGION_NEAR_COLOR` | cyan | walls of simulated (subscribed) regions |
| `REGION_FAR_COLOR` | purple | walls of far/LOD regions |

## Architecture / code shape

- Pure geometry helpers, unit-testable without Bevy render state:
  - `chunk_cell_of(pos: Vec3) -> IVec3` — floor-division cell keying.
  - `chunk_cube_lines(cell: IVec3) -> Vec<(Vec3, Vec3)>` — 12 edges.
  - `region_wall_lines(coords: RegionCoords, camera_y: f32) -> Vec<(Vec3, Vec3)>`.
- Thin systems:
  - `toggle_grid_debug` — input edge → resource flip.
  - `draw_grid_debug` — iterate helpers' output, `gizmos.line(a, b, color)`.
- No new plugins: `bevy_gizmos` is already in the workspace feature set and
  `GizmoPlugin` ships via `DefaultPlugins`.

## Performance

Immediate-mode gizmos re-tessellate per frame; at ≤ ~5k segments this is
negligible (the existing collider wireframe view draws comparable volumes).
If profiling ever disagrees, the fallback is retained `LineList` meshes
rebuilt on chunk-boundary crossings — explicitly out of scope now.

## Testing

- Unit tests on the pure helpers (in `grid_debug.rs` or
  `crates/client/tests/`):
  - `chunk_cell_of` at positive, negative, and boundary coordinates
    (floor vs truncation regression case).
  - `chunk_cube_lines` returns 12 edges spanning exactly `[cell*32,
    (cell+1)*32]` per axis.
  - `region_wall_lines` line count matches the interval math and spans
    `camera_y ± REGION_WALL_HALF_HEIGHT`; region origin honors
    `world_offset()` including negative region coords.
- Headless toggle test with `MinimalPlugins`: press F4 → `enabled` flips;
  press again → flips back. Mirror the existing HUD toggle test's key-edge
  handling (`reset_all()` vs `clear()` caveat documented in `hud.rs`).
- The gizmo-emitting draw system is a thin shell over tested helpers and is
  not itself tested.
- Keep new tests under the 100ms bar (no `#[ignore]` tags expected).

## Out of scope

- Showing grid state in the F3 debug text.
- Chunk load/mesh state coloring (loaded vs pending vs culled).
- Retained-mesh rendering path.
- Any UI for tunables.
