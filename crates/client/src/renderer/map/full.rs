//! The full map (M, and the start screen) on the map pass (plan Task 7, spec §5.5): one [`MapView`]
//! over the Slint window's transparent view. Slint reports drag, wheel and click; this owns the
//! view — drag pans, the wheel zooms about the pointer, a click selects the region under it — and
//! pushes the markers (home, selection, hover, sites) in the view's own logical pixels, so the zoom is
//! continuous and nothing is scaled by Slint. Opening centres on the player at the minimap's zoom.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::math::DVec2;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use game::map_tile::MapTileKey;
use game::{RegionCoords, WorldMap, REGION_BLOCKS};
use slint_ui::slint::{Color, ComponentHandle, ModelRc, SharedString, VecModel};
use slint_ui::{GameUi, MapMarker, WorldMapConfig};
use worldgen::tile_map::MapLayer;

use super::store::MapTiles;
use super::views::{top_level, MapLayerKind, MapView, MINIMAP_M_PER_PX};
use crate::renderer::bridge::LocalBody;
use crate::renderer::history_view::HistoryView;
use crate::renderer::hud::HudStatus;
use crate::renderer::slint_overlay::{map_tile, site_color, tile_region, GameUiActions, SlintLayer, UiAction};
use crate::renderer::world_map::WorldMapState;
use crate::session::{SessionLocal, SessionScoped};

/// The wheel's zoom per notch.
pub const FULL_MAP_ZOOM_STEP: f32 = 1.25;
/// The closest zoom (metres per window pixel).
pub const FULL_MAP_MIN_M_PER_PX: f32 = 0.25;
/// A site marker's edge (view logical px).
pub const SITE_MARKER_PX: f32 = 8.0;

/// The layer button captions: the block surface first, then the world map's layers.
pub fn layer_names() -> Vec<SharedString> {
    std::iter::once("Surface".into()).chain(MapLayer::ALL.iter().map(|l| l.label().into())).collect()
}

/// The layer of button `index` ([`layer_names`]'s order); the surface for an unknown one.
pub fn layer_kind(index: i32) -> MapLayerKind {
    match usize::try_from(index).ok().and_then(|i| i.checked_sub(1)).and_then(|i| MapLayer::ALL.get(i)) {
        Some(l) => MapLayerKind::Theme(*l),
        None => MapLayerKind::Surface,
    }
}

/// The full map's view entity.
#[derive(Component)]
pub struct FullMapView;

enum Input {
    Drag(f32, f32),
    Wheel(f32, f32, f32),
    Click(f32, f32),
}

/// The Slint callbacks' queue (`!Send`).
pub struct FullMapInputs(Rc<RefCell<Vec<Input>>>);

/// Connect the map window's callbacks (from the game UI layer's init).
pub fn connect(ui: &GameUi, world: &mut World) {
    let inputs = Rc::new(RefCell::new(Vec::new()));
    let q = inputs.clone();
    ui.on_map_drag(move |dx, dy| q.borrow_mut().push(Input::Drag(dx, dy)));
    let q = inputs.clone();
    ui.on_map_wheel(move |x, y, n| q.borrow_mut().push(Input::Wheel(x, y, n)));
    let q = inputs.clone();
    ui.on_map_click(move |x, y| q.borrow_mut().push(Input::Click(x, y)));
    ui.set_map_layer_names(ModelRc::new(VecModel::from(layer_names())));
    world.insert_non_send(FullMapInputs(inputs));
}

pub(super) fn build(app: &mut App) {
    app.add_systems(Update, drive_full_map);
}

/// The player in world blocks (x, z; the render frame's unwrapped copy): the body, else the commander
/// camera's focus.
pub fn player_block(
    body: &Query<&GlobalTransform, With<LocalBody>>,
    cam: Option<&crate::renderer::commander::CommanderCamera>,
) -> Option<DVec2> {
    let unit = game::movement::UNITS_PER_METER as f64;
    if let Some(t) = body.iter().next() {
        let p = t.translation();
        return Some(DVec2::new(p.x as f64 / unit, p.z as f64 / unit));
    }
    cam.map(|c| DVec2::new(c.focus.x as f64 / unit, c.focus.y as f64 / unit))
}

/// What the markers were last pushed for.
#[derive(Default)]
struct Pushed {
    open: bool,
    key: Option<(i64, i64, u32, u32, i32, i32, i32, i32, u64)>,
    readout: String,
    /// The selection as this view last saw it: a change made elsewhere (the arrival list) is brought
    /// into view.
    selection: (i32, i32),
}

#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn drive_full_map(
    mut commands: Commands,
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    inputs: Option<NonSend<FullMapInputs>>,
    actions: Option<NonSend<GameUiActions>>,
    mut views: Query<&mut MapView, With<FullMapView>>,
    state: Res<WorldMapState>,
    status: Res<HudStatus>,
    history: Res<HistoryView>,
    tiles: Res<MapTiles>,
    window: Query<&Window, With<PrimaryWindow>>,
    body: Query<&GlobalTransform, With<LocalBody>>,
    cam: Option<Res<crate::renderer::commander::CommanderCamera>>,
    mut pushed: SessionLocal<Pushed>,
) {
    let (Some(slint), Some(inputs)) = (slint, inputs) else { return };
    let Ok(mut view) = views.single_mut() else {
        commands.spawn((FullMapView, MapView::default(), SessionScoped));
        return;
    };
    let ui = slint.overlay.ui();
    let pushed = pushed.get();
    let open = ui.get_map_open() && state.map.is_some();
    // Window physical pixels per view logical pixel.
    let scale = slint.ui_scale() * window.single().map_or(1.0, |w| w.scale_factor());
    let origin = Vec2::new(ui.get_map_view_x(), ui.get_map_view_y());
    let size = Vec2::new(ui.get_map_view_width(), ui.get_map_view_height());
    view.rect = Rect::from_corners(origin * scale, (origin + size) * scale);
    view.layer = layer_kind(ui.get_map_layer());
    view.visible = open;
    let max_m = state.map.as_ref().map_or(1.0, |m| (1u64 << top_level_of(m)) as f32);
    if open && !pushed.open {
        // Opening: centred on the player at the minimap's zoom.
        if let Some(at) = player_block(&body, cam.as_deref()) {
            view.centre = at;
        }
        view.m_per_px = MINIMAP_M_PER_PX.clamp(FULL_MAP_MIN_M_PER_PX, max_m);
    }
    pushed.open = open;
    // View logical px → window physical px.
    let to_px = |x: f32, y: f32| (origin + Vec2::new(x, y)) * scale;
    for input in inputs.0.borrow_mut().drain(..) {
        if !open {
            continue;
        }
        match input {
            Input::Drag(dx, dy) => {
                let m = (scale * view.m_per_px) as f64;
                view.centre -= DVec2::new(dx as f64, dy as f64) * m;
            }
            Input::Wheel(x, y, notches) => {
                let p = to_px(x, y);
                let anchor = view.screen_to_world(p);
                let m = (view.m_per_px * FULL_MAP_ZOOM_STEP.powf(-notches)).clamp(FULL_MAP_MIN_M_PER_PX, max_m);
                view.m_per_px = m;
                // Keep the block under the pointer under the pointer.
                let after = view.screen_to_world(p);
                view.centre += anchor - after;
            }
            Input::Click(x, y) => {
                let Some(map) = state.map.as_ref() else { continue };
                let b = view.screen_to_world(to_px(x, y));
                let rc = region_of_block(b);
                if let Some((col, row)) = map_tile(map, rc) {
                    ui.set_selected_col(col);
                    ui.set_selected_row(row);
                    pushed.selection = (col, row);
                    if let Some(actions) = actions.as_ref() {
                        actions.0.borrow_mut().push(UiAction::SelectTile(col, row));
                    }
                }
            }
        }
    }
    if !open {
        return;
    }
    let Some(map) = state.map.as_ref() else { return };
    // A selection made elsewhere (the arrival panel's list): centre on it.
    let selection = (ui.get_selected_col(), ui.get_selected_row());
    if selection != pushed.selection {
        pushed.selection = selection;
        if let Some(rc) = tile_region(map, selection.0, selection.1) {
            let rc = nearest_copy(map, rc, view.centre);
            let rb = REGION_BLOCKS as f64;
            view.centre = DVec2::new((rc.x as f64 + 0.5) * rb, (rc.z as f64 + 0.5) * rb);
        }
    }
    // The readout under the pointer: block, region and height (from the loaded tile there).
    let readout = if ui.get_map_pointer_in() {
        let b = view.screen_to_world(to_px(ui.get_map_pointer_x(), ui.get_map_pointer_y()));
        pointer_line(b, &tiles, top_level(&state).unwrap_or(0))
    } else {
        String::new()
    };
    if pushed.readout != readout {
        ui.set_map_pointer_line(readout.as_str().into());
        pushed.readout = readout;
    }
    // Markers, in the view's logical pixels, only when what they follow changed.
    let hover = if ui.get_map_pointer_in() {
        map_tile(map, region_of_block(view.screen_to_world(to_px(ui.get_map_pointer_x(), ui.get_map_pointer_y()))))
    } else {
        None
    }
    .unwrap_or((-1, -1));
    let key = (
        (view.centre.x * 16.0) as i64,
        (view.centre.y * 16.0) as i64,
        view.m_per_px.to_bits(),
        size.x.to_bits() ^ size.y.to_bits(),
        ui.get_selected_col(),
        ui.get_selected_row(),
        hover.0,
        hover.1,
        history.sites.len() as u64 ^ (history.is_changed() as u64) << 32,
    );
    if pushed.key == Some(key) {
        return;
    }
    pushed.key = Some(key);
    let config = ui.global::<WorldMapConfig>();
    let mut marks = Vec::new();
    // A region's frame on screen (view logical px), from its tile (col, row).
    let frame = |col: i32, row: i32, color: Color, border: f32, inset: f32| {
        let rc = tile_region(map, col, row)?;
        // The copy of the region nearest the view (a wrapping map).
        let rc = nearest_copy(map, rc, view.centre);
        let rb = REGION_BLOCKS as f64;
        let a = view.world_to_screen(DVec2::new(rc.x as f64 * rb, rc.z as f64 * rb)) / scale - origin;
        let b = view.world_to_screen(DVec2::new((rc.x + 1) as f64 * rb, (rc.z + 1) as f64 * rb)) / scale - origin;
        Some(MapMarker {
            x: a.x + inset,
            y: a.y + inset,
            w: (b.x - a.x - 2.0 * inset).max(1.0),
            h: (b.y - a.y - 2.0 * inset).max(1.0),
            color,
            fill: false,
            border,
            label: SharedString::default(),
        })
    };
    if let Some((col, row)) = status.home_region.and_then(|h| map_tile(map, h)) {
        marks.extend(frame(col, row, config.get_home_color(), 2.0, 0.0));
    }
    if hover != (-1, -1) {
        marks.extend(frame(hover.0, hover.1, config.get_hover_color(), 1.0, 0.0));
    }
    for site in &history.sites {
        let Some(rc) = map.tile(site.region).map(|_| site.region) else { continue };
        let rc = nearest_copy(map, map.extent().canonical(rc), view.centre);
        let rb = REGION_BLOCKS as f64;
        let c = view.world_to_screen(DVec2::new((rc.x as f64 + 0.5) * rb, (rc.z as f64 + 0.5) * rb)) / scale - origin;
        let half = SITE_MARKER_PX / 2.0;
        let color = site_color(site.civ);
        marks.push(MapMarker {
            x: c.x - half,
            y: c.y - half,
            w: SITE_MARKER_PX,
            h: SITE_MARKER_PX,
            color,
            fill: !site.abandoned,
            border: if site.abandoned { 1.0 } else { 0.0 },
            label: site.name.as_str().into(),
        });
    }
    let (col, row) = (ui.get_selected_col(), ui.get_selected_row());
    if col >= 0 && row >= 0 {
        marks.extend(frame(col, row, config.get_selected_color(), 2.0, 2.0));
    }
    ui.set_map_markers(ModelRc::new(VecModel::from(marks)));
}

fn top_level_of(map: &WorldMap) -> u8 {
    game::map_tile::top_level(map)
}

/// The region holding block `b` (unwrapped).
fn region_of_block(b: DVec2) -> RegionCoords {
    let rb = REGION_BLOCKS as f64;
    RegionCoords::new((b.x / rb).floor() as i32, (b.y / rb).floor() as i32)
}

/// The copy of canonical region `rc` nearest `centre` (blocks) on a map that wraps; `rc` otherwise.
fn nearest_copy(map: &WorldMap, rc: RegionCoords, centre: DVec2) -> RegionCoords {
    let e = map.extent();
    if !e.wraps() {
        return rc;
    }
    let at = region_of_block(centre);
    let (dx, _) = e.delta(at, rc);
    RegionCoords::new(at.x + dx, rc.z)
}

/// "Block x, z · region rx, rz · height h m" for block `b`, the height from the finest loaded tile there.
fn pointer_line(b: DVec2, tiles: &MapTiles, top: u8) -> String {
    let (bx, bz) = (b.x.floor() as i64, b.y.floor() as i64);
    let rc = region_of_block(b);
    let height = (0..=top).find_map(|level| {
        let key = MapTileKey::containing(level, bx, bz);
        let tile = tiles.tile(key)?;
        let (ox, oz) = key.origin_block();
        let s = key.sample_m();
        let sample = tile.at(((bx - ox) / s) as usize, ((bz - oz) / s) as usize);
        (sample.flags & game::map_tile::sample_flags::OUTSIDE == 0).then_some(sample.height)
    });
    match height {
        Some(h) => format!("Block {bx}, {bz} · region {}, {} · {h} m", rc.x, rc.z),
        None => format!("Block {bx}, {bz} · region {}, {}", rc.x, rc.z),
    }
}
