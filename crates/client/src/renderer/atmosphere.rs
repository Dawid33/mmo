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

/// Spawn the sun and set the ambient fill. Called from `setup_scene`.
pub fn spawn_lights(commands: &mut Commands) {
    commands.spawn((
        DirectionalLight {
            color: SUN_COLOR,
            illuminance: SUN_ILLUMINANCE,
            shadow_maps_enabled: true,
            ..Default::default()
        },
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
    fn cameras_get_fog_once() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_systems(Update, attach_fog);
        let cam = app.world_mut().spawn(Camera3d::default()).id();
        app.update();
        assert!(app.world().entity(cam).contains::<DistanceFog>());
    }
}
