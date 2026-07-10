# Chunk/Region Debug Wireframe (F5) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Minecraft-F3+G-style debug view drawing chunk borders (yellow, current chunk red) and region boundary walls (cyan near / purple far) as gizmo wireframes in the client, toggled with F5.

**Architecture:** One new client-only module, `crates/client/src/renderer/grid_debug.rs`: pure geometry helpers (unit-tested), a `GridDebug` toggle resource flipped by an F5 input system, and a thin `Gizmos` draw system that re-derives all lines each frame from the camera position plus `bridge::RegionRoots`/`FarRegionRoots`. Mirrors the existing `collider_debug.rs` gizmo pattern. Spec: `docs/superpowers/specs/2026-07-10-chunk-region-debug-wireframe-design.md`.

**Tech Stack:** Rust, Bevy 0.18 (`Gizmos` immediate-mode lines, `ButtonInput<KeyCode>`, `MinimalPlugins` headless tests).

## Global Constraints

- Client-only: touch nothing outside `crates/client/` (spec: `game`/`server`/protocol untouched).
- The toggle key is **F5**. F3 is the HUD/collider toggle and F4 is cave culling (`meshing/visibility.rs:312`) — do not touch either.
- Read `KeyCode` directly via `Res<ButtonInput<KeyCode>>` in the client system; never add F5 to `renderer/input.rs::map_key` (that path forwards to the sim).
- All tunables are named `const`s at the top of `grid_debug.rs` (project preference; no UI).
- New tests must run <100ms each (no `#[ignore]` tags should be needed).
- Do not modify vendored fork crates (`crates/nalgebra`, `crates/rapier`, etc.).
- The justfile needs no changes (no new build/run/test commands).
- Working directly on `develop` (small self-contained task, per project preference). Commit after every task.
- Pre-existing dirty files from other work are in the tree — `git add` only the files each task names, never `git add -A`.

---

### Task 1: Pure geometry helpers

**Files:**
- Create: `crates/client/src/renderer/grid_debug.rs`
- Modify: `crates/client/src/renderer/mod.rs` (module declaration only)

**Interfaces:**
- Consumes: `game::RegionCoords` (`world_offset() -> [f32; 3]`), `game::REGION_SIZE` (`1024.0_f32`), both glob re-exported at `game` crate root.
- Produces (used by Task 3):
  - `pub fn chunk_cell_of(pos: Vec3) -> IVec3`
  - `pub fn chunk_cube_lines(cell: IVec3) -> Vec<(Vec3, Vec3)>`
  - `pub fn region_wall_lines(coords: RegionCoords, camera_y: f32) -> Vec<(Vec3, Vec3)>`
  - `pub const` tunables: `CHUNK_GRID_RADIUS_XZ: i32`, `CHUNK_GRID_RADIUS_Y: i32`, `REGION_WALL_HALF_HEIGHT: f32`, `REGION_WALL_INTERVAL: f32`, colors `CHUNK_GRID_COLOR`/`CHUNK_CURRENT_COLOR`/`REGION_NEAR_COLOR`/`REGION_FAR_COLOR: Srgba`.

- [ ] **Step 1: Declare the module and write the failing tests**

In `crates/client/src/renderer/mod.rs`, after the line `pub mod convert;`, add:

```rust
pub(crate) mod grid_debug;
```

Create `crates/client/src/renderer/grid_debug.rs` with the imports, constants, `todo!()` stubs for the three helpers, and the test module — the stubs let everything compile so the tests fail at runtime rather than at build time:

```rust
//! Chunk/region debug wireframe view (F5): draws the chunk grid around the
//! camera and region boundary walls as gizmo lines. Client-only debug aid —
//! see docs/superpowers/specs/2026-07-10-chunk-region-debug-wireframe-design.md.
use bevy::color::palettes::css::{AQUA, PURPLE, RED, YELLOW};
use bevy::prelude::*;
use game::{RegionCoords, REGION_SIZE};

/// Chunk edge length in world units (32 voxels at 1 unit each). Matches the
/// `meshing::queue::ChunkGrid` keying (world position / 32).
pub const CHUNK_SIZE: f32 = 32.0;

// Tunables (no UI; edit and rebuild).
/// Chunk cells drawn each side of the camera, horizontally.
pub const CHUNK_GRID_RADIUS_XZ: i32 = 2;
/// Chunk cells drawn above/below the camera.
pub const CHUNK_GRID_RADIUS_Y: i32 = 2;
/// Region walls span camera_y ± this.
pub const REGION_WALL_HALF_HEIGHT: f32 = 64.0;
/// Spacing of vertical lines along region boundary edges.
pub const REGION_WALL_INTERVAL: f32 = 32.0;
/// Chunk cube edges.
pub const CHUNK_GRID_COLOR: Srgba = YELLOW;
/// The chunk cell containing the camera.
pub const CHUNK_CURRENT_COLOR: Srgba = RED;
/// Walls of simulated (near/subscribed) regions.
pub const REGION_NEAR_COLOR: Srgba = AQUA;
/// Walls of far/LOD (read-only) regions.
pub const REGION_FAR_COLOR: Srgba = PURPLE;

/// Integer chunk-grid cell containing a world-space point. Floor division,
/// not truncation: -0.1 is cell -1, matching `RegionCoords::from_world` and
/// the `ChunkGrid` keying.
pub fn chunk_cell_of(pos: Vec3) -> IVec3 {
    todo!()
}

/// The 12 edges of the 32-unit cube for one chunk-grid cell, world space.
pub fn chunk_cube_lines(cell: IVec3) -> Vec<(Vec3, Vec3)> {
    todo!()
}

/// Boundary "wall band" lines for one region, centered on the camera's
/// height: per edge, a vertical line every `REGION_WALL_INTERVAL` plus
/// horizontal rails at the band's bottom, middle (camera height), and top.
pub fn region_wall_lines(coords: RegionCoords, camera_y: f32) -> Vec<(Vec3, Vec3)> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_cell_uses_floor_division() {
        assert_eq!(chunk_cell_of(Vec3::new(0.5, 0.5, 0.5)), IVec3::new(0, 0, 0));
        assert_eq!(chunk_cell_of(Vec3::new(63.9, 32.0, 95.9)), IVec3::new(1, 1, 2));
        // The classic bug: truncation would give 0 / 0 / -2 here.
        assert_eq!(chunk_cell_of(Vec3::new(-0.1, -32.0, -64.1)), IVec3::new(-1, -1, -3));
    }

    #[test]
    fn chunk_cube_has_12_edges_spanning_cell() {
        let lines = chunk_cube_lines(IVec3::new(1, -1, 2));
        assert_eq!(lines.len(), 12);
        for (a, b) in &lines {
            for p in [a, b] {
                assert!((32.0..=64.0).contains(&p.x), "x out of cell: {p:?}");
                assert!((-32.0..=0.0).contains(&p.y), "y out of cell: {p:?}");
                assert!((64.0..=96.0).contains(&p.z), "z out of cell: {p:?}");
            }
        }
        // Every edge of a cube has length CHUNK_SIZE; no zero-length lines.
        let total: f32 = lines.iter().map(|(a, b)| a.distance(*b)).sum();
        assert_eq!(total, 12.0 * CHUNK_SIZE);
    }

    #[test]
    fn region_walls_count_and_extents() {
        let lines = region_wall_lines(RegionCoords::new(0, 0), 100.0);
        // 4 edges × (33 verticals + 3 rails). Adjust if the interval consts change.
        assert_eq!(lines.len(), 4 * 36);
        for (a, b) in &lines {
            for p in [a, b] {
                assert!((0.0..=REGION_SIZE).contains(&p.x), "x outside region: {p:?}");
                assert!((0.0..=REGION_SIZE).contains(&p.z), "z outside region: {p:?}");
                assert!(
                    (100.0 - REGION_WALL_HALF_HEIGHT..=100.0 + REGION_WALL_HALF_HEIGHT)
                        .contains(&p.y),
                    "y outside band: {p:?}"
                );
            }
        }
        // Rails must include one at exactly camera height on each edge.
        let at_cam = lines.iter().filter(|(a, b)| a.y == 100.0 && b.y == 100.0).count();
        assert_eq!(at_cam, 4);
    }

    #[test]
    fn region_walls_honor_negative_world_offset() {
        // Region (-1, -2) spans x ∈ [-1024, 0], z ∈ [-2048, -1024].
        let lines = region_wall_lines(RegionCoords::new(-1, -2), 0.0);
        for (a, b) in &lines {
            for p in [a, b] {
                assert!((-REGION_SIZE..=0.0).contains(&p.x), "x outside region: {p:?}");
                assert!(
                    (-2.0 * REGION_SIZE..=-REGION_SIZE).contains(&p.z),
                    "z outside region: {p:?}"
                );
            }
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p client grid_debug`
Expected: compiles, then all 4 tests FAIL with `not yet implemented` panics from the `todo!()` stubs.

- [ ] **Step 3: Implement the three helpers**

Replace the three `todo!()` bodies in `crates/client/src/renderer/grid_debug.rs`:

```rust
pub fn chunk_cell_of(pos: Vec3) -> IVec3 {
    IVec3::new(
        (pos.x / CHUNK_SIZE).floor() as i32,
        (pos.y / CHUNK_SIZE).floor() as i32,
        (pos.z / CHUNK_SIZE).floor() as i32,
    )
}
```

```rust
pub fn chunk_cube_lines(cell: IVec3) -> Vec<(Vec3, Vec3)> {
    let min = cell.as_vec3() * CHUNK_SIZE;
    let max = min + Vec3::splat(CHUNK_SIZE);
    let corners = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(max.x, max.y, max.z),
        Vec3::new(min.x, max.y, max.z),
    ];
    const EDGES: [(usize, usize); 12] = [
        (0, 1), (1, 2), (2, 3), (3, 0), // bottom face
        (4, 5), (5, 6), (6, 7), (7, 4), // top face
        (0, 4), (1, 5), (2, 6), (3, 7), // verticals
    ];
    EDGES.iter().map(|&(a, b)| (corners[a], corners[b])).collect()
}
```

```rust
pub fn region_wall_lines(coords: RegionCoords, camera_y: f32) -> Vec<(Vec3, Vec3)> {
    let off = coords.world_offset();
    let (x0, z0) = (off[0], off[2]);
    let (y0, y1) = (camera_y - REGION_WALL_HALF_HEIGHT, camera_y + REGION_WALL_HALF_HEIGHT);
    // The four boundary edges as (start corner, direction along the edge).
    let edges = [
        (Vec3::new(x0, 0.0, z0), Vec3::X),                       // z = z0 edge
        (Vec3::new(x0, 0.0, z0 + REGION_SIZE), Vec3::X),          // z = z1 edge
        (Vec3::new(x0, 0.0, z0), Vec3::Z),                       // x = x0 edge
        (Vec3::new(x0 + REGION_SIZE, 0.0, z0), Vec3::Z),          // x = x1 edge
    ];
    let steps = (REGION_SIZE / REGION_WALL_INTERVAL) as i32; // 32 at defaults
    let mut out = Vec::with_capacity(edges.len() * (steps as usize + 1 + 3));
    for (start, dir) in edges {
        for i in 0..=steps {
            let p = start + dir * (i as f32 * REGION_WALL_INTERVAL);
            out.push((Vec3::new(p.x, y0, p.z), Vec3::new(p.x, y1, p.z)));
        }
        let end = start + dir * REGION_SIZE;
        for y in [y0, camera_y, y1] {
            out.push((Vec3::new(start.x, y, start.z), Vec3::new(end.x, y, end.z)));
        }
    }
    out
}
```

Note: region corners get a vertical line from two edges (duplicate line drawn twice). Accepted per spec — do not "fix" this by skipping corners; the count assertion (36/edge) encodes it.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p client grid_debug`
Expected: `test result: ok. 4 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/grid_debug.rs crates/client/src/renderer/mod.rs
git commit -m "feat(client): grid-debug geometry helpers for chunk/region wireframes"
```

---

### Task 2: GridDebug resource + F5 toggle system

**Files:**
- Modify: `crates/client/src/renderer/grid_debug.rs`

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces (used by Task 3):
  - `#[derive(Resource, Default)] pub struct GridDebug { pub enabled: bool }`
  - `pub fn toggle_grid_debug(keys: Res<ButtonInput<KeyCode>>, state: ResMut<GridDebug>)`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `grid_debug.rs`:

```rust
    #[test]
    fn f5_toggles_grid_debug() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<GridDebug>()
            .add_systems(Update, toggle_grid_debug);
        app.update();
        assert!(!app.world().resource::<GridDebug>().enabled, "must default off");

        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::F5);
        app.update();
        assert!(app.world().resource::<GridDebug>().enabled);

        // reset_all() drops both the pressed set and the just_pressed edge so
        // the second press registers as a fresh edge — clear() alone keeps the
        // key "held" and swallows the press (same caveat as the hud.rs test).
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().reset_all();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::F5);
        app.update();
        assert!(!app.world().resource::<GridDebug>().enabled);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p client grid_debug`
Expected: compile FAILURE — `GridDebug` and `toggle_grid_debug` not found. (A compile-error failing state is fine here; the test names the exact API.)

- [ ] **Step 3: Implement resource and toggle system**

Add to `grid_debug.rs` (above the helper functions):

```rust
/// Whether the chunk/region wireframe view is on. Client-only debug state,
/// defaults off; F5 flips it. Independent of the F3 debug overlay.
#[derive(Resource, Default)]
pub struct GridDebug {
    pub enabled: bool,
}

/// F5 toggles the wireframe view. Reads Bevy input directly (client-only
/// concern — F5 is never forwarded to the sim input path). F3 = HUD/collider
/// debug, F4 = cave culling; F5 is the next free debug key.
pub fn toggle_grid_debug(keys: Res<ButtonInput<KeyCode>>, mut state: ResMut<GridDebug>) {
    if keys.just_pressed(KeyCode::F5) {
        state.enabled = !state.enabled;
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p client grid_debug`
Expected: `test result: ok. 5 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/grid_debug.rs
git commit -m "feat(client): F5 toggle resource + system for grid debug view"
```

---

### Task 3: Draw system + plugin registration

**Files:**
- Modify: `crates/client/src/renderer/grid_debug.rs`
- Modify: `crates/client/src/renderer/mod.rs`

**Interfaces:**
- Consumes: Task 1's helpers and color consts; Task 2's `GridDebug`/`toggle_grid_debug`; `bridge::RegionRoots(pub BTreeMap<RegionId, Entity>)` and `bridge::FarRegionRoots(pub BTreeMap<RegionId, Entity>)` (`bridge.rs:18-26`; `RegionId = RegionCoords`); the camera marked `bevy::ui::IsDefaultUiCamera` (same query the HUD uses, `hud.rs:154`).
- Produces: `pub fn draw_grid_debug(...)` registered in `SimBridgePlugin`. Nothing downstream consumes it.

- [ ] **Step 1: Implement the draw system**

Add to `grid_debug.rs`. There is no headless test for this system (`Gizmos` needs render plumbing that `MinimalPlugins` lacks; it is a thin shell over the tested helpers — per spec). Note the extra import.

```rust
use bevy::ui::IsDefaultUiCamera;

use super::bridge::{FarRegionRoots, RegionRoots};
```

```rust
/// Draw the wireframe view while enabled: chunk cubes in a radius around the
/// camera (current cell highlighted), and boundary walls for every loaded
/// region — cyan for simulated (near), purple for far/LOD. All lines are
/// re-derived per frame; the camera's `GlobalTransform` already folds in the
/// region world offset, so everything here is true world space.
pub fn draw_grid_debug(
    state: Res<GridDebug>,
    camera: Query<&GlobalTransform, With<IsDefaultUiCamera>>,
    near: Res<RegionRoots>,
    far: Res<FarRegionRoots>,
    mut gizmos: Gizmos,
) {
    if !state.enabled {
        return;
    }
    let Some(cam) = camera.iter().next().map(|t| t.translation()) else {
        return;
    };

    let center = chunk_cell_of(cam);
    for dx in -CHUNK_GRID_RADIUS_XZ..=CHUNK_GRID_RADIUS_XZ {
        for dy in -CHUNK_GRID_RADIUS_Y..=CHUNK_GRID_RADIUS_Y {
            for dz in -CHUNK_GRID_RADIUS_XZ..=CHUNK_GRID_RADIUS_XZ {
                let cell = center + IVec3::new(dx, dy, dz);
                let color = if cell == center { CHUNK_CURRENT_COLOR } else { CHUNK_GRID_COLOR };
                for (a, b) in chunk_cube_lines(cell) {
                    gizmos.line(a, b, color);
                }
            }
        }
    }

    let regions = near
        .0
        .keys()
        .map(|c| (*c, REGION_NEAR_COLOR))
        .chain(far.0.keys().map(|c| (*c, REGION_FAR_COLOR)));
    for (coords, color) in regions {
        for (a, b) in region_wall_lines(coords, cam.y) {
            gizmos.line(a, b, color);
        }
    }
}
```

- [ ] **Step 2: Register resource and systems in the plugin**

In `crates/client/src/renderer/mod.rs`, inside `SimBridgePlugin::build`:

After the line `.init_resource::<collider_debug::DebugColliderLines>()`, add:

```rust
            .init_resource::<grid_debug::GridDebug>()
```

In the `Update` systems tuple, after the line `collider_debug::draw_collider_wireframes.after(hud::toggle_debug),`, add:

```rust
                    grid_debug::toggle_grid_debug,
                    grid_debug::draw_grid_debug.after(grid_debug::toggle_grid_debug),
```

(The `.after` gives same-frame toggle response, matching how `draw_collider_wireframes` orders after `hud::toggle_debug`.)

- [ ] **Step 3: Build and run the fast test suite**

Run: `cargo build -p client && just test`
Expected: build succeeds; all tests pass (including the 5 `grid_debug` tests).

- [ ] **Step 4: Run clippy**

Run: `just clippy`
Expected: no new warnings in `crates/client` (pre-existing warnings in other crates/vendored forks are not yours to fix).

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/grid_debug.rs crates/client/src/renderer/mod.rs
git commit -m "feat(client): draw chunk/region debug wireframes on F5"
```

---

### Task 4: Final verification (full suite + live smoke)

**Files:** none (verification only).

**Interfaces:** none.

- [ ] **Step 1: Run the complete test suite**

Run: `just test-full`
Expected: all tests pass. (Project rule: `just test-full` is the final check before declaring work done.)

- [ ] **Step 2: Live smoke test**

Run server + client (`just run`, or `just adapter=6700 run` on the eGPU machine). In game:
1. Press **F5** → yellow chunk grid appears around you with the cell you're in red; cyan walls at region boundaries (walk toward a region edge — regions are 1024 units, so the nearest wall may be a few hundred units away); purple walls further out at far-ring regions.
2. Walk across a chunk border → the red cell follows you.
3. Fly/jump → the region wall band follows your height.
4. Press **F5** again → everything disappears. Press **F3** and **F4** → HUD/collider debug and cave culling still work, unaffected.

Expected: no visible frame-rate drop with the view on.

- [ ] **Step 3: Report results**

Report test + smoke outcomes honestly (including FPS impression) before calling the feature done.
