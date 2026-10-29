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
//!   un-premultiplied into a window-sized `Image` shown by an `ImageNode`
//!   over everything else, one texel per physical pixel.
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
use bevy::window::PrimaryWindow;
use slint_ui::slint::platform::{Key as SlintKey, PointerEventButton, WindowEvent};
use slint_ui::slint::{ComponentHandle, LogicalPosition, ModelRc, SharedString, VecModel};
use slint_ui::{GameUi, Overlay};

use super::bridge::{SimEntity, SimKind};
use super::commander::designate::{DesignateState, DragReadout, Tool, TOOL_KEYS};
use super::commander::height::HeightGrid;
use super::commander::level::{focus_surface, surface_level, WorkingLevel};
use super::commander::selection::Selection;
use super::commander::tiles::TileMirror;
use super::commander::CommanderCamera;
use super::cursor::CursorPolicy;
use super::hud::HudStatus;

/// Shows and hides the panel.
pub const SLINT_PANEL_KEY: KeyCode = KeyCode::F9;
/// UI pixels per window logical pixel. The widgets are pixel-exact at a
/// total scale (this × the window's scale factor) of 1 and 2. Overridden by
/// `VOXEL_UI_SCALE`.
pub const UI_SCALE: f32 = 1.0;
/// Logical pixels one wheel notch scrolls.
pub const WHEEL_LINE_PX: f32 = 40.0;
/// Above the HUD, the minimap and bevy_ui widgets.
const OVERLAY_Z: i32 = 1_000;

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
}

impl<C> SlintLayerPlugin<C> {
    /// `make` is the component's generated `new` (e.g. `GameUi::new`).
    pub fn new(make: fn() -> Result<C, slint_ui::slint::PlatformError>) -> Self {
        Self { make, init: |_, _| {} }
    }

    pub fn with_init(mut self, init: fn(&C, &mut World)) -> Self {
        self.init = init;
        self
    }
}

impl<C: ComponentHandle + 'static> Plugin for SlintLayerPlugin<C> {
    fn build(&self, app: &mut App) {
        let (make, init) = (self.make, self.init);
        app.add_systems(Startup, move |world: &mut World| setup_slint_layer(world, make, init))
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
            .add_systems(Update, (toggle_panel, apply_ui_actions, push_ui_state).chain());
    }
}

fn connect_game_ui(ui: &GameUi, world: &mut World) {
    let actions = Rc::new(RefCell::new(Vec::new()));
    let queue = actions.clone();
    ui.on_clear_selection(move || queue.borrow_mut().push(UiAction::ClearSelection));
    world.insert_non_send(GameUiActions(actions));
}

fn setup_slint_layer<C: ComponentHandle + 'static>(
    world: &mut World,
    make: fn() -> Result<C, slint_ui::slint::PlatformError>,
    init: fn(&C, &mut World),
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
    world.spawn((
        Name::new("slint overlay"),
        Pickable::IGNORE,
        ImageNode::new(target.clone()),
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        GlobalZIndex(OVERLAY_Z),
    ));
    world.insert_non_send(SlintLayer { overlay, ui_scale: ui_scale(), target, owns_pointer: false });
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
    policy: Option<ResMut<CursorPolicy>>,
) {
    let Some(mut slint) = slint else { return };
    let Ok(window) = window.single() else { return };
    let scale = window.scale_factor() * slint.ui_scale;
    slint.overlay.resize(window.physical_width(), window.physical_height(), scale);
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
    let over_ui = cursor.is_some_and(|c| {
        let p = c * window.scale_factor();
        overlay.opaque_at(p.x as u32, p.y as u32)
    });

    for event in buttons.read() {
        let Some(p) = ui_pos else { continue };
        let position = LogicalPosition::new(p.x, p.y);
        let button = pointer_button(event.button);
        match event.state {
            ButtonState::Pressed => {
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

    let owns = over_ui || pointer.captured;
    slint.owns_pointer = owns;
    if let Some(mut policy) = policy.filter(|p| p.ui_pointer != owns) {
        policy.ui_pointer = owns;
    }
}

fn toggle_panel(keys: Res<ButtonInput<KeyCode>>, slint: Option<NonSend<SlintLayer<GameUi>>>) {
    let Some(slint) = slint else { return };
    if keys.just_pressed(SLINT_PANEL_KEY) {
        let ui = slint.overlay.ui();
        ui.set_panel_open(!ui.get_panel_open());
    }
}

fn apply_ui_actions(actions: Option<NonSend<GameUiActions>>, mut selection: Option<ResMut<Selection>>) {
    let Some(actions) = actions else { return };
    for action in actions.0.borrow_mut().drain(..) {
        match action {
            UiAction::ClearSelection => {
                if let Some(selection) = selection.as_mut() {
                    selection.0.clear();
                }
            }
        }
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

/// What a tool bar click asked for. Queued apart from [`UiAction`] and
/// applied by [`sync_toolbar`], which also connects the callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarAction {
    SelectTool(Option<Tool>),
    SetPriority(u8),
    /// The "Hide above" button: flip the cutaway, as `C` does.
    ToggleCut,
}

thread_local! {
    /// Filled by the tool bar's callbacks during event dispatch (main
    /// thread), drained by [`sync_toolbar`].
    static TOOLBAR_ACTIONS: RefCell<Vec<ToolbarAction>> = const { RefCell::new(Vec::new()) };
}

/// Tool bar button `index` (in [`TOOL_KEYS`] order) clicked; -1 (or any
/// index without a tool) puts the tool down.
pub fn tool_action(index: i32) -> ToolbarAction {
    let tool = usize::try_from(index).ok().and_then(|i| TOOL_KEYS.get(i)).map(|(_, tool)| *tool);
    ToolbarAction::SelectTool(tool)
}

/// Apply one tool bar click. Tools are picked only with a home region
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
        ToolbarAction::SetPriority(priority) => state.priority = priority,
        ToolbarAction::ToggleCut => {
            if let Some(level) = level {
                level.cut = !level.cut;
            }
        }
    }
}

/// A tool bar button's text: the tool and its hotkey, "[G] Dig".
fn tool_button_text(tool: Tool) -> String {
    format!("[{}] {}", tool.key_label(), tool.label())
}

/// The tool bar index of `tool`, -1 for none.
fn tool_index(tool: Option<Tool>) -> i32 {
    tool.and_then(|t| TOOL_KEYS.iter().position(|(_, k)| *k == t)).map_or(-1, |i| i as i32)
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

/// What the tool bar and its readouts read and drive. All optional: the
/// commander resources exist only in commander mode.
#[derive(SystemParam)]
struct ToolbarParams<'w, 's> {
    designate: Option<ResMut<'w, DesignateState>>,
    readout: Option<Res<'w, DragReadout>>,
    level: Option<ResMut<'w, WorkingLevel>>,
    grid: Option<Res<'w, HeightGrid>>,
    cam: Option<Res<'w, CommanderCamera>>,
    mirror: Option<Res<'w, TileMirror>>,
    window: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    /// The callbacks are connected.
    connected: Local<'s, bool>,
}

/// Tool bar clicks → [`DesignateState`] / [`WorkingLevel`] (as the hotkeys
/// set them), then the tool, priority, cut, level and drag readout → the
/// UI, each only when changed.
fn sync_toolbar(ui: &slint_ui::GameUi, ui_scale: f32, p: &mut ToolbarParams) {
    if !*p.connected {
        ui.on_tool_clicked(|i| TOOLBAR_ACTIONS.with(|q| q.borrow_mut().push(tool_action(i))));
        ui.on_priority_clicked(|priority| {
            let priority = priority.clamp(1, game::MAX_PRIORITY as i32) as u8;
            TOOLBAR_ACTIONS.with(|q| q.borrow_mut().push(ToolbarAction::SetPriority(priority)));
        });
        ui.on_cut_clicked(|| TOOLBAR_ACTIONS.with(|q| q.borrow_mut().push(ToolbarAction::ToggleCut)));
        let labels: Vec<SharedString> = TOOL_KEYS.iter().map(|(_, t)| tool_button_text(*t).into()).collect();
        ui.set_tool_labels(ModelRc::new(VecModel::from(labels)));
        *p.connected = true;
    }
    let actions = TOOLBAR_ACTIONS.with(|q| std::mem::take(&mut *q.borrow_mut()));
    // No home region (cell mode, or not joined yet): no tools, as with the hotkeys.
    let available = p.mirror.as_ref().is_some_and(|m| m.region().is_some());
    if ui.get_tools_available() != available {
        ui.set_tools_available(available);
    }
    let Some(state) = p.designate.as_mut() else { return };
    for action in actions {
        apply_toolbar_action(action, available, state, p.level.as_deref_mut());
    }
    let active = tool_index(state.tool);
    if ui.get_active_tool() != active {
        ui.set_active_tool(active);
    }
    if ui.get_priority() != state.priority as i32 {
        ui.set_priority(state.priority as i32);
    }

    // The cut is on by default; without a working level it shows as on.
    let cut = p.level.as_ref().is_none_or(|l| l.cut);
    if ui.get_cut_above() != cut {
        ui.set_cut_above(cut);
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

/// Client state → panel properties, only when it changed (every set marks
/// the UI dirty and costs a repaint).
fn push_ui_state(
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    status: Res<HudStatus>,
    selection: Option<Res<Selection>>,
    entities: Query<(&SimEntity, &SimKind)>,
    mut toolbar: ToolbarParams,
) {
    let Some(slint) = slint else { return };
    let ui = slint.overlay.ui();
    sync_toolbar(ui, slint.ui_scale, &mut toolbar);
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
/// (`ImageNode` blends straight alpha). The texture follows the window size.
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
    use slint_ui::slint::platform::software_renderer::PremultipliedRgbaColor;

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
        assert_eq!(tool_button_text(Tool::Dig), "[G] Dig");
        assert_eq!(tool_button_text(Tool::Build), "[B] Build");
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
