//! Fortress view uniform: the z-level cutaway (and, later, the designation
//! overlay) for the pulled-quad terrain path. One small uniform at group(1)
//! binding 6 of `voxel_pull.wgsl` / `voxel_pull_depth.wgsl`, mirrored there as
//! `FortressView`.
//!
//! While the cut is on (`cutaway_active`), the pull pipelines specialize with
//! the `CUTAWAY` shader def and no face culling: quads wholly above the cut
//! plane collapse in the vertex stage, fragments above it are discarded (in
//! the forward, depth-prepass and shadow passes alike), and a back face that
//! shows through is drawn as the cut's cap — shaded where the eye ray crosses
//! the plane, with its block's top texture, facing up, darkened by
//! [`CAP_DARKEN`]. Front faces below the cut darken per level down
//! ([`DEPTH_DARKEN_PER_LEVEL`], never below [`DEPTH_DARKEN_FLOOR`]). The GPU
//! face-direction cull keeps every face range meanwhile: the cap is made of
//! back faces. All of it applies inside the home region's xz rect only
//! (`origin.xz` .. `origin.xz + cut.w`, decided per quad): outside it — the
//! neighbouring terrain and the LOD rings — the terrain draws exactly as
//! without the cut, the shader dropping back faces itself there.
//!
//! Not supported by the per-chunk Mesh fallback (WebGL2) or the brick far
//! renderer: there the terrain draws uncut.

use bevy::prelude::*;
use bevy::render::{extract_resource::ExtractResource, render_resource::ShaderType};
use game::{RegionId, REGION_SIZE};

use super::commander::level::{self, WorkingLevel};
use super::commander::tiles::TileMirror;

/// How much darker (0..1) the cap is than a lit top face.
pub const CAP_DARKEN: f32 = 0.25;
/// Brightness lost per level below the cut (front faces). Read only by the
/// shader, as a WGSL `const` mirroring this one (test-pinned).
#[cfg_attr(not(test), allow(dead_code))]
pub const DEPTH_DARKEN_PER_LEVEL: f32 = 0.12;
/// The darkest a level below the cut gets. WGSL-mirrored like the above.
#[cfg_attr(not(test), allow(dead_code))]
pub const DEPTH_DARKEN_FLOOR: f32 = 0.35;

/// Layout mirrored by `FortressView` in `voxel_pull.wgsl` and
/// `voxel_pull_depth.wgsl`. Extracted into the render world, where
/// `prepare_chunk_quads` uploads it and the queue systems pick the `CUTAWAY`
/// pipeline variant from it.
#[derive(Resource, Clone, Copy, PartialEq, Debug, ExtractResource, ShaderType)]
pub struct FortressViewUniform {
    /// x: cut plane world y (f32::MAX = off); y: cap darkening 0..1; z: overlay on (0/1);
    /// w: home region extent (world units) — the cut applies in [origin.xz, origin.xz + w)
    pub cut: Vec4,
    /// xyz: home region world offset; w: overlay base level (block y of texture layer 0) as f32
    pub origin: Vec4,
    /// x: overlay width (blocks, region x), y: depth (region z), z: layers; w: unused
    pub overlay_dims: UVec4,
}

impl Default for FortressViewUniform {
    fn default() -> Self {
        Self {
            cut: Vec4::new(f32::MAX, 0.0, 0.0, 0.0),
            origin: Vec4::ZERO,
            overlay_dims: UVec4::ZERO,
        }
    }
}

/// Whether the cut is on (the pipelines then take the `CUTAWAY` variant).
pub fn cutaway_active(u: &FortressViewUniform) -> bool {
    u.cut.x < f32::MAX
}

/// Pure uniform builder: the cut follows the working level in the home
/// region, off when the cut is toggled off, the level is unknown or there is
/// no home. The overlay fields stay off.
pub fn build_uniform(working: &WorkingLevel, home: Option<RegionId>) -> FortressViewUniform {
    let mut u = FortressViewUniform::default();
    if let Some(home) = home {
        u.origin = Vec3::from_array(home.world_offset()).extend(0.0);
    }
    if let Some(cut) = level::cut_y(working, home) {
        u.cut = Vec4::new(cut, CAP_DARKEN, 0.0, REGION_SIZE);
    }
    u
}

/// Rebuild the uniform from the working level. Only writes on a change, so
/// extraction re-sends it only then.
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
        let working = WorkingLevel { level: Some(10), cut: true };
        let u = build_uniform(&working, home);
        assert!(cutaway_active(&u));
        assert_eq!(u.cut.x, 176.0);
        assert_eq!(u.cut.y, CAP_DARKEN);
        assert_eq!(u.cut.z, 0.0, "overlay stays off");

        let off = WorkingLevel { level: Some(10), cut: false };
        assert!(!cutaway_active(&build_uniform(&off, home)));
        assert!(!cutaway_active(&build_uniform(&working, None)));
    }

    #[test]
    fn shader_mirrors_the_depth_constants() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/shaders/voxel_pull.wgsl"
        ))
        .expect("voxel_pull.wgsl");
        for (name, value) in [
            ("DEPTH_DARKEN_PER_LEVEL", DEPTH_DARKEN_PER_LEVEL),
            ("DEPTH_DARKEN_FLOOR", DEPTH_DARKEN_FLOOR),
        ] {
            let line = format!("const {name}: f32 = {value};");
            assert!(source.contains(&line), "voxel_pull.wgsl must declare `{line}`");
        }
    }

    /// The cut is confined to the home region: its rect is origin.xz ..
    /// origin.xz + cut.w.
    #[test]
    fn uniform_carries_the_home_region_rect() {
        let working = WorkingLevel { level: Some(10), cut: true };
        let u = build_uniform(&working, Some(RegionId { x: 2, z: -1 }));
        assert_eq!(u.origin.truncate(), Vec3::new(2.0, 0.0, -1.0) * REGION_SIZE);
        assert_eq!(u.cut.w, REGION_SIZE);
    }
}
