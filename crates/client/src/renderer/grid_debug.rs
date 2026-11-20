//! Chunk/region debug wireframe view (F10): draws the chunk grid around the
//! camera and region boundary walls as gizmo lines. Client-only debug aid —
//! see docs/superpowers/specs/2026-07-10-chunk-region-debug-wireframe-design.md.
use super::UiCamera;
use bevy::color::palettes::css::{AQUA, PURPLE, RED, YELLOW};
use bevy::prelude::*;
use game::{REGION_SIZE, RegionCoords};

use super::bridge::RegionRoots;
use super::lod_rings::LodRings;

/// Whether the chunk/region wireframe view is on. Client-only debug state,
/// defaults off; [`GRID_DEBUG_KEY`] flips it. Also a checkbox in the F3 dev panel.
#[derive(Resource, Default)]
pub struct GridDebug {
    pub enabled: bool,
}

/// The wireframe view's key. F3 = dev panel, F4 = cave culling, F5 = first ↔
/// third person (`commander::first_person::BODY_VIEW_KEY`), F6 / F7 = terrain
/// layers, F8 = region boundaries; F10 was free.
pub const GRID_DEBUG_KEY: KeyCode = KeyCode::F10;

/// [`GRID_DEBUG_KEY`] toggles the wireframe view. Reads Bevy input directly
/// (client-only concern — it is never forwarded to the sim input path).
pub fn toggle_grid_debug(keys: Res<ButtonInput<KeyCode>>, mut state: ResMut<GridDebug>) {
    if keys.just_pressed(GRID_DEBUG_KEY) {
        state.enabled = !state.enabled;
        info!("grid debug: {}", if state.enabled { "on" } else { "off" });
    }
}

/// Debug view: hide the LOD or the near (full-detail) terrain wholesale, to
/// tell which layer a visual artifact belongs to. F6 / F7 or the F3 panel.
#[derive(Resource, Default, Debug)]
pub struct TerrainDebug {
    pub hide_lod: bool,
    pub hide_near: bool,
}

/// F6 toggles LOD, F7 near terrain (client-only keys, never sent to the sim).
pub fn toggle_terrain_debug(keys: Res<ButtonInput<KeyCode>>, mut state: ResMut<TerrainDebug>) {
    if keys.just_pressed(KeyCode::F6) {
        state.hide_lod = !state.hide_lod;
        info!(
            "terrain debug: LOD {}",
            if state.hide_lod { "hidden" } else { "shown" }
        );
    }
    if keys.just_pressed(KeyCode::F7) {
        state.hide_near = !state.hide_near;
        info!(
            "terrain debug: near {}",
            if state.hide_near { "hidden" } else { "shown" }
        );
    }
}

/// Apply `TerrainDebug` to the LOD roots (live and retiring) and the near
/// region roots. Every frame: roots come and go, and there are only a few.
pub fn apply_terrain_debug(
    state: Res<TerrainDebug>,
    rings: Res<super::lod_rings::LodRings>,
    near: Res<super::bridge::RegionRoots>,
    mut vis: Query<&mut Visibility>,
) {
    let want = |hide: bool| {
        if hide {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        }
    };
    let lod = rings
        .roots
        .values()
        .flat_map(|s| std::iter::once(s.root).chain(s.retiring))
        .chain(rings.retired.iter().map(|r| r.entity));
    for e in lod {
        if let Ok(mut v) = vis.get_mut(e) {
            v.set_if_neq(want(state.hide_lod));
        }
    }
    for (&region, &e) in &near.0 {
        if let Ok(mut v) = vis.get_mut(e) {
            // A tree the frame moved away from stays hidden (`RegionRoots::drawn`).
            v.set_if_neq(want(state.hide_near || !near.drawn(region)));
        }
    }
}

/// Render-brick edge in world units (`BRICK_VOXELS`, 1 unit per voxel). Matches
/// the `meshing::queue::ChunkGrid` keying (world position / `BRICK_VOXELS`).
pub const CHUNK_SIZE: f32 = game::BRICK_VOXELS as f32;

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
/// Walls of streamed-LOD (read-only) regions.
pub const REGION_LOD_COLOR: Srgba = PURPLE;

/// Integer chunk-grid cell containing a world-space point. Floor division,
/// not truncation: -0.1 is cell -1, matching `RegionCoords::from_world` and
/// the `ChunkGrid` keying.
pub fn chunk_cell_of(pos: Vec3) -> IVec3 {
    IVec3::new(
        (pos.x / CHUNK_SIZE).floor() as i32,
        (pos.y / CHUNK_SIZE).floor() as i32,
        (pos.z / CHUNK_SIZE).floor() as i32,
    )
}

/// The 12 edges of the 32-unit cube for one chunk-grid cell, world space.
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
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0), // bottom face
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4), // top face
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7), // verticals
    ];
    EDGES
        .iter()
        .map(|&(a, b)| (corners[a], corners[b]))
        .collect()
}

/// Boundary "wall band" lines for one region, centered on the camera's
/// height: per edge, a vertical line every `REGION_WALL_INTERVAL` plus
/// horizontal rails at the band's bottom, middle (camera height), and top.
/// `coords` is the region's copy in the `WrapFrame` (render space).
pub fn region_wall_lines(coords: RegionCoords, camera_y: f32) -> Vec<(Vec3, Vec3)> {
    let off = coords.world_offset();
    let (x0, z0) = (off[0], off[2]);
    let (y0, y1) = (
        camera_y - REGION_WALL_HALF_HEIGHT,
        camera_y + REGION_WALL_HALF_HEIGHT,
    );
    // The four boundary edges as (start corner, direction along the edge).
    let edges = [
        (Vec3::new(x0, 0.0, z0), Vec3::X),               // z = z0 edge
        (Vec3::new(x0, 0.0, z0 + REGION_SIZE), Vec3::X), // z = z1 edge
        (Vec3::new(x0, 0.0, z0), Vec3::Z),               // x = x0 edge
        (Vec3::new(x0 + REGION_SIZE, 0.0, z0), Vec3::Z), // x = x1 edge
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

/// Draw the wireframe view while enabled: chunk cubes in a radius around the
/// camera (current cell highlighted), and boundary walls for every loaded
/// region — cyan for simulated (near), purple for streamed LOD. Only
/// region-sized LOD keys (span 1, factors ≤ 16) draw walls: the 2×2/8×8
/// tile keys sit past any distance where per-region walls help. All lines
/// are re-derived per frame; the camera's `GlobalTransform` already folds in
/// the region world offset, so everything here is true world space.
pub fn draw_grid_debug(
    state: Res<GridDebug>,
    camera: Query<&GlobalTransform, With<UiCamera>>,
    near: Res<RegionRoots>,
    lod: Res<LodRings>,
    mut gizmos: Gizmos,
) {
    if !state.enabled {
        return;
    }
    let Some(cam) = camera.iter().next().map(|t| t.translation()) else {
        // One-shot diagnostic on the toggle frame: enabled but nothing to
        // anchor the grid to — the local camera hasn't spawned/been marked.
        if state.is_changed() {
            info!("grid debug: enabled but no UiCamera found");
        }
        return;
    };

    let center = chunk_cell_of(cam);
    if state.is_changed() {
        info!(
            "grid debug: drawing at cam={cam:?} cell={center:?} near_regions={} lod_keys={}",
            near.0.len(),
            lod.roots.len()
        );
    }
    for dx in -CHUNK_GRID_RADIUS_XZ..=CHUNK_GRID_RADIUS_XZ {
        for dy in -CHUNK_GRID_RADIUS_Y..=CHUNK_GRID_RADIUS_Y {
            for dz in -CHUNK_GRID_RADIUS_XZ..=CHUNK_GRID_RADIUS_XZ {
                let cell = center + IVec3::new(dx, dy, dz);
                let color = if cell == center {
                    CHUNK_CURRENT_COLOR
                } else {
                    CHUNK_GRID_COLOR
                };
                for (a, b) in chunk_cube_lines(cell) {
                    gizmos.line(a, b, color);
                }
            }
        }
    }

    // Near regions where their trees are drawn; LOD keys are already in the frame.
    let regions = near
        .0
        .keys()
        .filter(|c| near.drawn(**c))
        .filter_map(|c| Some((near.alias(*c)?, REGION_NEAR_COLOR)))
        .chain(
            lod.roots
                .keys()
                .filter(|k| k.region_span() == 1)
                .map(|k| (k.origin_region(), REGION_LOD_COLOR)),
        );
    for (coords, color) in regions {
        for (a, b) in region_wall_lines(coords, cam.y) {
            gizmos.line(a, b, color);
        }
    }
}
