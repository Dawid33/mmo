//! Outdoor lighting and aerial perspective: the sun, the sky's ambient fill,
//! and distance fog. One place for the tunables, because they only read as
//! natural in proportion to each other: shadowed faces are lit by ambient
//! alone, so the sun-to-ambient ratio IS the shadow contrast, and the fog
//! colour has to match the skybox horizon or the terrain's far edge shows.
//!
//! The terrain shaders (`voxel_pull.wgsl`, `far_voxel.wgsl`) do their own
//! lighting, so they read these through Bevy's view uniforms (the directional
//! light, `GlobalAmbientLight`, and the camera's `DistanceFog`) rather than
//! duplicating the numbers; the per-chunk fallback gets them through PBR.

use bevy::camera::Exposure;
use bevy::light::{CascadeShadowConfig, CascadeShadowConfigBuilder};
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;

use super::skybox;

/// Direct sunlight, in lux. With [`AMBIENT_BRIGHTNESS`] this puts a lit
/// grass top near where the old 10 klx sun at 25° left it, so exposure and
/// tonemapping need no retune.
pub const SUN_ILLUMINANCE: f32 = 5_000.0;
/// Warm white sunlight.
pub const SUN_COLOR: Color = Color::srgb(0.98, 0.95, 0.82);
/// Direction the sunlight travels (toward the ground). ~40° above the
/// horizon: low enough to model the terrain, high enough that most slopes
/// facing away from the sun still see some of it.
pub const SUN_DIRECTION: Vec3 = Vec3::new(-0.15, -0.18, 0.15);

/// How far from the camera (view depth, metres) the sun casts shadows.
/// Bevy's default cascades (10 / 150) assume 1 unit = 1 m; at
/// `UNITS_PER_METER` = 16 they ended 9.4 m out, so the commander camera —
/// 45 m from its focus at the start zoom, up to `ZOOM_FAR_M` (80 m) — saw
/// shadows only when zoomed right in. This covers the whole view at full
/// zoom-out except the far ground of a shallow pitch.
pub const SHADOW_MAX_DISTANCE_M: f32 = 300.0;
/// Far bound of the first (sharpest) cascade, metres.
pub const SHADOW_FIRST_CASCADE_M: f32 = 20.0;
/// Number of shadow cascades (WebGL2 supports only one; Bevy clamps there).
pub const SHADOW_CASCADES: usize = 4;

/// The sun's cascade layout, in world units.
pub fn sun_cascades() -> CascadeShadowConfig {
    let m = game::movement::UNITS_PER_METER;
    let defaults = CascadeShadowConfigBuilder::default();
    CascadeShadowConfigBuilder {
        num_cascades: SHADOW_CASCADES.min(defaults.num_cascades),
        minimum_distance: defaults.minimum_distance * m,
        maximum_distance: SHADOW_MAX_DISTANCE_M * m,
        first_cascade_far_bound: SHADOW_FIRST_CASCADE_M * m,
        overlap_proportion: defaults.overlap_proportion,
    }
    .build()
}

/// Top-down view (`commander/top_down.rs`): how far above the working level
/// terrain still casts and receives sun shadows, metres. The eye sits
/// `TOP_DOWN_EYE_HEIGHT_M` above the cut, so the cascades start that much
/// less this far from it; higher neighbouring terrain goes unshadowed.
pub const TOP_DOWN_SHADOW_ABOVE_M: f32 = 64.0;
/// Top-down: far bound of the first (sharpest) cascade below the eye's
/// height above the cut, metres.
pub const TOP_DOWN_FIRST_CASCADE_M: f32 = 16.0;

/// The sun's cascades for the top-down view: measured from its eye, high
/// above the working level (`top_down::TOP_DOWN_EYE_HEIGHT_M`), so the same
/// depth of ground as in 3D is covered from the level down.
pub fn top_down_cascades() -> CascadeShadowConfig {
    let m = game::movement::UNITS_PER_METER;
    let eye = crate::renderer::commander::top_down::TOP_DOWN_EYE_HEIGHT_M;
    let defaults = CascadeShadowConfigBuilder::default();
    CascadeShadowConfigBuilder {
        num_cascades: SHADOW_CASCADES.min(defaults.num_cascades),
        minimum_distance: (eye - TOP_DOWN_SHADOW_ABOVE_M) * m,
        maximum_distance: (eye + SHADOW_MAX_DISTANCE_M) * m,
        first_cascade_far_bound: (eye + TOP_DOWN_FIRST_CASCADE_M) * m,
        overlap_proportion: defaults.overlap_proportion,
    }
    .build()
}

/// Sky fill light. Blue-tinted, as open shade under a clear sky is; bright
/// enough that a face in full shadow keeps ~30–40 % of a sunlit top instead
/// of going near-black (the old 100 against 10 klx was ~7 %).
pub const AMBIENT_COLOR: Color = Color::srgb(0.80, 0.88, 1.0);
/// Ambient brightness, in Bevy's ambient units (same scale as `lux / PI`
/// on the terrain shaders' `ambient + sun·ndotl/π` sum).
pub const AMBIENT_BRIGHTNESS: f32 = 900.0;

/// Where the distance fog begins, as a share of the LOD horizon
/// (`game::lod::LOD_HORIZON_UNITS`). Linear fog: everything nearer is
/// perfectly clear, then haze ramps to opaque exactly at the horizon's
/// nearest edge, so the end of the rings dissolves into the sky and never
/// shows as a cut. (The old exponential fog hazed from the camera outward —
/// half a surface's colour gone by 500 m.)
pub const FOG_START_FRACTION: f32 = 0.6;

/// The fog every terrain path renders with. The colour is the skybox
/// horizon in the same post-exposure units the skybox itself draws in —
/// Bevy's fog colour is not exposure-scaled — so fogged terrain meets the
/// sky without a seam.
pub fn distance_fog() -> DistanceFog {
    let horizon = LinearRgba::from(Color::srgb_u8(
        skybox::HORIZON[0],
        skybox::HORIZON[1],
        skybox::HORIZON[2],
    ));
    let scale = skybox::BRIGHTNESS * Exposure::default().exposure();
    DistanceFog {
        color: Color::LinearRgba(LinearRgba::rgb(
            horizon.red * scale,
            horizon.green * scale,
            horizon.blue * scale,
        )),
        falloff: FogFalloff::Linear {
            start: FOG_START_FRACTION * game::lod::LOD_HORIZON_UNITS,
            end: game::lod::LOD_HORIZON_UNITS,
        },
        ..Default::default()
    }
}

/// Underground (the eye inside a solid block) the fog closes in on the
/// rock backdrop (`skybox::UNDERGROUND_*`): dug spaces seen through the rock
/// stay clear up to START metres away and dissolve into the dark by END, so
/// the far hollow geometry never shows against nothing.
pub const UNDERGROUND_FOG_START_M: f32 = 24.0;
pub const UNDERGROUND_FOG_END_M: f32 = 120.0;

/// The fog while the eye is inside a block: the backdrop's mid colour, in
/// the skybox's post-exposure units like [`distance_fog`], over the
/// underground range.
pub fn underground_fog() -> DistanceFog {
    let mid = skybox::underground_color(0.0);
    let backdrop = LinearRgba::from(Color::srgb_u8(mid[0], mid[1], mid[2]));
    let scale = skybox::BRIGHTNESS * Exposure::default().exposure();
    let unit = game::movement::UNITS_PER_METER;
    DistanceFog {
        color: Color::LinearRgba(LinearRgba::rgb(backdrop.red * scale, backdrop.green * scale, backdrop.blue * scale)),
        falloff: FogFalloff::Linear { start: UNDERGROUND_FOG_START_M * unit, end: UNDERGROUND_FOG_END_M * unit },
        ..Default::default()
    }
}

/// The fog in the top-down view: none. Its eye is far above the ground
/// (`top_down::TOP_DOWN_EYE_HEIGHT_M`), so eye-distance fog would haze the
/// working level; the levels below darken by depth instead
/// (`fortress_view`). Same colour as [`distance_fog`], beyond the
/// projection's far plane.
pub fn top_down_fog() -> DistanceFog {
    use crate::renderer::commander::top_down::{TOP_DOWN_DEPTH_M, TOP_DOWN_EYE_HEIGHT_M};
    let far = (TOP_DOWN_EYE_HEIGHT_M + TOP_DOWN_DEPTH_M) * game::movement::UNITS_PER_METER;
    DistanceFog { falloff: FogFalloff::Linear { start: far + 1.0, end: far + 2.0 }, ..distance_fog() }
}

/// Spawn the sun and set the ambient fill. Called from `setup_scene`.
pub fn spawn_lights(commands: &mut Commands) {
    commands.spawn((
        DirectionalLight {
            color: SUN_COLOR,
            illuminance: SUN_ILLUMINANCE,
            shadow_maps_enabled: true,
            ..Default::default()
        },
        sun_cascades(),
        Transform::default().looking_at(SUN_DIRECTION, Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight {
        color: AMBIENT_COLOR,
        brightness: AMBIENT_BRIGHTNESS,
        ..Default::default()
    });
}

/// Give every 3D camera the distance fog. A catch-up system for the same
/// reason as `skybox::attach_skybox`: cameras spawn dynamically.
pub fn attach_fog(
    mut commands: Commands,
    cameras: Query<Entity, (With<Camera3d>, Without<DistanceFog>)>,
) {
    for e in &cameras {
        commands.entity(e).insert(distance_fog());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fog_colour_matches_the_rendered_skybox_horizon() {
        let fog = LinearRgba::from(distance_fog().color);
        let horizon = LinearRgba::from(Color::srgb_u8(165, 200, 235));
        let exposure = Exposure::default().exposure();
        // What the skybox pass writes for a horizon texel.
        let sky = horizon.blue * skybox::BRIGHTNESS * exposure;
        assert!((fog.blue - sky).abs() < 1e-4, "fog {fog:?} vs sky blue {sky}");
    }

    #[test]
    fn shadowed_ground_keeps_a_third_of_sunlit_ground() {
        let up = Vec3::Y;
        let to_sun = -SUN_DIRECTION.normalize();
        let ambient = LinearRgba::from(AMBIENT_COLOR).luminance() * AMBIENT_BRIGHTNESS;
        let lit = ambient + SUN_ILLUMINANCE * up.dot(to_sun).max(0.0) / std::f32::consts::PI;
        let ratio = ambient / lit;
        assert!((0.3..0.5).contains(&ratio), "shadow/lit ratio {ratio}");
    }

    #[test]
    fn shadows_reach_past_the_farthest_commander_zoom() {
        let m = game::movement::UNITS_PER_METER;
        let far = sun_cascades().bounds.last().copied().unwrap();
        assert!(far >= 2.0 * crate::renderer::commander::ZOOM_FAR_M * m, "shadow far bound {far}");
    }

    #[test]
    fn top_down_fog_never_reaches_the_ground() {
        use crate::renderer::commander::top_down::{top_down_projection, TOP_DOWN_ZOOM_START};
        let far = top_down_projection(TOP_DOWN_ZOOM_START, 1.0).far;
        let FogFalloff::Linear { start, end } = top_down_fog().falloff else { panic!("linear fog") };
        assert!(start > far && end > start, "fog {start}..{end} vs far {far}");
        assert_eq!(top_down_fog().color, distance_fog().color);
    }

    #[test]
    fn top_down_shadows_cover_the_working_level_and_what_is_under_it() {
        use crate::renderer::commander::top_down::TOP_DOWN_EYE_HEIGHT_M;
        let m = game::movement::UNITS_PER_METER;
        let c = top_down_cascades();
        assert!(c.minimum_distance <= (TOP_DOWN_EYE_HEIGHT_M - 1.0) * m, "starts above the level");
        let far = c.bounds.last().copied().unwrap();
        assert!(far >= (TOP_DOWN_EYE_HEIGHT_M + crate::renderer::commander::ZOOM_FAR_M) * m, "far bound {far}");
    }

    #[test]
    fn cameras_get_fog_once() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_systems(Update, attach_fog);
        let cam = app.world_mut().spawn(Camera3d::default()).id();
        app.update();
        assert!(app.world().entity(cam).contains::<DistanceFog>());
    }
}
