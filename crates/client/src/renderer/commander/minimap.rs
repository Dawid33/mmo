//! The minimap: a square in the bottom-left corner showing the home region
//! (the one the commander plays in) from above. Two stacked images: the terrain (coloured from the height grid,
//! rebuilt when it changes) and an overlay redrawn at [`OVERLAY_HZ`] with a
//! dot per entity and the camera's ground footprint. Left press or drag on it
//! moves the camera there; right click stays free for Phase 4 orders.
//!
//! Orientation: north (−z) up, like the camera; image row 0 is the region's
//! minimum z. Nothing shows until the home region is known.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use game::EntityKind;

use super::height::HeightGrid;
use super::selection::Selection;
use super::{CommanderCamera, CommanderView};
use crate::renderer::bridge::{SimEntity, SimKind};
use crate::renderer::cursor::CursorPolicy;

/// Edge of the minimap on screen, logical pixels.
pub const MINIMAP_SIZE_PX: f32 = 220.0;
/// Gap between the minimap and the window corner.
pub const MINIMAP_MARGIN_PX: f32 = 12.0;
/// Overlay (dots + footprint) redraw rate.
pub const OVERLAY_HZ: f32 = 10.0;
/// Overlay image edge in pixels (finer than the terrain so lines stay thin).
pub const OVERLAY_PX: u32 = 440;
/// Terrain image edge in pixels. Each pixel samples the block column at its
/// centre, so a region wider than this many blocks is subsampled.
pub const BASE_PX: u32 = 256;
/// Shortest gap between two terrain rebuilds while chunks stream in.
pub const BASE_REBUILD_S: f32 = 0.25;
/// Entity dot half-size, overlay pixels.
pub const DOT_RADIUS_PX: i32 = 2;
/// How far a footprint corner whose ray misses the ground plane is put, as a
/// multiple of the far zoom distance.
pub const FOOTPRINT_MISS_ZOOMS: f32 = 2.0;

const DOT_COLOR: [u8; 4] = [235, 70, 60, 255];
const SELECTED_DOT_COLOR: [u8; 4] = [120, 255, 120, 255];
const FOOTPRINT_COLOR: [u8; 4] = [255, 255, 255, 230];

/// The minimap's rectangle in window coordinates (logical px, y down).
pub fn screen_rect(window: Vec2) -> Rect {
    let x0 = MINIMAP_MARGIN_PX;
    let y1 = window.y - MINIMAP_MARGIN_PX;
    Rect::new(x0, y1 - MINIMAP_SIZE_PX, x0 + MINIMAP_SIZE_PX, y1)
}

/// Window point on the minimap → world (x, z) on the map.
pub fn screen_to_map(cursor: Vec2, screen: Rect, map: Rect) -> Vec2 {
    let t = ((cursor - screen.min) / screen.size()).clamp(Vec2::ZERO, Vec2::ONE);
    map.min + t * map.size()
}

/// World (x, z) → minimap pixel (fractional) in an image of edge `px`.
pub fn map_to_pixel(world: Vec2, map: Rect, px: u32) -> Vec2 {
    (world - map.min) / map.size() * px as f32
}

/// Pointer state: a left press that started on the minimap drags the camera
/// until it is released, wherever the pointer goes meanwhile.
#[derive(Resource, Default)]
pub struct MinimapPointer {
    pub dragging: bool,
}

/// The minimap's pixels (RGBA, row 0 = the region's minimum z), drawn by the
/// HUD layer (`HudUi`): the terrain base and the dots/footprint overlay. A
/// buffer is uploaded to Slint when its `*_dirty` flag is set.
#[derive(Resource)]
pub struct MinimapPixels {
    pub base: Vec<[u8; 4]>,
    pub overlay: Vec<[u8; 4]>,
    pub base_dirty: bool,
    pub overlay_dirty: bool,
}

impl Default for MinimapPixels {
    fn default() -> Self {
        Self {
            base: vec![[0; 4]; (BASE_PX * BASE_PX) as usize],
            overlay: vec![[0; 4]; (OVERLAY_PX * OVERLAY_PX) as usize],
            base_dirty: false,
            overlay_dirty: false,
        }
    }
}

/// Left press / drag on the minimap → move the camera focus there.
pub fn minimap_pointer(
    mut follow: ResMut<super::follow::Follow>,
    buttons: Res<ButtonInput<MouseButton>>,
    window: Query<&Window, With<PrimaryWindow>>,
    policy: Res<CursorPolicy>,
    mut pointer: ResMut<MinimapPointer>,
    mut cam: ResMut<CommanderCamera>,
) {
    if !buttons.pressed(MouseButton::Left) {
        pointer.dragging = false;
    }
    let Ok(window) = window.single() else { return };
    let Some(cursor) = window.cursor_position() else { return };
    let rect = screen_rect(window.size());
    if buttons.just_pressed(MouseButton::Left)
        && !policy.blocks_game()
        && !policy.ui_pointer
        && rect.contains(cursor)
    {
        pointer.dragging = true;
    }
    if let (true, Some(bounds)) = (pointer.dragging, cam.bounds) {
        cam.focus = screen_to_map(cursor, rect, bounds);
        follow.0 = None;
    }
}

/// Colour of one map column (port of `worldgen/examples/world_map.rs`
/// `base_color`): water darkens with depth, sand at the shore, grass tinted
/// by elevation above the sea. Heights in metres; NaN = not loaded.
pub fn column_color(surface_m: f32, ground_m: f32) -> [u8; 4] {
    if surface_m.is_nan() {
        return [0, 0, 0, 0];
    }
    let sea = worldgen::pipeline::SEA_LEVEL as f32;
    let rgb = if ground_m.is_nan() || surface_m > ground_m + 0.01 {
        let depth = if ground_m.is_nan() { 128.0 } else { surface_m - ground_m };
        ramp(
            &[
                (0.0, [110.0, 190.0, 215.0]),
                (8.0, [60.0, 140.0, 200.0]),
                (40.0, [30.0, 80.0, 160.0]),
                (128.0, [12.0, 30.0, 90.0]),
            ],
            depth,
        )
    } else if ground_m <= sea + 1.0 {
        [222.0, 205.0, 150.0]
    } else {
        ramp(
            &[
                (0.0, [96.0, 168.0, 84.0]),
                (30.0, [70.0, 140.0, 66.0]),
                (80.0, [120.0, 130.0, 80.0]),
                (140.0, [140.0, 120.0, 100.0]),
                (200.0, [170.0, 165.0, 160.0]),
                (250.0, [245.0, 245.0, 250.0]),
            ],
            ground_m - sea,
        )
    };
    [rgb[0] as u8, rgb[1] as u8, rgb[2] as u8, 255]
}

/// Piecewise-linear colour ramp over ascending `(value, rgb)` stops.
fn ramp(stops: &[(f32, [f32; 3])], v: f32) -> [f32; 3] {
    let mut prev = stops[0];
    for &stop in stops {
        if v <= stop.0 {
            let span = (stop.0 - prev.0).max(f32::EPSILON);
            let t = ((v - prev.0) / span).clamp(0.0, 1.0);
            return std::array::from_fn(|i| prev.1[i] + (stop.1[i] - prev.1[i]) * t);
        }
        prev = stop;
    }
    prev.1
}

/// Recolour the terrain image when the height grid changed (rate-limited
/// while the join streams chunks in).
pub fn rebuild_minimap_base(
    grid: Res<HeightGrid>,
    cam: Res<CommanderCamera>,
    mut pixels: ResMut<MinimapPixels>,
    time: Res<Time>,
    mut built: Local<Option<(u64, Option<Rect>)>>,
    mut since: Local<f32>,
) {
    *since += time.delta_secs();
    let key = (grid.version, cam.bounds);
    if *built == Some(key) || *since < BASE_REBUILD_S {
        return;
    }
    let data = &mut pixels.base;
    let unit = game::movement::UNITS_PER_METER;
    let px = BASE_PX as usize;
    for py in 0..px {
        for pxx in 0..px {
            let i = pxx + px * py;
            let color = cam.bounds.and_then(|b| {
                let at = b.min + (Vec2::new(pxx as f32, py as f32) + 0.5) / px as f32 * b.size();
                grid.column_at(at.x, at.y)
            });
            let color = color.map_or([0; 4], |(surface, ground)| column_color(surface / unit, ground / unit));
            data[i] = color;
        }
    }
    pixels.base_dirty = true;
    *built = Some(key);
    *since = 0.0;
}

/// The camera's ground footprint: where the four screen-corner rays meet the
/// plane at the focus height (world x, z), clockwise from top-left. A ray that
/// misses the plane (or meets it absurdly far) is clamped to
/// [`FOOTPRINT_MISS_ZOOMS`] far-zoom distances along its horizontal heading.
pub fn footprint(rays: [Ray3d; 4], plane_y: f32) -> [Vec2; 4] {
    let max = FOOTPRINT_MISS_ZOOMS * super::ZOOM_FAR_M * game::movement::UNITS_PER_METER;
    rays.map(|ray| {
        let dir = *ray.direction;
        let flat = Vec2::new(dir.x, dir.z).normalize_or_zero();
        let origin = Vec2::new(ray.origin.x, ray.origin.z);
        let t = (plane_y - ray.origin.y) / dir.y;
        let hit = (dir.y < 0.0 && t > 0.0).then(|| origin + Vec2::new(dir.x, dir.z) * t);
        match hit {
            Some(p) if p.distance(origin) <= max => p,
            _ => origin + flat * max,
        }
    })
}

/// Redraw dots and the footprint at [`OVERLAY_HZ`].
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn draw_minimap_overlay(
    mut pixels: ResMut<MinimapPixels>,
    time: Res<Time>,
    mut since: Local<f32>,
    mut scratch: Local<Vec<[u8; 4]>>,
    entities: Query<(&SimEntity, &SimKind, &GlobalTransform)>,
    selection: Res<Selection>,
    cam: Res<CommanderCamera>,
    view: Query<(&Camera, &Transform), With<CommanderView>>,
) {
    *since += time.delta_secs();
    if *since < 1.0 / OVERLAY_HZ {
        return;
    }
    *since = 0.0;
    // Drawn into a scratch buffer and kept only if it differs: the HUD
    // re-uploads its whole window-sized texture for any change.
    let data = &mut *scratch;
    data.clear();
    data.resize((OVERLAY_PX * OVERLAY_PX) as usize, [0; 4]);
    draw_overlay(data, &entities, &selection, &cam, &view);
    if *data != pixels.overlay {
        std::mem::swap(data, &mut pixels.overlay);
        pixels.overlay_dirty = true;
    }
}

fn draw_overlay(
    data: &mut [[u8; 4]],
    entities: &Query<(&SimEntity, &SimKind, &GlobalTransform)>,
    selection: &Selection,
    cam: &CommanderCamera,
    view: &Query<(&Camera, &Transform), With<CommanderView>>,
) {
    let Some(map) = cam.bounds else { return };
    let px = OVERLAY_PX;
    let mut put = |x: i32, y: i32, c: [u8; 4]| {
        if x >= 0 && y >= 0 && (x as u32) < px && (y as u32) < px {
            data[x as usize + px as usize * y as usize] = c;
        }
    };
    for (sim, kind, gt) in entities {
        if matches!(kind.0, EntityKind::Player) {
            continue; // commanders have no body to show
        }
        let p = gt.translation();
        let at = map_to_pixel(Vec2::new(p.x, p.z), map, px).as_ivec2();
        let color = if selection.contains(sim.region, sim.key) { SELECTED_DOT_COLOR } else { DOT_COLOR };
        for dy in -DOT_RADIUS_PX..=DOT_RADIUS_PX {
            for dx in -DOT_RADIUS_PX..=DOT_RADIUS_PX {
                put(at.x + dx, at.y + dy, color);
            }
        }
    }
    if let Ok((camera, tf)) = view.single() {
        let gt = GlobalTransform::from(*tf);
        if let Some(size) = camera.logical_viewport_size() {
            let corners = [Vec2::ZERO, Vec2::new(size.x, 0.0), size, Vec2::new(0.0, size.y)];
            let rays = corners.map(|c| camera.viewport_to_world(&gt, c).ok());
            if let [Some(a), Some(b), Some(c), Some(d)] = rays {
                let quad = footprint([a, b, c, d], cam.ground_y);
                for k in 0..4 {
                    let from = map_to_pixel(quad[k], map, px);
                    let to = map_to_pixel(quad[(k + 1) % 4], map, px);
                    let steps = from.distance(to).ceil().max(1.0) as i32;
                    for s in 0..=steps {
                        let p = from.lerp(to, s as f32 / steps as f32);
                        put(p.x as i32, p.y as i32, FOOTPRINT_COLOR);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_marks_the_overlay_dirty() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_secs_f32(2.0 / OVERLAY_HZ),
            ))
            .init_resource::<MinimapPixels>()
            .init_resource::<Selection>()
            .insert_resource(CommanderCamera {
                bounds: Some(Rect::new(0.0, 0.0, 1024.0, 1024.0)),
                ..default()
            })
            .add_systems(Update, draw_minimap_overlay);
        // A stale pixel from an earlier frame: the redraw changes the buffer.
        app.world_mut().resource_mut::<MinimapPixels>().overlay[0] = [1, 2, 3, 4];
        app.update();
        app.update();
        let px = app.world().resource::<MinimapPixels>();
        assert!(px.overlay_dirty);
        assert_eq!(px.overlay[0], [0; 4]);
        assert_eq!(px.overlay.len(), (OVERLAY_PX * OVERLAY_PX) as usize);
        assert_eq!(px.base.len(), (BASE_PX * BASE_PX) as usize);
    }

    /// An overlay redrawn with nothing changed is not uploaded again (the HUD
    /// re-uploads its whole window-sized texture for any change).
    #[test]
    fn redrawing_an_unchanged_overlay_is_not_dirty() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_secs_f32(2.0 / OVERLAY_HZ),
            ))
            .init_resource::<MinimapPixels>()
            .init_resource::<Selection>()
            .insert_resource(CommanderCamera {
                bounds: Some(Rect::new(0.0, 0.0, 1024.0, 1024.0)),
                ..default()
            })
            .add_systems(Update, draw_minimap_overlay);
        app.update();
        app.update();
        app.world_mut().resource_mut::<MinimapPixels>().overlay_dirty = false; // uploaded
        app.update();
        assert!(!app.world().resource::<MinimapPixels>().overlay_dirty);
    }

    #[test]
    fn screen_rect_sits_in_the_bottom_left_corner() {
        let r = screen_rect(Vec2::new(1920.0, 1080.0));
        assert_eq!(r.min, Vec2::new(MINIMAP_MARGIN_PX, 1080.0 - MINIMAP_MARGIN_PX - MINIMAP_SIZE_PX));
        assert_eq!(r.size(), Vec2::splat(MINIMAP_SIZE_PX));
    }

    #[test]
    fn minimap_and_map_coordinates_round_trip() {
        let map = super::super::region_rect(game::RegionCoords { x: 1, z: -2 });
        let screen = screen_rect(Vec2::new(1280.0, 720.0));
        assert_eq!(screen_to_map(screen.min, screen, map), map.min);
        assert_eq!(screen_to_map(screen.max, screen, map), map.max);
        assert_eq!(screen_to_map(screen.center(), screen, map), map.center());
        assert_eq!(screen_to_map(screen.max + 50.0, screen, map), map.max, "clamped");
        let p = map_to_pixel(map.center(), map, 440);
        assert_eq!(p, Vec2::splat(220.0));
    }

    #[test]
    fn footprint_is_a_trapezoid_wider_at_the_far_edge() {
        let pitch = super::super::PITCH_DEG.to_radians();
        let tf = super::super::camera_transform(Vec3::ZERO, 640.0, 0.0, pitch);
        let half_v = super::super::FOV_Y_DEG.to_radians() / 2.0;
        let half_h = (half_v.tan() * 16.0 / 9.0).atan();
        let ray = |sx: f32, sy: f32| {
            let local = Vec3::new(sx * half_h.tan(), sy * half_v.tan(), -1.0);
            Ray3d::new(tf.translation, Dir3::new(tf.rotation * local).unwrap())
        };
        // top-left, top-right, bottom-right, bottom-left
        let q = footprint([ray(-1.0, 1.0), ray(1.0, 1.0), ray(1.0, -1.0), ray(-1.0, -1.0)], 0.0);
        let far = q[1].x - q[0].x;
        let near = q[2].x - q[3].x;
        assert!(far > near, "far edge {far} vs near edge {near}");
        assert!(q[0].y < q[3].y, "the top edge is north of the bottom edge");
    }

    #[test]
    fn footprint_clamps_a_ray_that_misses_the_plane() {
        let up = Ray3d::new(Vec3::new(0.0, 100.0, 0.0), Dir3::new(Vec3::new(0.0, 0.5, -1.0)).unwrap());
        let q = footprint([up; 4], 0.0);
        let max = FOOTPRINT_MISS_ZOOMS * super::super::ZOOM_FAR_M * game::movement::UNITS_PER_METER;
        assert!((q[0].length() - max).abs() < 1e-2);
        assert!(q[0].y < 0.0, "along the ray's heading (north)");
    }

    #[test]
    fn column_colours_tell_water_sand_land_and_unknown_apart() {
        let sea = worldgen::pipeline::SEA_LEVEL as f32;
        assert_eq!(column_color(f32::NAN, f32::NAN)[3], 0, "unloaded is transparent");
        let water = column_color(sea, sea - 10.0);
        assert!(water[2] > water[0], "water is blue");
        assert_eq!(column_color(sea, sea), [222, 205, 150, 255], "shore is sand");
        let grass = column_color(sea + 10.0, sea + 10.0);
        assert!(grass[1] > grass[0] && grass[1] > grass[2], "land is green");
    }
}
