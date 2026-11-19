//! The Load world screen (`FrontendUi` screen 2, `load-world.slint`): the
//! worlds under `worlds/`, the selected one's preview and settings, Load /
//! Delete (asks first) / Back.
//!
//! The list is read on entering the screen and after a delete — never per
//! frame. Each row's thumbnail (`preview.png` downscaled to [`THUMB_PX`]) is
//! decoded once per refresh; the selected world's full preview is decoded when
//! the selection changes. Load sets [`PendingWorld`] and goes to
//! [`AppState::Starting`].

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use bevy::prelude::*;
use slint_ui::slint::{Image as SlintImage, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use slint_ui::{FrontendUi, WorldRow};
use worldgen::forge::knobs::{Knobs, SPECS};
use worldgen::forge::presets::{self, PRESETS};
use worldgen::world_store::{self, WorldEntry, WorldMeta};
use worldgen::forged;

use super::generator::format_seed;
use crate::renderer::slint_overlay::SlintLayer;
use crate::session::{AppState, PendingWorld};

/// Edge of a row's square thumbnail, in pixels.
pub const THUMB_PX: u32 = 48;

/// Settings lines shown under the preview (the rest is "+N more").
const MAX_SETTINGS_LINES: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadAction {
    Select(usize),
    Load,
    Delete,
    ConfirmYes,
    ConfirmNo,
}

/// "seed 0x…, 16 × 16 km, 2026-10-30" (`world_store::size_text`).
pub fn detail_line(meta: &WorldMeta) -> String {
    let date = meta.created_utc.get(..10).unwrap_or(&meta.created_utc);
    format!("seed {}, {}, {}", format_seed(meta.seed), world_store::size_text(meta.width_m, meta.height_m), date)
}

fn short(v: f64) -> String {
    if v.abs() >= 1000.0 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// The preset the world was made from, then each knob that differs from it
/// ("Custom" and unknown presets compare against the defaults).
pub fn settings_lines(meta: &WorldMeta) -> Vec<String> {
    let base = PRESETS.iter().find(|p| p.name == meta.preset).map(presets::apply).unwrap_or_else(Knobs::default);
    let mut lines = vec![format!("Preset: {}", meta.preset)];
    let mut changed = Vec::new();
    for spec in SPECS {
        let (Some(&value), Some(was)) = (meta.knobs.get(spec.key), base.get(spec.key)) else { continue };
        if value != was {
            changed.push(format!("{}: {} (preset {})", spec.label, short(value), short(was)));
        }
    }
    let more = changed.len().saturating_sub(MAX_SETTINGS_LINES - 1);
    changed.truncate(MAX_SETTINGS_LINES - 1);
    lines.extend(changed);
    if more > 0 {
        lines.push(format!("+{more} more"));
    }
    lines
}

/// The list row of an entry: (name, detail, loadable, reason, legacy).
pub fn row_parts(entry: &WorldEntry) -> (String, String, bool, String, bool) {
    let (name, detail) = match &entry.meta {
        Some(meta) => (meta.name.clone(), detail_line(meta)),
        None if entry.legacy => ("Legacy world (default.world)".to_string(), "no details".to_string()),
        None => (
            entry.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            "no details".to_string(),
        ),
    };
    let (loadable, reason) = match &entry.loadable {
        Ok(()) => (true, String::new()),
        Err(reason) => (false, reason.clone()),
    };
    (name, detail, loadable, reason, entry.legacy)
}


fn decode_preview(path: &Path) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let image = image::open(path).ok()?.to_rgba8();
    Some(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(image.as_raw(), image.width(), image.height()))
}

fn decode_thumb(path: &Path) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let image = image::open(path).ok()?.thumbnail(THUMB_PX, THUMB_PX).to_rgba8();
    Some(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(image.as_raw(), image.width(), image.height()))
}

/// The screen's state; the systems push it into the UI on change.
#[derive(Resource)]
pub struct LoadState {
    pub root: PathBuf,
    pub entries: Vec<WorldEntry>,
    pub selected: Option<usize>,
    pub confirm_delete: bool,
    /// The list or selection changed since the last push.
    pub dirty: bool,
    pub big: Option<SharedPixelBuffer<Rgba8Pixel>>,
    /// One per entry, decoded in `refresh`.
    pub thumbs: Vec<Option<SharedPixelBuffer<Rgba8Pixel>>>,
    /// Why the last Delete failed.
    pub error: Option<String>,
}

impl Default for LoadState {
    fn default() -> Self {
        Self {
            root: forged::from_repo_root(Path::new(world_store::WORLDS_DIR)),
            entries: Vec::new(),
            selected: None,
            confirm_delete: false,
            dirty: true,
            big: None,
            thumbs: Vec::new(),
            error: None,
        }
    }
}

impl LoadState {
    /// Re-read `worlds/`, keeping the selection at the same index (clamped).
    pub fn refresh(&mut self) {
        self.entries = world_store::list(&self.root);
        self.thumbs = self.entries.iter().map(|e| e.preview.as_deref().and_then(decode_thumb)).collect();
        self.selected = match (self.selected, self.entries.len()) {
            (_, 0) => None,
            (Some(i), n) => Some(i.min(n - 1)),
            (None, _) => Some(0),
        };
        self.confirm_delete = false;
        self.reload_preview();
        self.dirty = true;
    }

    fn reload_preview(&mut self) {
        self.big = self
            .selected
            .and_then(|i| self.entries.get(i))
            .and_then(|e| e.preview.as_deref())
            .and_then(decode_preview);
    }

    pub fn selected_entry(&self) -> Option<&WorldEntry> {
        self.selected.and_then(|i| self.entries.get(i))
    }

    /// Apply an action; `Some` is the world to start (Load).
    pub fn apply(&mut self, action: LoadAction) -> Option<PendingWorld> {
        match action {
            LoadAction::Select(i) => {
                if i < self.entries.len() && self.selected != Some(i) {
                    self.selected = Some(i);
                    self.reload_preview();
                    self.error = None;
                }
                self.dirty = true;
                None
            }
            LoadAction::Load => {
                if self.confirm_delete {
                    return None;
                }
                let entry = self.selected_entry().filter(|e| e.loadable.is_ok())?;
                let (name, ..) = row_parts(entry);
                Some(PendingWorld { entry_dir: entry.dir.clone(), terrain: entry.terrain.clone(), name })
            }
            LoadAction::Delete => {
                if self.selected_entry().is_some_and(|e| !e.legacy) {
                    self.confirm_delete = true;
                    self.dirty = true;
                }
                None
            }
            LoadAction::ConfirmNo => {
                self.confirm_delete = false;
                self.dirty = true;
                None
            }
            LoadAction::ConfirmYes => {
                if !std::mem::take(&mut self.confirm_delete) {
                    return None;
                }
                if let Some(entry) = self.selected_entry().cloned() {
                    match world_store::delete(&self.root, &entry) {
                        Ok(()) => self.error = None,
                        Err(e) => self.error = Some(format!("Could not delete: {e}")),
                    }
                }
                self.refresh();
                None
            }
        }
    }
}

struct LoadUi {
    actions: Rc<RefCell<Vec<LoadAction>>>,
    worlds: Rc<VecModel<WorldRow>>,
}

pub fn connect(ui: &FrontendUi, world: &mut World) {
    let actions = Rc::new(RefCell::new(Vec::new()));
    let worlds = Rc::new(VecModel::<WorldRow>::default());
    ui.set_load_worlds(ModelRc::from(worlds.clone()));
    let queue = |action: fn() -> LoadAction| {
        let queue = actions.clone();
        move || queue.borrow_mut().push(action())
    };
    ui.on_load_load(queue(|| LoadAction::Load));
    ui.on_load_delete(queue(|| LoadAction::Delete));
    ui.on_load_confirm_delete_yes(queue(|| LoadAction::ConfirmYes));
    ui.on_load_confirm_delete_no(queue(|| LoadAction::ConfirmNo));
    let select = actions.clone();
    ui.on_load_select(move |i| select.borrow_mut().push(LoadAction::Select(i.max(0) as usize)));
    world.insert_non_send(LoadUi { actions, worlds });
}

pub struct LoadWorldPlugin;

impl Plugin for LoadWorldPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LoadState>()
            .add_systems(OnEnter(AppState::LoadWorld), enter_load_world)
            .add_systems(
                Update,
                (apply_load_actions, push_load_state).chain().run_if(in_state(AppState::LoadWorld)),
            );
    }
}

fn enter_load_world(mut state: ResMut<LoadState>) {
    state.selected = None;
    state.error = None;
    state.refresh();
}

fn apply_load_actions(
    mut commands: Commands,
    ui: Option<NonSend<LoadUi>>,
    mut state: ResMut<LoadState>,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(ui) = ui else { return };
    let queued = std::mem::take(&mut *ui.actions.borrow_mut());
    for action in queued {
        if let Some(pending) = state.apply(action) {
            commands.insert_resource(pending);
            next.set(AppState::Starting);
            break;
        }
    }
}

fn push_load_state(
    ui: Option<NonSend<LoadUi>>,
    layer: Option<NonSend<SlintLayer<FrontendUi>>>,
    mut state: ResMut<LoadState>,
) {
    let (Some(models), Some(layer)) = (ui, layer) else { return };
    let ui = layer.overlay.ui();
    if std::mem::take(&mut state.dirty) {
        let rows: Vec<WorldRow> = state
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let (name, detail, loadable, reason, legacy) = row_parts(e);
                let thumb = state.thumbs.get(i).cloned().flatten().map(SlintImage::from_rgba8).unwrap_or_default();
                WorldRow { name: name.into(), detail: detail.into(), thumb, loadable, reason: reason.into(), legacy }
            })
            .collect();
        models.worlds.set_vec(rows);
        let selected = state.selected.map_or(-1, |i| i as i32);
        if ui.get_load_selected() != selected {
            ui.set_load_selected(selected);
        }
        ui.set_load_big_preview(state.big.clone().map(SlintImage::from_rgba8).unwrap_or_default());
        let mut lines = state
            .selected_entry()
            .and_then(|e| e.meta.as_ref())
            .map(settings_lines)
            .unwrap_or_default();
        if let Some(error) = &state.error {
            lines.insert(0, error.clone());
        }
        ui.set_load_settings_lines(ModelRc::new(VecModel::from(
            lines.into_iter().map(SharedString::from).collect::<Vec<_>>(),
        )));
    }
    if ui.get_load_confirm_delete() != state.confirm_delete {
        ui.set_load_confirm_delete(state.confirm_delete);
    }
}

