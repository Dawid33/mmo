//! Fortress view uniform: the z-level cutaway of the terrain materials
//! (`stud/material.rs` — `stud_terrain.wgsl` — and the water material,
//! `water.wgsl`), mirrored there as `FortressView`.
//!
//! While the cut is on ([`cutaway_active`]) fragments above the cut plane are
//! discarded inside the home region's xz rect (`origin.xz` .. `origin.xz +
//! cut.w`), and a back face that shows through is drawn as the cut's cap —
//! shaded where the eye ray crosses the plane, facing up, darkened by
//! [`CAP_DARKEN`]. Front faces below the cut darken per level down
//! ([`DEPTH_DARKEN_PER_LEVEL`], never below [`DEPTH_DARKEN_FLOOR`]). Outside
//! the home rect — the neighbouring terrain and the far tiles — the terrain
//! draws exactly as without the cut.
//!
//! Designations are drawn as boxes (`stud/overlay.rs`); the drag in progress
//! is [`DragPreview`].

use bevy::prelude::*;
use bevy::render::render_resource::ShaderType;
use game::{DesignationKind, Invalid, RegionId, TileBox, TilePos, REGION_SIZE};

use super::commander::level::{self, WorkingLevel};
use super::commander::tiles::TileMirror;

/// How much darker (0..1) the cap is than a lit top face.
pub const CAP_DARKEN: f32 = 0.25;
/// Brightness lost per level below the cut (front faces).
pub const DEPTH_DARKEN_PER_LEVEL: f32 = 0.12;
/// The darkest a level below the cut gets.
pub const DEPTH_DARKEN_FLOOR: f32 = 0.35;

/// The designation tool's drag in progress: the tiles it would cover and
/// whether the order is valid on each. Empty when no drag is on. Filled by
/// the designation tools (`commander::designate`).
#[derive(Resource, Default)]
pub struct DragPreview {
    pub area: Option<TileBox>,
    pub kind: Option<DesignationKind>,
    pub cells: Vec<(TilePos, Result<(), Invalid>)>,
}

/// The fortress view's per-session resources.
pub struct FortressViewPlugin;

impl Plugin for FortressViewPlugin {
    fn build(&self, app: &mut App) {
        use crate::session::SessionResetExt;
        app.add_session_reset::<DragPreview>();
    }
}

/// Layout mirrored by `FortressView` in `stud_terrain.wgsl` and `water.wgsl`.
#[derive(Resource, Clone, Copy, PartialEq, Debug, ShaderType)]
pub struct FortressViewUniform {
    /// x: cut plane world y (f32::MAX = off); y: cap darkening 0..1; z: unused;
    /// w: home region extent (world units) — the cut applies in [origin.xz, origin.xz + w)
    pub cut: Vec4,
    /// xyz: home region world offset; w: unused
    pub origin: Vec4,
}

impl Default for FortressViewUniform {
    fn default() -> Self {
        Self { cut: Vec4::new(f32::MAX, 0.0, 0.0, 0.0), origin: Vec4::ZERO }
    }
}

/// Whether the cut is on.
pub fn cutaway_active(u: &FortressViewUniform) -> bool {
    u.cut.x < f32::MAX
}

/// Pure uniform builder: the cut follows the working level in the home
/// region, off when the cut is toggled off, the level is unknown or there is
/// no home.
pub fn build_uniform(working: &WorkingLevel, home: Option<RegionId>) -> FortressViewUniform {
    let mut u = FortressViewUniform::default();
    let Some(region) = home else { return u };
    u.origin = Vec3::from_array(region.world_offset()).extend(0.0);
    u.cut.w = REGION_SIZE;
    if let Some(cut) = level::cut_y(working, home) {
        u.cut.x = cut;
        u.cut.y = CAP_DARKEN;
    }
    u
}

/// Rebuild the uniform from the working level. Only writes on a change, so
/// the materials are re-synced only then.
pub fn update_fortress_view(
    working: Res<WorkingLevel>,
    tiles: Res<TileMirror>,
    mut uniform: ResMut<FortressViewUniform>,
) {
    let next = build_uniform(&working, tiles.region());
    if uniform.as_ref() != &next {
        *uniform = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_is_off_by_default() {
        assert!(!cutaway_active(&Default::default()));
    }

    #[test]
    fn uniform_tracks_working_level() {
        let home = Some(RegionId { x: 0, z: 0 });
        let working = WorkingLevel { level: Some(10), cut: true, ..Default::default() };
        let u = build_uniform(&working, home);
        assert!(cutaway_active(&u));
        assert_eq!(u.cut.x, level::cut_plane_world_y(10, RegionId { x: 0, z: 0 }));
        assert_eq!(u.cut.y, CAP_DARKEN);

        let off = WorkingLevel { level: Some(10), cut: false, ..Default::default() };
        assert!(!cutaway_active(&build_uniform(&off, home)));
        assert!(!cutaway_active(&build_uniform(&working, None)));
        assert!(!cutaway_active(&build_uniform(&WorkingLevel { level: None, cut: true, ..Default::default() }, home)));
    }

    /// The cut is confined to the home region: its rect is origin.xz ..
    /// origin.xz + cut.w.
    #[test]
    fn uniform_carries_the_home_region_rect() {
        let working = WorkingLevel { level: Some(10), cut: true, ..Default::default() };
        let u = build_uniform(&working, Some(RegionId { x: 2, z: -1 }));
        assert_eq!(u.origin.truncate(), Vec3::new(2.0, 0.0, -1.0) * REGION_SIZE);
        assert_eq!(u.cut.w, REGION_SIZE);
    }
}
