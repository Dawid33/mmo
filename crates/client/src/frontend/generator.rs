//! The map generator screen (New world, `ui/map-generator.slint`): preset,
//! seed and name, the forge's knobs in five tabs, a live preview forged in the
//! background at a coarse cell, and Play, which forges the world at full
//! resolution, writes `worlds/<slug>/` and starts a session on it.
//!
//! Every row comes from `worldgen::forge::knobs::SPECS` / `CHOICES` at
//! runtime: the only table here is which tab a knob GROUP goes in
//! ([`TABS`], [`TAB_KEY_OVERRIDES`]); a knob of an unlisted group lands in
//! Advanced, so a new knob is never missing from the screen.
//!
//! Native only: the preview worker and the forge are `std::thread`s talking
//! over crossbeam channels, and Play writes to disk. Spec: "Screens → Map
//! generator" in docs/superpowers/specs/2026-10-30-main-menu-and-map-generator-design.md.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use crossbeam::channel::{self, Receiver, RecvTimeoutError, Sender};
use slint_ui::slint::{Image as SlintImage, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use slint_ui::{FrontendUi, KnobRow};
use worldgen::forge::knobs::{Knobs, Spec, CHOICES, SPECS};
use worldgen::forge::presets::{self, CUSTOM, PRESETS};
use worldgen::forge::render::{self, PreviewLayer, PreviewStats, Rgba};
use worldgen::forge::{forge_with, ForgeParams, ForgeProgress, ForgeStage};
use worldgen::{forged, world_store};

use crate::renderer::slint_overlay::SlintLayer;
use crate::session::{AppState, PendingWorld};

/// The tabs, each with the knob GROUPS it shows (`Spec::group`).
pub const TABS: [(&str, &[&str]); 5] = [
    ("Land", &["Island"]),
    ("Mountains", &["Uplift"]),
    ("Water", &["Rivers", "Climate"]),
    ("Geology", &["Geology"]),
    ("Advanced", &["Erosion"]),
];

/// Knobs shown in another tab than their group's: the "Shaping" group is
/// split between Mountains (peak, valley floors) and Water (lakes).
const TAB_KEY_OVERRIDES: &[(&str, &str)] = &[
    ("max_peak_m", "Mountains"),
    ("floodplain_m_per_sqrt_km2", "Mountains"),
    ("floodplain_max_m", "Mountains"),
    ("lake_basins", "Water"),
    ("lake_spacing_m", "Water"),
    ("lake_radius_max_m", "Water"),
    ("lake_depth_max_m", "Water"),
];

/// Where a knob nobody placed goes.
const ADVANCED: usize = 4;

/// The tab a knob is shown in: its key's override, else its group's tab,
/// else Advanced.
pub fn tab_of(spec: &Spec) -> usize {
    let named = |name: &str| TABS.iter().position(|(tab, _)| *tab == name);
    TAB_KEY_OVERRIDES
        .iter()
        .find(|(key, _)| *key == spec.key)
        .and_then(|(_, tab)| named(tab))
        .or_else(|| TABS.iter().position(|(_, groups)| groups.contains(&spec.group)))
        .unwrap_or(ADVANCED)
}

/// The preview forges this long after the last change.
pub const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(250);

/// Advanced's two rows that are not forge knobs: the preview's and the
/// saved world's cell size, as choices (index 0 is the default).
pub const PREVIEW_CELL_KEY: &str = "preview_cell";
pub const WORLD_CELL_KEY: &str = "world_cell";
pub const PREVIEW_CELLS: [(f64, &str); 2] = [(64.0, "64 m"), (32.0, "32 m")];
pub const WORLD_CELLS: [(f64, &str); 2] = [(16.0, "16 m"), (8.0, "8 m (slow)")];

/// Land's non-knob row: the world's edge length (`ForgeParams::size_m`) for
/// the preview and Play forges. [`DEFAULT_WORLD_SIZE`] is `DEFAULT_SIZE_M`.
pub const WORLD_SIZE_KEY: &str = "world_size";
pub const WORLD_SIZES: [(f64, &str); 3] =
    [(8_000.0, "8 km"), (worldgen::forge::world::DEFAULT_SIZE_M, "16 km"), (32_000.0, "32 km (slow)")];
pub const DEFAULT_WORLD_SIZE: usize = 1;

/// A knob whose range is whole numbers at least this wide slides in steps of 1.
const WHOLE_STEP_MIN_RANGE: f64 = 10.0;

/// Lines under the preview: land, islands, peak, water.
const STATS_LINES: usize = 4;

// ---- seed, name, time ----

/// A seed as the forge prints it.
pub fn format_seed(seed: u64) -> String {
    format!("{seed:#018X}")
}

/// Parses a seed like forge_proto's `--seed`: hex, `0x` and `_` allowed.
pub fn parse_seed(text: &str) -> Option<u64> {
    let t = text.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    let hex = t.replace('_', "");
    if hex.is_empty() {
        return None;
    }
    u64::from_str_radix(&hex, 16).ok()
}

/// A fresh seed: a hash of the system clock.
pub fn shuffle_seed() -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    SystemTime::now().hash(&mut hasher);
    Instant::now().hash(&mut hasher);
    hasher.finish()
}

/// The name a world gets unless the player types one.
pub fn default_name(seed: u64) -> String {
    format!("Island {:04X}", seed & 0xFFFF)
}

/// RFC 3339 UTC to the second, e.g. `2023-11-14T22:13:20Z`.
pub fn rfc3339_utc(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

// ---- rows ----

fn strings(items: impl IntoIterator<Item = impl Into<SharedString>>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.into_iter().map(Into::into).collect::<Vec<SharedString>>()))
}

fn choice_row(key: &str, label: &str, help: &str, choices: &[&str], value: usize, default: usize) -> KnobRow {
    KnobRow {
        key: key.into(),
        label: label.into(),
        help: help.into(),
        min: 0.0,
        max: (choices.len() - 1) as f32,
        value: value as f32,
        default: default as f32,
        step: 1.0,
        is_choice: true,
        choices: strings(choices.iter().copied()),
    }
}

fn knob_row(spec: &Spec, value: f64) -> KnobRow {
    let choices = CHOICES.iter().find(|(key, _)| *key == spec.key).map(|(_, c)| *c);
    let whole = [spec.min, spec.max, spec.default].iter().all(|v| v.fract() == 0.0)
        && spec.max - spec.min >= WHOLE_STEP_MIN_RANGE;
    KnobRow {
        key: spec.key.into(),
        label: spec.label.into(),
        help: spec.help.into(),
        min: spec.min as f32,
        max: spec.max as f32,
        value: value as f32,
        default: spec.default as f32,
        step: if choices.is_some() || whole { 1.0 } else { 0.0 },
        is_choice: choices.is_some(),
        choices: strings(choices.unwrap_or(&[]).iter().copied()),
    }
}

// ---- state ----

/// Everything the screen shows; the UI only reports edits.
#[derive(Resource)]
pub struct GeneratorState {
    pub knobs: Knobs,
    pub seed: u64,
    /// The seed box holds text that is not a seed.
    pub seed_invalid: bool,
    pub tab: usize,
    /// Index into `PreviewLayer::ALL`.
    pub layer: usize,
    /// Index into [`PREVIEW_CELLS`] / [`WORLD_CELLS`].
    pub preview_cell: usize,
    pub world_cell: usize,
    /// Index into [`WORLD_SIZES`].
    pub world_size: usize,
    /// The latest preview request; results of older ones are dropped.
    pub generation: u64,
    /// Settings changed since the last preview request.
    pub stale: bool,
    /// Refresh was pressed: request even with auto-refresh off.
    pub refresh_requested: bool,
    pub preview_busy: bool,
    pub stats_lines: Vec<String>,
    pub play_error: String,
    /// While forging for Play: (progress 0..1, label).
    pub forging: Option<(f32, String)>,
    /// The rows of the current tab must be rebuilt (tab, preset, reset).
    rows_dirty: bool,
    /// One row's value changed (a slider edit): update just that row.
    row_updates: Vec<String>,
    /// The seed box's text must be rewritten (shuffle, first show).
    seed_text_dirty: bool,
    /// The name box must be set to this.
    name_update: Option<String>,
}

impl Default for GeneratorState {
    fn default() -> Self {
        GeneratorState {
            knobs: Knobs::default(),
            seed: shuffle_seed(),
            seed_invalid: false,
            tab: 0,
            layer: 0,
            preview_cell: 0,
            world_cell: 0,
            world_size: DEFAULT_WORLD_SIZE,
            generation: 0,
            stale: true,
            refresh_requested: false,
            preview_busy: false,
            stats_lines: Vec::new(),
            play_error: String::new(),
            forging: None,
            rows_dirty: true,
            row_updates: Vec::new(),
            seed_text_dirty: true,
            name_update: None,
        }
    }
}

impl GeneratorState {
    /// The preset dropdown's index: the matching preset, else Custom (last).
    pub fn preset_index(&self) -> usize {
        presets::matching(&self.knobs)
            .and_then(|m| PRESETS.iter().position(|p| p.name == m.name))
            .unwrap_or(PRESETS.len())
    }

    pub fn preset_description(&self) -> &'static str {
        PRESETS.get(self.preset_index()).map_or("Custom settings.", |p| p.description)
    }

    /// The current tab's rows, in `SPECS` order (Land starts with the world
    /// size, Advanced adds the cell sizes).
    pub fn rows(&self) -> Vec<KnobRow> {
        let labels = |cells: &[(f64, &'static str)]| cells.iter().map(|c| c.1).collect::<Vec<_>>();
        let mut rows = Vec::new();
        if self.tab == 0 {
            rows.push(choice_row(
                WORLD_SIZE_KEY,
                "World size",
                "Edge length of the square world, ocean all round: 32 km takes about four times longer.",
                &labels(&WORLD_SIZES),
                self.world_size,
                DEFAULT_WORLD_SIZE,
            ));
        }
        rows.extend(
            SPECS
                .iter()
                .filter(|s| tab_of(s) == self.tab)
                .map(|s| knob_row(s, self.knobs.get(s.key).unwrap_or(s.default))),
        );
        if self.tab == ADVANCED {
            rows.push(choice_row(
                PREVIEW_CELL_KEY,
                "Preview cell",
                "Cell size of the preview: 32 m is sharper and slower to refresh.",
                &labels(&PREVIEW_CELLS),
                self.preview_cell,
                0,
            ));
            rows.push(choice_row(
                WORLD_CELL_KEY,
                "World cell",
                "Cell size the world is forged at on Play: 8 m takes several times longer.",
                &labels(&WORLD_CELLS),
                self.world_cell,
                0,
            ));
        }
        rows
    }

    pub fn params(&self, cell_m: f64) -> ForgeParams {
        ForgeParams {
            size_m: WORLD_SIZES[self.world_size.min(WORLD_SIZES.len() - 1)].0,
            cell_m,
            seed: self.seed,
            knobs: self.knobs.clone(),
            quiet: true,
        }
    }

    pub fn layer(&self) -> PreviewLayer {
        PreviewLayer::ALL[self.layer.min(PreviewLayer::ALL.len() - 1)]
    }

    /// Apply one edit from the screen. `name` is the name box's text now.
    pub fn apply(&mut self, action: GeneratorAction, name: &str) {
        match action {
            GeneratorAction::Preset(i) => {
                if let Some(preset) = PRESETS.get(i) {
                    self.knobs = presets::apply(preset);
                    self.rows_dirty = true;
                    self.stale = true;
                }
            }
            GeneratorAction::Seed(text) => match parse_seed(&text) {
                Some(seed) => {
                    self.seed_invalid = false;
                    self.set_seed(seed, name);
                }
                None => self.seed_invalid = true,
            },
            GeneratorAction::Shuffle => {
                self.set_seed(shuffle_seed(), name);
                self.seed_invalid = false;
                self.seed_text_dirty = true;
            }
            GeneratorAction::Knob(key, value) => self.set_knob(&key, f64::from(value)),
            GeneratorAction::Reset(key) => {
                let default = match key.as_str() {
                    PREVIEW_CELL_KEY | WORLD_CELL_KEY => 0.0,
                    WORLD_SIZE_KEY => DEFAULT_WORLD_SIZE as f64,
                    _ => SPECS.iter().find(|s| s.key == key).map_or(0.0, |s| s.default),
                };
                self.set_knob(&key, default);
                self.rows_dirty = true;
            }
            GeneratorAction::Tab(i) => {
                if i < TABS.len() && i != self.tab {
                    self.tab = i;
                    self.rows_dirty = true;
                }
            }
            GeneratorAction::Layer(i) => {
                if i < PreviewLayer::ALL.len() && i != self.layer {
                    self.layer = i;
                    self.stale = true;
                }
            }
            GeneratorAction::Refresh => {
                self.stale = true;
                self.refresh_requested = true;
            }
            // Handled by the systems (they own the threads).
            GeneratorAction::Play | GeneratorAction::CancelForge => {}
        }
    }

    fn set_seed(&mut self, seed: u64, name: &str) {
        if seed == self.seed {
            return;
        }
        if name.is_empty() || name == default_name(self.seed) {
            self.name_update = Some(default_name(seed));
        }
        self.seed = seed;
        self.stale = true;
    }

    fn set_knob(&mut self, key: &str, value: f64) {
        match key {
            PREVIEW_CELL_KEY => {
                self.preview_cell = (value.round().max(0.0) as usize).min(PREVIEW_CELLS.len() - 1);
                self.stale = true;
            }
            WORLD_SIZE_KEY => {
                self.world_size = (value.round().max(0.0) as usize).min(WORLD_SIZES.len() - 1);
                self.stale = true;
            }
            WORLD_CELL_KEY => self.world_cell = (value.round().max(0.0) as usize).min(WORLD_CELLS.len() - 1),
            _ => {
                if self.knobs.get(key) == Some(value) || !self.knobs.set(key, value) {
                    return;
                }
                self.stale = true;
            }
        }
        self.row_updates.push(key.to_string());
    }
}

/// What the generator's widgets asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum GeneratorAction {
    Preset(usize),
    Seed(String),
    Shuffle,
    Knob(String, f32),
    Reset(String),
    Tab(usize),
    Layer(usize),
    Refresh,
    Play,
    CancelForge,
}

/// The lines under the preview.
pub fn stats_lines(stats: &PreviewStats) -> Vec<String> {
    vec![
        format!("Land: {:.0} % of the world", stats.land_share * 100.0),
        format!("Islands: {} (largest {:.1} km²)", stats.islands, stats.largest_island_km2),
        format!("Highest peak: {:.0} m", stats.highest_peak_m),
        format!("Lakes: {} · river reaches: {}", stats.lakes, stats.river_reaches),
    ]
}

// ---- preview worker ----

/// What the preview of a forge depends on, bit for bit.
fn forge_key(p: &ForgeParams) -> (u64, u64, u64, Vec<u64>) {
    let knobs = SPECS.iter().map(|s| p.knobs.get(s.key).unwrap_or(0.0).to_bits()).collect();
    (p.seed, p.size_m.to_bits(), p.cell_m.to_bits(), knobs)
}

struct PreviewRequest {
    params: ForgeParams,
    layer: PreviewLayer,
    generation: u64,
}

/// One background thread forging previews. Requests are coalesced: it waits
/// [`PREVIEW_DEBOUNCE`] after the last one, then forges the newest; a newer
/// request abandons a forge in progress. The last forge is kept, so a layer
/// change only re-renders.
#[derive(Resource)]
pub struct PreviewWorker {
    requests: Option<Sender<PreviewRequest>>,
    latest: Arc<AtomicU64>,
    /// `(generation, image, stats)` of finished previews.
    pub results: Receiver<(u64, Rgba, PreviewStats)>,
}

impl PreviewWorker {
    pub fn spawn() -> Self {
        let (requests, inbox) = channel::unbounded();
        let (outbox, results) = channel::unbounded();
        let latest = Arc::new(AtomicU64::new(0));
        let current = latest.clone();
        std::thread::Builder::new()
            .name("map-preview".into())
            .spawn(move || preview_loop(inbox, current, outbox))
            .expect("spawn the preview thread");
        PreviewWorker { requests: Some(requests), latest, results }
    }

    pub fn request(&self, params: ForgeParams, layer: PreviewLayer, generation: u64) {
        self.latest.store(generation, Ordering::SeqCst);
        if let Some(requests) = &self.requests {
            let _ = requests.send(PreviewRequest { params, layer, generation });
        }
    }
}

impl Drop for PreviewWorker {
    fn drop(&mut self) {
        // Abandon the forge in progress; the closed channel ends the thread.
        self.latest.store(u64::MAX, Ordering::SeqCst);
        self.requests = None;
    }
}

fn preview_loop(inbox: Receiver<PreviewRequest>, latest: Arc<AtomicU64>, outbox: Sender<(u64, Rgba, PreviewStats)>) {
    let mut last: Option<((u64, u64, u64, Vec<u64>), worldgen::forge::Forged)> = None;
    while let Ok(mut request) = inbox.recv() {
        loop {
            match inbox.recv_timeout(PREVIEW_DEBOUNCE) {
                Ok(newer) => request = newer,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let generation = request.generation;
        let key = forge_key(&request.params);
        if last.as_ref().is_none_or(|(k, _)| *k != key) {
            last = None;
            let forged = forge_with(&request.params, &mut |_| {
                if latest.load(Ordering::SeqCst) == generation { ControlFlow::Continue(()) } else { ControlFlow::Break(()) }
            });
            match forged {
                Some(forged) => last = Some((key, forged)),
                None => continue,
            }
        }
        if latest.load(Ordering::SeqCst) != generation {
            continue;
        }
        let forged = &last.as_ref().expect("forged above").1;
        if outbox.send((generation, render::render(forged, request.layer), render::stats(forged))).is_err() {
            return;
        }
    }
}

// ---- Play: the full forge ----

/// What the Play forge reports.
#[derive(Debug)]
pub enum ForgeEvent {
    Progress(ForgeProgress),
    /// Forged; writing the world directory.
    Saving,
    Done(PendingWorld),
    Failed(String),
    /// Cancelled; nothing was left on disk.
    Cancelled,
}

/// The progress bar's fill and label for a forge report.
pub fn progress_text(p: ForgeProgress) -> (f32, String) {
    match p.stage {
        ForgeStage::Setup => (0.0, p.stage.label().to_string()),
        ForgeStage::Eroding => {
            (0.9 * p.iter as f32 / p.iters.max(1) as f32, format!("{} {} / {}", p.stage.label(), p.iter, p.iters))
        }
        ForgeStage::Shaping => (0.92, p.stage.label().to_string()),
        ForgeStage::Hydrology => (0.95, p.stage.label().to_string()),
    }
}

/// Play's forge on its own thread: forge, then `world_store::create` under
/// `root`. Cancel stops it and removes anything it wrote.
#[derive(Resource)]
pub struct ForgeJob {
    cancel: Arc<AtomicBool>,
    pub events: Receiver<ForgeEvent>,
}

impl ForgeJob {
    pub fn start(root: PathBuf, params: ForgeParams, name: String) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, events) = channel::unbounded();
        let flag = cancel.clone();
        std::thread::Builder::new()
            .name("world-forge".into())
            .spawn(move || {
                let end = forge_world(&root, &params, name, &flag, &tx);
                let _ = tx.send(end);
            })
            .expect("spawn the forge thread");
        ForgeJob { cancel, events }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// An event as the game should take it: a `Done` that lost the race to a
    /// Cancel pressed after the thread's last check removes the new world and
    /// becomes `Cancelled`.
    pub fn settle(&self, event: ForgeEvent) -> ForgeEvent {
        match event {
            ForgeEvent::Done(world) if self.cancel.load(Ordering::SeqCst) => {
                let _ = std::fs::remove_dir_all(&world.entry_dir);
                ForgeEvent::Cancelled
            }
            event => event,
        }
    }
}

fn forge_world(root: &Path, params: &ForgeParams, name: String, cancel: &AtomicBool, tx: &Sender<ForgeEvent>) -> ForgeEvent {
    let started = Instant::now();
    let cancelled = || cancel.load(Ordering::SeqCst);
    let forged = forge_with(params, &mut |p| {
        let _ = tx.send(ForgeEvent::Progress(p));
        if cancelled() { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    });
    let Some(forged) = forged else { return ForgeEvent::Cancelled };
    let _ = tx.send(ForgeEvent::Saving);
    // `artifact` panics when the world has no flat dry land for a spawn.
    let artifact = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| forged.artifact(params))) {
        Ok(a) => a,
        Err(_) => return ForgeEvent::Failed("This world has no dry land to start on; try other settings.".into()),
    };
    if cancelled() {
        return ForgeEvent::Cancelled;
    }
    let png = render::encode_png(&render::render(&forged, PreviewLayer::Relief));
    if let Err(e) = std::fs::create_dir_all(root) {
        return ForgeEvent::Failed(format!("Could not create {}: {e}", root.display()));
    }
    let meta = world_store::WorldMeta {
        format_version: world_store::META_FORMAT,
        slug: world_store::slug_for(root, &name),
        name: name.clone(),
        created_utc: rfc3339_utc(SystemTime::now()),
        seed: params.seed,
        size_m: params.size_m,
        cell_m: params.cell_m,
        preset: presets::matching(&params.knobs).map_or(CUSTOM, |p| p.name).to_string(),
        knobs: world_store::knobs_map(&params.knobs),
        forge_ms: started.elapsed().as_millis() as u64,
        region_blocks: game::REGION_BLOCKS as u32,
        chunk_blocks: game::CHUNK_BLOCKS as u32,
        artifact_version: forged::ARTIFACT_VERSION,
    };
    let dir = match world_store::create(root, &meta, &artifact, &png) {
        Ok(dir) => dir,
        Err(e) => return ForgeEvent::Failed(format!("Could not save the world: {e}")),
    };
    if cancelled() {
        let _ = std::fs::remove_dir_all(&dir);
        return ForgeEvent::Cancelled;
    }
    ForgeEvent::Done(PendingWorld { terrain: dir.join(world_store::TERRAIN_FILE), entry_dir: dir, name })
}

// ---- Bevy wiring ----

/// The generator's half of the frontend layer: queued actions and the rows model.
struct GeneratorUi {
    actions: Rc<RefCell<Vec<GeneratorAction>>>,
    rows: Rc<VecModel<KnobRow>>,
}

/// Connect the generator's callbacks (from the frontend layer's init).
pub fn connect(ui: &FrontendUi, world: &mut World) {
    let actions = Rc::new(RefCell::new(Vec::new()));
    let rows = Rc::new(VecModel::<KnobRow>::default());
    ui.set_gen_rows(ModelRc::from(rows.clone()));
    ui.set_gen_presets(strings(PRESETS.iter().map(|p| p.name).chain([CUSTOM])));
    ui.set_gen_tabs(strings(TABS.iter().map(|t| t.0)));
    ui.set_gen_layers(strings(PreviewLayer::ALL.iter().map(|l| l.label())));
    macro_rules! queue {
        (|$($arg:ident),*| $action:expr) => {{
            let queue = actions.clone();
            move |$($arg),*| queue.borrow_mut().push($action)
        }};
    }
    ui.on_gen_preset_chosen(queue!(|i| GeneratorAction::Preset(i.max(0) as usize)));
    ui.on_gen_seed_edited(queue!(|text| GeneratorAction::Seed(text.to_string())));
    ui.on_gen_shuffle_seed(queue!(| | GeneratorAction::Shuffle));
    ui.on_gen_knob_edited(queue!(|key, value| GeneratorAction::Knob(key.to_string(), value)));
    ui.on_gen_knob_reset(queue!(|key| GeneratorAction::Reset(key.to_string())));
    ui.on_gen_tab_changed(queue!(|i| GeneratorAction::Tab(i.max(0) as usize)));
    ui.on_gen_layer_chosen(queue!(|i| GeneratorAction::Layer(i.max(0) as usize)));
    ui.on_gen_refresh(queue!(| | GeneratorAction::Refresh));
    ui.on_gen_play(queue!(| | GeneratorAction::Play));
    ui.on_gen_cancel_forge(queue!(| | GeneratorAction::CancelForge));
    world.insert_non_send(GeneratorUi { actions, rows });
}

pub struct GeneratorPlugin;

impl Plugin for GeneratorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GeneratorState>()
            .add_systems(OnEnter(AppState::MapGenerator), enter_generator)
            .add_systems(OnExit(AppState::MapGenerator), exit_generator)
            .add_systems(
                Update,
                (apply_generator_actions, request_preview, receive_preview, poll_forge, push_generator_state)
                    .chain()
                    .run_if(in_state(AppState::MapGenerator)),
            );
    }
}

fn enter_generator(mut commands: Commands, mut state: ResMut<GeneratorState>, layer: Option<NonSend<SlintLayer<FrontendUi>>>) {
    commands.insert_resource(PreviewWorker::spawn());
    state.stale = true;
    state.rows_dirty = true;
    state.seed_text_dirty = true;
    if layer.is_some_and(|layer| layer.overlay.ui().get_gen_name_text().is_empty()) {
        state.name_update = Some(default_name(state.seed));
    }
}

fn exit_generator(mut commands: Commands, job: Option<Res<ForgeJob>>, mut state: ResMut<GeneratorState>) {
    commands.remove_resource::<PreviewWorker>();
    if let Some(job) = job {
        job.cancel();
        commands.remove_resource::<ForgeJob>();
    }
    state.forging = None;
    state.preview_busy = false;
}

fn apply_generator_actions(
    mut commands: Commands,
    ui: Option<NonSend<GeneratorUi>>,
    layer: Option<NonSend<SlintLayer<FrontendUi>>>,
    mut state: ResMut<GeneratorState>,
    job: Option<Res<ForgeJob>>,
) {
    let (Some(ui), Some(layer)) = (ui, layer) else { return };
    let queued = std::mem::take(&mut *ui.actions.borrow_mut());
    let name = layer.overlay.ui().get_gen_name_text().to_string();
    for action in queued {
        match action {
            GeneratorAction::Play if job.is_none() && state.forging.is_none() => {
                let name = if name.trim().is_empty() { default_name(state.seed) } else { name.trim().to_string() };
                let root = forged::from_repo_root(Path::new(world_store::WORLDS_DIR));
                let params = state.params(WORLD_CELLS[state.world_cell].0);
                commands.insert_resource(ForgeJob::start(root, params, name));
                state.forging = Some((0.0, ForgeStage::Setup.label().to_string()));
                state.play_error.clear();
            }
            GeneratorAction::CancelForge => {
                if let Some(job) = &job {
                    job.cancel();
                }
            }
            action => state.apply(action, &name),
        }
    }
}

fn request_preview(
    layer: Option<NonSend<SlintLayer<FrontendUi>>>,
    worker: Option<Res<PreviewWorker>>,
    mut state: ResMut<GeneratorState>,
) {
    let (Some(layer), Some(worker)) = (layer, worker) else { return };
    let auto = layer.overlay.ui().get_gen_auto_refresh();
    if !state.stale || !(auto || state.refresh_requested) || state.forging.is_some() {
        return;
    }
    state.stale = false;
    state.refresh_requested = false;
    state.generation += 1;
    state.preview_busy = true;
    worker.request(state.params(PREVIEW_CELLS[state.preview_cell].0), state.layer(), state.generation);
}

fn receive_preview(
    layer: Option<NonSend<SlintLayer<FrontendUi>>>,
    worker: Option<Res<PreviewWorker>>,
    mut state: ResMut<GeneratorState>,
) {
    let (Some(layer), Some(worker)) = (layer, worker) else { return };
    let newest = worker.results.try_iter().filter(|(generation, ..)| *generation == state.generation).last();
    let Some((_, image, stats)) = newest else { return };
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&image.pixels, image.width, image.height);
    layer.overlay.ui().set_gen_preview(SlintImage::from_rgba8(buffer));
    state.stats_lines = stats_lines(&stats);
    state.preview_busy = false;
}

fn poll_forge(
    mut commands: Commands,
    job: Option<Res<ForgeJob>>,
    mut state: ResMut<GeneratorState>,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(job) = job else { return };
    for event in job.events.try_iter() {
        match job.settle(event) {
            ForgeEvent::Progress(p) => state.forging = Some(progress_text(p)),
            ForgeEvent::Saving => state.forging = Some((0.97, "Saving the world".into())),
            ForgeEvent::Done(world) => {
                info!("created world {:?} in {}", world.name, world.entry_dir.display());
                commands.remove_resource::<ForgeJob>();
                commands.insert_resource(world);
                state.forging = None;
                next.set(AppState::Starting);
                return;
            }
            ForgeEvent::Failed(why) => {
                commands.remove_resource::<ForgeJob>();
                state.forging = None;
                state.play_error = why;
                return;
            }
            ForgeEvent::Cancelled => {
                commands.remove_resource::<ForgeJob>();
                state.forging = None;
                return;
            }
        }
    }
}

fn push_generator_state(
    ui: Option<NonSend<GeneratorUi>>,
    layer: Option<NonSend<SlintLayer<FrontendUi>>>,
    mut state: ResMut<GeneratorState>,
) {
    let (Some(models), Some(layer)) = (ui, layer) else { return };
    let ui = layer.overlay.ui();
    if std::mem::take(&mut state.rows_dirty) {
        models.rows.set_vec(state.rows());
        state.row_updates.clear();
    }
    for key in std::mem::take(&mut state.row_updates) {
        let fresh = state.rows().into_iter().find(|r| r.key == key.as_str());
        let index = (0..models.rows.row_count()).find(|&i| models.rows.row_data(i).is_some_and(|r| r.key == key.as_str()));
        if let (Some(fresh), Some(index)) = (fresh, index) {
            models.rows.set_row_data(index, fresh);
        }
    }
    if std::mem::take(&mut state.seed_text_dirty) {
        ui.set_gen_seed_text(format_seed(state.seed).into());
    }
    if let Some(name) = state.name_update.take() {
        ui.set_gen_name_text(name.into());
    }
    let preset = state.preset_index() as i32;
    if ui.get_gen_preset() != preset {
        ui.set_gen_preset(preset);
    }
    let description = state.preset_description();
    if ui.get_gen_preset_description() != description {
        ui.set_gen_preset_description(description.into());
    }
    if ui.get_gen_seed_invalid() != state.seed_invalid {
        ui.set_gen_seed_invalid(state.seed_invalid);
    }
    if ui.get_gen_tab() != state.tab as i32 {
        ui.set_gen_tab(state.tab as i32);
    }
    if ui.get_gen_layer() != state.layer as i32 {
        ui.set_gen_layer(state.layer as i32);
    }
    if ui.get_gen_preview_busy() != state.preview_busy {
        ui.set_gen_preview_busy(state.preview_busy);
    }
    let shown: Vec<SharedString> = ui.get_gen_stats_lines().iter().collect();
    if shown.len() != state.stats_lines.len().min(STATS_LINES) || shown.iter().zip(&state.stats_lines).any(|(a, b)| a != b) {
        ui.set_gen_stats_lines(strings(state.stats_lines.iter().take(STATS_LINES).map(String::as_str)));
    }
    let forging = state.forging.is_some();
    if ui.get_gen_forging() != forging {
        ui.set_gen_forging(forging);
    }
    if let Some((progress, label)) = &state.forging {
        if ui.get_gen_forge_progress() != *progress {
            ui.set_gen_forge_progress(*progress);
        }
        if ui.get_gen_forge_label() != label.as_str() {
            ui.set_gen_forge_label(label.as_str().into());
        }
    }
    if ui.get_gen_play_error() != state.play_error.as_str() {
        ui.set_gen_play_error(state.play_error.as_str().into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_knob_lands_in_exactly_one_tab() {
        for spec in SPECS {
            let tab = tab_of(spec);
            assert!(tab < TABS.len(), "{} → {tab}", spec.key);
            // Only the Erosion group belongs in Advanced: anything else there
            // fell through the table by accident.
            if tab == ADVANCED {
                assert_eq!(spec.group, "Erosion", "{} fell through to Advanced", spec.key);
            }
        }
        let tab = |key: &str| tab_of(SPECS.iter().find(|s| s.key == key).unwrap());
        for key in ["max_peak_m", "floodplain_m_per_sqrt_km2", "floodplain_max_m"] {
            assert_eq!(tab(key), 1, "{key}");
        }
        for key in ["lake_basins", "lake_spacing_m", "lake_radius_max_m", "lake_depth_max_m"] {
            assert_eq!(tab(key), 2, "{key}");
        }
        assert_eq!(tab("layout"), 0);
        assert_eq!(tab("uplift"), 1);
        assert_eq!(tab("rain_max"), 2);
        assert_eq!(tab("river_width_m"), 2);
        assert_eq!(tab("hard_granite"), 3);
        assert_eq!(tab("iters"), 4);
        // Every Shaping knob has an override.
        for spec in SPECS.iter().filter(|s| s.group == "Shaping") {
            assert!(TAB_KEY_OVERRIDES.iter().any(|(k, _)| *k == spec.key), "{}", spec.key);
        }
    }

    #[test]
    fn rows_follow_the_tab() {
        let mut state = GeneratorState { seed: 1, ..Default::default() };
        let rows = state.rows();
        assert_eq!(rows[0].key, WORLD_SIZE_KEY);
        assert_eq!(rows[1].key, "layout");
        assert!(rows[1].is_choice);
        assert_eq!(rows[1].choices.row_count(), 3);
        state.tab = ADVANCED;
        let keys: Vec<String> = state.rows().iter().map(|r| r.key.to_string()).collect();
        assert_eq!(keys.last().map(String::as_str), Some(WORLD_CELL_KEY));
        assert_eq!(keys[keys.len() - 2], PREVIEW_CELL_KEY);
        let total: usize = (0..TABS.len())
            .map(|t| GeneratorState { tab: t, ..Default::default() }.rows().len())
            .sum();
        assert_eq!(total, SPECS.len() + 3);
    }

    #[test]
    fn world_size_row_sets_size_m() {
        let mut state = GeneratorState::default();
        assert_eq!(WORLD_SIZES[DEFAULT_WORLD_SIZE].0, worldgen::forge::world::DEFAULT_SIZE_M);
        let size_row = state.rows().into_iter().find(|r| r.key == WORLD_SIZE_KEY).unwrap();
        assert_eq!((size_row.value, size_row.default), (1.0, 1.0));
        assert_eq!(state.params(64.0).size_m, 16_000.0);
        state.stale = false;
        state.apply(GeneratorAction::Knob(WORLD_SIZE_KEY.into(), 2.0), "");
        assert!(state.stale, "the preview re-forges");
        assert_eq!(state.params(PREVIEW_CELLS[state.preview_cell].0).size_m, 32_000.0);
        assert_eq!(state.params(WORLD_CELLS[state.world_cell].0).size_m, 32_000.0);
        state.apply(GeneratorAction::Reset(WORLD_SIZE_KEY.into()), "");
        assert_eq!(state.params(16.0).size_m, 16_000.0);
    }

    #[test]
    fn done_after_cancel_removes_the_world() {
        let dir = scratch("late-cancel").join("island");
        std::fs::create_dir_all(&dir).unwrap();
        let (_tx, events) = channel::unbounded();
        let job = ForgeJob { cancel: Arc::new(AtomicBool::new(false)), events };
        let done = || ForgeEvent::Done(PendingWorld { entry_dir: dir.clone(), terrain: dir.join("t"), name: "x".into() });
        assert!(matches!(job.settle(done()), ForgeEvent::Done(_)));
        assert!(dir.is_dir());
        job.cancel();
        assert!(matches!(job.settle(done()), ForgeEvent::Cancelled));
        assert!(!dir.exists(), "late Done's world removed");
    }

    #[test]
    fn seeds_round_trip_and_bad_text_keeps_the_seed() {
        assert_eq!(format_seed(0x00D5_C0DE_0206), "0x000000D5C0DE0206");
        assert_eq!(parse_seed("0x000000D5C0DE0206"), Some(0x00D5_C0DE_0206));
        assert_eq!(parse_seed("00D5_C0DE_0206"), Some(0x00D5_C0DE_0206));
        assert_eq!(parse_seed(" ff "), Some(255));
        assert_eq!(parse_seed("xyz"), None);
        assert_eq!(parse_seed(""), None);
        let mut state = GeneratorState { seed: 0x2F1A, ..Default::default() };
        state.apply(GeneratorAction::Seed("nope".into()), "Island 2F1A");
        assert!(state.seed_invalid);
        assert_eq!(state.seed, 0x2F1A);
        state.apply(GeneratorAction::Seed("0xBEEF".into()), "Island 2F1A");
        assert!(!state.seed_invalid);
        assert_eq!(state.seed, 0xBEEF);
        assert_eq!(state.name_update.as_deref(), Some("Island BEEF"), "default name follows the seed");
        state.name_update = None;
        state.apply(GeneratorAction::Seed("0x1234".into()), "My island");
        assert_eq!(state.name_update, None, "a typed name stays");
    }

    #[test]
    fn editing_a_knob_turns_the_preset_custom_and_a_preset_reapplies() {
        let mut state = GeneratorState::default();
        assert_eq!(state.preset_index(), 0);
        state.apply(GeneratorAction::Knob("uplift".into(), 1.0e-3), "");
        assert_eq!(state.preset_index(), PRESETS.len(), "Custom");
        assert_eq!(state.preset_description(), "Custom settings.");
        state.apply(GeneratorAction::Preset(1), "");
        assert_eq!(state.preset_index(), 1);
        state.apply(GeneratorAction::Reset("layout".into()), "");
        assert_eq!(state.preset_index(), 0);
    }

    #[test]
    fn eroding_label_counts_iterations() {
        let p = ForgeProgress { stage: ForgeStage::Eroding, iter: 312, iters: 500 };
        assert_eq!(progress_text(p).1, "Eroding 312 / 500");
    }

    #[test]
    fn rfc3339_of_a_known_time() {
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(rfc3339_utc(t), "2023-11-14T22:13:20Z");
        assert_eq!(rfc3339_utc(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(rfc3339_utc(leap), "2000-02-29T00:00:00Z");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/generator-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn small(cell_m: f64) -> ForgeParams {
        ForgeParams { cell_m, quiet: true, ..Default::default() }
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn preview_worker_is_latest_wins() {
        let worker = PreviewWorker::spawn();
        let mut p = ForgeParams { size_m: 4_096.0, cell_m: 128.0, quiet: true, ..Default::default() };
        p.knobs.iters = 20.0;
        for generation in 1..=3 {
            p.seed = generation;
            worker.request(p.clone(), PreviewLayer::Relief, generation);
            std::thread::sleep(Duration::from_millis(3));
        }
        let (generation, image, _) = worker.results.recv_timeout(Duration::from_secs(60)).expect("a preview");
        assert_eq!(generation, 3);
        assert_eq!(image.pixels.len(), (image.width * image.height * 4) as usize);
        assert!(worker.results.recv_timeout(Duration::from_secs(1)).is_err(), "exactly one result");
    }

    fn finish(job: &ForgeJob) -> ForgeEvent {
        loop {
            match job.events.recv_timeout(Duration::from_secs(120)).expect("forge event") {
                ForgeEvent::Progress(_) | ForgeEvent::Saving => {}
                end => return end,
            }
        }
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn play_cancel_leaves_no_world_dir() {
        let root = scratch("cancel");
        let job = ForgeJob::start(root.clone(), small(128.0), "Cancelled".into());
        loop {
            if let ForgeEvent::Progress(p) = job.events.recv_timeout(Duration::from_secs(60)).expect("progress") {
                if p.stage == ForgeStage::Eroding {
                    break;
                }
            }
        }
        job.cancel();
        assert!(matches!(finish(&job), ForgeEvent::Cancelled));
        let left = std::fs::read_dir(&root).map(|d| d.count()).unwrap_or(0);
        assert_eq!(left, 0, "nothing left under {}", root.display());
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn play_writes_a_loadable_world() {
        let root = scratch("play");
        let job = ForgeJob::start(root.clone(), small(128.0), "Test Island".into());
        let ForgeEvent::Done(world) = finish(&job) else { panic!("forge did not finish") };
        assert_eq!(world.name, "Test Island");
        assert_eq!(world.entry_dir, root.join("test-island"));
        assert!(world.terrain.is_file());
        let listed = world_store::list(&root);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].loadable, Ok(()));
        let meta = listed[0].meta.as_ref().unwrap();
        assert_eq!(meta.preset, "Default");
        assert_eq!(meta.cell_m, 128.0);
    }
}
