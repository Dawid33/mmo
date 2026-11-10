//! Terrain boundary overlay: a thin warm line along the perimeter of the
//! player's home region, just outside it (a quarter block wide, never under
//! ~1.5 px; nothing is drawn inside the play area), with the world outside it slightly dimmed and desaturated. Painted by
//! the stud terrain material (`stud_terrain.wgsl`) from one small uniform, so it hugs
//! every slope and reaches the far tiles with no geometry of its own. (Only
//! the home region is simulated at the play region size, so it also marks the
//! simulated / unsimulated edge.) F8 (or the F3 panel) toggles it.

use bevy::prelude::*;
use bevy::render::render_resource::ShaderType;
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
/// Outside the home region: how much darker (0..1) and how desaturated (0..1)
/// the terrain gets, reached over `OUTSIDE_FADE` from the edge.
pub const OUTSIDE_DARKEN: f32 = 0.12;
pub const OUTSIDE_DESATURATE: f32 = 0.2;
pub const OUTSIDE_FADE: f32 = 6.0 * M;

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
/// `stud_terrain.wgsl`. An opacity of 0 turns it off; `stud::material::sync_stud_materials`
/// copies it into the stud materials.
#[derive(Resource, Clone, Copy, Default, PartialEq, ShaderType)]
pub struct BoundaryUniform {
    /// Home region rect on the ground: min x, min z, max x, max z.
    pub home_rect: Vec4,
    /// rgb + opacity.
    pub home_color: Vec4,
    /// line width (world units), min line width (px), outside fade distance, unused.
    pub shape: Vec4,
    /// outside darken, outside desaturate, unused, unused.
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
    mut uniform: ResMut<BoundaryUniform>,
) {
    let next = build_uniform(state.enabled, status.home_region);
    if uniform.as_ref() != &next {
        *uniform = next;
    }
}

/// Pure uniform builder.
pub fn build_uniform(enabled: bool, home: Option<RegionCoords>) -> BoundaryUniform {
    let mut u = BoundaryUniform {
        shape: Vec4::new(HOME_LINE_WIDTH, HOME_LINE_MIN_PX, OUTSIDE_FADE, 0.0),
        dim: Vec4::new(OUTSIDE_DARKEN, OUTSIDE_DESATURATE, 0.0, 0.0),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_sets_the_rect_and_turns_the_line_on() {
        let u = build_uniform(true, Some(RegionCoords { x: 2, z: -1 }));
        assert_eq!(u.home_rect, Vec4::new(2.0, -1.0, 3.0, 0.0) * REGION_SIZE);
        assert!(u.home_color.w > 0.0);
    }

    #[test]
    fn disabled_or_homeless_draws_nothing() {
        let home = Some(RegionCoords { x: 0, z: 0 });
        for u in [build_uniform(false, home), build_uniform(true, None)] {
            assert_eq!(u.home_color.w, 0.0);
        }
    }
}
