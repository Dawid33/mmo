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
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::{MouseButtonInput, MouseScrollUnit, MouseWheel};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use game::{RegionCoords, WorldMap};
use slint_ui::slint::platform::{Key as SlintKey, PointerEventButton, WindowEvent};
use slint_ui::slint::{
    ComponentHandle, Image as SlintImage, LogicalPosition, ModelRc, Rgba8Pixel, SharedPixelBuffer,
    SharedString, VecModel,
};
use slint_ui::wheel::{self, WheelInput, WheelPhase};
use slint_ui::{GameUi, HudConfig, HudUi, Overlay};
use worldgen::tile_map::tile_info_lines;

use super::ClientRequests;
use super::arrival_tabs;
use super::bridge::{LocalBody, SimEntity, SimKind};
use super::commander::CommanderCamera;
use super::commander::adventure::{ClientPlayMode, EditorState};
use super::commander::designate::{DesignateState, DragReadout, TOOL_KEYS, Tool};
use super::commander::follow::{Follow, FollowTarget};
use super::commander::height::HeightGrid;
use super::commander::level::{WorkingLevel, focus_surface, surface_level};
use super::commander::minimap;
use super::commander::selection::{Selection, SelectionBand};
use super::commander::tiles::TileMirror;
use super::cursor::CursorPolicy;
use super::history_view::{self, ArrivalTab, HistoryView};
use super::hud::{HudStatus, crosshair_visible};
use super::slint_composite::{SlintCompositePlugin, SlintLayerTextures};
use super::world_map::WorldMapState;
use crate::instance::ClientRequest;
use crate::session::{
    AppState, InPlay, SessionResetExt, arrival_map_shown, frontend_owns, world_systems_enabled,
};

/// Closes the world map if it is open, else opens / closes the menu —
/// unless a designation is in progress, which it cancels instead.
pub const MENU_KEY: KeyCode = KeyCode::Escape;
/// Opens the inventory on its Map tab (or selects Map if it is already open).
pub const MAP_KEY: KeyCode = KeyCode::KeyM;
/// Held, opens the tool wheel at the pointer ([`tool_wheel`]).
pub const WHEEL_KEY: KeyCode = KeyCode::KeyQ;
/// UI pixels per window logical pixel, before the player's setting
/// ([`OverlayScale`]). The widgets are pixel-exact at a total scale (the UI
/// scale × the window's scale factor) of 1 and 2: their sprite sheets are
/// drawn texel for pixel there and resampled in between (layout still lands
/// on whole physical pixels at any scale). 1.5 is the default, and what the
/// Settings window calls 100 % (owner, 2026-11-19).
pub const UI_SCALE: f32 = 1.5;
/// The interface-scale setting's range and step (`graphics_settings`), in
/// this unit: a quarter at a time, so the two sharp scales, 1 and 2, are
/// stops (shown as 67 % and 133 %).
pub const UI_SCALE_MIN: f32 = 0.75;
pub const UI_SCALE_MAX: f32 = 3.0;
pub const UI_SCALE_STEP: f32 = 0.25;
/// The least room (UI pixels) a scale above 100 % must leave the interface:
/// the Settings window with its margin, so a scale too large for the window
/// can always be turned down again ([`fitted_ui_scale`]).
pub const UI_FIT_WIDTH_PX: f32 = 640.0;
pub const UI_FIT_HEIGHT_PX: f32 = 400.0;

/// The UI scale asked for: UI pixels per window logical pixel, every layer's.
/// Starts as `VOXEL_UI_SCALE` (else [`UI_SCALE`]); the Settings window's
/// "Interface scale" writes it (`graphics_settings::apply_ui_scale`). What a
/// layer draws with is this fitted to the window ([`fitted_ui_scale`]).
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct OverlayScale(pub f32);

/// `wish` lowered, a step at a time and never below 1, until the window
/// (`physical` pixels at `window_scale` physical per logical) leaves the UI
/// [`UI_FIT_WIDTH_PX`] × [`UI_FIT_HEIGHT_PX`] of its own pixels.
pub fn fitted_ui_scale(wish: f32, physical: (u32, u32), window_scale: f32) -> f32 {
    if wish <= 1.0 || window_scale <= 0.0 {
        return wish;
    }
    let fit = (physical.0 as f32 / (UI_FIT_WIDTH_PX * window_scale))
        .min(physical.1 as f32 / (UI_FIT_HEIGHT_PX * window_scale));
    let cap = ((fit / UI_SCALE_STEP).floor() * UI_SCALE_STEP).max(1.0);
    wish.min(cap)
}
/// Logical pixels one wheel notch scrolls.
pub const WHEEL_LINE_PX: f32 = 40.0;
/// Draw order of the Slint layers (higher on top). The interactive game UI
/// (tool bar, menu, map) sits above the pass-through HUD.
pub const GAME_LAYER_ORDER: i32 = 1_000;
/// The pass-through HUD (crosshair, selection band, minimap).
pub const HUD_LAYER_ORDER: i32 = 0;
/// The frontend (menu screens, starting screen) above the game UI.
pub const FRONTEND_LAYER_ORDER: i32 = 1_500;

/// When a layer gives way to the frontend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrontendHide {
    /// Always drawn (the frontend itself).
    #[default]
    Never,
    /// Hidden while the frontend owns the screen (the HUD), and under the open full world map, which is
    /// drawn beneath every layer (`renderer/map`): the HUD would show through its view.
    Always,
    /// Hidden while the frontend owns the screen, except over the arrival map
    /// ([`arrival_map_shown`]): the game UI, whose world map is the start screen.
    ExceptArrivalMap,
}

/// Whether a layer with `mode` draws nothing and takes no input right now.
/// Without the session state (headless tests, examples) the frontend never
/// owns the screen.
fn frontend_hides(
    mode: FrontendHide,
    state: Option<&State<AppState>>,
    in_play: Option<&InPlay>,
    world_map: Option<&WorldMapState>,
    full_map_open: bool,
) -> bool {
    if mode == FrontendHide::Always && full_map_open {
        return true;
    }
    let Some(state) = state.map(|s| *s.get()) else {
        return false;
    };
    let in_play = in_play.is_some_and(|e| e.0);
    let map_arrived = world_map.is_some_and(|m| m.map.is_some());
    match mode {
        FrontendHide::Never => false,
        FrontendHide::Always => frontend_owns(state, in_play),
        FrontendHide::ExceptArrivalMap => {
            frontend_owns(state, in_play) && !arrival_map_shown(state, in_play, map_arrived)
        }
    }
}

/// `VOXEL_UI_SCALE`, if set to a positive number: the UI scale at start,
/// whatever the saved setting says (a slider moved afterwards still applies).
pub fn ui_scale_pinned_by_env() -> Option<f32> {
    std::env::var("VOXEL_UI_SCALE")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|s| *s > 0.0)
}

/// What a Slint callback asked the client to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    ClearSelection,
    /// The menu's World Map button.
    OpenMap,
    CloseMap,
    CloseMenu,
    /// The menu's Main menu button: leave the world (`session` flow).
    MainMenu,
    /// A map tile clicked (column, row); Slint has already selected it.
    SelectTile(i32, i32),
    /// Follow the selected entity, or stop following.
    ToggleFollow,
    /// The context box's "3D view" button: leave the top-down view.
    ToggleTopDown,
    /// The menu column's Settings / Encyclopedia buttons, and Close on
    /// either stub window.
    OpenSettings,
    OpenEncyclopedia,
    CloseWindow,
    /// The arrival panel's tab row (0 Adventure, 1 Legends).
    ArrivalTab(i32),
    /// The Adventure tab's civ button (index into `AdventureSelection::civs`).
    AdventurePickCiv(i32),
    /// The Adventure tab's site button (index into the picked civ's sites).
    AdventurePickSite(i32),
    /// The Adventure tab's Start adventure button.
    AdventureStart,
    /// The Esc menu's End adventure button.
    EndAdventure,
    /// A Legends filter row (index into `arrival_tabs::legends_filter_entries`).
    LegendsPickFilter(i32),
    /// The Legends list reached its bottom: the next older page.
    LegendsMore,
    /// A Legends line clicked (index into `LegendsState::lines`).
    LegendsLine(i32),
    /// The inventory outside-panel press requests one authoritative cursor
    /// throw. No client-side stack data accompanies it.
    ThrowCursorStack,
}

/// Which modal game screens are open. The client keeps them exclusive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Screens {
    pub menu: bool,
    pub map: bool,
    /// The tool wheel ([`tool_wheel`]): modal while open, so the commander
    /// leaves the right button and the world alone.
    pub wheel: bool,
    /// The Settings window (`ui/settings.slint`: the menu column's and the
    /// Esc menu's Settings) and the Encyclopedia stub (`ui/stub-window.slint`).
    pub settings: bool,
    pub encyclopedia: bool,
    /// The inventory / chest screen: opened and closed by the sim
    /// (`HudItems`), modal while open.
    pub inventory: bool,
}

impl Screens {
    fn of(ui: &GameUi) -> Self {
        Self {
            menu: ui.get_menu_open(),
            map: ui.get_map_open(),
            wheel: ui.get_wheel_open(),
            settings: ui.get_settings_open(),
            encyclopedia: ui.get_encyclopedia_open(),
            inventory: ui.get_inventory_open(),
        }
    }

    pub fn any(self) -> bool {
        self.menu || self.map || self.wheel || self.settings || self.encyclopedia || self.inventory
    }

    /// [`MENU_KEY`] (with no designation to cancel): the wheel closes first,
    /// then a stub window, then the map.
    pub fn escape(self) -> Self {
        if self.wheel {
            Self {
                wheel: false,
                ..self
            }
        } else if self.settings || self.encyclopedia {
            Self {
                settings: false,
                encyclopedia: false,
                ..self
            }
        } else if self.map {
            Self { map: false, ..self }
        } else {
            Self {
                menu: !self.menu,
                ..self
            }
        }
    }

    pub fn open_map() -> Self {
        Self {
            map: true,
            ..Self::default()
        }
    }

    /// The menu column's and the Esc menu's Settings button.
    pub fn open_settings() -> Self {
        Self {
            settings: true,
            ..Self::default()
        }
    }

    /// The menu column's Encyclopedia button.
    pub fn open_encyclopedia() -> Self {
        Self {
            encyclopedia: true,
            ..Self::default()
        }
    }

    /// [`MENU_KEY`] with the arrival map `modal` (not in play yet): Esc
    /// only swaps the map for the Esc menu (Main menu only) and back.
    pub fn escape_in(self, modal: bool) -> Self {
        if modal {
            Self {
                menu: !self.menu,
                map: self.menu,
                ..Self::default()
            }
        } else {
            self.escape()
        }
    }

    /// The arrival map stays up while `modal` unless the Esc menu is over it:
    /// Close and the rest leave it open.
    pub fn held(self, modal: bool) -> Self {
        if modal && !self.menu {
            Self {
                map: true,
                wheel: false,
                ..self
            }
        } else {
            self
        }
    }
}

/// Whether the arrival map is modal now (not in play, the map received).
fn map_modal(in_play: Option<&InPlay>, world_map: &WorldMapState) -> bool {
    !in_play.is_none_or(|e| e.0) && world_map.map.is_some()
}

/// Slot presses on the inventory screen: (in the chest, index within its
/// inventory, right button).
struct InventoryClicks(Rc<RefCell<Vec<(bool, i32, bool)>>>);

/// An editor palette press as delivered by Slint. The held stack bit is
/// advisory; the action system uses the authoritative inventory projection.
struct EditorPaletteClicks(Rc<RefCell<Vec<(i32, bool, bool, bool)>>>);

/// The sim slot a screen press names: container slots are `Container(i)`,
/// the player's are `Player(i)` (hotbar `0..9`, main grid `9..36`).
pub fn slot_ref(container: bool, index: i32) -> Option<game::SlotRef> {
    let i = u8::try_from(index).ok()?;
    Some(if container {
        game::SlotRef::Container(i)
    } else {
        game::SlotRef::Player(i)
    })
}

/// Filled by the game UI's callbacks during event dispatch (`!Send`, a
/// non-send resource on the main thread).
pub(crate) struct GameUiActions(pub(crate) Rc<RefCell<Vec<UiAction>>>);

/// Queues `action` as if its button in the game UI had been pressed (chat
/// commands); `apply_ui_actions` takes it with the same checks. `false`
/// without a game UI.
pub(crate) fn queue_ui_action(world: &World, action: UiAction) -> bool {
    match world.get_non_send::<GameUiActions>() {
        Some(actions) => {
            actions.0.borrow_mut().push(action);
            true
        }
        None => false,
    }
}

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
    hidden_by_frontend: FrontendHide,
    session_scoped: bool,
}

impl<C> SlintLayerPlugin<C> {
    /// `make` is the component's generated `new` (e.g. `GameUi::new`).
    pub fn new(make: fn() -> Result<C, slint_ui::slint::PlatformError>) -> Self {
        Self {
            make,
            init: |_, _| {},
            order: GAME_LAYER_ORDER,
            passthrough: false,
            hidden_by_frontend: FrontendHide::Never,
            session_scoped: false,
        }
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

    /// Draw nothing and take no input while the frontend owns the screen
    /// (every state but an `InWorld` in play): the in-world layers.
    pub fn hidden_by_frontend(mut self) -> Self {
        self.hidden_by_frontend = FrontendHide::Always;
        self
    }

    /// Like [`Self::hidden_by_frontend`], but drawn and fed input over the
    /// arrival map (in the world, not in play, the map received): the game
    /// UI, whose world map is the start screen, in the frontend's place.
    pub fn shown_on_arrival_map(mut self) -> Self {
        self.hidden_by_frontend = FrontendHide::ExceptArrivalMap;
        self
    }

    /// The component shows the world session: `end_session` replaces it
    /// with a fresh one (every property back at its default, `init` run
    /// again), so nothing it held of one world shows in the next.
    pub fn session_scoped(mut self) -> Self {
        self.session_scoped = true;
        self
    }

    pub fn with_init(mut self, init: fn(&C, &mut World)) -> Self {
        self.init = init;
        self
    }
}

impl<C: ComponentHandle + 'static> Plugin for SlintLayerPlugin<C> {
    fn build(&self, app: &mut App) {
        let (make, init, order, passthrough, hidden) = (
            self.make,
            self.init,
            self.order,
            self.passthrough,
            self.hidden_by_frontend,
        );
        if !app.world().contains_resource::<OverlayScale>() {
            app.insert_resource(OverlayScale(ui_scale_pinned_by_env().unwrap_or(UI_SCALE)));
        }
        if !app.is_plugin_added::<SlintCompositePlugin>() {
            app.add_plugins(SlintCompositePlugin);
        }
        app.init_resource::<SlintPointer>()
            // PreStartup: an app's Startup systems may use the layer and
            // whatever `init` inserted.
            .add_systems(PreStartup, move |world: &mut World| {
                setup_slint_layer(world, make, init, order, passthrough, hidden)
            })
            .add_systems(PreUpdate, slint_input::<C>.after(bevy::input::InputSystems))
            .add_systems(PostUpdate, render_slint_layer::<C>);
        if self.session_scoped {
            app.add_session_reset_with(rebuild_slint_layer::<C>);
        }
    }
}

/// Session reset of a [`SlintLayerPlugin::session_scoped`] layer: a fresh
/// component in place of the old one, so every per-world property (menu and
/// map open, the map, selection, tool, level, wheel, band, minimap…) is back
/// at its default without listing them. The layer's texture and order stay.
fn rebuild_slint_layer<C: ComponentHandle + 'static>(world: &mut World) {
    let Some((make, init)) = world
        .get_non_send::<SlintLayer<C>>()
        .map(|l| (l.make, l.init))
    else {
        return;
    };
    let overlay = match Overlay::with(make) {
        Ok(overlay) => overlay,
        Err(e) => {
            error!(
                "[slint] could not rebuild {}: {e}",
                std::any::type_name::<C>()
            );
            return;
        }
    };
    init(overlay.ui(), world);
    if let Some(mut layer) = world.get_non_send_mut::<SlintLayer<C>>() {
        layer.overlay = overlay;
        layer.owns_pointer = false;
        // Not blanked: a hidden layer (the frontend has the screen) zero-fills
        // its texture next frame, so the old world's pixels never show under
        // the menu; a visible one uploads the new component's full first frame.
        layer.blanked = false;
    }
}

/// One component's layer, `!Send` (a non-send resource on the main thread).
pub struct SlintLayer<C: 'static> {
    pub overlay: Overlay<C>,
    /// How the component was made, to make it again ([`rebuild_slint_layer`]).
    make: fn() -> Result<C, slint_ui::slint::PlatformError>,
    init: fn(&C, &mut World),
    /// UI pixels per window logical pixel in effect: [`OverlayScale`] fitted
    /// to the window, set each frame by [`slint_input`].
    ui_scale: f32,
    /// The texture the overlay is drawn into.
    target: Handle<Image>,
    /// The pointer is over a pixel the UI drew, or a press that started
    /// there is still held: the view underneath should leave it alone.
    pub owns_pointer: bool,
    order: i32,
    passthrough: bool,
    hidden_by_frontend: FrontendHide,
    /// The texture was blanked for the frontend and has not been redrawn.
    blanked: bool,
}

impl<C: 'static> SlintLayer<C> {
    /// UI pixels per window logical pixel in effect this frame.
    pub fn ui_scale(&self) -> f32 {
        self.ui_scale
    }

    /// The texture the layer is drawn into.
    pub fn target(&self) -> &Handle<Image> {
        &self.target
    }
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
        app.add_plugins(
            SlintLayerPlugin::new(GameUi::new)
                .shown_on_arrival_map()
                .session_scoped()
                .with_init(connect_game_ui),
        )
        .add_systems(
            Update,
            (
                sync_inventory_screen.before(menu_keys),
                editor_search_keys,
                inventory_clicks,
                editor_palette_clicks,
                // Before every commander input system (the chain starts
                // at `minimap_pointer`, `designate_keys` is in it), so
                // `CursorPolicy::ui_modal` holds this frame and one Esc
                // press does one thing.
                menu_keys.before(minimap::minimap_pointer),
                apply_ui_actions.before(minimap::minimap_pointer),
                apply_adventure_result.before(minimap::minimap_pointer),
                arrival_map.before(minimap::minimap_pointer),
                // Also before the commander: the clicks the open wheel
                // takes must not reach the world.
                tool_wheel.before(minimap::minimap_pointer),
                push_ui_state,
            )
                .chain()
                .run_if(world_systems_enabled()),
        );
    }
}

/// Receiver half of the sim's UI projection channel (`game::UiUpdate`), once
/// the instance announces it (`ClientUpdateEvent::UiChannel`).
#[derive(Resource, Default)]
pub struct UiUpdates(pub Option<crossbeam::channel::Receiver<game::UiUpdate>>);

/// The sim's item UI as last projected: the hotbar and mining progress the
/// HUD draws in Adventure first person, and the inventory / chest screen.
#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct HudItems {
    pub hotbar: game::HotbarView,
    pub inventory: game::InventoryView,
    /// Mining progress, 0..=1000.
    pub mining: Option<u32>,
}

/// Apply the sim's UI projection: the cursor grab it asks for, the play
/// mode and the item UI ([`HudItems`]).
pub fn drain_ui_updates(
    updates: Res<UiUpdates>,
    mut items: ResMut<HudItems>,
    mut editor: ResMut<EditorState>,
    mut cursor_policy: ResMut<CursorPolicy>,
    mut play_mode: Option<ResMut<ClientPlayMode>>,
) {
    let Some(receiver) = updates.0.as_ref() else {
        return;
    };
    while let Ok(update) = receiver.try_recv() {
        match update {
            game::UiUpdate::Hotbar(hotbar) => items.hotbar = hotbar,
            game::UiUpdate::Inventory(view) => items.inventory = view,
            game::UiUpdate::Mining(m) => items.mining = m,
            game::UiUpdate::Editor { active, allowed } => {
                editor.active = active;
                editor.allowed = allowed;
                editor.allowed_known = true;
            }
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
                .hidden_by_frontend()
                .session_scoped()
                .passthrough()
                .with_init(check_hud_config),
        )
        .add_systems(Update, push_hud_state.run_if(world_systems_enabled()));
    }
}

fn check_hud_config(ui: &HudUi, _: &mut World) {
    let config = ui.global::<HudConfig>();
    debug_assert_eq!(
        config.get_minimap_size(),
        minimap::MINIMAP_SIZE_PX,
        "HudConfig.minimap-size"
    );
    debug_assert_eq!(
        config.get_minimap_margin(),
        minimap::MINIMAP_MARGIN_PX,
        "HudConfig.minimap-margin"
    );
}

/// Client state → HUD properties, only where it changed.
fn push_hud_state(
    hud: Option<NonSend<SlintLayer<HudUi>>>,
    cursor: Query<&CursorOptions, With<PrimaryWindow>>,
    policy: Option<Res<CursorPolicy>>,
    band: Option<Res<SelectionBand>>,
    minimap_pixels: Option<ResMut<minimap::MinimapPixels>>,
    status: Option<Res<HudStatus>>,
    items: Option<Res<HudItems>>,
    first_person: Option<Res<super::commander::first_person::FirstPerson>>,
    adventure: Option<Res<ClientPlayMode>>,
    editor: Option<Res<EditorState>>,
    mut hotbar_shown: crate::session::SessionLocal<Option<game::HotbarView>>,
) {
    let Some(hud) = hud else { return };
    let ui = hud.overlay.ui();

    // The hotbar strip and the mining bar: Adventure first person only.
    let hotbar_on = (adventure.is_some_and(|m| m.adventure())
        || editor.as_ref().is_some_and(|e| e.active))
        && first_person.as_ref().is_some_and(|f| f.on);
    let third_person = first_person.as_ref().and_then(|f| f.body_view())
        == Some(super::commander::first_person::BodyView::Third);
    let hotbar_shown = hotbar_shown.get();
    match items {
        Some(items) if hotbar_on => {
            if hotbar_shown.as_ref() != Some(&items.hotbar) {
                let slots: Vec<slint_ui::ItemSlot> =
                    items.hotbar.slots.iter().map(item_slot).collect();
                ui.set_hotbar(ModelRc::new(VecModel::from(slots)));
                ui.set_hotbar_selected(items.hotbar.selected as i32);
                *hotbar_shown = Some(items.hotbar.clone());
            }
            let progress = items.mining.map_or(0.0, |m| m as f32 / 1000.0);
            if ui.get_mining_progress() != progress {
                ui.set_mining_progress(progress);
            }
        }
        _ if hotbar_shown.is_some() => {
            ui.set_hotbar(ModelRc::new(VecModel::from(
                Vec::<slint_ui::ItemSlot>::new(),
            )));
            ui.set_mining_progress(0.0);
            *hotbar_shown = None;
        }
        _ => {}
    }

    // The minimap shows once a home region is joined (a session that is still
    // starting, or just reset, draws none, as a fresh HUD).
    let minimap = minimap_pixels.is_some() && status.is_some_and(|s| s.home_region.is_some());
    if ui.get_minimap_visible() != minimap {
        ui.set_minimap_visible(minimap);
    }
    if let Some(mut pixels) = minimap_pixels {
        let image = |data: &[[u8; 4]], edge: u32| {
            SlintImage::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                data.as_flattened(),
                edge,
                edge,
            ))
        };
        if pixels.overlay_dirty {
            ui.set_minimap_overlay(image(&pixels.overlay, minimap::OVERLAY_PX));
            pixels.overlay_dirty = false;
        }
    }

    let grab = cursor
        .iter()
        .next()
        .map_or(CursorGrabMode::None, |c| c.grab_mode);
    let crosshair = crosshair_visible(grab, policy.is_some_and(|p| p.orbit), third_person);
    if ui.get_crosshair_visible() != crosshair {
        ui.set_crosshair_visible(crosshair);
    }

    // The minimap keeps its window-pixel rectangle (`minimap::screen_rect`).
    if ui.get_ui_scale() != hud.ui_scale {
        ui.set_ui_scale(hud.ui_scale);
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

/// Inventory screen presses → the sim's `UiAction::Click` (Shift from the
/// keyboard).
fn inventory_clicks(
    clicks: Option<NonSend<InventoryClicks>>,
    keys: Res<ButtonInput<KeyCode>>,
    player: Option<Res<super::LocalPlayer>>,
    game: Option<Res<super::GameEvents>>,
) {
    let Some(clicks) = clicks else { return };
    let pressed: Vec<_> = clicks.0.borrow_mut().drain(..).collect();
    let (Some(Some(player)), Some(game)) = (player.map(|p| p.0), game) else {
        return;
    };
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    for (container, index, right) in pressed {
        let Some(slot) = slot_ref(container, index) else {
            continue;
        };
        let button = if right {
            game::ClickButton::Right
        } else {
            game::ClickButton::Left
        };
        let _ = game.0.send(game::GameEventKind::UiAction(
            player,
            game::UiAction::Click {
                slot,
                button,
                shift,
            },
        ));
    }
}

fn editor_palette_clicks(
    clicks: Option<NonSend<EditorPaletteClicks>>,
    keys: Res<ButtonInput<KeyCode>>,
    items: Res<HudItems>,
    editor: Res<EditorState>,
    player: Option<Res<super::LocalPlayer>>,
    events: Option<Res<super::GameEvents>>,
) {
    let Some(clicks) = clicks else { return };
    let pressed: Vec<_> = clicks.0.borrow_mut().drain(..).collect();
    let (Some(Some(player)), Some(events)) = (player.map(|p| p.0), events) else {
        return;
    };
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    for (item, right, callback_shift, _held) in pressed {
        if !editor.active || !items.inventory.open || items.inventory.container.is_some() {
            continue;
        }
        let Ok(item) = u16::try_from(item) else {
            continue;
        };
        let item = game::ItemId(item);
        if !game::item_exists(item) {
            continue;
        }
        let action = if items.inventory.cursor.is_some() {
            game::UiAction::EditorTrash
        } else if shift || callback_shift {
            game::UiAction::EditorGive {
                item,
                count: game::MAX_STACK,
            }
        } else {
            game::UiAction::EditorTake {
                item,
                count: if right { 1 } else { game::MAX_STACK },
            }
        };
        let _ = events.0.send(game::GameEventKind::UiAction(player, action));
    }
}

fn editor_search_keys(
    mut events: MessageReader<KeyboardInput>,
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    mut entry: ResMut<crate::renderer::text_entry::TextEntry>,
) {
    if !entry.palette {
        return;
    }
    let escape = events.read().any(|event| {
        event.state == ButtonState::Pressed && matches!(event.logical_key, Key::Escape)
    });
    if escape {
        if let Some(slint) = slint {
            slint.overlay.ui().invoke_inventory_drop_focus();
        }
        entry.palette = false;
    }
}

/// One sim slot as the Slint item slot.
pub fn item_slot(slot: &Option<game::SlotView>) -> slint_ui::ItemSlot {
    match slot {
        Some(s) => slint_ui::ItemSlot {
            filled: true,
            count: if s.count > 1 {
                s.count.to_string().into()
            } else {
                SharedString::default()
            },
            icon: super::item_icons::icon(game::ItemId(s.icon.0 as u16)),
            name: super::look_target::display_name(game::item_name(game::ItemId(s.icon.0 as u16)))
                .into(),
        },
        None => slint_ui::ItemSlot::default(),
    }
}

fn item_model(slots: &[Option<game::SlotView>]) -> ModelRc<slint_ui::ItemSlot> {
    ModelRc::new(VecModel::from(
        slots.iter().map(item_slot).collect::<Vec<_>>(),
    ))
}

fn palette_tab(tab: Option<game::CreativeTab>) -> i32 {
    match tab {
        Some(game::CreativeTab::Natural) => 1,
        Some(game::CreativeTab::Wood) => 2,
        Some(game::CreativeTab::Building) => 3,
        Some(game::CreativeTab::Colour) => 4,
        Some(game::CreativeTab::Functional) => 5,
        None => 6,
    }
}

fn palette_model(tab: i32, search: &str) -> ModelRc<slint_ui::CreativePaletteEntry> {
    let search = search.trim().to_ascii_lowercase();
    let mut entries: Vec<slint_ui::CreativePaletteEntry> = game::palette_entries()
        .into_iter()
        .filter_map(|entry| {
            let entry_tab = palette_tab(entry.tab);
            if tab != 0 && tab != entry_tab {
                return None;
            }
            let name = entry.name;
            (search.is_empty() || name.to_ascii_lowercase().contains(&search)).then(|| {
                slint_ui::CreativePaletteEntry {
                    item: i32::from(entry.item.0),
                    name: super::look_target::display_name(name).into(),
                    icon: super::item_icons::icon(entry.item),
                    tab: entry_tab,
                }
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        a.name
            .to_string()
            .to_ascii_lowercase()
            .cmp(&b.name.to_string().to_ascii_lowercase())
    });
    ModelRc::new(VecModel::from(entries))
}

#[derive(Default, PartialEq, Eq, Clone)]
struct EditorPaletteShown {
    active: bool,
    open: bool,
    chest: bool,
    tab: i32,
    search: String,
}

/// The sim's inventory screen → the game UI: open state, slot models (only
/// when the projection changed), the held stack and the pointer it follows.
/// Each opening puts the pointer in the middle of the window, once the grab
/// that mouse look held is actually released (a warp while the pointer is
/// still locked would be undone by the unlock).
fn sync_inventory_screen(
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    items: Res<HudItems>,
    editor: Res<EditorState>,
    mut entry: ResMut<crate::renderer::text_entry::TextEntry>,
    mut window: Query<(&mut Window, &bevy::window::CursorOptions), With<PrimaryWindow>>,
    mut shown: crate::session::SessionLocal<Option<game::InventoryView>>,
    mut center_pending: crate::session::SessionLocal<bool>,
    mut was_open: crate::session::SessionLocal<bool>,
    mut palette_shown: crate::session::SessionLocal<EditorPaletteShown>,
) {
    let Some(slint) = slint else { return };
    let ui = slint.overlay.ui();
    let view = &items.inventory;
    if ui.get_inventory_open() != view.open {
        ui.set_inventory_open(view.open);
        *center_pending.get() = view.open;
        if view.open {
            ui.invoke_inventory_take_focus();
        } else {
            ui.invoke_inventory_drop_focus();
        }
    }
    if *was_open.get() && !view.open && ui.get_inventory_tab() != 0 {
        ui.set_inventory_tab(0);
    }
    *was_open.get() = view.open;
    let editor_active = editor.active;
    if ui.get_editor_active() != editor_active {
        ui.set_editor_active(editor_active);
    }
    if ui.get_editor_search_focused()
        && (!view.open || !editor_active || view.container.is_some() || ui.get_inventory_tab() != 0)
    {
        ui.invoke_inventory_drop_focus();
    }
    let palette_focus =
        view.open && editor_active && view.container.is_none() && ui.get_editor_search_focused();
    if entry.palette != palette_focus {
        entry.palette = palette_focus;
    }
    let current = EditorPaletteShown {
        active: editor_active,
        open: view.open,
        chest: view.container.is_some(),
        tab: ui.get_editor_tab(),
        search: ui.get_editor_search().to_string(),
    };
    let shown_palette = palette_shown.get();
    if *shown_palette != current {
        if current.active && current.open && !current.chest {
            ui.set_editor_palette(palette_model(current.tab, &current.search));
        } else {
            ui.set_editor_palette(ModelRc::new(VecModel::from(Vec::<
                slint_ui::CreativePaletteEntry,
            >::new())));
        }
        *shown_palette = current;
    }
    if !view.open {
        *shown.get() = None;
        return;
    }
    if *center_pending.get() {
        if let Ok((mut window, cursor)) = window.single_mut() {
            if window.focused && cursor.grab_mode == bevy::window::CursorGrabMode::None {
                let center = Vec2::new(window.width(), window.height()) / 2.0;
                window.set_cursor_position(Some(center));
                *center_pending.get() = false;
            }
        }
    }
    let shown = shown.get();
    if shown.as_ref() != Some(view) {
        let part = |range: std::ops::Range<usize>| view.slots.get(range).unwrap_or(&[]);
        ui.set_inventory_hotbar(item_model(part(0..game::HOTBAR_SLOTS)));
        ui.set_inventory_container(item_model(view.container.as_deref().unwrap_or(&[])));
        ui.set_inventory_cursor(item_slot(&view.cursor));
        *shown = Some(view.clone());
    }
    if ui.get_inventory_selected() != items.hotbar.selected as i32 {
        ui.set_inventory_selected(items.hotbar.selected as i32);
    }
    if let Some(cursor) = window.single().ok().and_then(|(w, _)| w.cursor_position()) {
        let at = cursor / slint.ui_scale;
        if ui.get_inventory_pointer_x() != at.x || ui.get_inventory_pointer_y() != at.y {
            ui.set_inventory_pointer_x(at.x);
            ui.set_inventory_pointer_y(at.y);
        }
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
    ui.on_main_menu(push(UiAction::MainMenu));
    let queue = actions.clone();
    ui.on_arrival_tab_changed(move |i| queue.borrow_mut().push(UiAction::ArrivalTab(i)));
    let queue = actions.clone();
    ui.on_adventure_pick_civ(move |i| queue.borrow_mut().push(UiAction::AdventurePickCiv(i)));
    let queue = actions.clone();
    ui.on_adventure_pick_site(move |i| queue.borrow_mut().push(UiAction::AdventurePickSite(i)));
    ui.on_adventure_start(push(UiAction::AdventureStart));
    ui.on_end_adventure(push(UiAction::EndAdventure));
    let queue = actions.clone();
    ui.on_legends_pick_filter(move |i| queue.borrow_mut().push(UiAction::LegendsPickFilter(i)));
    ui.on_legends_load_more(push(UiAction::LegendsMore));
    let queue = actions.clone();
    ui.on_legends_line_clicked(move |i| queue.borrow_mut().push(UiAction::LegendsLine(i)));
    ui.on_follow_clicked(push(UiAction::ToggleFollow));
    ui.on_top_down_clicked(push(UiAction::ToggleTopDown));
    ui.on_open_settings(push(UiAction::OpenSettings));
    // The Settings window's sliders (graphics_settings.rs).
    crate::graphics_settings::connect_settings!(ui, world);
    ui.on_open_encyclopedia(push(UiAction::OpenEncyclopedia));
    ui.on_close_window(push(UiAction::CloseWindow));
    // The map window: its layer buttons, pan, zoom and click (`renderer/map/full.rs`).
    super::map::full::connect(ui, world);
    let clicks = Rc::new(RefCell::new(Vec::new()));
    let queue = clicks.clone();
    ui.on_inventory_slot_pressed(move |container, index, right| {
        queue.borrow_mut().push((container, index, right))
    });
    let queue = actions.clone();
    ui.on_inventory_throw_cursor_stack(move || {
        queue.borrow_mut().push(UiAction::ThrowCursorStack);
    });
    let palette_clicks = Rc::new(RefCell::new(Vec::new()));
    let queue = palette_clicks.clone();
    ui.on_editor_palette_pressed(move |item, right, shift, held| {
        queue.borrow_mut().push((item, right, shift, held))
    });
    world.insert_non_send(InventoryClicks(clicks));
    world.insert_non_send(EditorPaletteClicks(palette_clicks));
    world.insert_non_send(GameUiActions(actions));
    super::chat::ui::connect(ui, world);
    #[cfg(feature = "dev_ui")]
    super::dev_ui::connect_debug_pane(ui, world);
}

fn setup_slint_layer<C: ComponentHandle + 'static>(
    world: &mut World,
    make: fn() -> Result<C, slint_ui::slint::PlatformError>,
    init: fn(&C, &mut World),
    order: i32,
    passthrough: bool,
    hidden_by_frontend: FrontendHide,
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
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    image.sampler = ImageSampler::nearest();
    let target = world.resource_mut::<Assets<Image>>().add(image);
    world
        .get_resource_or_init::<SlintLayerTextures>()
        .0
        .push((order, target.clone()));
    world.insert_non_send(SlintLayer {
        overlay,
        make,
        init,
        ui_scale: UI_SCALE,
        target,
        owns_pointer: false,
        order,
        passthrough,
        hidden_by_frontend,
        blanked: false,
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
    window: Query<(&Window, &bevy::window::CursorOptions), With<PrimaryWindow>>,
    mut buttons: MessageReader<MouseButtonInput>,
    mut wheel: MessageReader<MouseWheel>,
    mut keys: MessageReader<KeyboardInput>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut pointer: crate::session::SessionLocal<PointerState>,
    mut shared: ResMut<SlintPointer>,
    policy: Option<ResMut<CursorPolicy>>,
    state: Option<Res<State<AppState>>>,
    in_play: Option<Res<InPlay>>,
    world_map: Option<Res<WorldMapState>>,
    wish: Option<Res<OverlayScale>>,
    full_map: Option<Res<super::map::full::FullMapOpen>>,
) {
    let pointer = pointer.get();
    let Some(mut slint) = slint else { return };
    let Ok((window, cursor_options)) = window.single() else {
        return;
    };
    slint.ui_scale = fitted_ui_scale(
        wish.map_or(UI_SCALE, |w| w.0),
        (window.physical_width(), window.physical_height()),
        window.scale_factor(),
    );
    let scale = window.scale_factor() * slint.ui_scale;
    slint
        .overlay
        .resize(window.physical_width(), window.physical_height(), scale);
    let full_map_open = full_map.is_some_and(|m| m.0);
    if frontend_hides(
        slint.hidden_by_frontend,
        state.as_deref(),
        in_play.as_deref(),
        world_map.as_deref(),
        full_map_open,
    ) {
        // The frontend has the screen: nothing of ours is under the pointer.
        buttons.clear();
        wheel.clear();
        keys.clear();
        if pointer.last.take().is_some() {
            slint.overlay.dispatch(WindowEvent::PointerExited);
        }
        pointer.captured = false;
        let order = slint.order;
        slint.owns_pointer = false;
        shared.set(order, false, false);
        let owned = shared.any_owner();
        if let Some(mut policy) = policy.filter(|p| p.ui_pointer != owned) {
            policy.ui_pointer = owned;
        }
        return;
    }
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
    // A grabbed cursor (mouse look) is the game's: the UI has no pointer.
    // The hidden cursor still moves under the grab, and forwarding it sent
    // every layer a `PointerMoved` per frame — each a walk of its whole item
    // tree; turning the camera in a debug build fell from ~160 to 11-70 fps
    // with 84 % of the main thread in Slint (2026-11-19 perf capture).
    let grabbed = cursor_options.grab_mode != bevy::window::CursorGrabMode::None;
    let cursor = if grabbed {
        None
    } else {
        window.cursor_position()
    };
    let ui_pos = cursor.map(|c| c / slint.ui_scale);
    if ui_pos != pointer.last {
        overlay.dispatch(match ui_pos {
            Some(p) => WindowEvent::PointerMoved {
                position: LogicalPosition::new(p.x, p.y),
            },
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
        let (Some(p), true) = (ui_pos, over_ui) else {
            continue;
        };
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
    // Keys reach the UI only while it has the keyboard: a menu, an overlay or
    // the loading screen is up, a text field is typed in, or the frontend
    // has the screen. Otherwise they are the game's. Every dispatched key
    // event makes Slint walk the layer's whole item tree, in every layer —
    // and a held movement key repeats ~30 times a second: walking around in a
    // debug build stuttered in 40-200 ms frames with the main thread in
    // `ensure_instantiated` (2026-11-19 perf capture). No `.slint` file
    // handles a key itself; they matter to text fields and focused widgets.
    let ui_has_keys = policy.as_deref().is_none_or(CursorPolicy::blocks_game)
        || crate::session::frontend_has_screen(state.as_deref(), in_play.as_deref());
    for event in keys.read() {
        if !ui_has_keys {
            continue;
        }
        let Some(text) = key_text(&event.logical_key) else {
            continue;
        };
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

/// The map tile (column, row) of region `rc` (canonical or any copy of it),
/// `None` off the map.
pub fn map_tile(map: &WorldMap, rc: RegionCoords) -> Option<(i32, i32)> {
    map.tile(rc)?;
    let rc = map.extent().canonical(rc);
    Some((rc.x - map.origin.x, rc.z - map.origin.z))
}

/// The (canonical) region of map tile (`col`, `row`), `None` off the map.
pub fn tile_region(map: &WorldMap, col: i32, row: i32) -> Option<RegionCoords> {
    let rc = RegionCoords::new(map.origin.x + col, map.origin.z + row);
    (col >= 0 && row >= 0 && map.tile(rc).is_some()).then(|| map.extent().canonical(rc))
}

/// The 8 civ colours of the site markers, picked by `civ.0 % 8`.
pub const SITE_PALETTE: [(u8, u8, u8); 8] = [
    (230, 80, 70),
    (70, 160, 235),
    (240, 200, 60),
    (90, 200, 110),
    (200, 110, 230),
    (240, 150, 60),
    (80, 220, 220),
    (235, 235, 235),
];

/// The marker colour of a civ.
pub fn site_color(civ: game::history::CivId) -> slint_ui::slint::Color {
    let (r, g, b) = SITE_PALETTE[(civ.0 % 8) as usize];
    slint_ui::slint::Color::from_rgb_u8(r, g, b)
}

/// "Site: Name (civ), 12 dwarves" for the selected tile.
pub fn site_line(site: &game::SiteMarker, civ_name: &str) -> String {
    let abandoned = if site.abandoned { ", abandoned" } else { "" };
    format!(
        "Site: {} ({civ_name}), {} dwarves{abandoned}",
        site.name, site.population
    )
}

/// Show `to`. Keeps `CursorPolicy::ui_modal` in step (its only writer):
/// any screen open, or not in play.
/// Takes the `ResMut`s themselves and writes through them only on a real
/// change, so an idle frame does not mark either resource changed (that
/// would make `sync_world_map` compare the whole map every frame).
fn set_screens(
    ui: &GameUi,
    to: Screens,
    // Unused since the map centres itself on opening (`renderer/map/full.rs`); kept so the five
    // callers keep their shape.
    _world_map: &mut ResMut<WorldMapState>,
    _home: Option<RegionCoords>,
    policy: Option<&mut ResMut<CursorPolicy>>,
    in_play: bool,
) {
    let from = Screens::of(ui);
    if from.menu != to.menu {
        ui.set_menu_open(to.menu);
    }
    if from.map != to.map {
        ui.set_map_open(to.map);
    }
    if from.wheel != to.wheel {
        ui.set_wheel_open(to.wheel);
    }
    if from.settings != to.settings {
        ui.set_settings_open(to.settings);
    }
    if from.encyclopedia != to.encyclopedia {
        ui.set_encyclopedia_open(to.encyclopedia);
    }
    if from.inventory != to.inventory {
        ui.set_inventory_open(to.inventory);
    }
    // Not in play, the world behind is not ours to touch even with no
    // screen open (before the map arrives).
    let modal = to.any() || !in_play;
    if let Some(policy) = policy.filter(|p| p.ui_modal != modal) {
        policy.ui_modal = modal;
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
    in_play: Option<Res<InPlay>>,
    items: Res<HudItems>,
    time: Res<Time>,
    (player, game): (
        Option<Res<super::LocalPlayer>>,
        Option<Res<super::GameEvents>>,
    ),
) {
    let Some(slint) = slint else { return };
    let player = player.as_deref().and_then(|p| p.0);
    let game = game.as_deref();
    // Esc closes the inventory: the sim owns it, so ask the sim to. Close,
    // never toggle: `HudItems` may lag the sim, and a toggle on a stale view
    // would reopen a screen just closed.
    if items.inventory.open && keys.just_pressed(MENU_KEY) {
        if let (Some(player), Some(game)) = (player, game) {
            let _ = game.0.send(game::GameEventKind::UiAction(
                player,
                game::UiAction::CloseInventory,
            ));
        }
        keys.clear_just_pressed(MENU_KEY);
        return;
    }
    // A client-local overlay holding the pointer holds the keyboard too.
    if policy.as_ref().is_some_and(|p| p.overlay_open) {
        return;
    }
    let ui = slint.overlay.ui();
    let not_in_play = in_play.as_deref().is_some_and(|e| !e.0);
    let modal = map_modal(in_play.as_deref(), &world_map);
    if not_in_play && !modal {
        // Before the world map arrives the game UI is hidden (the frontend's
        // starting screen, with its Back, has the screen): a menu opened now
        // could not be seen, and would later stand in for the arrival map.
        return;
    }
    let mut to = Screens::of(ui);
    if keys.just_pressed(MENU_KEY) {
        let designating = designate.as_ref().is_some_and(|d| d.in_progress());
        if to.any() || !designating {
            to = to.escape_in(modal);
            keys.clear_just_pressed(MENU_KEY);
        }
    }
    if keys.just_pressed(MAP_KEY) {
        if modal {
            to = to.held(true);
        } else {
            ui.set_inventory_tab(2);
            if items.inventory.open {
                ui.invoke_inventory_take_focus();
            }
            to.menu = false;
            to.map = false;
            to.wheel = false;
            to.settings = false;
            to.encyclopedia = false;
            if let (Some(player), Some(game)) = (player, game) {
                let _ = game.0.send(game::GameEventKind::UiAction(
                    player,
                    game::UiAction::OpenInventory,
                ));
            }
            if !items.inventory.open {
                if let Some(policy) = policy.as_mut() {
                    policy.inventory_opening = Some(time.elapsed());
                }
            }
            keys.clear_just_pressed(MAP_KEY);
        }
    }
    let in_play = in_play.is_none_or(|e| e.0);
    set_screens(
        ui,
        to.held(modal),
        &mut world_map,
        status.home_region,
        policy.as_mut(),
        in_play,
    );
}

/// The arrival map: while not in play it is modal (`map-modal`) and opened
/// as soon as the world map arrives; playing closes it.
fn arrival_map(
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    in_play: Option<Res<InPlay>>,
    mut world_map: ResMut<WorldMapState>,
    status: Res<HudStatus>,
    mut policy: Option<ResMut<CursorPolicy>>,
    mut was_modal: crate::session::SessionLocal<bool>,
) {
    let Some(slint) = slint else { return };
    let ui = slint.overlay.ui();
    let not_in_play = in_play.as_deref().is_some_and(|e| !e.0);
    if ui.get_map_modal() != not_in_play {
        ui.set_map_modal(not_in_play);
    }
    let modal = map_modal(in_play.as_deref(), &world_map);
    let was_modal = was_modal.get();
    let mut to = Screens::of(ui).held(modal);
    if !*was_modal && modal {
        to = Screens::open_map(); // the map arrived: it opens, nothing else
    }
    if *was_modal && !modal && !not_in_play {
        to.map = false; // in play: the world takes the screen
    }
    *was_modal = modal;
    set_screens(
        ui,
        to,
        &mut world_map,
        status.home_region,
        policy.as_mut(),
        !not_in_play,
    );
}

#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn apply_ui_actions(
    actions: Option<NonSend<GameUiActions>>,
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    mut selection: Option<ResMut<Selection>>,
    mut world_map: ResMut<WorldMapState>,
    mut policy: Option<ResMut<CursorPolicy>>,
    requests: Option<Res<ClientRequests>>,
    status: Res<HudStatus>,
    (mut follow, mut top_down_requests): (
        Option<ResMut<Follow>>,
        Option<ResMut<Messages<super::commander::top_down::ToggleTopDownRequest>>>,
    ),
    local_body: Query<&SimEntity, With<LocalBody>>,
    mut session_ended: Option<ResMut<Messages<crate::session::SessionEnded>>>,
    mut in_play: Option<ResMut<InPlay>>,
    mut history: ResMut<HistoryView>,
    time: Res<Time>,
    (player, game): (
        Option<Res<super::LocalPlayer>>,
        Option<Res<super::GameEvents>>,
    ),
) {
    let (Some(actions), Some(slint)) = (actions, slint) else {
        return;
    };
    let ui = slint.overlay.ui();
    let player = player.as_deref().and_then(|p| p.0);
    let game = game.as_deref();
    for action in actions.0.borrow_mut().drain(..) {
        let is_in_play = in_play.as_ref().is_none_or(|e| e.0);
        let mut to = Screens::of(ui);
        match action {
            UiAction::ClearSelection => {
                if let Some(selection) = selection.as_mut() {
                    selection.0.clear();
                }
            }
            UiAction::OpenSettings => to = Screens::open_settings(),
            UiAction::OpenEncyclopedia => to = Screens::open_encyclopedia(),
            UiAction::CloseWindow => {
                to.settings = false;
                to.encyclopedia = false;
            }
            UiAction::ToggleTopDown => {
                if let Some(requests) = top_down_requests.as_mut() {
                    requests.write(super::commander::top_down::ToggleTopDownRequest);
                }
            }
            UiAction::ToggleFollow => {
                let (Some(follow), Some(selection)) = (follow.as_mut(), selection.as_ref()) else {
                    continue;
                };
                if follow.0.is_some() {
                    follow.0 = None;
                } else if let (1, Some(&(region, key))) =
                    (selection.0.len(), selection.0.iter().next())
                {
                    let is_local = local_body
                        .iter()
                        .any(|s| s.region == region && s.key == key);
                    follow.0 = Some(if is_local {
                        FollowTarget::LocalPlayer
                    } else {
                        FollowTarget::Entity(region, key)
                    });
                }
            }
            UiAction::EndAdventure => {
                if history.adventuring.is_none() || history.adventure_pending {
                    continue;
                }
                if !requests
                    .as_ref()
                    .is_some_and(|r| r.0.send(ClientRequest::EndAdventure).is_ok())
                {
                    warn!("[adventure] end not sent: no game thread");
                    continue;
                }
                // Back to the start screen.
                history.adventuring = None;
                history.adventure_result = None;
                history.adventure_refusal = None;
                if let Some(in_play) = in_play.as_mut() {
                    in_play.0 = false;
                }
                if let Some(follow) = follow.as_mut() {
                    follow.0 = None;
                }
                to.menu = false;
                // `arrival_map` opens the (now modal) map.
            }
            UiAction::ArrivalTab(i) => {
                let tab = arrival_tabs::arrival_tab(i);
                if history.tab != tab {
                    history.tab = tab;
                }
                // The first visit to Legends asks for the newest page.
                let unasked = history.legends.pending.is_none()
                    && history.legends.lines.is_empty()
                    && history.legends.total == 0;
                if tab == ArrivalTab::Legends && unasked {
                    let filter = history.legends.filter.clone();
                    send_legends_query(requests.as_deref(), &mut history, Some(filter));
                }
            }
            UiAction::AdventurePickCiv(i) => {
                if let Some(civ) = arrival_tabs::adventure_civ_at(&history.sites, &history.civs, i)
                {
                    history.adventure.pick_civ(civ);
                }
            }
            UiAction::AdventurePickSite(i) => {
                let Some(site) =
                    arrival_tabs::adventure_site_at(&history.adventure, &history.sites, i)
                else {
                    continue;
                };
                history.adventure.pick_site(site.site);
                if let Some(tile) = world_map
                    .map
                    .as_ref()
                    .and_then(|m| map_tile(m, site.region))
                {
                    arrival_tabs::select_map_tile(ui, tile);
                }
            }
            UiAction::AdventureStart => {
                if history.adventure_pending {
                    continue;
                }
                let Some(site) = history.adventure.site else {
                    continue;
                };
                if requests
                    .as_ref()
                    .is_some_and(|r| r.0.send(ClientRequest::StartAdventure(site)).is_ok())
                {
                    // A stale result must never drive the UI.
                    history.adventure_result = None;
                    history.adventure_refusal = None;
                    history.adventure_pending = true;
                } else {
                    warn!("[adventure] request not sent: no game thread");
                }
            }
            UiAction::LegendsPickFilter(i) => {
                let entries = arrival_tabs::legends_filter_entries(&history.civs, &history.sites);
                let Some(entry) = usize::try_from(i)
                    .ok()
                    .and_then(|i| entries.into_iter().nth(i))
                else {
                    continue;
                };
                if let game::history::LegendsFilter::Site(site) = entry.filter {
                    let tile = world_map
                        .map
                        .as_ref()
                        .and_then(|m| arrival_tabs::site_tile(m, &history.sites, site));
                    if let Some(tile) = tile {
                        arrival_tabs::select_map_tile(ui, tile);
                    }
                }
                send_legends_query(requests.as_deref(), &mut history, Some(entry.filter));
            }
            UiAction::LegendsMore => {
                // Fires while the list sits at its bottom: read first, so an
                // idle repeat does not mark the history changed.
                if history.legends.pending.is_none() && history.legends.next.is_some() {
                    send_legends_query(requests.as_deref(), &mut history, None);
                }
            }
            UiAction::LegendsLine(i) => {
                let line = usize::try_from(i)
                    .ok()
                    .and_then(|i| history.legends.lines.get(i));
                let Some(site) = line.and_then(arrival_tabs::legends_line_site) else {
                    continue;
                };
                let tile = world_map
                    .map
                    .as_ref()
                    .and_then(|m| arrival_tabs::site_tile(m, &history.sites, site));
                if let Some(tile) = tile {
                    arrival_tabs::select_map_tile(ui, tile);
                }
            }
            UiAction::OpenMap => {
                if is_in_play {
                    ui.set_inventory_tab(2);
                    if ui.get_inventory_open() {
                        ui.invoke_inventory_take_focus();
                    }
                    if let (Some(player), Some(game)) = (player, game) {
                        let _ = game.0.send(game::GameEventKind::UiAction(
                            player,
                            game::UiAction::OpenInventory,
                        ));
                    }
                    to.menu = false;
                    to.map = false;
                    to.wheel = false;
                    to.settings = false;
                    to.encyclopedia = false;
                    if !ui.get_inventory_open() {
                        if let Some(policy) = policy.as_mut() {
                            policy.inventory_opening = Some(time.elapsed());
                        }
                    }
                } else {
                    to = Screens::open_map();
                }
            }
            UiAction::ThrowCursorStack => {
                if let (Some(player), Some(game)) = (player, game) {
                    let _ = game.0.send(game::GameEventKind::UiAction(
                        player,
                        game::UiAction::ThrowCursorStack,
                    ));
                }
            }
            UiAction::CloseMap => to.map = false,
            UiAction::CloseMenu => to.menu = false,
            UiAction::MainMenu => {
                to.menu = false;
                if let Some(ended) = session_ended.as_mut() {
                    ended.write(crate::session::SessionEnded { reason: None });
                }
            }
            // The selection lives in Slint; `push_ui_state` follows it.
            // In the Adventure tab a populated site's tile picks that site.
            UiAction::SelectTile(col, row) => {
                if history.tab != ArrivalTab::Adventure {
                    continue;
                }
                let region = world_map
                    .map
                    .as_ref()
                    .and_then(|m| tile_region(m, col, row));
                let site = region
                    .and_then(|rc| arrival_tabs::adventure_site_in(&history.sites, rc))
                    .map(|s| (s.civ, s.site));
                if let Some((civ, site)) = site {
                    history.adventure.pick_civ(civ);
                    history.adventure.pick_site(site);
                }
            }
        }
        let modal = !is_in_play && world_map.map.is_some();
        set_screens(
            ui,
            to.held(modal),
            &mut world_map,
            status.home_region,
            policy.as_mut(),
            is_in_play,
        );
    }
}

/// Send the first page of `filter` (`Some`) or the next older page (`None`)
/// as `RequestLegends`; `LegendsState` keeps at most one query in flight.
fn send_legends_query(
    requests: Option<&ClientRequests>,
    history: &mut HistoryView,
    filter: Option<game::history::LegendsFilter>,
) {
    let query = match filter {
        Some(filter) => Some(history.legends.query_first(filter)),
        None => history.legends.query_next(),
    };
    let Some(query) = query else { return };
    if !requests.is_some_and(|r| r.0.send(ClientRequest::RequestLegends(query)).is_ok()) {
        warn!("[legends] query not sent: no game thread");
        history.legends.pending = None;
    }
}

/// An `AdventureResult` (answer to Start adventure, or to an End the host could
/// not do — the adventure goes on): `Ok` starts it (the
/// arrival map closes, the camera re-places over the body), a refusal goes on
/// the map status line for `REFUSAL_SHOW_SECS`. The result is taken, never
/// read twice.
fn apply_adventure_result(
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    mut history: ResMut<HistoryView>,
    mut in_play: Option<ResMut<InPlay>>,
    mut cam: Option<ResMut<CommanderCamera>>,
    mut world_map: ResMut<WorldMapState>,
    status: Res<HudStatus>,
    mut policy: Option<ResMut<CursorPolicy>>,
    time: Res<Time>,
) {
    let Some(slint) = slint else { return };
    if history.adventure_result.is_none() {
        return;
    }
    let Some(result) = history.adventure_result.take() else {
        return;
    };
    history.adventure_pending = false;
    match result {
        Ok(site) => {
            history.adventuring = Some(site);
            if let Some(in_play) = in_play.as_mut() {
                in_play.0 = true;
            }
            if let Some(cam) = cam.as_mut() {
                cam.placed = false;
            }
            let ui = slint.overlay.ui();
            let mut to = Screens::of(ui);
            to.map = false;
            set_screens(
                ui,
                to,
                &mut world_map,
                status.home_region,
                policy.as_mut(),
                true,
            );
        }
        Err(why) => history.adventure_refusal = Some((why, time.elapsed_secs())),
    }
}

/// A selected entity kind as the panel lists it.
fn kind_label(kind: game::EntityKind) -> String {
    match kind {
        game::EntityKind::Player => "Player".into(),
        game::EntityKind::Slime { size } => format!("Slime (size {size})"),
        game::EntityKind::Person { .. } => "Dwarf".into(),
        game::EntityKind::Body => "Body".into(),
    }
}

/// "Slime (size 2) ×3" per kind, sorted, from what the selection holds.
fn selection_lines(kinds: impl IntoIterator<Item = game::EntityKind>) -> Vec<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for kind in kinds {
        *counts.entry(kind_label(kind)).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(label, n)| format!("{label} ×{n}"))
        .collect()
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
    let tool = usize::try_from(index)
        .ok()
        .and_then(|i| TOOL_KEYS.get(i))
        .map(|(_, tool)| *tool);
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
    TOOL_KEYS
        .iter()
        .map(|(_, tool)| tool.label().to_string())
        .chain([cut])
        .collect()
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
    tool.and_then(|t| TOOL_KEYS.iter().position(|(_, k)| *k == t))
        .map_or(-1, |i| i as i32)
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
    editor: Option<Res<EditorState>>,
    mut policy: Option<ResMut<CursorPolicy>>,
    mut world_map: ResMut<WorldMapState>,
    status: Res<HudStatus>,
    mut phase: crate::session::SessionLocal<WheelPhase>,
    in_play: Option<Res<InPlay>>,
) {
    let phase = phase.get();
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
        && !mode.as_ref().is_some_and(|m| m.adventure())
        && !editor.as_ref().is_some_and(|e| e.active);
    let free = policy
        .as_ref()
        .is_none_or(|p| !p.blocks_game() && !p.ui_pointer);
    let cursor = window.cursor_position();
    let input = WheelInput {
        open_press: keys.just_pressed(WHEEL_KEY)
            && available
            && free
            && window.focused
            && cursor.is_some(),
        hold_press: keys.just_pressed(WHEEL_KEY),
        // A release lost with the focus counts as one.
        hold_release: keys.just_released(WHEEL_KEY)
            || (*phase == WheelPhase::Held && !keys.pressed(WHEEL_KEY)),
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
    let to = Screens {
        wheel: next.is_open(),
        ..Screens::of(ui)
    };
    let in_play = in_play.is_none_or(|e| e.0);
    set_screens(
        ui,
        to,
        &mut world_map,
        status.home_region,
        policy.as_mut(),
        in_play,
    );
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
    editor: Option<Res<'w, EditorState>>,
    follow: Option<Res<'w, Follow>>,
    top_down: Option<Res<'w, super::commander::top_down::TopDown>>,
    window: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    /// The cut the wheel's items were last captioned for; `None` before
    /// the first set.
    wheel_cut: crate::session::SessionLocal<'w, 's, Option<bool>>,
}

/// The tool, priority, cut (the wheel's captions), level and drag readout
/// → the UI, each only when changed.
fn sync_toolbar(ui: &slint_ui::GameUi, ui_scale: f32, p: &mut ToolbarParams) {
    // No home region (not joined yet) or Adventure: no tools, as with the
    // hotkeys.
    let adventure = p.mode.as_ref().is_some_and(|m| m.adventure())
        || p.editor.as_ref().is_some_and(|e| e.active);
    if ui.get_adventure() != adventure {
        ui.set_adventure(adventure);
    }
    let top_down = p.top_down.as_ref().is_some_and(|t| t.on);
    if ui.get_top_down() != top_down {
        ui.set_top_down(top_down);
    }
    let following = p.follow.as_ref().is_some_and(|f| f.0.is_some());
    if ui.get_following() != following {
        ui.set_following(following);
    }
    let available = p.mirror.as_ref().is_some_and(|m| m.region().is_some()) && !adventure;
    if ui.get_tools_available() != available {
        ui.set_tools_available(available);
    }
    // The cut is on by default; without a working level it shows as on.
    let cut = p.level.as_ref().is_none_or(|l| l.cut);
    if *p.wheel_cut.get() != Some(cut) {
        let items: Vec<slint_ui::RadialItem> = wheel_labels(cut)
            .into_iter()
            .map(|text| slint_ui::RadialItem {
                text: text.into(),
                enabled: true,
                ..Default::default()
            })
            .collect();
        ui.set_wheel_items(ModelRc::new(VecModel::from(items)));
        *p.wheel_cut.get() = Some(cut);
    }
    let Some(state) = p.designate.as_mut() else {
        return;
    };
    let active = tool_index(state.tool);
    if ui.get_active_tool() != active {
        ui.set_active_tool(active);
    }
    let tool_name = state.tool.map_or("", Tool::label);
    if ui.get_tool_name() != tool_name {
        ui.set_tool_name(tool_name.into());
    }
    if ui.get_priority() != state.priority as i32 {
        ui.set_priority(state.priority as i32);
    }

    let surface = match (&p.grid, &p.cam) {
        (Some(grid), Some(cam)) => focus_surface(grid, cam.focus).map(surface_level),
        _ => None,
    };
    let level: SharedString = p
        .level
        .as_ref()
        .and_then(|l| l.level)
        .map_or(String::new(), |l| level_text(l, surface))
        .into();
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
    let Some(cursor) = p.window.single().ok().and_then(|w| w.cursor_position()) else {
        return;
    };
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
    /// (generation, selected column, selected row) the tile info was last
    /// computed for.
    info_for: Option<(u64, i32, i32)>,
}

/// What the world map window reads.
#[derive(SystemParam)]
struct MapParams<'w, 's> {
    world_map: Res<'w, WorldMapState>,
    time: Res<'w, Time>,
    in_play: Option<Res<'w, InPlay>>,
    shown: crate::session::SessionLocal<'w, 's, MapShown>,
    history: ResMut<'w, HistoryView>,
    drain: Option<Res<'w, super::drain_notice::DrainNotice>>,
    requests: Option<Res<'w, ClientRequests>>,
    inspected: crate::session::SessionLocal<'w, 's, Inspected>,
    /// The arrival tabs as last pushed.
    tabs: crate::session::SessionLocal<'w, 's, Option<arrival_tabs::TabsView>>,
}

/// The person last requested for the inspector, and when (secs).
#[derive(Default)]
struct Inspected {
    asked: Option<(game::history::PersonId, f32)>,
}

/// The map picture, home and selected tile, tile info and status line → the
/// UI, each only when changed.
fn sync_world_map(ui: &GameUi, p: &mut MapParams) {
    let shown = p.shown.get();
    if p.world_map.is_changed() && shown.map.as_ref() != p.world_map.map.as_ref() {
        ui.set_map_available(p.world_map.map.is_some());
        shown.map = p.world_map.map.clone();
        shown.generation += 1;
    }
    let map = p.world_map.map.as_ref();
    // The map itself, its markers and its view are `renderer/map/full.rs`'s.

    let in_play = p.in_play.as_ref().is_none_or(|e| e.0);

    let (col, row) = (ui.get_selected_col(), ui.get_selected_row());
    // History changes (sites, the adventure choice and result) redo everything below.
    let history_changed = p.history.is_changed();
    if history_changed {
        shown.info_for = None;
    }

    // The start screen's panel stands beside the map while not in play, unless
    // the world has no populated site to pick.
    let panel = map.is_some() && !in_play;
    let no_site = no_site_to_pick(in_play, p.history.sites_received, &p.history.sites);
    let arrival_ui = panel && !no_site;
    if ui.get_arrival_visible() != arrival_ui {
        ui.set_arrival_visible(arrival_ui);
    }

    let info_for = Some((shown.generation, col, row));
    if shown.info_for != info_for {
        shown.info_for = info_for;
        let picked = map.and_then(|map| Some((map, tile_region(map, col, row)?)));
        let mut lines: Vec<String> =
            picked.map_or_else(Vec::new, |(map, rc)| tile_info_lines(map, rc));
        if let Some((_, rc)) = picked {
            if let Some(site) = p.history.sites.iter().find(|s| s.region == rc) {
                lines.push(site_line(site, &p.history.civ_name(site.civ)));
            }
        }
        let lines: Vec<SharedString> = lines.into_iter().map(Into::into).collect();
        ui.set_tile_lines(ModelRc::new(VecModel::from(lines)));
    }

    // In the map window and, with the map closed, on the game-level status line.
    let now = p.time.elapsed_secs();
    let status = p
        .history
        .visible_adventure_refusal(now)
        .map(|why| why.describe())
        .unwrap_or("");
    if ui.get_map_status() != status {
        ui.set_map_status(status.into());
    }
    // The map status wins; with none, the current history notice.
    let game_status = if status.is_empty() {
        p.history.current_notice(now).unwrap_or("")
    } else {
        status
    };
    if ui.get_game_status() != game_status {
        ui.set_game_status(game_status.into());
    }
    // The drain countdown: the text changes once a second, so compare before setting.
    let drain_line = p.drain.as_ref().map(|d| d.line()).unwrap_or_default();
    if ui.get_drain_line() != drain_line.as_str() {
        ui.set_drain_line(drain_line.into());
    }
    let can_end_adventure = p.history.adventuring.is_some() && in_play;
    if ui.get_can_end_adventure() != can_end_adventure {
        ui.set_can_end_adventure(can_end_adventure);
    }
    // The Adventure and Legends tabs.
    let tabs = p.tabs.get();
    if history_changed || tabs.is_none() {
        arrival_tabs::sync_arrival_tabs(ui, &p.history, tabs);
    }
}

/// Client state → panel properties, only when it changed (every set marks
/// the UI dirty and costs a repaint).
fn push_ui_state(
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    selection: Option<Res<Selection>>,
    entities: Query<(&SimEntity, &SimKind)>,
    mut toolbar: ToolbarParams,
    mut map: MapParams,
) {
    let Some(slint) = slint else { return };
    let ui = slint.overlay.ui();
    sync_toolbar(ui, slint.ui_scale, &mut toolbar);
    sync_world_map(ui, &mut map);
    let Some(selection) = selection else { return };
    let kinds: Vec<game::EntityKind> = entities
        .iter()
        .filter(|(e, _)| selection.contains(e.region, e.key))
        .map(|(_, kind)| kind.0)
        .collect();
    // Exactly one person selected: the inspector asks for (and shows) their info.
    let person = match kinds[..] {
        [game::EntityKind::Person { person }] => Some(person),
        _ => None,
    };
    let now = map.time.elapsed_secs();
    if let Some(person) = person {
        let inspected = map.inspected.get();
        let due = map.history.person_refresh_due(person, now)
            && inspected.asked.is_none_or(|(p, at)| {
                p != person || history_view::person_info_refresh_due(Some(at), now)
            });
        if due
            && map
                .requests
                .as_ref()
                .is_some_and(|r| r.0.send(ClientRequest::RequestPersonInfo(person)).is_ok())
        {
            inspected.asked = Some((person, now));
        }
    }
    if !(selection.is_changed() || map.history.is_changed()) {
        return;
    }
    let lines: Vec<SharedString> = match person {
        Some(person) => {
            let info = map.history.people.get(&person).map(|(i, _)| i);
            history_view::person_lines(person, info)
                .into_iter()
                .map(Into::into)
                .collect()
        }
        None => selection_lines(kinds).into_iter().map(Into::into).collect(),
    };
    ui.set_selected_count(selection.0.len() as i32);
    ui.set_selection_lines(ModelRc::new(VecModel::from(lines)));
}

/// Redraw what changed and copy that rect into the texture, un-premultiplied
/// (the composite pass blends straight alpha, as `bevy_ui` did). The texture follows the window size.
pub(crate) fn render_slint_layer<C: ComponentHandle + 'static>(
    slint: Option<NonSendMut<SlintLayer<C>>>,
    mut images: ResMut<Assets<Image>>,
    state: Option<Res<State<AppState>>>,
    in_play: Option<Res<InPlay>>,
    world_map: Option<Res<WorldMapState>>,
    full_map: Option<Res<super::map::full::FullMapOpen>>,
) {
    let Some(mut slint) = slint else { return };
    let full_map_open = full_map.is_some_and(|m| m.0);
    let hidden = frontend_hides(
        slint.hidden_by_frontend,
        state.as_deref(),
        in_play.as_deref(),
        world_map.as_deref(),
        full_map_open,
    );
    if hidden {
        if !slint.blanked {
            if let Some(mut image) = images.get_mut(&slint.target) {
                if let Some(data) = image.data.as_mut() {
                    data.fill(0);
                }
            }
            slint.blanked = true;
        }
        return;
    }
    let mut dirty = slint.overlay.render();
    if std::mem::take(&mut slint.blanked) {
        // Coming back from the frontend: the overlay's buffer still holds the
        // last frame, so upload all of it.
        let (width, height) = slint.overlay.size();
        dirty = Some(slint_ui::Dirty {
            x: 0,
            y: 0,
            width,
            height,
        });
    }
    let Some(dirty) = dirty else { return };
    let Some(mut image) = images.get_mut(&slint.target) else {
        return;
    };
    let (width, height) = slint.overlay.size();
    let size = Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    if image.texture_descriptor.size != size {
        image.resize(size);
    }
    let Some(data) = image.data.as_mut() else {
        return;
    };
    let pixels = slint.overlay.pixels();
    for y in dirty.y..dirty.y + dirty.height {
        let row = (y * width) as usize;
        for x in dirty.x..dirty.x + dirty.width {
            let i = row + x as usize;
            data[i * 4..i * 4 + 4].copy_from_slice(&unpremultiply(pixels[i]));
        }
    }
}

fn unpremultiply(
    p: slint_ui::slint::platform::software_renderer::PremultipliedRgbaColor,
) -> [u8; 4] {
    let a = p.alpha as u16;
    if a == 0 {
        return [0; 4];
    }
    let un = |c: u8| ((c as u16 * 255 + a / 2) / a).min(255) as u8;
    [un(p.red), un(p.green), un(p.blue), p.alpha]
}

/// Whether the start screen has no site to pick (out of play, the sites told, none of
/// them populated): the arrival panel is then not shown. Before the sites arrive
/// nothing is known yet.
fn no_site_to_pick(in_play: bool, sites_received: bool, sites: &[game::SiteMarker]) -> bool {
    !in_play && sites_received && !sites.iter().any(history_view::populated)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: asset viewer panicked at start when launched (fac81288)
    /// Startup systems may use what a layer's `with_init` inserted (the asset
    /// viewer's `scan_at_start` reads its `Actions`): layers are set up first.
    #[test]
    fn layer_init_runs_before_startup_systems() {
        struct Marker;
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::asset::AssetPlugin::default(),
            bevy::input::InputPlugin,
        ))
        .init_asset::<Image>()
        .add_plugins(
            SlintLayerPlugin::new(HudUi::new).with_init(|_, world| world.insert_non_send(Marker)),
        );
        // Registered after the plugin, as an app's own Startup systems are.
        app.add_systems(Startup, |_marker: NonSend<Marker>| {});
        app.update();
        assert!(app.world().get_non_send::<SlintLayer<HudUi>>().is_some());
    }
}
