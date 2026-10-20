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
//! Commander mode only. Design and trade-offs (CPU raster, blending,
//! licences): docs/superpowers/specs/2026-10-20-slint-ui-library-in-bevy-investigation.md.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::{MouseButtonInput, MouseScrollUnit, MouseWheel};
use bevy::input::ButtonState;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::window::PrimaryWindow;
use slint_ui::slint::platform::{Key as SlintKey, PointerEventButton, WindowEvent};
use slint_ui::slint::{LogicalPosition, ModelRc, SharedString, VecModel};
use slint_ui::Overlay;

use super::bridge::{SimEntity, SimKind};
use super::commander::selection::Selection;
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

/// The Slint side, `!Send` (kept as a non-send resource on the main thread).
struct SlintUi {
    overlay: Overlay,
    /// Filled by the component's callbacks during event dispatch.
    actions: Rc<RefCell<Vec<UiAction>>>,
    ui_scale: f32,
}

/// The texture the overlay is drawn into.
#[derive(Resource)]
struct SlintTarget(Handle<Image>);

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
        app.add_systems(Startup, setup_slint_overlay)
            .add_systems(PreUpdate, slint_input.after(bevy::input::InputSystems))
            .add_systems(Update, (toggle_panel, apply_ui_actions, push_ui_state).chain())
            .add_systems(PostUpdate, render_slint_overlay);
    }
}

fn setup_slint_overlay(world: &mut World) {
    let overlay = match Overlay::new() {
        Ok(overlay) => overlay,
        Err(e) => {
            error!("[slint] game UI unavailable: {e}");
            return;
        }
    };
    let actions = Rc::new(RefCell::new(Vec::new()));
    let queue = actions.clone();
    overlay.ui().on_clear_selection(move || queue.borrow_mut().push(UiAction::ClearSelection));
    world.insert_non_send(SlintUi { overlay, actions, ui_scale: ui_scale() });

    let mut image = Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    image.sampler = ImageSampler::nearest();
    let handle = world.resource_mut::<Assets<Image>>().add(image);
    world.spawn((
        Name::new("slint overlay"),
        Pickable::IGNORE,
        ImageNode::new(handle.clone()),
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        GlobalZIndex(OVERLAY_Z),
    ));
    world.insert_resource(SlintTarget(handle));
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
fn slint_input(
    slint: Option<NonSendMut<SlintUi>>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut buttons: MessageReader<MouseButtonInput>,
    mut wheel: MessageReader<MouseWheel>,
    mut keys: MessageReader<KeyboardInput>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut pointer: Local<PointerState>,
    mut policy: ResMut<CursorPolicy>,
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
    if policy.ui_pointer != owns {
        policy.ui_pointer = owns;
    }
}

fn toggle_panel(keys: Res<ButtonInput<KeyCode>>, slint: Option<NonSend<SlintUi>>) {
    let Some(slint) = slint else { return };
    if keys.just_pressed(SLINT_PANEL_KEY) {
        let ui = slint.overlay.ui();
        ui.set_panel_open(!ui.get_panel_open());
    }
}

fn apply_ui_actions(slint: Option<NonSend<SlintUi>>, mut selection: Option<ResMut<Selection>>) {
    let Some(slint) = slint else { return };
    for action in slint.actions.borrow_mut().drain(..) {
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

/// Client state → panel properties, only when it changed (every set marks
/// the UI dirty and costs a repaint).
fn push_ui_state(
    slint: Option<NonSend<SlintUi>>,
    status: Res<HudStatus>,
    selection: Option<Res<Selection>>,
    entities: Query<(&SimEntity, &SimKind)>,
) {
    let Some(slint) = slint else { return };
    let ui = slint.overlay.ui();
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
fn render_slint_overlay(
    slint: Option<NonSendMut<SlintUi>>,
    target: Option<Res<SlintTarget>>,
    mut images: ResMut<Assets<Image>>,
) {
    let (Some(mut slint), Some(target)) = (slint, target) else { return };
    let Some(dirty) = slint.overlay.render() else { return };
    let Some(mut image) = images.get_mut(&target.0) else { return };
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
}
