//! The minimap: a square in the bottom-left corner, the world map's tiles (`renderer/map`) drawn by a
//! [`MapView`] centred on the player (the commander camera's focus in the mayor's view), north (−z) up,
//! [`MinimapZoom`] metres per window pixel (the wheel over it zooms, [`MINIMAP_M_PER_PX_MIN`] ..=
//! [`MINIMAP_M_PER_PX_MAX`]). Over it the HUD draws an overlay redrawn at [`OVERLAY_HZ`]: a dot per
//! entity and the camera's ground footprint, placed through the same view. Left press or drag on it moves
//! the camera there; right click stays free for Phase 4 orders. Nothing shows until the home region is
//! known.

use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::math::DVec2;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use game::EntityKind;

use super::selection::Selection;
use crate::renderer::map::views::MINIMAP_M_PER_PX;
use crate::renderer::map::MapView;
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
/// The closest and farthest minimap zoom (metres per window pixel).
pub const MINIMAP_M_PER_PX_MIN: f32 = 0.5;
pub const MINIMAP_M_PER_PX_MAX: f32 = 8.0;
/// The wheel's zoom per notch over the minimap.
pub const MINIMAP_ZOOM_STEP: f32 = 1.25;
/// Pixels of a wheel "line" (as the Slint layers count one).
const WHEEL_LINE_PX: f32 = 40.0;
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

/// The minimap's view entity.
#[derive(Component)]
pub struct MinimapView;

/// The minimap's zoom (metres per window pixel); a preference, kept across sessions.
#[derive(Resource)]
pub struct MinimapZoom(pub f32);

impl Default for MinimapZoom {
    fn default() -> Self {
        MinimapZoom(MINIMAP_M_PER_PX)
    }
}

/// World `p` (render-frame sim units, x, z) → overlay pixel (fractional) in an image of edge `px`
/// covering `view`'s rect.
pub fn world_to_overlay(view: &MapView, p: Vec2, px: u32) -> Vec2 {
    let unit = game::movement::UNITS_PER_METER as f64;
    let s = view.world_to_screen(DVec2::new(p.x as f64 / unit, p.y as f64 / unit));
    (s - view.rect.min) / view.rect.size() * px as f32
}

/// Keep the minimap's view on the player, its rect on the HUD's square and its zoom on the wheel over it.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn update_minimap_view(
    mut commands: Commands,
    mut views: Query<&mut MapView, With<MinimapView>>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Option<NonSend<crate::renderer::slint_overlay::SlintLayer<slint_ui::HudUi>>>,
    game_ui: Option<NonSend<crate::renderer::slint_overlay::SlintLayer<slint_ui::GameUi>>>,
    body: Query<&GlobalTransform, With<crate::renderer::bridge::LocalBody>>,
    cam: Res<CommanderCamera>,
    policy: Res<CursorPolicy>,
    mut wheel: MessageReader<MouseWheel>,
    mut zoom: ResMut<MinimapZoom>,
) {
    let Ok(mut view) = views.single_mut() else {
        commands.spawn((MinimapView, MapView::default(), crate::session::SessionScoped));
        return;
    };
    let Ok(window) = window.single() else { return };
    let rect = screen_rect(window.size());
    let over = window.cursor_position().is_some_and(|c| rect.contains(c)) && !policy.blocks_game() && !policy.ui_pointer;
    for event in wheel.read() {
        if !over {
            continue;
        }
        let notches = match event.unit {
            MouseScrollUnit::Line => event.y,
            MouseScrollUnit::Pixel => event.y / WHEEL_LINE_PX,
        };
        zoom.0 = (zoom.0 * MINIMAP_ZOOM_STEP.powf(-notches)).clamp(MINIMAP_M_PER_PX_MIN, MINIMAP_M_PER_PX_MAX);
    }
    let scale = window.scale_factor();
    view.rect = Rect::from_corners(rect.min * scale, rect.max * scale);
    view.m_per_px = zoom.0 / scale;
    if let Some(at) = crate::renderer::map::full::player_block(&body, Some(&cam)) {
        view.centre = at;
    }
    // Shown with the HUD's minimap, and never under the open full map (both are the map pass's).
    let shown = hud.is_some_and(|h| h.overlay.ui().get_minimap_visible());
    let map_open = game_ui.is_some_and(|g| g.overlay.ui().get_map_open());
    view.visible = shown && !map_open;
}

/// Whether the window point `cursor` (logical px) is over the minimap.
pub fn over_minimap(window: &Window, cursor: Vec2) -> bool {
    screen_rect(window.size()).contains(cursor)
}

/// Pointer state: a left press that started on the minimap drags the camera
/// until it is released, wherever the pointer goes meanwhile.
#[derive(Resource, Default)]
pub struct MinimapPointer {
    pub dragging: bool,
}

/// The minimap overlay's pixels (RGBA, row 0 = the view's top), drawn by the HUD layer (`HudUi`) over
/// the map pass: the dots and the footprint. Uploaded to Slint when `overlay_dirty` is set.
#[derive(Resource)]
pub struct MinimapPixels {
    pub overlay: Vec<[u8; 4]>,
    pub overlay_dirty: bool,
}

impl Default for MinimapPixels {
    fn default() -> Self {
        Self { overlay: vec![[0; 4]; (OVERLAY_PX * OVERLAY_PX) as usize], overlay_dirty: false }
    }
}

/// Left press / drag on the minimap → move the camera focus there.
pub fn minimap_pointer(
    views: Query<&MapView, With<MinimapView>>,
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
    let Ok(view) = views.single() else { return };
    if pointer.dragging && view.visible {
        let unit = game::movement::UNITS_PER_METER as f64;
        let at = view.screen_to_world(cursor.clamp(rect.min, rect.max) * window.scale_factor()) * unit;
        cam.focus = Vec2::new(at.x as f32, at.y as f32);
        follow.0 = None;
    }
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
    map: Query<&MapView, With<MinimapView>>,
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
    if let Ok(map) = map.single() {
        draw_overlay(data, &entities, &selection, &cam, &view, map);
    }
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
    map: &MapView,
) {
    if !map.visible || map.rect.width() <= 0.0 {
        return;
    }
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
        let at = world_to_overlay(map, Vec2::new(p.x, p.z), px).as_ivec2();
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
                    let from = world_to_overlay(map, quad[k], px);
                    let to = world_to_overlay(map, quad[(k + 1) % 4], px);
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

