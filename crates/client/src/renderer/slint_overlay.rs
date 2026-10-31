//! The player-facing game UI: `slint_ui`'s [`GameUi`] (Factorio-style widgets
//! from `~/Projects/ui`), software-rendered and composited over the 3D view.
//!
//! - **Input:** Bevy pointer, wheel and key events → Slint window events, in
//!   `PreUpdate`. The UI owns the pointer wherever it drew a pixel
//!   (`CursorPolicy::ui_pointer`), and keeps it while a press that started
//!   there is held, so the commander's selection, panning and zoom leave it
//!   alone.
//! - **State:** client state → Slint properties, only when it changed;
//!   Slint callbacks → [`UiAction`]s applied to the client in `Update`.
//! - **Pixels:** `Overlay::render` redraws what changed; that rect is
//!   un-premultiplied into a window-sized `Image`, one texel per physical
//!   pixel, which `slint_composite` draws over the [`UiCamera`](super::UiCamera)'s
//!   view in layer order ([`HUD_LAYER_ORDER`] < [`GAME_LAYER_ORDER`]).
//!
//! The component-agnostic part — sizing, input, pointer ownership, upload —
//! is [`SlintLayerPlugin`], generic over the Slint component, so other apps
//! can show their own screen the same way (the asset viewer example,
//! `just asset-viewer`). The game UI is commander mode only. Design and trade-offs (CPU raster, blending,
//! licences): docs/superpowers/specs/2026-10-20-slint-ui-library-in-bevy-investigation.md.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::SystemParam;
use bevy::image::ImageSampler;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::{MouseButtonInput, MouseScrollUnit, MouseWheel};
use bevy::input::ButtonState;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use game::{PlayRefusal, RegionCoords, WorldMap};
use slint_ui::slint::platform::{Key as SlintKey, PointerEventButton, WindowEvent};
use slint_ui::slint::{
    ComponentHandle, Image as SlintImage, LogicalPosition, ModelRc, Rgba8Pixel, SharedPixelBuffer,
    SharedString, VecModel,
};
use slint_ui::wheel::{self, WheelInput, WheelPhase};
use slint_ui::{GameUi, HudConfig, HudUi, Overlay, WorldMapConfig};
use worldgen::tile_map::{play_blocker, rasterize, tile_info_lines};

use super::bridge::{SimEntity, SimKind};
use super::commander::designate::{DesignateState, DragReadout, Tool, TOOL_KEYS};
use super::commander::height::HeightGrid;
use super::commander::level::{focus_surface, surface_level, WorkingLevel};
use super::commander::minimap;
use super::commander::selection::{Selection, SelectionBand};
use super::commander::tiles::TileMirror;
use super::commander::adventure::{ClientPlayMode, ToggleModeRequest};
use super::commander::CommanderCamera;
use super::cursor::CursorPolicy;
use super::slint_composite::{SlintCompositePlugin, SlintLayerTextures};
use super::hud::{crosshair_visible, HudStatus};
use super::world_map::WorldMapState;
use super::ClientRequests;
use crate::instance::ClientRequest;

/// Shows and hides the panel.
pub const SLINT_PANEL_KEY: KeyCode = KeyCode::F9;
/// Closes the world map if it is open, else opens / closes the menu —
/// unless a designation is in progress, which it cancels instead.
pub const MENU_KEY: KeyCode = KeyCode::Escape;
/// Opens / closes the world map (opening it closes the menu).
pub const MAP_KEY: KeyCode = KeyCode::KeyM;
/// Held, opens the tool wheel at the pointer ([`tool_wheel`]).
pub const WHEEL_KEY: KeyCode = KeyCode::KeyQ;
/// The map's status line from Play until the new region has loaded.
pub const LOADING_TEXT: &str = "Loading region\u{2026}";
/// UI pixels per window logical pixel. The widgets are pixel-exact at a
/// total scale (this × the window's scale factor) of 1 and 2. Overridden by
/// `VOXEL_UI_SCALE`.
pub const UI_SCALE: f32 = 1.0;
/// Logical pixels one wheel notch scrolls.
pub const WHEEL_LINE_PX: f32 = 40.0;
/// Draw order of the Slint layers (higher on top). The interactive game UI
/// (tool bar, menu, map) sits above the pass-through HUD.
pub const GAME_LAYER_ORDER: i32 = 1_000;
/// The pass-through HUD (crosshair, selection band, minimap).
pub const HUD_LAYER_ORDER: i32 = 0;
/// The F3 dev panel, above everything (`dev_ui`).
pub const DEV_LAYER_ORDER: i32 = 2_000;

fn ui_scale() -> f32 {
    std::env::var("VOXEL_UI_SCALE")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|s| *s > 0.0)
        .unwrap_or(UI_SCALE)
}

/// What a Slint callback asked the client to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    ClearSelection,
    /// The menu's World Map button.
    OpenMap,
    CloseMap,
    CloseMenu,
    /// A map tile clicked (column, row); Slint has already selected it.
    SelectTile(i32, i32),
    /// The map's Play button: ask the server for the selected tile.
    Play,
    /// The panel's mode button: Fortress ↔ Adventure
    /// (`commander::adventure`).
    ToggleMode,
}

/// The region a Play request was sent for, until we are there with its region
/// loaded ([`play_settled`]) or the server refuses it.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayPending(pub Option<RegionCoords>);

/// Which modal game screens are open. The client keeps them exclusive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Screens {
    pub menu: bool,
    pub map: bool,
    /// The tool wheel ([`tool_wheel`]): modal while open, so the commander
    /// leaves the right button and the world alone.
    pub wheel: bool,
}

impl Screens {
    fn of(ui: &GameUi) -> Self {
        Self { menu: ui.get_menu_open(), map: ui.get_map_open(), wheel: ui.get_wheel_open() }
    }

    pub fn any(self) -> bool {
        self.menu || self.map || self.wheel
    }

    /// [`MENU_KEY`] (with no designation to cancel): the wheel closes first,
    /// then the map.
    pub fn escape(self) -> Self {
        if self.wheel {
            Self { wheel: false, ..self }
        } else if self.map {
            Self { map: false, ..self }
        } else {
            Self { menu: !self.menu, ..self }
        }
    }

    /// [`MAP_KEY`].
    pub fn toggle_map(self) -> Self {
        if self.map {
            Self { map: false, wheel: false, ..self }
        } else {
            Self::open_map()
        }
    }

    pub fn open_map() -> Self {
        Self { map: true, ..Self::default() }
    }
}

/// Filled by the game UI's callbacks during event dispatch (`!Send`, a
/// non-send resource on the main thread).
struct GameUiActions(Rc<RefCell<Vec<UiAction>>>);

/// Any Slint component drawn as a window-sized layer over the 3D view:
/// sized to the window, fed its input, deciding who owns the pointer, and
/// uploaded into the texture it is shown with. The game UI is one
/// ([`SlintOverlayPlugin`]); the asset viewer example is another.
pub struct SlintLayerPlugin<C> {
    make: fn() -> Result<C, slint_ui::slint::PlatformError>,
    /// Runs once on the new component, before it is inserted: connect its
    /// callbacks here.
    init: fn(&C, &mut World),
    order: i32,
    passthrough: bool,
    follows_ui_scale: bool,
}

impl<C> SlintLayerPlugin<C> {
    /// `make` is the component's generated `new` (e.g. `GameUi::new`).
    pub fn new(make: fn() -> Result<C, slint_ui::slint::PlatformError>) -> Self {
        Self { make, init: |_, _| {}, order: GAME_LAYER_ORDER, passthrough: false, follows_ui_scale: true }
    }

    /// Draw order among the layers (higher on top); [`GAME_LAYER_ORDER`] by default.
    pub fn order(mut self, order: i32) -> Self {
        self.order = order;
        self
    }

    /// Drawn and sized, never fed input: the view underneath keeps the pointer
    /// even over its pixels.
    pub fn passthrough(mut self) -> Self {
        self.passthrough = true;
        self
    }

    /// Ignore `VOXEL_UI_SCALE` (window scale factor only): for a layer whose
    /// sizes Bevy-side code shares in window pixels (the HUD's minimap).
    pub fn window_scale_only(mut self) -> Self {
        self.follows_ui_scale = false;
        self
    }

    pub fn with_init(mut self, init: fn(&C, &mut World)) -> Self {
        self.init = init;
        self
    }
}

impl<C: ComponentHandle + 'static> Plugin for SlintLayerPlugin<C> {
    fn build(&self, app: &mut App) {
        let (make, init, order, passthrough) = (self.make, self.init, self.order, self.passthrough);
        let ui_scale = if self.follows_ui_scale { ui_scale() } else { 1.0 };
        if !app.is_plugin_added::<SlintCompositePlugin>() {
            app.add_plugins(SlintCompositePlugin);
        }
        app.init_resource::<SlintPointer>()
            // PreStartup: an app's Startup systems may use the layer and
            // whatever `init` inserted.
            .add_systems(PreStartup, move |world: &mut World| {
                setup_slint_layer(world, make, init, order, passthrough, ui_scale)
            })
            .add_systems(PreUpdate, slint_input::<C>.after(bevy::input::InputSystems))
            .add_systems(PostUpdate, render_slint_layer::<C>);
    }
}

/// One component's layer, `!Send` (a non-send resource on the main thread).
pub struct SlintLayer<C: 'static> {
    pub overlay: Overlay<C>,
    ui_scale: f32,
    /// The texture the overlay is drawn into.
    target: Handle<Image>,
    /// The pointer is over a pixel the UI drew, or a press that started
    /// there is still held: the view underneath should leave it alone.
    pub owns_pointer: bool,
    order: i32,
    passthrough: bool,
}

/// Whether a layer takes the pointer from the view underneath.
pub fn layer_owns_pointer(passthrough: bool, over_ui: bool, captured: bool) -> bool {
    !passthrough && (over_ui || captured)
}

/// Every layer's pointer state, by order, so stacked layers agree: the
/// topmost layer under the pointer takes presses and the wheel, and the view
/// underneath has the pointer only while no layer owns it.
#[derive(Resource, Default)]
pub struct SlintPointer {
    /// order → (over one of its pixels, owns the pointer)
    layers: BTreeMap<i32, (bool, bool)>,
}

impl SlintPointer {
    pub fn set(&mut self, order: i32, over: bool, owns: bool) {
        self.layers.insert(order, (over, owns));
    }

    /// A higher layer is under the pointer.
    pub fn blocked_above(&self, order: i32) -> bool {
        self.layers.range(order + 1..).any(|(_, (over, _))| *over)
    }

    pub fn any_owner(&self) -> bool {
        self.layers.values().any(|(_, owns)| *owns)
    }
}

/// Pointer bookkeeping between frames.
#[derive(Default)]
struct PointerState {
    /// Last position sent to Slint, logical UI pixels.
    last: Option<Vec2>,
    /// A press started on the UI and a button is still down.
    captured: bool,
}

pub struct SlintOverlayPlugin;

impl Plugin for SlintOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(SlintLayerPlugin::new(GameUi::new).with_init(connect_game_ui))
            .init_resource::<PlayPending>()
            .add_systems(
                Update,
                (
                    toggle_panel,
                    // Before every commander input system (the chain starts
                    // at `minimap_pointer`, `designate_keys` is in it), so
                    // `CursorPolicy::ui_modal` holds this frame and one Esc
                    // press does one thing.
                    menu_keys.before(minimap::minimap_pointer),
                    apply_ui_actions.before(minimap::minimap_pointer),
                    // Also before the commander: the clicks the open wheel
                    // takes must not reach the world.
                    tool_wheel.before(minimap::minimap_pointer),
                    push_ui_state,
                )
                    .chain(),
            );
    }
}

/// Receiver half of the sim's UI projection channel (`game::UiUpdate`), once
/// the instance announces it (`ClientUpdateEvent::UiChannel`).
#[derive(Resource, Default)]
pub struct UiUpdates(pub Option<crossbeam::channel::Receiver<game::UiUpdate>>);

/// Apply the sim's UI projection: the cursor grab it asks for. Its hotbar
/// is not drawn (the HUD's hotbar strip was removed; the fortress tools are
/// on the tool wheel).
pub fn drain_ui_updates(
    updates: Res<UiUpdates>,
    mut cursor_policy: ResMut<CursorPolicy>,
    mut play_mode: Option<ResMut<ClientPlayMode>>,
) {
    let Some(receiver) = updates.0.as_ref() else { return };
    while let Ok(update) = receiver.try_recv() {
        match update {
            game::UiUpdate::Hotbar(_) => {}
            game::UiUpdate::PlayMode(mode) => {
                if let Some(play_mode) = play_mode.as_mut() {
                    play_mode.set_if_neq(ClientPlayMode(mode));
                }
            }
            game::UiUpdate::SetCursorGrab(grab) => cursor_policy.sim_grab = grab,
        }
    }
}

/// The pass-through HUD (`HudUi`: crosshair, selection band, minimap), in
/// both camera modes, under the game UI. It never takes the
/// pointer, so commander selection and minimap drags work over its pixels.
pub struct HudLayerPlugin;

impl Plugin for HudLayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(
            SlintLayerPlugin::new(HudUi::new)
                .order(HUD_LAYER_ORDER)
                .passthrough()
                .window_scale_only()
                .with_init(check_hud_config),
        )
        .add_systems(Update, push_hud_state);
    }
}

fn check_hud_config(ui: &HudUi, _: &mut World) {
    let config = ui.global::<HudConfig>();
    debug_assert_eq!(config.get_minimap_size(), minimap::MINIMAP_SIZE_PX, "HudConfig.minimap-size");
    debug_assert_eq!(config.get_minimap_margin(), minimap::MINIMAP_MARGIN_PX, "HudConfig.minimap-margin");
}

/// Client state → HUD properties, only where it changed.
fn push_hud_state(
    hud: Option<NonSend<SlintLayer<HudUi>>>,
    cursor: Query<&CursorOptions, With<PrimaryWindow>>,
    policy: Option<Res<CursorPolicy>>,
    band: Option<Res<SelectionBand>>,
    minimap_pixels: Option<ResMut<minimap::MinimapPixels>>,
) {
    let Some(hud) = hud else { return };
    let ui = hud.overlay.ui();

    // The minimap exists in commander mode only.
    if ui.get_minimap_visible() != minimap_pixels.is_some() {
        ui.set_minimap_visible(minimap_pixels.is_some());
    }
    if let Some(mut pixels) = minimap_pixels {
        let image = |data: &[[u8; 4]], edge: u32| {
            SlintImage::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(data.as_flattened(), edge, edge))
        };
        if pixels.base_dirty {
            ui.set_minimap_base(image(&pixels.base, minimap::BASE_PX));
            pixels.base_dirty = false;
        }
        if pixels.overlay_dirty {
            ui.set_minimap_overlay(image(&pixels.overlay, minimap::OVERLAY_PX));
            pixels.overlay_dirty = false;
        }
    }

    let grab = cursor.iter().next().map_or(CursorGrabMode::None, |c| c.grab_mode);
    let crosshair = crosshair_visible(grab, policy.is_some_and(|p| p.orbit));
    if ui.get_crosshair_visible() != crosshair {
        ui.set_crosshair_visible(crosshair);
    }

    // Window logical → UI logical pixels.
    match band.and_then(|b| b.0) {
        Some(r) => {
            let (min, size) = (r.min / hud.ui_scale, r.size() / hud.ui_scale);
            if ui.get_band_x() != min.x || ui.get_band_y() != min.y {
                ui.set_band_x(min.x);
                ui.set_band_y(min.y);
            }
            if ui.get_band_width() != size.x || ui.get_band_height() != size.y {
                ui.set_band_width(size.x);
                ui.set_band_height(size.y);
            }
            if !ui.get_band_visible() {
                ui.set_band_visible(true);
            }
        }
        None if ui.get_band_visible() => ui.set_band_visible(false),
        None => {}
    }
}

fn connect_game_ui(ui: &GameUi, world: &mut World) {
    let actions = Rc::new(RefCell::new(Vec::new()));
    let queue = actions.clone();
    ui.on_clear_selection(move || queue.borrow_mut().push(UiAction::ClearSelection));
    let push = |action: UiAction| {
        let queue = actions.clone();
        move || queue.borrow_mut().push(action)
    };
    ui.on_open_map(push(UiAction::OpenMap));
    ui.on_close_map(push(UiAction::CloseMap));
    ui.on_close_menu(push(UiAction::CloseMenu));
    ui.on_play(push(UiAction::Play));
    ui.on_toggle_mode(push(UiAction::ToggleMode));
    let queue = actions.clone();
    ui.on_select_tile(move |col, row| queue.borrow_mut().push(UiAction::SelectTile(col, row)));
    debug_assert_eq!(
        ui.global::<WorldMapConfig>().get_tile_px(),
        worldgen::tile_map::MAP_TILE_PX as i32,
        "the map image's pixels per tile and the map window's must agree"
    );
    debug_assert_eq!(
        ui.global::<WorldMapConfig>().get_grid_px(),
        worldgen::tile_map::MAP_GRID_PX as i32,
        "the map image's grid line and the map window's must agree"
    );
    world.insert_non_send(GameUiActions(actions));
}

fn setup_slint_layer<C: ComponentHandle + 'static>(
    world: &mut World,
    make: fn() -> Result<C, slint_ui::slint::PlatformError>,
    init: fn(&C, &mut World),
    order: i32,
    passthrough: bool,
    ui_scale: f32,
) {
    let overlay = match Overlay::with(make) {
        Ok(overlay) => overlay,
        Err(e) => {
            error!("[slint] {} unavailable: {e}", std::any::type_name::<C>());
            return;
        }
    };
    init(overlay.ui(), world);

    let mut image = Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    image.sampler = ImageSampler::nearest();
    let target = world.resource_mut::<Assets<Image>>().add(image);
    world.get_resource_or_init::<SlintLayerTextures>().0.push((order, target.clone()));
    world.insert_non_send(SlintLayer {
        overlay,
        ui_scale,
        target,
        owns_pointer: false,
        order,
        passthrough,
    });
}

fn pointer_button(button: MouseButton) -> PointerEventButton {
    match button {
        MouseButton::Left => PointerEventButton::Left,
        MouseButton::Right => PointerEventButton::Right,
        MouseButton::Middle => PointerEventButton::Middle,
        MouseButton::Back => PointerEventButton::Back,
        MouseButton::Forward => PointerEventButton::Forward,
        MouseButton::Other(_) => PointerEventButton::Other,
    }
}

/// The text Slint expects for a key: the character, or its private-use code
/// for a named key. `None` for keys the widgets don't use.
fn key_text(key: &Key) -> Option<SharedString> {
    let named = match key {
        Key::Character(s) => return Some(s.as_str().into()),
        Key::Space => SlintKey::Space,
        Key::Enter => SlintKey::Return,
        Key::Tab => SlintKey::Tab,
        Key::Escape => SlintKey::Escape,
        Key::Backspace => SlintKey::Backspace,
        Key::Delete => SlintKey::Delete,
        Key::ArrowLeft => SlintKey::LeftArrow,
        Key::ArrowRight => SlintKey::RightArrow,
        Key::ArrowUp => SlintKey::UpArrow,
        Key::ArrowDown => SlintKey::DownArrow,
        Key::Home => SlintKey::Home,
        Key::End => SlintKey::End,
        Key::PageUp => SlintKey::PageUp,
        Key::PageDown => SlintKey::PageDown,
        Key::Shift => SlintKey::Shift,
        Key::Control => SlintKey::Control,
        Key::Alt => SlintKey::Alt,
        _ => return None,
    };
    Some(named.into())
}

/// Size the overlay to the window, forward this frame's input, and decide
/// who owns the pointer.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn slint_input<C: ComponentHandle + 'static>(
    slint: Option<NonSendMut<SlintLayer<C>>>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut buttons: MessageReader<MouseButtonInput>,
    mut wheel: MessageReader<MouseWheel>,
    mut keys: MessageReader<KeyboardInput>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut pointer: Local<PointerState>,
    mut shared: ResMut<SlintPointer>,
    policy: Option<ResMut<CursorPolicy>>,
) {
    let Some(mut slint) = slint else { return };
    let Ok(window) = window.single() else { return };
    let scale = window.scale_factor() * slint.ui_scale;
    slint.overlay.resize(window.physical_width(), window.physical_height(), scale);
    if slint.passthrough {
        shared.set(slint.order, false, false);
        let owned = shared.any_owner();
        if let Some(mut policy) = policy.filter(|p| p.ui_pointer != owned) {
            policy.ui_pointer = owned;
        }
        return;
    }
    let blocked = shared.blocked_above(slint.order);
    let overlay = &slint.overlay;

    // Window logical → UI logical pixels; physical for the hit test.
    let cursor = window.cursor_position();
    let ui_pos = cursor.map(|c| c / slint.ui_scale);
    if ui_pos != pointer.last {
        overlay.dispatch(match ui_pos {
            Some(p) => WindowEvent::PointerMoved { position: LogicalPosition::new(p.x, p.y) },
            None => WindowEvent::PointerExited,
        });
        pointer.last = ui_pos;
    }
    let drawn_here = cursor.is_some_and(|c| {
        let p = c * window.scale_factor();
        overlay.opaque_at(p.x as u32, p.y as u32)
    });
    // A higher layer under the pointer takes the press and the wheel.
    let over_ui = drawn_here && !blocked;

    for event in buttons.read() {
        let Some(p) = ui_pos else { continue };
        let position = LogicalPosition::new(p.x, p.y);
        let button = pointer_button(event.button);
        match event.state {
            ButtonState::Pressed => {
                if blocked {
                    continue;
                }
                if over_ui {
                    pointer.captured = true;
                }
                overlay.dispatch(WindowEvent::PointerPressed { position, button });
            }
            ButtonState::Released => {
                overlay.dispatch(WindowEvent::PointerReleased { position, button });
            }
        }
    }
    if pointer.captured && mouse.get_pressed().next().is_none() {
        pointer.captured = false;
    }
    for event in wheel.read() {
        let (Some(p), true) = (ui_pos, over_ui) else { continue };
        let px = match event.unit {
            MouseScrollUnit::Line => WHEEL_LINE_PX,
            MouseScrollUnit::Pixel => 1.0,
        };
        overlay.dispatch(WindowEvent::PointerScrolled {
            position: LogicalPosition::new(p.x, p.y),
            delta_x: event.x * px,
            delta_y: event.y * px,
        });
    }
    for event in keys.read() {
        let Some(text) = key_text(&event.logical_key) else { continue };
        overlay.dispatch(match (event.state, event.repeat) {
            (ButtonState::Pressed, false) => WindowEvent::KeyPressed { text },
            (ButtonState::Pressed, true) => WindowEvent::KeyPressRepeated { text },
            (ButtonState::Released, _) => WindowEvent::KeyReleased { text },
        });
    }

    let owns = layer_owns_pointer(false, over_ui, pointer.captured);
    let order = slint.order;
    slint.owns_pointer = owns;
    shared.set(order, drawn_here, owns);
    let owned = shared.any_owner();
    if let Some(mut policy) = policy.filter(|p| p.ui_pointer != owned) {
        policy.ui_pointer = owned;
    }
}

fn toggle_panel(keys: Res<ButtonInput<KeyCode>>, slint: Option<NonSend<SlintLayer<GameUi>>>) {
    let Some(slint) = slint else { return };
    if keys.just_pressed(SLINT_PANEL_KEY) {
        let ui = slint.overlay.ui();
        ui.set_panel_open(!ui.get_panel_open());
    }
}

/// The map tile (column, row) of region `rc`, `None` off the map.
pub fn map_tile(map: &WorldMap, rc: RegionCoords) -> Option<(i32, i32)> {
    map.tile(rc)?;
    Some((rc.x - map.origin.x, rc.z - map.origin.z))
}

/// The region of map tile (`col`, `row`), `None` off the map.
pub fn tile_region(map: &WorldMap, col: i32, row: i32) -> Option<RegionCoords> {
    let rc = RegionCoords::new(map.origin.x + col, map.origin.z + row);
    (col >= 0 && row >= 0 && map.tile(rc).is_some()).then_some(rc)
}

/// The pan (view-relative position of the map's top-left) that puts the
/// centre of tile `index` at the centre of a view `view_len` long.
pub fn centred_pan(view_len: f32, index: i32, tile_len: f32) -> f32 {
    // Whole pixels, like every pan the map window sets itself.
    (view_len / 2.0 - (index as f32 + 0.5) * tile_len).round()
}

/// What the player reads when the server refuses Play.
pub fn refusal_text(why: PlayRefusal) -> &'static str {
    match why {
        PlayRefusal::NoWorldMap => "No world map",
        PlayRefusal::OffMap => "Not on the map",
        PlayRefusal::NoDryLand => "No dry land there",
        PlayRefusal::AlreadyHere => "You are already here",
    }
}

/// Whether a Play pending for `target` is over: refused, or we are there —
/// home in `target` with the camera put over the body inside it, i.e. its
/// region snapshot has loaded (`HudStatus` names the new home as soon as
/// `PlayerRegion` arrives, long before that).
pub fn play_settled(target: RegionCoords, arrived_in: impl Fn(RegionCoords) -> bool, refused: bool) -> bool {
    refused || arrived_in(target)
}

/// The map's status line: loading while a Play is pending, else the last
/// refusal, else nothing.
pub fn map_status_text(pending: Option<RegionCoords>, refused: Option<PlayRefusal>) -> &'static str {
    match (pending, refused) {
        (Some(_), _) => LOADING_TEXT,
        (None, Some(why)) => refusal_text(why),
        (None, None) => "",
    }
}

/// Show `to`; on opening the map, drop a stale refusal and centre the view
/// on the home tile. Keeps `CursorPolicy::ui_modal` in step (its only writer).
/// Takes the `ResMut`s themselves and writes through them only on a real
/// change, so an idle frame does not mark either resource changed (that
/// would make `sync_world_map` compare the whole map every frame).
fn set_screens(
    ui: &GameUi,
    to: Screens,
    world_map: &mut ResMut<WorldMapState>,
    home: Option<RegionCoords>,
    policy: Option<&mut ResMut<CursorPolicy>>,
) {
    let from = Screens::of(ui);
    if to.map && !from.map {
        if world_map.refused.is_some() {
            world_map.refused = None;
        }
        let home_tile = world_map.map.as_ref().zip(home).and_then(|(map, home)| map_tile(map, home));
        if let Some((col, row)) = home_tile {
            let tile_len = ui.global::<WorldMapConfig>().get_tile_px() as f32 * ui.get_map_zoom();
            ui.set_map_pan_x(centred_pan(ui.get_map_view_width(), col, tile_len));
            ui.set_map_pan_y(centred_pan(ui.get_map_view_height(), row, tile_len));
        }
    }
    if from.menu != to.menu {
        ui.set_menu_open(to.menu);
    }
    if from.map != to.map {
        ui.set_map_open(to.map);
    }
    if from.wheel != to.wheel {
        ui.set_wheel_open(to.wheel);
    }
    if let Some(policy) = policy.filter(|p| p.ui_modal != to.any()) {
        policy.ui_modal = to.any();
    }
}

/// [`MENU_KEY`] and [`MAP_KEY`]. Esc with a designation in progress (and no
/// screen open) is left to `designate_keys`, which cancels it; otherwise it
/// is consumed here, so `designate_keys` never sees the same press.
fn menu_keys(
    mut keys: ResMut<ButtonInput<KeyCode>>,
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    designate: Option<Res<DesignateState>>,
    mut policy: Option<ResMut<CursorPolicy>>,
    mut world_map: ResMut<WorldMapState>,
    status: Res<HudStatus>,
) {
    let Some(slint) = slint else { return };
    // The dev panel holds the keyboard.
    if policy.as_ref().is_some_and(|p| p.overlay_open) {
        return;
    }
    let ui = slint.overlay.ui();
    let mut to = Screens::of(ui);
    if keys.just_pressed(MENU_KEY) {
        let designating = designate.as_ref().is_some_and(|d| d.in_progress());
        if to.any() || !designating {
            to = to.escape();
            keys.clear_just_pressed(MENU_KEY);
        }
    }
    if keys.just_pressed(MAP_KEY) {
        to = to.toggle_map();
    }
    set_screens(ui, to, &mut world_map, status.home_region, policy.as_mut());
}

#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn apply_ui_actions(
    actions: Option<NonSend<GameUiActions>>,
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    mut selection: Option<ResMut<Selection>>,
    mut world_map: ResMut<WorldMapState>,
    mut pending: ResMut<PlayPending>,
    mut policy: Option<ResMut<CursorPolicy>>,
    requests: Option<Res<ClientRequests>>,
    status: Res<HudStatus>,
    mut mode_toggles: Option<ResMut<Messages<ToggleModeRequest>>>,
) {
    let (Some(actions), Some(slint)) = (actions, slint) else { return };
    let ui = slint.overlay.ui();
    for action in actions.0.borrow_mut().drain(..) {
        let mut to = Screens::of(ui);
        match action {
            UiAction::ClearSelection => {
                if let Some(selection) = selection.as_mut() {
                    selection.0.clear();
                }
            }
            UiAction::ToggleMode => {
                if let Some(toggles) = mode_toggles.as_mut() {
                    toggles.write(ToggleModeRequest);
                }
            }
            UiAction::OpenMap => to = Screens::open_map(),
            UiAction::CloseMap => to.map = false,
            UiAction::CloseMenu => to.menu = false,
            // The selection lives in Slint; `push_ui_state` follows it.
            UiAction::SelectTile(..) => {}
            UiAction::Play => {
                // One request at a time: Play is disabled while one is pending.
                if pending.0.is_some() {
                    continue;
                }
                let home = status.home_region;
                let target = world_map.map.as_ref().and_then(|map| {
                    let rc = tile_region(map, ui.get_selected_col(), ui.get_selected_row())?;
                    play_blocker(map, rc, home).is_none().then_some(rc)
                });
                let Some(rc) = target else { continue };
                let sent = requests.as_ref().is_some_and(|r| r.0.send(ClientRequest::PlayRegion(rc)).is_ok());
                if !sent {
                    warn!("[world-map] Play request for ({}, {}) not sent: no game thread", rc.x, rc.z);
                    continue;
                }
                pending.0 = Some(rc);
                world_map.refused = None;
                to.map = false;
            }
        }
        set_screens(ui, to, &mut world_map, status.home_region, policy.as_mut());
    }
}

/// A selected entity kind as the panel lists it.
fn kind_label(kind: game::EntityKind) -> String {
    match kind {
        game::EntityKind::Player => "Player".into(),
        game::EntityKind::Slime { size } => format!("Slime (size {size})"),
    }
}

/// "Slime (size 2) ×3" per kind, sorted, from what the selection holds.
fn selection_lines(kinds: impl IntoIterator<Item = game::EntityKind>) -> Vec<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for kind in kinds {
        *counts.entry(kind_label(kind)).or_default() += 1;
    }
    counts.into_iter().map(|(label, n)| format!("{label} ×{n}")).collect()
}

/// What a tool wheel wedge asked for, applied by [`apply_toolbar_action`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarAction {
    SelectTool(Option<Tool>),
    /// The "Hide above" wedge: flip the cutaway, as `C` does.
    ToggleCut,
}

/// Index `index` in [`TOOL_KEYS`] order; -1 (or any index without a tool)
/// puts the tool down.
pub fn tool_action(index: i32) -> ToolbarAction {
    let tool = usize::try_from(index).ok().and_then(|i| TOOL_KEYS.get(i)).map(|(_, tool)| *tool);
    ToolbarAction::SelectTool(tool)
}

/// The tool wheel's wedge `index`: the tools in [`TOOL_KEYS`] order (the
/// active one's wedge puts it down), then "Hide above".
pub fn wheel_action(index: usize, active: Option<Tool>) -> Option<ToolbarAction> {
    match TOOL_KEYS.get(index) {
        Some((_, tool)) if active == Some(*tool) => Some(ToolbarAction::SelectTool(None)),
        Some((_, tool)) => Some(ToolbarAction::SelectTool(Some(*tool))),
        None if index == TOOL_KEYS.len() => Some(ToolbarAction::ToggleCut),
        None => None,
    }
}

/// The wheel's wedge captions, in [`wheel_action`] order; the cut's wedge
/// says what picking it does. Hotkeys are left off: the wedges are narrow,
/// and the keys still work.
pub fn wheel_labels(cut: bool) -> Vec<String> {
    let cut = if cut { "Show above" } else { "Hide above" }.to_string();
    TOOL_KEYS.iter().map(|(_, tool)| tool.label().to_string()).chain([cut]).collect()
}

/// Apply one wedge pick. Tools are picked only with a home region
/// (`available`), as with the hotkeys; putting the tool down always works.
pub fn apply_toolbar_action(
    action: ToolbarAction,
    available: bool,
    state: &mut DesignateState,
    level: Option<&mut WorkingLevel>,
) {
    match action {
        ToolbarAction::SelectTool(tool) if available || tool.is_none() => state.set_tool(tool),
        ToolbarAction::SelectTool(_) => {}
        ToolbarAction::ToggleCut => {
            if let Some(level) = level {
                level.cut = !level.cut;
            }
        }
    }
}

/// The wheel index of `tool`, -1 for none.
fn tool_index(tool: Option<Tool>) -> i32 {
    tool.and_then(|t| TOOL_KEYS.iter().position(|(_, k)| *k == t)).map_or(-1, |i| i as i32)
}

/// [`WHEEL_KEY`] held: the tool wheel, which replaced the bottom tool bar.
/// [`slint_ui::wheel::step`] decides when it opens, picks and closes; this
/// feeds it the key and mouse, puts the wheel where it opened, applies the
/// pick, and keeps it a modal [`Screens`] entry. The clicks it uses are
/// consumed, so the commander never sees them.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn tool_wheel(
    mut buttons: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    window: Query<&Window, With<PrimaryWindow>>,
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    mut designate: Option<ResMut<DesignateState>>,
    mut level: Option<ResMut<WorkingLevel>>,
    mirror: Option<Res<TileMirror>>,
    mode: Option<Res<ClientPlayMode>>,
    mut policy: Option<ResMut<CursorPolicy>>,
    mut world_map: ResMut<WorldMapState>,
    status: Res<HudStatus>,
    mut phase: Local<WheelPhase>,
) {
    let Some(slint) = slint else { return };
    let ui = slint.overlay.ui();
    // Esc or the map closed it.
    if !ui.get_wheel_open() {
        *phase = WheelPhase::Closed;
    }
    let Ok(window) = window.single() else { return };
    // Commander mode with a home region and not in Adventure, as for the
    // tool hotkeys.
    let available = designate.is_some()
        && mirror.as_ref().is_some_and(|m| m.region().is_some())
        && !mode.as_ref().is_some_and(|m| m.adventure());
    let free = policy.as_ref().is_none_or(|p| !p.blocks_game() && !p.ui_pointer);
    let cursor = window.cursor_position();
    let input = WheelInput {
        open_press: keys.just_pressed(WHEEL_KEY) && available && free && window.focused && cursor.is_some(),
        hold_press: keys.just_pressed(WHEEL_KEY),
        // A release lost with the focus counts as one.
        hold_release: keys.just_released(WHEEL_KEY) || (*phase == WheelPhase::Held && !keys.pressed(WHEEL_KEY)),
        cancel_press: buttons.just_pressed(MouseButton::Right),
        left_release: buttons.just_released(MouseButton::Left),
        hovered: ui.get_wheel_hovered(),
    };
    let (next, picked) = wheel::step(*phase, input);
    if next == *phase && !next.is_open() {
        return;
    }
    if *phase == WheelPhase::Closed {
        if let Some(at) = cursor.map(|c| c / slint.ui_scale) {
            ui.set_wheel_x(at.x);
            ui.set_wheel_y(at.y);
        }
    }
    // The clicks the open wheel took stay out of the commander.
    if phase.is_open() {
        if input.cancel_press {
            buttons.clear_just_pressed(MouseButton::Right);
        }
        if input.left_release {
            buttons.clear_just_released(MouseButton::Left);
        }
    }
    if let (Some(i), Some(state)) = (picked, designate.as_mut()) {
        if let Some(action) = wheel_action(i, state.tool) {
            apply_toolbar_action(action, available, state, level.as_deref_mut());
        }
    }
    *phase = next;
    let to = Screens { wheel: next.is_open(), ..Screens::of(ui) };
    set_screens(ui, to, &mut world_map, status.home_region, policy.as_mut());
}

/// The level readout: the working level and how far it is from the surface
/// under the camera focus (just the level while that column isn't loaded).
pub fn level_text(level: i32, surface_level: Option<i32>) -> String {
    match surface_level.map(|surface| level - surface) {
        None => format!("Level {level}"),
        Some(0) => format!("Level {level} · surface"),
        Some(d) if d < 0 => format!("Level {level} · {} below surface", -d),
        Some(d) => format!("Level {level} · {d} above surface"),
    }
}

/// What the tool wheel's state and the readouts read. All optional: the
/// commander resources exist only in commander mode.
#[derive(SystemParam)]
struct ToolbarParams<'w, 's> {
    designate: Option<ResMut<'w, DesignateState>>,
    readout: Option<Res<'w, DragReadout>>,
    level: Option<ResMut<'w, WorkingLevel>>,
    grid: Option<Res<'w, HeightGrid>>,
    cam: Option<Res<'w, CommanderCamera>>,
    mirror: Option<Res<'w, TileMirror>>,
    mode: Option<Res<'w, ClientPlayMode>>,
    window: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    /// The cut the wheel's items were last captioned for; `None` before
    /// the first set.
    wheel_cut: Local<'s, Option<bool>>,
}

/// The tool, priority, cut (the wheel's captions), level and drag readout
/// → the UI, each only when changed.
fn sync_toolbar(ui: &slint_ui::GameUi, ui_scale: f32, p: &mut ToolbarParams) {
    // No home region (not joined yet) or Adventure: no tools, as with the
    // hotkeys.
    let adventure = p.mode.as_ref().is_some_and(|m| m.adventure());
    if ui.get_adventure() != adventure {
        ui.set_adventure(adventure);
    }
    let available = p.mirror.as_ref().is_some_and(|m| m.region().is_some()) && !adventure;
    if ui.get_tools_available() != available {
        ui.set_tools_available(available);
    }
    // The cut is on by default; without a working level it shows as on.
    let cut = p.level.as_ref().is_none_or(|l| l.cut);
    if *p.wheel_cut != Some(cut) {
        let items: Vec<slint_ui::RadialItem> = wheel_labels(cut)
            .into_iter()
            .map(|text| slint_ui::RadialItem { text: text.into(), enabled: true, ..Default::default() })
            .collect();
        ui.set_wheel_items(ModelRc::new(VecModel::from(items)));
        *p.wheel_cut = Some(cut);
    }
    let Some(state) = p.designate.as_mut() else { return };
    let active = tool_index(state.tool);
    if ui.get_active_tool() != active {
        ui.set_active_tool(active);
    }
    if ui.get_priority() != state.priority as i32 {
        ui.set_priority(state.priority as i32);
    }

    let surface = match (&p.grid, &p.cam) {
        (Some(grid), Some(cam)) => focus_surface(grid, cam.focus).map(surface_level),
        _ => None,
    };
    let level: SharedString =
        p.level.as_ref().and_then(|l| l.level).map_or(String::new(), |l| level_text(l, surface)).into();
    if ui.get_level_text() != level {
        ui.set_level_text(level);
    }

    let drag = p.readout.as_ref().map_or("", |r| r.0.as_str());
    if ui.get_drag_text() != drag {
        ui.set_drag_text(drag.into());
    }
    if drag.is_empty() {
        return;
    }
    let Some(cursor) = p.window.single().ok().and_then(|w| w.cursor_position()) else { return };
    let at = cursor / ui_scale;
    if ui.get_drag_x() != at.x || ui.get_drag_y() != at.y {
        ui.set_drag_x(at.x);
        ui.set_drag_y(at.y);
    }
}

/// The map as last pushed to the UI.
#[derive(Default)]
struct MapShown {
    map: Option<WorldMap>,
    /// Bumped whenever `map` is replaced.
    generation: u64,
    /// (generation, selected column, selected row, home, Play pending) the
    /// tile info was last computed for.
    info_for: Option<(u64, i32, i32, Option<RegionCoords>, bool)>,
}

/// What the world map window reads.
#[derive(SystemParam)]
struct MapParams<'w, 's> {
    world_map: Res<'w, WorldMapState>,
    time: Res<'w, Time>,
    pending: ResMut<'w, PlayPending>,
    cam: Option<Res<'w, CommanderCamera>>,
    shown: Local<'s, MapShown>,
}

/// The map picture, home and selected tile, tile info, Play state and
/// status line → the UI, each only when changed.
fn sync_world_map(ui: &GameUi, home: Option<RegionCoords>, p: &mut MapParams) {
    if p.world_map.is_changed() && p.shown.map.as_ref() != p.world_map.map.as_ref() {
        match &p.world_map.map {
            Some(map) => {
                let (width, height, pixels) = rasterize(map);
                let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(pixels.as_flattened(), width, height);
                ui.set_map_image(SlintImage::from_rgba8(buffer));
                ui.set_map_cols(map.width as i32);
                ui.set_map_rows(map.height as i32);
            }
            None => ui.set_map_image(SlintImage::default()),
        }
        ui.set_map_available(p.world_map.map.is_some());
        p.shown.map = p.world_map.map.clone();
        p.shown.generation += 1;
    }
    let map = p.world_map.map.as_ref();

    let (home_col, home_row) = map.zip(home).and_then(|(map, home)| map_tile(map, home)).unwrap_or((-1, -1));
    if ui.get_home_col() != home_col || ui.get_home_row() != home_row {
        ui.set_home_col(home_col);
        ui.set_home_row(home_row);
    }

    // Pending until we are in the target with its region loaded, or the
    // server refused it. Without a commander camera, being home has to do.
    if let Some(target) = p.pending.0 {
        let refused = p.world_map.refused.is_some_and(|(rc, _)| rc == target);
        let cam = p.cam.as_deref();
        let arrived_in = |rc| cam.map_or(home == Some(rc), |cam| cam.placed_in(rc));
        if play_settled(target, arrived_in, refused) {
            p.pending.0 = None;
        }
    }

    let (col, row) = (ui.get_selected_col(), ui.get_selected_row());
    let pending = p.pending.0.is_some();
    let info_for = Some((p.shown.generation, col, row, home, pending));
    if p.shown.info_for != info_for {
        p.shown.info_for = info_for;
        let picked = map.and_then(|map| Some((map, tile_region(map, col, row)?)));
        let lines: Vec<SharedString> = picked
            .map_or_else(Vec::new, |(map, rc)| tile_info_lines(map, rc))
            .into_iter()
            .map(Into::into)
            .collect();
        ui.set_tile_lines(ModelRc::new(VecModel::from(lines)));
        let blocker = picked.map(|(map, rc)| play_blocker(map, rc, home));
        ui.set_play_enabled(!pending && matches!(blocker, Some(None)));
        ui.set_play_reason(blocker.flatten().unwrap_or("").into());
    }

    // In the map window and, with the map closed (Play closes it), on the
    // game-level status line.
    let status = map_status_text(p.pending.0, p.world_map.visible_refusal(p.time.elapsed_secs()));
    if ui.get_map_status() != status {
        ui.set_map_status(status.into());
    }
    if ui.get_game_status() != status {
        ui.set_game_status(status.into());
    }
}

/// Client state → panel properties, only when it changed (every set marks
/// the UI dirty and costs a repaint).
fn push_ui_state(
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    status: Res<HudStatus>,
    selection: Option<Res<Selection>>,
    entities: Query<(&SimEntity, &SimKind)>,
    mut toolbar: ToolbarParams,
    mut map: MapParams,
) {
    let Some(slint) = slint else { return };
    let ui = slint.overlay.ui();
    sync_toolbar(ui, slint.ui_scale, &mut toolbar);
    sync_world_map(ui, status.home_region, &mut map);
    if status.is_changed() {
        ui.set_home_region(
            status.home_region.map_or("--".into(), |r| format!("({}, {})", r.x, r.z)).into(),
        );
        ui.set_connection(format!("{:?}", status.connection).into());
    }
    let Some(selection) = selection.filter(|s| s.is_changed()) else { return };
    let kinds = entities
        .iter()
        .filter(|(e, _)| selection.contains(e.region, e.key))
        .map(|(_, kind)| kind.0);
    let lines: Vec<SharedString> = selection_lines(kinds).into_iter().map(Into::into).collect();
    ui.set_selected_count(selection.0.len() as i32);
    ui.set_selection_lines(ModelRc::new(VecModel::from(lines)));
}

/// Redraw what changed and copy that rect into the texture, un-premultiplied
/// (the composite pass blends straight alpha, as `bevy_ui` did). The texture follows the window size.
fn render_slint_layer<C: ComponentHandle + 'static>(
    slint: Option<NonSendMut<SlintLayer<C>>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(mut slint) = slint else { return };
    let Some(dirty) = slint.overlay.render() else { return };
    let Some(mut image) = images.get_mut(&slint.target) else { return };
    let (width, height) = slint.overlay.size();
    let size = Extent3d { width, height, depth_or_array_layers: 1 };
    if image.texture_descriptor.size != size {
        image.resize(size);
    }
    let Some(data) = image.data.as_mut() else { return };
    let pixels = slint.overlay.pixels();
    for y in dirty.y..dirty.y + dirty.height {
        let row = (y * width) as usize;
        for x in dirty.x..dirty.x + dirty.width {
            let i = row + x as usize;
            data[i * 4..i * 4 + 4].copy_from_slice(&unpremultiply(pixels[i]));
        }
    }
}

fn unpremultiply(p: slint_ui::slint::platform::software_renderer::PremultipliedRgbaColor) -> [u8; 4] {
    let a = p.alpha as u16;
    if a == 0 {
        return [0; 4];
    }
    let un = |c: u8| ((c as u16 * 255 + a / 2) / a).min(255) as u8;
    [un(p.red), un(p.green), un(p.blue), p.alpha]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Startup systems may use what a layer's `with_init` inserted (the asset
    /// viewer's `scan_at_start` reads its `Actions`): layers are set up first.
    #[test]
    fn layer_init_runs_before_startup_systems() {
        struct Marker;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default(), bevy::input::InputPlugin))
            .init_asset::<Image>()
            .add_plugins(SlintLayerPlugin::new(HudUi::new).with_init(|_, world| world.insert_non_send(Marker)));
        // Registered after the plugin, as an app's own Startup systems are.
        app.add_systems(Startup, |_marker: NonSend<Marker>| {});
        app.update();
        assert!(app.world().get_non_send::<SlintLayer<HudUi>>().is_some());
    }

    /// The HUD's sizes are shared with Bevy-side hit rects in window pixels
    /// (the minimap), so it ignores `VOXEL_UI_SCALE`.
    #[test]
    fn hud_layer_ignores_the_ui_scale_knob() {
        std::env::set_var("VOXEL_UI_SCALE", "2");
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default(), bevy::input::InputPlugin))
            .init_asset::<Image>()
            .add_plugins(HudLayerPlugin);
        app.update();
        std::env::remove_var("VOXEL_UI_SCALE");
        assert_eq!(app.world().get_non_send::<SlintLayer<HudUi>>().expect("hud layer").ui_scale, 1.0);
    }

    #[test]
    fn passthrough_layer_never_owns_the_pointer() {
        assert!(!layer_owns_pointer(true, true, true));
        assert!(layer_owns_pointer(false, true, false));
        assert!(layer_owns_pointer(false, false, true)); // a press that started on the UI
        assert!(!layer_owns_pointer(false, false, false));
    }

    #[test]
    fn layer_orders_stack_hud_game_dev() {
        assert!(HUD_LAYER_ORDER < GAME_LAYER_ORDER);
    }

    #[test]
    fn a_higher_layer_under_the_pointer_takes_the_press() {
        let mut p = SlintPointer::default();
        p.set(GAME_LAYER_ORDER, true, true);
        p.set(HUD_LAYER_ORDER, true, false);
        assert!(p.blocked_above(HUD_LAYER_ORDER));
        assert!(!p.blocked_above(GAME_LAYER_ORDER));
        assert!(p.any_owner());
        p.set(GAME_LAYER_ORDER, false, false);
        assert!(!p.blocked_above(HUD_LAYER_ORDER));
        assert!(!p.any_owner(), "a pass-through layer never owns the pointer");
    }
    use crate::renderer::world_map::REFUSAL_SHOW_SECS;
    use slint_ui::slint::platform::software_renderer::PremultipliedRgbaColor;

    #[test]
    fn escape_closes_the_map_first_then_toggles_the_menu() {
        let none = Screens::default();
        let menu = Screens { menu: true, ..Default::default() };
        let map = Screens { map: true, ..Default::default() };
        assert_eq!(none.escape(), menu);
        assert_eq!(menu.escape(), none);
        assert_eq!(map.escape(), none);
        assert_eq!(none.toggle_map(), map);
        assert_eq!(menu.toggle_map(), map, "opening the map closes the menu");
        assert_eq!(map.toggle_map(), none);
    }

    #[test]
    fn escape_and_the_map_close_the_tool_wheel_first() {
        let wheel = Screens { wheel: true, ..Default::default() };
        assert!(wheel.any(), "an open wheel holds the game like a screen");
        assert_eq!(wheel.escape(), Screens::default(), "Esc only closes the wheel");
        assert_eq!(wheel.toggle_map(), Screens::open_map());
        assert!(!Screens::open_map().wheel);
    }

    #[test]
    fn status_text_is_loading_while_pending_else_the_refusal() {
        let rc = RegionCoords::new(1, 2);
        assert_eq!(map_status_text(Some(rc), Some(PlayRefusal::OffMap)), LOADING_TEXT);
        assert_eq!(map_status_text(None, Some(PlayRefusal::NoDryLand)), "No dry land there");
        assert_eq!(map_status_text(None, None), "");
    }

    #[test]
    fn play_settles_on_arrival_with_the_region_loaded_or_a_refusal() {
        let target = RegionCoords::new(1, 2);
        let rect = super::super::commander::region_rect(target);
        let mut cam = CommanderCamera { bounds: Some(rect), placed: false, ..Default::default() };
        // Home is the target (PlayerRegion arrived) but the snapshot has not.
        assert!(!play_settled(target, |rc| cam.placed_in(rc), false));
        assert!(play_settled(target, |rc| cam.placed_in(rc), true), "a refusal ends it");
        cam.placed = true;
        assert!(play_settled(target, |rc| cam.placed_in(rc), false));
        assert!(!play_settled(RegionCoords::new(0, 0), |rc| cam.placed_in(rc), false));
    }

    #[test]
    fn a_refusal_expires_after_the_show_time() {
        let mut state = WorldMapState::default();
        state.refused = Some((RegionCoords::new(1, 2), PlayRefusal::OffMap));
        state.refused_at_secs = 10.0;
        assert_eq!(state.visible_refusal(10.0 + REFUSAL_SHOW_SECS - 0.1), Some(PlayRefusal::OffMap));
        assert_eq!(state.visible_refusal(10.0 + REFUSAL_SHOW_SECS), None);
    }

    #[test]
    fn centred_pan_puts_the_tile_centre_mid_view() {
        assert_eq!(centred_pan(100.0, 2, 10.0), 25.0);
        assert_eq!(centred_pan(100.0, 0, 10.0), 45.0);
    }

    #[test]
    fn tile_region_and_map_tile_round_trip_and_reject_off_map() {
        let tile = game::TileSummary {
            ground_min_m: 0.0,
            ground_mean_m: 0.0,
            ground_max_m: 0.0,
            relief_m: 0.0,
            slope: [0.0; 2],
            sea_fraction: 0.0,
            lake_fraction: 0.0,
            lake_level_m: None,
            rivers: 0,
            river_max_discharge_m2: 0.0,
            river_max_width_m: 0.0,
            surface_rock: None,
            landing: None,
        };
        let map = WorldMap {
            origin: RegionCoords::new(-2, 3),
            width: 3,
            height: 2,
            tiles: vec![tile; 6],
            rock_names: Vec::new(),
        };
        assert_eq!(tile_region(&map, 0, 0), Some(RegionCoords::new(-2, 3)));
        assert_eq!(tile_region(&map, 2, 1), Some(RegionCoords::new(0, 4)));
        assert_eq!(map_tile(&map, RegionCoords::new(0, 4)), Some((2, 1)));
        for (col, row) in [(-1, 0), (0, -1), (-1, -1), (3, 0), (0, 2)] {
            assert_eq!(tile_region(&map, col, row), None, "({col}, {row}) is off the map");
        }
        for rc in [RegionCoords::new(-3, 3), RegionCoords::new(1, 3), RegionCoords::new(-2, 5)] {
            assert_eq!(map_tile(&map, rc), None);
        }
    }

    #[test]
    fn selection_lines_count_each_kind() {
        let lines = selection_lines([
            game::EntityKind::Slime { size: 2 },
            game::EntityKind::Slime { size: 1 },
            game::EntityKind::Slime { size: 2 },
        ]);
        assert_eq!(lines, ["Slime (size 1) ×1", "Slime (size 2) ×2"]);
    }

    #[test]
    fn unpremultiply_restores_straight_alpha() {
        let p = |red, green, blue, alpha| PremultipliedRgbaColor { red, green, blue, alpha };
        assert_eq!(unpremultiply(p(0, 0, 0, 0)), [0, 0, 0, 0]);
        assert_eq!(unpremultiply(p(10, 20, 30, 255)), [10, 20, 30, 255]);
        assert_eq!(unpremultiply(p(64, 0, 128, 128)), [128, 0, 255, 128]);
    }

    #[test]
    fn level_text_above_and_below_surface() {
        assert_eq!(level_text(64, Some(70)), "Level 64 · 6 below surface");
        assert_eq!(level_text(72, Some(70)), "Level 72 · 2 above surface");
        assert_eq!(level_text(70, Some(70)), "Level 70 · surface");
        assert_eq!(level_text(70, None), "Level 70");
    }

    #[test]
    fn ui_actions_map_tool_indices() {
        assert_eq!(tool_action(2), ToolbarAction::SelectTool(Some(Tool::Build)));
        assert_eq!(tool_action(-1), ToolbarAction::SelectTool(None));
        assert_eq!(tool_action(TOOL_KEYS.len() as i32), ToolbarAction::SelectTool(None));
        for (i, (_, tool)) in TOOL_KEYS.iter().enumerate() {
            assert_eq!(tool_index(Some(*tool)), i as i32);
        }
        assert_eq!(tool_index(None), -1);
    }

    #[test]
    fn wheel_wedges_pick_tools_then_the_cut() {
        assert_eq!(wheel_action(2, None), Some(ToolbarAction::SelectTool(Some(Tool::Build))));
        // The active tool's wedge puts it down, as its toggled button did.
        assert_eq!(wheel_action(2, Some(Tool::Build)), Some(ToolbarAction::SelectTool(None)));
        assert_eq!(wheel_action(TOOL_KEYS.len(), None), Some(ToolbarAction::ToggleCut));
        assert_eq!(wheel_action(TOOL_KEYS.len() + 1, None), None);
        let labels = wheel_labels(true);
        assert_eq!(labels.len(), TOOL_KEYS.len() + 1);
        assert_eq!(labels[0], "Dig");
        // The cut's wedge names what clicking it does.
        assert_eq!(labels[TOOL_KEYS.len()], "Show above");
        assert_eq!(wheel_labels(false)[TOOL_KEYS.len()], "Hide above");
    }

    #[test]
    fn cut_button_flips_the_cut_like_c() {
        let mut state = DesignateState::default();
        let mut level = WorkingLevel::default();
        assert!(level.cut, "hiding what is above is the default");
        apply_toolbar_action(ToolbarAction::ToggleCut, true, &mut state, Some(&mut level));
        assert!(!level.cut);
        apply_toolbar_action(ToolbarAction::ToggleCut, true, &mut state, Some(&mut level));
        assert!(level.cut);
        // Not in commander mode: nothing to flip, no panic.
        apply_toolbar_action(ToolbarAction::ToggleCut, true, &mut state, None);
    }

    #[test]
    fn tool_buttons_select_and_clear_the_box() {
        let mut state = DesignateState::default();
        apply_toolbar_action(tool_action(2), false, &mut state, None);
        assert_eq!(state.tool, None, "no tools without a home region");
        apply_toolbar_action(tool_action(2), true, &mut state, None);
        assert_eq!(state.tool, Some(Tool::Build));
        let drag = crate::renderer::commander::designate::Drag { anchor: [0; 3], corner: [0; 3], erase: false };
        state.drag = Some(drag);
        apply_toolbar_action(tool_action(0), true, &mut state, None);
        assert_eq!((state.tool, state.drag), (Some(Tool::Dig), None));
        apply_toolbar_action(tool_action(-1), false, &mut state, None);
        assert_eq!(state.tool, None, "putting the tool down always works");
    }
}
