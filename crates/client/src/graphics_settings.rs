//! The player's graphics settings (the Settings window's Graphics tab,
//! `ui/settings.slint`, opened from the main menu, the Esc menu and the menu
//! column): the render distance — how far terrain is requested and drawn,
//! fog past it — and the full-detail distance, where sim chunks hand over to
//! their block-scale coarse twins (`meshing/coarse.rs`); vsync; the body
//! view's field of view, its walking bob and its sprint widening
//! (`renderer/commander/view_feel.rs`); and, on the General tab, the
//! interface scale ([`OverlayScale`]).
//!
//! Kept between runs: native `graphics_settings.json` in the client's data
//! directory (`player_name::data_dir`), the browser `localStorage[`
//! [`STORAGE_KEY`]`]`. An environment that pins the view
//! ([`crate::world_scale::view_pinned_by_env`], `VOXEL_NEAR_DETAIL_M`) wins
//! over the saved values at start; a slider moved afterwards still applies.
//!
//! A change applies at once to the fog and the far plane
//! ([`ViewDistance`]) and the detail switch ([`NearDetail`]); the LOD ring
//! table — a re-subscription on the server — and the save wait until the
//! slider has rested [`SETTLE_SECS`], so a drag does not request every ring
//! table it passes.
//!
//! [`NearDetail`]: crate::renderer::voxel_pull::NearDetail

use std::cell::RefCell;
use std::rc::Rc;

use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::window::{PresentMode, PrimaryWindow};

use crate::renderer::atmosphere::{ViewDistance, squared_haze_density};
use crate::renderer::slint_overlay::{
    OverlayScale, SlintLayer, UI_SCALE, UI_SCALE_MAX, UI_SCALE_MIN, UI_SCALE_STEP,
    ui_scale_pinned_by_env,
};
use crate::world_scale::WorldScale;

/// Render distance slider range and step, metres. The step is the play
/// region (256 m): the ring table is whole regions anyway.
pub const VIEW_DISTANCE_MIN_M: f32 = 256.0;
pub const VIEW_DISTANCE_MAX_M: f32 = 8_192.0;
pub const VIEW_DISTANCE_STEP_M: f32 = 256.0;
/// Full-detail distance slider range and step, metres. Past the simulated
/// near window there is no full-detail terrain to keep, so more than about
/// a region and a half buys nothing.
pub const DETAIL_DISTANCE_MIN_M: f32 = 32.0;
pub const DETAIL_DISTANCE_MAX_M: f32 = 512.0;
pub const DETAIL_DISTANCE_STEP_M: f32 = 32.0;
/// Field of view slider range and step, degrees (vertical).
pub const FOV_MIN_DEG: f32 = 30.0;
pub const FOV_MAX_DEG: f32 = 110.0;
pub const FOV_STEP_DEG: f32 = 1.0;
/// Interface scale slider range and step, percent of the default
/// ([`UI_SCALE`] = 100 %): a stepped slider, since the widgets are
/// pixel-exact only at a scale of 1 and 2 (its 67 % and 133 % stops).
pub const UI_SCALE_MIN_PERCENT: f32 = UI_SCALE_MIN / UI_SCALE * 100.0;
pub const UI_SCALE_MAX_PERCENT: f32 = UI_SCALE_MAX / UI_SCALE * 100.0;
pub const UI_SCALE_STEP_PERCENT: f32 = UI_SCALE_STEP / UI_SCALE * 100.0;
/// How long a slider must rest before its ring table is requested and the
/// settings are saved.
pub const SETTLE_SECS: f32 = 0.4;
/// The browser's `localStorage` key.
pub const STORAGE_KEY: &str = "mmo.graphics_settings";
/// The native file's name, in `player_name::data_dir`.
pub const FILE_NAME: &str = "graphics_settings.json";

/// The graphics settings in effect.
#[derive(Resource, Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GraphicsSettings {
    /// Nothing is drawn past this, metres: the LOD rings end here and the fog
    /// is opaque by then.
    pub view_distance_m: f32,
    /// Terrain keeps its full detail this far, metres; block-scale beyond.
    pub detail_distance_m: f32,
    /// Present in step with the display's refresh (no tearing, the frame
    /// rate capped at it). Native only: the browser paces its own frames.
    pub vsync: bool,
    /// The body view's vertical field of view, degrees.
    pub fov_deg: f32,
    /// The first-person view bobs with each step.
    pub view_bobbing: bool,
    /// The field of view widens while sprinting.
    pub sprint_fov: bool,
    /// The interface's size: UI pixels per window logical pixel
    /// ([`OverlayScale`]), a whole number of [`UI_SCALE_STEP`]s. The slider
    /// shows it as a percentage of [`UI_SCALE`].
    pub ui_scale: f32,
}

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            view_distance_m: game::lod::LOD_HORIZON_UNITS / game::movement::UNITS_PER_METER,
            detail_distance_m: crate::renderer::voxel_pull::NEAR_DETAIL_DISTANCE_M,
            vsync: true,
            fov_deg: crate::renderer::commander::first_person::FIRST_PERSON_FOV_Y_DEG,
            view_bobbing: true,
            sprint_fov: true,
            ui_scale: UI_SCALE,
        }
    }
}

impl GraphicsSettings {
    /// Each value clamped to its slider's range (a hand-edited file, an old
    /// range).
    pub fn clamped(self) -> Self {
        Self {
            view_distance_m: clamp_or(
                self.view_distance_m,
                VIEW_DISTANCE_MIN_M,
                VIEW_DISTANCE_MAX_M,
            ),
            detail_distance_m: clamp_or(
                self.detail_distance_m,
                DETAIL_DISTANCE_MIN_M,
                DETAIL_DISTANCE_MAX_M,
            ),
            vsync: self.vsync,
            fov_deg: if self.fov_deg.is_finite() {
                self.fov_deg.clamp(FOV_MIN_DEG, FOV_MAX_DEG)
            } else {
                Self::default().fov_deg
            },
            view_bobbing: self.view_bobbing,
            sprint_fov: self.sprint_fov,
            ui_scale: snap_ui_scale(self.ui_scale),
        }
    }

    /// The interface scale as the slider shows it.
    pub fn ui_scale_percent(&self) -> f32 {
        self.ui_scale / UI_SCALE * 100.0
    }
}

/// `scale` on the slider's nearest stop (not a number: the default).
fn snap_ui_scale(scale: f32) -> f32 {
    if !scale.is_finite() {
        return UI_SCALE;
    }
    ((scale / UI_SCALE_STEP).round() * UI_SCALE_STEP).clamp(UI_SCALE_MIN, UI_SCALE_MAX)
}

fn clamp_or(v: f32, min: f32, max: f32) -> f32 {
    if v.is_finite() {
        v.clamp(min, max)
    } else {
        max
    }
}

/// The settings at start, and the world scale and view distance they give:
/// the saved values unless the environment pins them, else what the
/// environment resolved to.
pub fn startup(
    scale: WorldScale,
    view: ViewDistance,
) -> (WorldScale, ViewDistance, GraphicsSettings) {
    let saved = load();
    // The render benchmark measures the environment's view, never a saved one.
    #[cfg(not(target_arch = "wasm32"))]
    let bench = crate::renderer::render_bench::enabled();
    #[cfg(target_arch = "wasm32")]
    let bench = false;
    let pinned = crate::world_scale::view_pinned_by_env() || bench;
    let (scale, view, view_distance_m) = match saved {
        Some(saved) if !pinned => {
            let m = saved.clamped().view_distance_m;
            let scale = WorldScale {
                lod_table: crate::world_scale::ring_table_for_distance_m(m),
                ..scale
            };
            (scale, ViewDistance::for_reach_m(m), m)
        }
        _ => (scale, view, view.far / game::movement::UNITS_PER_METER),
    };
    let detail_distance_m = match saved {
        Some(saved) if !crate::renderer::voxel_pull::near_detail_pinned_by_env() => {
            saved.clamped().detail_distance_m
        }
        _ => crate::renderer::voxel_pull::near_detail_distance_m(),
    };
    // `VOXEL_VSYNC` (and the render benchmark) pin the present mode at start
    // (`present_mode_at_start`); the box then shows what is in effect.
    let vsync = match vsync_pinned_at_start() {
        Some(mode) => !matches!(mode, PresentMode::AutoNoVsync | PresentMode::Immediate),
        None => saved.map_or(GraphicsSettings::default().vsync, |s| s.vsync),
    };
    let rest = saved.map(GraphicsSettings::clamped).unwrap_or_default();
    let (fov_deg, view_bobbing, sprint_fov) = (rest.fov_deg, rest.view_bobbing, rest.sprint_fov);
    // `VOXEL_UI_SCALE` pins the scale at start (`apply_ui_scale` leaves it);
    // the slider then shows its nearest stop.
    let ui_scale = match ui_scale_pinned_by_env() {
        Some(scale) => snap_ui_scale(scale),
        None => rest.ui_scale,
    };
    (
        scale,
        view,
        GraphicsSettings {
            view_distance_m,
            detail_distance_m,
            vsync,
            fov_deg,
            view_bobbing,
            sprint_fov,
            ui_scale,
        },
    )
}

/// The present mode the environment asks for, if it does: `VOXEL_VSYNC=on|1|true`
/// = vsync (`AutoVsync`); `immediate` / `mailbox` / `fifo` = that mode exactly
/// (fails at startup if the surface lacks it); any other value (`off`) =
/// `AutoNoVsync`. Unset or empty = no pin: the saved setting decides. The
/// render benchmark always presents unsynced. Measuring recipes set
/// `VOXEL_VSYNC=off` — a vsync'd frame sits at the refresh rate however much
/// headroom it has.
pub fn vsync_pinned_at_start() -> Option<PresentMode> {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::renderer::render_bench::enabled() {
        return Some(PresentMode::AutoNoVsync);
    }
    let value = std::env::var("VOXEL_VSYNC")
        .ok()
        .filter(|v| !v.trim().is_empty())?;
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" => PresentMode::AutoVsync,
        "immediate" => PresentMode::Immediate,
        "mailbox" => PresentMode::Mailbox,
        "fifo" => PresentMode::Fifo,
        _ => PresentMode::AutoNoVsync,
    })
}

/// The window's present mode at start: the environment's, else the setting's.
pub fn present_mode_at_start(settings: &GraphicsSettings) -> PresentMode {
    vsync_pinned_at_start().unwrap_or_else(|| present_mode_for(settings.vsync))
}

/// On is strict `Fifo` (every surface has it), not `AutoVsync`: that one
/// prefers `FifoRelaxed`, which presents a late frame at once — after a long
/// frame the rate then ran well past the refresh for a while.
fn present_mode_for(vsync: bool) -> PresentMode {
    if vsync {
        PresentMode::Fifo
    } else {
        PresentMode::AutoNoVsync
    }
}

/// The saved settings, if any (unreadable = none, warned).
pub fn load() -> Option<GraphicsSettings> {
    #[cfg(not(target_arch = "wasm32"))]
    let text = {
        let path = crate::player_name::data_dir()?.join(FILE_NAME);
        std::fs::read_to_string(path).ok()?
    };
    #[cfg(target_arch = "wasm32")]
    let text = {
        let storage = web_sys::window()?.local_storage().ok().flatten()?;
        storage.get_item(STORAGE_KEY).ok().flatten()?
    };
    match serde_json::from_str::<GraphicsSettings>(&text) {
        Ok(settings) => Some(settings),
        Err(e) => {
            log::warn!("[client] ignoring the saved graphics settings: {e}");
            None
        }
    }
}

/// Keeps `settings` for the next run. A failure is logged, never fatal.
pub fn save(settings: &GraphicsSettings) {
    let Ok(text) = serde_json::to_string_pretty(settings) else {
        return;
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(dir) = crate::player_name::data_dir() else {
            log::warn!("[client] no home directory: the graphics settings are not kept");
            return;
        };
        let path = dir.join(FILE_NAME);
        let tmp = dir.join(format!("{FILE_NAME}.tmp"));
        let written = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&tmp, text))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            log::warn!(
                "[client] cannot keep the graphics settings in {}: {e}",
                path.display()
            );
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let storage = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
        match storage {
            Some(storage) if storage.set_item(STORAGE_KEY, &text).is_ok() => {}
            _ => {
                log::warn!("[client] localStorage unavailable: the graphics settings are not kept")
            }
        }
    }
}

/// One slider edit from either Settings window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingsEdit {
    ViewDistance(f32),
    DetailDistance(f32),
    Vsync(bool),
    Fov(f32),
    ViewBobbing(bool),
    SprintFov(bool),
    UiScale(f32),
}

/// Edits queued by the Slint callbacks of both layers (main thread only).
#[derive(Clone, Default)]
pub struct SettingsEdits(pub Rc<RefCell<Vec<SettingsEdit>>>);

/// The shared edit queue, made on first use (each layer's init calls it; the
/// game UI is made again per session).
pub fn edits(world: &mut World) -> SettingsEdits {
    if world.get_non_send::<SettingsEdits>().is_none() {
        world.insert_non_send(SettingsEdits::default());
    }
    world.non_send::<SettingsEdits>().clone()
}

/// Wire a Settings window's callbacks (`settings-*-edited`) to the queue.
macro_rules! connect_settings {
    ($ui:expr, $world:expr) => {{
        let queue = $crate::graphics_settings::edits($world);
        let q = queue.clone();
        $ui.on_settings_view_distance_edited(move |v| {
            q.0.borrow_mut()
                .push($crate::graphics_settings::SettingsEdit::ViewDistance(v))
        });
        let q = queue.clone();
        $ui.on_settings_detail_distance_edited(move |v| {
            q.0.borrow_mut()
                .push($crate::graphics_settings::SettingsEdit::DetailDistance(v))
        });
        let q = queue.clone();
        $ui.on_settings_vsync_edited(move |on| {
            q.0.borrow_mut()
                .push($crate::graphics_settings::SettingsEdit::Vsync(on))
        });
        let q = queue.clone();
        $ui.on_settings_fov_edited(move |v| {
            q.0.borrow_mut()
                .push($crate::graphics_settings::SettingsEdit::Fov(v))
        });
        let q = queue.clone();
        $ui.on_settings_view_bobbing_edited(move |on| {
            q.0.borrow_mut()
                .push($crate::graphics_settings::SettingsEdit::ViewBobbing(on))
        });
        let q = queue.clone();
        $ui.on_settings_sprint_fov_edited(move |on| {
            q.0.borrow_mut()
                .push($crate::graphics_settings::SettingsEdit::SprintFov(on))
        });
        let q = queue;
        $ui.on_settings_ui_scale_edited(move |v| {
            q.0.borrow_mut()
                .push($crate::graphics_settings::SettingsEdit::UiScale(v))
        });
        $ui.set_settings_ui_scale_min($crate::graphics_settings::UI_SCALE_MIN_PERCENT);
        $ui.set_settings_ui_scale_max($crate::graphics_settings::UI_SCALE_MAX_PERCENT);
        $ui.set_settings_ui_scale_step($crate::graphics_settings::UI_SCALE_STEP_PERCENT);
        $ui.set_settings_fov_min($crate::graphics_settings::FOV_MIN_DEG);
        $ui.set_settings_fov_max($crate::graphics_settings::FOV_MAX_DEG);
        $ui.set_settings_fov_step($crate::graphics_settings::FOV_STEP_DEG);
        $ui.set_settings_view_distance_min($crate::graphics_settings::VIEW_DISTANCE_MIN_M);
        $ui.set_settings_view_distance_max($crate::graphics_settings::VIEW_DISTANCE_MAX_M);
        $ui.set_settings_view_distance_step($crate::graphics_settings::VIEW_DISTANCE_STEP_M);
        $ui.set_settings_detail_distance_min($crate::graphics_settings::DETAIL_DISTANCE_MIN_M);
        $ui.set_settings_detail_distance_max($crate::graphics_settings::DETAIL_DISTANCE_MAX_M);
        $ui.set_settings_detail_distance_step($crate::graphics_settings::DETAIL_DISTANCE_STEP_M);
    }};
}
pub(crate) use connect_settings;

/// Show the settings' values in a Settings window (only when they differ,
/// so a drag in progress is never fought).
macro_rules! push_settings {
    ($ui:expr, $settings:expr) => {{
        let s: &$crate::graphics_settings::GraphicsSettings = $settings;
        if $ui.get_settings_view_distance() != s.view_distance_m {
            $ui.set_settings_view_distance(s.view_distance_m);
        }
        if $ui.get_settings_detail_distance() != s.detail_distance_m {
            $ui.set_settings_detail_distance(s.detail_distance_m);
        }
        if $ui.get_settings_vsync() != s.vsync {
            $ui.set_settings_vsync(s.vsync);
        }
        if $ui.get_settings_fov() != s.fov_deg {
            $ui.set_settings_fov(s.fov_deg);
        }
        if $ui.get_settings_view_bobbing() != s.view_bobbing {
            $ui.set_settings_view_bobbing(s.view_bobbing);
        }
        if $ui.get_settings_sprint_fov() != s.sprint_fov {
            $ui.set_settings_sprint_fov(s.sprint_fov);
        }
        if $ui.get_settings_ui_scale() != s.ui_scale_percent() {
            $ui.set_settings_ui_scale(s.ui_scale_percent());
        }
    }};
}

pub struct GraphicsSettingsPlugin;

impl Plugin for GraphicsSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GraphicsSettings>().add_systems(
            Update,
            (
                take_edits,
                apply_settings,
                apply_vsync,
                apply_ui_scale,
                refresh_distance_fog,
                push_to_game_ui,
                push_to_frontend,
            )
                .chain(),
        );
    }
}

/// The queued slider edits into [`GraphicsSettings`].
fn take_edits(edits: Option<NonSend<SettingsEdits>>, mut settings: ResMut<GraphicsSettings>) {
    let Some(edits) = edits else { return };
    let queued = std::mem::take(&mut *edits.0.borrow_mut());
    if queued.is_empty() {
        return;
    }
    let mut next = *settings;
    for edit in queued {
        match edit {
            SettingsEdit::ViewDistance(m) => next.view_distance_m = m,
            SettingsEdit::DetailDistance(m) => next.detail_distance_m = m,
            SettingsEdit::Vsync(on) => next.vsync = on,
            SettingsEdit::Fov(deg) => next.fov_deg = deg,
            SettingsEdit::ViewBobbing(on) => next.view_bobbing = on,
            SettingsEdit::SprintFov(on) => next.sprint_fov = on,
            SettingsEdit::UiScale(percent) => next.ui_scale = percent / 100.0 * UI_SCALE,
        }
    }
    settings.set_if_neq(next.clamped());
}

/// A settings change: the fog and far plane at once; once the sliders have
/// rested, the ring table (the [`WorldScale`] new sessions start with, and
/// the running session's sim) and the save.
fn apply_settings(
    settings: Res<GraphicsSettings>,
    mut view: ResMut<ViewDistance>,
    mut scale: ResMut<WorldScale>,
    requests: Option<Res<crate::renderer::ClientRequests>>,
    time: Res<Time<Real>>,
    mut changed_at: Local<Option<f32>>,
) {
    let now = time.elapsed_secs();
    if settings.is_changed() && !settings.is_added() {
        view.set_if_neq(ViewDistance::for_reach_m(settings.view_distance_m));
        *changed_at = Some(now);
    }
    let Some(at) = *changed_at else { return };
    if now - at < SETTLE_SECS {
        return;
    }
    *changed_at = None;
    let regions = crate::world_scale::regions_for_distance_m(settings.view_distance_m);
    let table = crate::world_scale::ring_table_leaked(regions);
    if scale.lod_table != table {
        scale.lod_table = table;
        if let Some(requests) = requests {
            // No session: the channel goes nowhere, and the next session
            // starts from `WorldScale` anyway.
            let _ = requests
                .0
                .send(crate::instance::ClientRequest::SetLodHorizon(regions));
        }
        info!(
            "[client] render distance {:.0} m: lod rings {:?}",
            settings.view_distance_m, table
        );
    }
    save(&settings);
}

/// The vsync box into the window's present mode, when it is toggled (the
/// start's mode is `present_mode_at_start`'s, which the environment may pin).
fn apply_vsync(
    settings: Res<GraphicsSettings>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut shown: Local<Option<bool>>,
) {
    if shown.is_none() {
        // Once, at start: what the window actually presents with.
        for window in &windows {
            info!(
                "[client] present mode {:?} (vsync setting: {})",
                window.present_mode, settings.vsync
            );
        }
    }
    let last = *shown.get_or_insert(settings.vsync);
    if last == settings.vsync {
        return;
    }
    *shown = Some(settings.vsync);
    for mut window in &mut windows {
        window.present_mode = present_mode_for(settings.vsync);
    }
    info!(
        "[client] vsync {}",
        if settings.vsync { "on" } else { "off" }
    );
}

/// The interface scale into [`OverlayScale`]: the saved one at start (unless
/// `VOXEL_UI_SCALE` pins it), then each change — but only with the left
/// button up: a scale applied mid-drag moves the slider under the pointer,
/// which then reads as another value.
fn apply_ui_scale(
    settings: Res<GraphicsSettings>,
    scale: Option<ResMut<OverlayScale>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    mut applied: Local<Option<f32>>,
) {
    let Some(mut scale) = scale else { return };
    let want = settings.ui_scale;
    match *applied {
        None => {
            if ui_scale_pinned_by_env().is_none() {
                scale.set_if_neq(OverlayScale(want));
            }
            *applied = Some(want);
        }
        Some(last) if last != want => {
            if mouse.is_some_and(|m| m.pressed(MouseButton::Left)) {
                return;
            }
            scale.set_if_neq(OverlayScale(want));
            *applied = Some(want);
            info!(
                "[client] interface scale {:.0} % (x{want})",
                settings.ui_scale_percent()
            );
        }
        Some(_) => {}
    }
}

/// Keep the open-air haze of every camera at the [`ViewDistance`]
/// visibility. Only the exponential-squared (open-air) fog: the underground,
/// underwater and top-down fogs are linear and keep their own range.
fn refresh_distance_fog(view: Res<ViewDistance>, mut fogs: Query<&mut DistanceFog>) {
    if !view.is_changed() {
        return;
    }
    let density = squared_haze_density(view.visibility);
    for mut fog in &mut fogs {
        if let FogFalloff::ExponentialSquared { density: d } = fog.falloff {
            if d != density {
                fog.falloff = FogFalloff::ExponentialSquared { density };
            }
        }
    }
}

fn push_to_game_ui(
    layer: Option<NonSend<SlintLayer<slint_ui::GameUi>>>,
    settings: Res<GraphicsSettings>,
) {
    let Some(layer) = layer else { return };
    push_settings!(layer.overlay.ui(), &*settings);
}

fn push_to_frontend(
    layer: Option<NonSend<SlintLayer<slint_ui::FrontendUi>>>,
    settings: Res<GraphicsSettings>,
) {
    let Some(layer) = layer else { return };
    push_settings!(layer.overlay.ui(), &*settings);
}
