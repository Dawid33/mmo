//! Terrain boundary overlay: a thin warm line along the perimeter of the
//! player's home region, just outside it (a quarter block wide, never under
//! ~1.5 px; nothing is drawn inside the play area); the world outside it keeps its colours. Painted by
//! `voxel_pull.wgsl` from one small uniform (group(1) binding 5), so it hugs
//! every slope and reaches the LOD rings with no geometry of its own. (Only
//! the home region is simulated at the play region size, so it also marks the
//! simulated / unsimulated edge.) F8 (or the F3 panel) toggles it. Not drawn by
//! the per-chunk Mesh fallback (WebGL2) or the brick far renderer.

use bevy::prelude::*;
use bevy::render::{extract_resource::ExtractResource, render_resource::ShaderType};
use game::{RegionCoords, REGION_SIZE};

use super::hud::HudStatus;

/// World units per metre (`game::UNITS_PER_METER`), for the tunables below.
const M: f32 = 16.0;

/// Perimeter line: linear RGB, opacity, width in world units (a quarter
/// block) and the minimum on-screen width it keeps from far away.
pub const HOME_LINE_COLOR: Vec3 = Vec3::new(1.0, 0.78, 0.42);
pub const HOME_LINE_OPACITY: f32 = 0.6;
pub const HOME_LINE_WIDTH: f32 = 0.25 * M;
pub const HOME_LINE_MIN_PX: f32 = 1.5;

/// Whether the overlay is drawn. Client-only, on by default; F8 flips it.
#[derive(Resource)]
pub struct BoundaryOverlay {
    pub enabled: bool,
}

impl Default for BoundaryOverlay {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// The overlay uniform, layout mirrored by `TerrainBoundaries` in
/// `voxel_pull.wgsl`. An opacity of 0 turns it off. Extracted into the render
/// world, where `prepare_chunk_quads` uploads it.
#[derive(Resource, Clone, Copy, Default, PartialEq, ShaderType, ExtractResource)]
pub struct BoundaryUniform {
    /// Home region rect on the ground: min x, min z, max x, max z.
    pub home_rect: Vec4,
    /// rgb + opacity.
    pub home_color: Vec4,
    /// line width (world units), min line width (px), unused, unused.
    pub shape: Vec4,
    /// Unused (was the outside darken / desaturate; kept for the shader's layout).
    pub dim: Vec4,
}

/// F8 toggles the overlay (client-only key, never sent to the sim).
pub fn toggle_boundary_overlay(keys: Res<ButtonInput<KeyCode>>, mut state: ResMut<BoundaryOverlay>) {
    if keys.just_pressed(KeyCode::F8) {
        state.enabled = !state.enabled;
        info!("region boundaries: {}", if state.enabled { "on" } else { "off" });
    }
}

/// Rebuild the uniform from the home region. Only writes on a change, so
/// extraction re-sends it only then.
pub fn update_boundary_uniform(
    state: Res<BoundaryOverlay>,
    status: Res<HudStatus>,
    frame: Option<Res<crate::wrap_frame::WrapFrame>>,
    mut uniform: ResMut<BoundaryUniform>,
) {
    let frame = frame.as_deref().copied().unwrap_or_default();
    let next = build_uniform(state.enabled, status.home_region.map(|home| frame.alias(home)));
    if uniform.as_ref() != &next {
        *uniform = next;
    }
}

/// Pure uniform builder; `home` is the home region's copy in the `WrapFrame`.
pub fn build_uniform(enabled: bool, home: Option<RegionCoords>) -> BoundaryUniform {
    let mut u = BoundaryUniform {
        shape: Vec4::new(HOME_LINE_WIDTH, HOME_LINE_MIN_PX, 0.0, 0.0),
        dim: Vec4::ZERO,
        ..default()
    };
    let Some(home) = home.filter(|_| enabled) else {
        return u;
    };
    let lo = home.world_offset();
    u.home_rect = Vec4::new(lo[0], lo[2], lo[0] + REGION_SIZE, lo[2] + REGION_SIZE);
    u.home_color = HOME_LINE_COLOR.extend(HOME_LINE_OPACITY);
    u
}

