//! Outdoor lighting and aerial perspective: the sun, the sky's ambient fill,
//! and distance fog. One place for the tunables, because they only read as
//! natural in proportion to each other: shadowed faces are lit by ambient
//! alone, so the sun-to-ambient ratio IS the shadow contrast, and the fog
//! colour has to match the skybox horizon or the terrain's far edge shows.
//!
//! The terrain shader (`voxel_pull.wgsl`) does its own
//! lighting, so it reads these through Bevy's view uniforms (the directional
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

/// `VOXEL_SHADOW_CASCADES=N` / `VOXEL_SHADOW_DISTANCE_M=M` override
/// [`SHADOW_CASCADES`] / [`SHADOW_MAX_DISTANCE_M`] for A/B perf runs. Native only.
fn shadow_override<T: std::str::FromStr>(name: &str) -> Option<T> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var(name).ok()?.parse().ok()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = name;
        None
    }
}

fn shadow_cascades() -> usize {
    shadow_override("VOXEL_SHADOW_CASCADES").unwrap_or(SHADOW_CASCADES).max(1)
}

fn shadow_max_distance_m() -> f32 {
    shadow_override("VOXEL_SHADOW_DISTANCE_M").unwrap_or(SHADOW_MAX_DISTANCE_M)
}

/// The sun's cascade layout, in world units.
pub fn sun_cascades() -> CascadeShadowConfig {
    let m = game::movement::UNITS_PER_METER;
    let defaults = CascadeShadowConfigBuilder::default();
    CascadeShadowConfigBuilder {
        num_cascades: shadow_cascades().min(defaults.num_cascades),
        minimum_distance: defaults.minimum_distance * m,
        maximum_distance: shadow_max_distance_m() * m,
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
        num_cascades: shadow_cascades().min(defaults.num_cascades),
        minimum_distance: (eye - TOP_DOWN_SHADOW_ABOVE_M) * m,
        maximum_distance: (eye + shadow_max_distance_m()) * m,
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

/// Contrast left at the visibility distance (Koschmieder's classic 2 %, the
/// meteorological definition; Bevy's `from_visibility` uses 5 %). The haze
/// is exponential-SQUARED: contrast `exp(-(d / visibility)² · ln(1/C))` —
/// the same 2 % at the visibility distance, but nearly clear close up (a
/// quarter of the way out keeps ~78 % of a surface's contrast, where plain
/// exponential haze kept ~38 % and read as a strong mist over the near
/// terrain).
pub const VISIBILITY_CONTRAST: f32 = 0.02;

/// Default visibility as a share of the LOD rings' reach. At 0.8 the ring
/// edge (1.25 × visibility) keeps ~0.75 % contrast — invisible, where a
/// higher share starts to show the cut. With the play-size 15 km rings that
/// is ~12 km: an overcast or hazy day.
pub const VISIBILITY_SHARE_OF_REACH: f32 = 0.8;

/// The reach the default haze is sized for never drops below this, in
/// metres, however short the ring table (`small`, `tiny`, a short
/// `VOXEL_LOD_RINGS`): those show the edge of what is loaded, as they always
/// have, rather than the haze closing in on the near window.
pub const MIN_DEFAULT_REACH_M: f32 = 1024.0;

/// How far the outdoor view reaches: the exponential haze's visibility and
/// the perspective cameras' far plane. Defaults follow the LOD rings
/// ([`WorldScale`]'s table reach, at least [`MIN_DEFAULT_REACH_M`]):
/// visibility [`VISIBILITY_SHARE_OF_REACH`] of it, far plane at it;
/// `VOXEL_VIEW_DISTANCE_M` lengthens the rings and with them the haze.
/// `VOXEL_FOG_VISIBILITY_M=M` sets the haze alone (native): shorter = a
/// mistier day over the same rings; past 0.8 × the reach shows their edge.
/// Fog is measured from the camera; at these distances the commander eye's
/// 45–150 m offset from what it looks at changes the haze by ~1 %.
///
/// [`WorldScale`]: crate::world_scale::WorldScale
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct ViewDistance {
    /// Distance at which terrain keeps [`VISIBILITY_CONTRAST`], world units.
    pub visibility: f32,
    /// The perspective cameras' far plane, world units.
    pub far: f32,
}

impl ViewDistance {
    /// The defaults for a world scale.
    pub fn for_scale(scale: &crate::world_scale::WorldScale) -> Self {
        let reach = scale.lod_reach_units().max(MIN_DEFAULT_REACH_M * game::movement::UNITS_PER_METER);
        Self { visibility: VISIBILITY_SHARE_OF_REACH * reach, far: reach }
    }

    /// [`Self::for_scale`] with `VOXEL_FOG_VISIBILITY_M` (unset or empty =
    /// default); the browser has no environment.
    pub fn from_env(scale: &crate::world_scale::WorldScale) -> Result<Self, String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let v = std::env::var("VOXEL_FOG_VISIBILITY_M").ok().filter(|v| !v.is_empty());
            Self::resolve(scale, v.as_deref())
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(Self::for_scale(scale))
        }
    }

    /// An explicit visibility keeps the far plane at least where the haze
    /// would have hidden the ring edge, so a clearer day is never clipped.
    pub fn resolve(scale: &crate::world_scale::WorldScale, visibility_m: Option<&str>) -> Result<Self, String> {
        let defaults = Self::for_scale(scale);
        let Some(v) = visibility_m else { return Ok(defaults) };
        match v.trim().parse::<f32>() {
            Ok(m) if m > 0.0 && m.is_finite() => {
                let visibility = m * game::movement::UNITS_PER_METER;
                Ok(Self { visibility, far: defaults.far.max(visibility / VISIBILITY_SHARE_OF_REACH) })
            }
            _ => Err(format!("invalid VOXEL_FOG_VISIBILITY_M value `{v}` (expected metres > 0)")),
        }
    }
}

impl Default for ViewDistance {
    fn default() -> Self {
        Self::for_scale(&crate::world_scale::WorldScale::default())
    }
}

/// The skybox horizon colour in the post-exposure units the skybox draws in
/// — Bevy's fog colour is not exposure-scaled — so fogged terrain meets the
/// sky without a seam.
fn horizon_fog_color() -> Color {
    let horizon = LinearRgba::from(Color::srgb_u8(
        skybox::HORIZON[0],
        skybox::HORIZON[1],
        skybox::HORIZON[2],
    ));
    let scale = skybox::BRIGHTNESS * Exposure::default().exposure();
    Color::LinearRgba(LinearRgba::rgb(horizon.red * scale, horizon.green * scale, horizon.blue * scale))
}

/// The fog every terrain path renders with outdoors: exponential-squared
/// haze in the skybox horizon colour at the [`ViewDistance`] visibility.
pub fn distance_fog(view: &ViewDistance) -> DistanceFog {
    DistanceFog {
        color: horizon_fog_color(),
        falloff: FogFalloff::ExponentialSquared { density: squared_haze_density(view.visibility) },
        ..Default::default()
    }
}

/// `ExponentialSquared` density leaving [`VISIBILITY_CONTRAST`] at
/// `visibility` (world units): Bevy fogs by `1 - exp(-(d·density)²)`, so
/// `density = sqrt(ln(1/C)) / visibility`. (Bevy 0.19's
/// `from_visibility_contrast_squared` returns `sqrt(ln(1/C) / visibility)`,
/// which is not a distance scale — opaque within a few units here.)
pub fn squared_haze_density(visibility: f32) -> f32 {
    (1.0 / VISIBILITY_CONTRAST).ln().sqrt() / visibility
}

/// Keep every perspective 3D camera's far plane at [`ViewDistance::far`]:
/// cameras are created with the default horizon (`commander_perspective`,
/// the first person body camera, the top-down view switching back), and
/// nothing is drawn past the rings anyway. Runs before Bevy derives the frusta.
pub fn sync_far_plane(view: Option<Res<ViewDistance>>, mut cameras: Query<&mut Projection, With<Camera3d>>) {
    let far = view.as_deref().copied().unwrap_or_default().far;
    for mut projection in &mut cameras {
        if let Projection::Perspective(p) = &*projection {
            if p.far != far {
                if let Projection::Perspective(p) = projection.as_mut() {
                    p.far = far;
                }
            }
        }
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

/// With the eye in water the fog closes in short and blue: clear to START
/// metres, gone by END. The sky and the sun's shadows stay; only the fog swaps.
pub const UNDERWATER_FOG_START_M: f32 = 2.0;
pub const UNDERWATER_FOG_END_M: f32 = 24.0;
/// The underwater fog colour, sRGB (scaled to the skybox's exposure units like the other fogs).
pub const UNDERWATER_FOG_COLOR: [f32; 3] = [0.06, 0.22, 0.30];

/// The fog while the eye is in water ([`UNDERWATER_FOG_COLOR`], linear over `START..END`).
pub fn underwater_fog() -> DistanceFog {
    let c = LinearRgba::from(Color::srgb(UNDERWATER_FOG_COLOR[0], UNDERWATER_FOG_COLOR[1], UNDERWATER_FOG_COLOR[2]));
    let scale = skybox::BRIGHTNESS * Exposure::default().exposure();
    let unit = game::movement::UNITS_PER_METER;
    DistanceFog {
        color: Color::LinearRgba(LinearRgba::rgb(c.red * scale, c.green * scale, c.blue * scale)),
        falloff: FogFalloff::Linear { start: UNDERWATER_FOG_START_M * unit, end: UNDERWATER_FOG_END_M * unit },
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
    DistanceFog {
        color: horizon_fog_color(),
        falloff: FogFalloff::Linear { start: far + 1.0, end: far + 2.0 },
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
    view: Option<Res<ViewDistance>>,
    cameras: Query<Entity, (With<Camera3d>, Without<DistanceFog>)>,
) {
    let view = view.as_deref().copied().unwrap_or_default();
    for e in &cameras {
        commands.entity(e).insert(distance_fog(&view));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underwater_fog_is_short_and_blue() {
        let fog = underwater_fog();
        let FogFalloff::Linear { start, end } = fog.falloff else { panic!("linear") };
        let unit = game::movement::UNITS_PER_METER;
        assert!(start < end && end <= 24.0 * unit, "{start}..{end}");
        let c = LinearRgba::from(fog.color);
        assert!(c.blue > c.red, "{c:?}");
    }

    #[test]
    fn fog_colour_matches_the_rendered_skybox_horizon() {
        let fog = LinearRgba::from(distance_fog(&ViewDistance::default()).color);
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
        assert_eq!(top_down_fog().color, distance_fog(&ViewDistance::default()).color);
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
    fn haze_follows_the_ring_reach_and_hides_its_edge() {
        use crate::world_scale::WorldScale;
        let m = game::movement::UNITS_PER_METER;
        let full = ViewDistance::for_scale(&WorldScale::FULL);
        let reach = WorldScale::FULL.lod_reach_units().max(MIN_DEFAULT_REACH_M * m);
        assert_eq!(full.far, reach);
        assert_eq!(full.visibility, VISIBILITY_SHARE_OF_REACH * reach);
        // What is left of a surface's contrast at the ring edge.
        let FogFalloff::ExponentialSquared { density } = distance_fog(&full).falloff else {
            panic!("exponential-squared haze")
        };
        let contrast = |d: f32| (-(d * density).powi(2)).exp();
        assert!((contrast(full.visibility) - VISIBILITY_CONTRAST).abs() < 1e-4);
        assert!(contrast(reach) < 0.01, "ring edge keeps {} contrast", contrast(reach));
        assert!(contrast(full.visibility / 4.0) > 0.75, "near terrain stays clear");
        assert_eq!(ViewDistance::for_scale(&WorldScale::TINY).far, MIN_DEFAULT_REACH_M * m);
        let far = WorldScale::FULL.with_view_distance_m("4000").unwrap();
        assert!(ViewDistance::for_scale(&far).far >= 4000.0 * m);
    }

    #[test]
    fn visibility_knob_overrides_and_rejects_nonsense() {
        use crate::world_scale::WorldScale;
        let m = game::movement::UNITS_PER_METER;
        let defaults = ViewDistance::for_scale(&WorldScale::FULL);
        let misty = ViewDistance::resolve(&WorldScale::FULL, Some("300")).unwrap();
        assert_eq!(misty, ViewDistance { visibility: 300.0 * m, far: defaults.far });
        let clear = ViewDistance::resolve(&WorldScale::FULL, Some("100000")).unwrap();
        assert!(clear.far >= 100_000.0 * m, "a clear day is not clipped");
        assert!(ViewDistance::resolve(&WorldScale::FULL, Some("-1")).is_err());
        assert!(ViewDistance::resolve(&WorldScale::FULL, Some("far")).is_err());
        assert_eq!(ViewDistance::resolve(&WorldScale::FULL, None), Ok(ViewDistance::default()));
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
