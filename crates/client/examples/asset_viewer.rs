//! Browse every Blockbench model: `just asset-viewer [roots…]`.
//!
//! The left panel lists each `.bbmodel` under the roots (default `assets` and
//! the bbmodel fixtures, relative to the repo root); pick one and it is shown
//! in the middle, framed from its bounds on a floor one block (16 units) per
//! grid square. The right panel lists its animations: pick one to play it,
//! "(rest pose)" to stop; scrub the slider, pause, restart, halve or double
//! the speed. Left- or right-drag on the view orbits, the wheel zooms. Saved
//! files hot-reload (the `file_watcher` feature, which the recipe turns on);
//! Rescan picks up new ones. The panels are the Slint
//! widget library's (`slint_ui_tools::AssetViewerUi`), drawn like the game UI.
//!
//! Self-check without a human: `--model <part of a path>` picks a model at
//! start, `--anim <name>` a clip, `--time <s>` freezes it there, and
//! `--shot <png>` saves a screenshot once it has loaded and exits (its window
//! opens unfocused, behind everything, on the laptop screen: `VOXEL_SHOT_POS` /
//! `VOXEL_SHOT_MONITOR` move it);
//! `--yaw <deg>` / `--pitch <deg>` set the view (default: front three-quarter
//! of a creature facing -Z; pitch 70 is roughly the commander camera's).
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use bevy::app::AppExit;
use bevy::asset::LoadState;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use bevy::window::WindowLevel;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use client::renderer::model::{BbAnimationPlayer, BbModel, BbModelAsset, BbModelPlugin, BbModelSpawned};
use client::renderer::slint_overlay::{SlintLayer, SlintLayerPlugin};
use slint_ui::slint::{Model, ModelRc, SharedString, VecModel};
use slint_ui_tools::AssetViewerUi;

/// Where a model is looked for when no roots are given.
const DEFAULT_ROOTS: [&str; 2] = ["assets", "crates/bbmodel/tests/fixtures"];
/// The clip list's first entry, which stops the player.
const REST_POSE: &str = "(rest pose)";
/// Radians the view turns per pixel dragged.
const ORBIT_PER_PX: f32 = 0.008;
/// Distance factor per wheel line.
const ZOOM_PER_LINE: f32 = 0.9;
/// Turntable speed, radians per second.
const TURN_SPEED: f32 = 0.5;
/// The starting view: front three-quarter of a model facing -Z (the creature
/// style guide's facing), a little from above. Degrees.
const DEFAULT_YAW: f32 = 145.0;
const DEFAULT_PITCH: f32 = 25.0;
/// Playback speed bounds for Slower / Faster.
const SPEED_RANGE: (f32, f32) = (1.0 / 16.0, 16.0);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[derive(Resource)]
struct Args {
    roots: Vec<String>,
    model: Option<String>,
    anim: Option<String>,
    time: Option<f32>,
    shot: Option<String>,
    yaw: f32,
    pitch: f32,
}

/// What the panels asked for, queued by the Slint callbacks.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    PickModel(usize),
    PickClip(usize),
    Rescan,
    TogglePause,
    Restart,
    Scrub(f32),
    ChangeSpeed(f32),
}

struct Actions(Rc<RefCell<Vec<Action>>>);

#[derive(Resource, Default)]
struct Viewer {
    /// Paths relative to the repo root, sorted.
    models: Vec<String>,
    selected: Option<usize>,
    /// The shown model's entity.
    entity: Option<Entity>,
    handle: Option<Handle<BbModelAsset>>,
    /// `REST_POSE` and the model's clips, as listed.
    clips: Vec<String>,
}

#[derive(Resource)]
struct Orbit {
    focus: Vec3,
    distance: f32,
    yaw: f32,
    pitch: f32,
}

#[derive(Component)]
struct Shown;

fn main() {
    let mut args = Args {
        roots: Vec::new(),
        model: None,
        anim: None,
        time: None,
        shot: None,
        yaw: DEFAULT_YAW,
        pitch: DEFAULT_PITCH,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--model" => args.model = it.next(),
            "--anim" => args.anim = it.next(),
            "--time" => args.time = it.next().and_then(|t| t.parse().ok()),
            "--shot" => args.shot = it.next(),
            "--yaw" => args.yaw = it.next().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_YAW),
            "--pitch" => args.pitch = it.next().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_PITCH),
            root => args.roots.push(root.to_string()),
        }
    }
    if args.roots.is_empty() {
        args.roots = DEFAULT_ROOTS.iter().map(|r| r.to_string()).collect();
    }
    let start_view = Orbit { focus: Vec3::ZERO, distance: 60.0, yaw: args.yaw.to_radians(), pitch: args.pitch.to_radians() };
    let mut window = Window { title: "asset viewer".into(), resolution: (1600, 900).into(), ..default() };
    if args.shot.is_some() {
        // Screenshot runs (creature sheets fire dozens) must not land under the
        // cursor and take the keyboard: one fixed place, unfocused, below every
        // other window. `VOXEL_SHOT_POS=x,y` (desktop pixels; default 160,90:
        // centred on this box's laptop screen, eDP-1 at the desktop origin), or
        // `VOXEL_SHOT_MONITOR=<winit index>` to centre on a monitor instead.
        let env = |name| std::env::var(name).ok();
        window.position = match env("VOXEL_SHOT_MONITOR").and_then(|m| m.parse().ok()) {
            Some(monitor) => WindowPosition::Centered(MonitorSelection::Index(monitor)),
            None => {
                let pos = env("VOXEL_SHOT_POS").unwrap_or_else(|| "160,90".into());
                let xy: Vec<i32> = pos.split(',').filter_map(|v| v.trim().parse().ok()).collect();
                WindowPosition::At(IVec2::new(*xy.first().unwrap_or(&160), *xy.get(1).unwrap_or(&90)))
            }
        };
        window.focused = false;
        window.window_level = WindowLevel::AlwaysOnBottom;
    }
    App::new()
        .add_plugins(
            DefaultPlugins
                // Absolute, so a directly launched binary finds them too.
                .set(AssetPlugin { file_path: repo_root().to_string_lossy().into_owned(), ..default() })
                .set(WindowPlugin { primary_window: Some(window), ..default() })
                .set(client::renderer::render_plugin()),
        )
        .add_plugins(BbModelPlugin)
        .add_plugins(SlintLayerPlugin::new(AssetViewerUi::new).with_init(connect))
        .insert_resource(ClearColor(Color::srgb(0.55, 0.68, 0.82)))
        .insert_resource(args)
        .insert_resource(start_view)
        .init_resource::<Viewer>()
        .add_systems(Startup, (setup, scan_at_start))
        .add_systems(Update, (apply_actions, reload_on_save, on_loaded, orbit, turn, push_state, shoot).chain())
        .run();
}

/// How often the shown model's file is checked for a new save.
const RELOAD_POLL_SECS: f32 = 0.5;

/// Hot reload without Bevy's `file_watcher` feature, which would make the viewer
/// a different build of the client from `just run` (a second copy of Bevy in
/// `target/`): the shown model's file is polled, and a newer save reloads it.
fn reload_on_save(
    time: Res<Time>,
    assets: Res<AssetServer>,
    viewer: Res<Viewer>,
    mut seen: Local<(Option<String>, Option<std::time::SystemTime>)>,
    mut since: Local<f32>,
) {
    *since += time.delta_secs();
    if *since < RELOAD_POLL_SECS {
        return;
    }
    *since = 0.0;
    let Some(path) = viewer.selected.and_then(|i| viewer.models.get(i)) else { return };
    let modified = std::fs::metadata(repo_root().join(path)).and_then(|m| m.modified()).ok();
    if seen.0.as_deref() == Some(path.as_str()) && modified != seen.1 {
        assets.reload(path.clone());
    }
    *seen = (Some(path.clone()), modified);
}

fn connect(ui: &AssetViewerUi, world: &mut World) {
    let actions = Rc::new(RefCell::new(Vec::new()));
    let q = actions.clone();
    ui.on_pick_model(move |i| q.borrow_mut().push(Action::PickModel(i.max(0) as usize)));
    let q = actions.clone();
    ui.on_pick_clip(move |i| q.borrow_mut().push(Action::PickClip(i.max(0) as usize)));
    let q = actions.clone();
    ui.on_rescan(move || q.borrow_mut().push(Action::Rescan));
    let q = actions.clone();
    ui.on_toggle_pause(move || q.borrow_mut().push(Action::TogglePause));
    let q = actions.clone();
    ui.on_restart(move || q.borrow_mut().push(Action::Restart));
    let q = actions.clone();
    ui.on_scrub(move |t| q.borrow_mut().push(Action::Scrub(t)));
    let q = actions.clone();
    ui.on_change_speed(move |f| q.borrow_mut().push(Action::ChangeSpeed(f)));
    if world.resource::<Args>().shot.is_some() {
        ui.set_turntable(false); // a still frame for the screenshot
    }
    world.insert_non_send(Actions(actions));
}

/// Every `.bbmodel` under `roots` (relative to `base`), as sorted paths
/// relative to `base` with `/` separators.
fn scan(base: &Path, roots: &[String]) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "bbmodel") {
                out.push(path);
            }
        }
    }
    let mut found = Vec::new();
    for root in roots {
        walk(&base.join(root), &mut found);
    }
    let mut paths: Vec<String> = found
        .iter()
        .filter_map(|p| p.strip_prefix(base).ok())
        .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// How a path is listed: without the extension, and without the fixtures'
/// long prefix.
fn label(path: &str) -> String {
    let short = path.strip_prefix("crates/bbmodel/tests/").unwrap_or(path);
    short.strip_suffix(".bbmodel").unwrap_or(short).to_string()
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(512.0, 512.0))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.45, 0.45, 0.42), ..default() })),
    ));
    commands.spawn((
        DirectionalLight { illuminance: 8_000.0, shadow_maps_enabled: true, ..default() },
        Transform::default().looking_at(Vec3::new(-0.4, -1.0, -0.6), Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight { brightness: 500.0, ..default() });
    commands.spawn((Camera3d::default(), Transform::default(), client::renderer::UiCamera));
}

fn scan_at_start(args: Res<Args>, mut viewer: ResMut<Viewer>, actions: NonSend<Actions>) {
    viewer.models = scan(&repo_root(), &args.roots);
    info!("{} models under {:?}", viewer.models.len(), args.roots);
    let wanted = match &args.model {
        Some(part) => viewer.models.iter().position(|m| m.contains(part.as_str())),
        None => (!viewer.models.is_empty()).then_some(0),
    };
    if let Some(i) = wanted {
        actions.0.borrow_mut().push(Action::PickModel(i));
    }
}

#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn apply_actions(
    mut commands: Commands,
    args: Res<Args>,
    assets: Res<AssetServer>,
    actions: NonSend<Actions>,
    mut viewer: ResMut<Viewer>,
    mut players: Query<&mut BbAnimationPlayer, With<Shown>>,
) {
    let queued: Vec<Action> = actions.0.borrow_mut().drain(..).collect();
    for action in queued {
        match action {
            Action::PickModel(i) => {
                let Some(path) = viewer.models.get(i).cloned() else { continue };
                if let Some(old) = viewer.entity.take() {
                    commands.entity(old).despawn();
                }
                let handle: Handle<BbModelAsset> = assets.load(path.clone());
                let entity = commands.spawn((BbModel(handle.clone()), BbAnimationPlayer::default(), Shown)).id();
                info!("showing {path}");
                viewer.selected = Some(i);
                viewer.entity = Some(entity);
                viewer.handle = Some(handle);
                viewer.clips.clear();
            }
            Action::Rescan => {
                let current = viewer.selected.and_then(|i| viewer.models.get(i).cloned());
                viewer.models = scan(&repo_root(), &args.roots);
                viewer.selected = current.and_then(|c| viewer.models.iter().position(|m| *m == c));
            }
            other => {
                let Ok(mut player) = players.single_mut() else { continue };
                match other {
                    Action::PickClip(0) => player.stop(),
                    Action::PickClip(i) => {
                        if let Some(name) = viewer.clips.get(i) {
                            player.play(name);
                            player.paused = false;
                        }
                    }
                    Action::TogglePause => player.paused = !player.paused,
                    Action::Restart => player.elapsed = 0.0,
                    Action::Scrub(t) => player.elapsed = t,
                    Action::ChangeSpeed(f) => {
                        player.speed = (player.speed * f).clamp(SPEED_RANGE.0, SPEED_RANGE.1);
                    }
                    Action::PickModel(_) | Action::Rescan => unreachable!(),
                }
            }
        }
    }
}

/// A model finished loading (or reloaded): frame it, list its clips, play
/// the first (or `--anim`).
fn on_loaded(
    args: Res<Args>,
    models: Res<Assets<BbModelAsset>>,
    mut viewer: ResMut<Viewer>,
    mut orbit: ResMut<Orbit>,
    mut loaded: Query<(&BbModel, &mut BbAnimationPlayer), (With<Shown>, Added<BbModelSpawned>)>,
) {
    for (model, mut player) in &mut loaded {
        let Some(asset) = models.get(&model.0) else { continue };
        let (lo, hi) = asset.bounds.unwrap_or((Vec3::splat(-8.0), Vec3::splat(8.0)));
        orbit.focus = (lo + hi) / 2.0;
        orbit.distance = (hi - lo).length().max(4.0) * 1.3;
        viewer.clips = std::iter::once(REST_POSE.to_string())
            .chain(asset.animation_names().map(str::to_owned))
            .collect();
        if player.animation.is_none() {
            let first = asset.animation_names().next().map(str::to_owned);
            if let Some(name) = args.anim.clone().or(first) {
                player.play(&name);
                if let Some(t) = args.time {
                    player.elapsed = t;
                    player.paused = true;
                }
            }
        }
        for warning in &asset.skeleton.warnings {
            warn!("{}: {warning}", asset.name);
        }
    }
}

fn orbit(
    layer: Option<NonSend<SlintLayer<AssetViewerUi>>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut orbit: ResMut<Orbit>,
    mut camera: Query<&mut Transform, With<Camera3d>>,
) {
    let on_ui = layer.is_some_and(|l| l.owns_pointer);
    if !on_ui {
        if mouse.any_pressed([MouseButton::Left, MouseButton::Right]) {
            orbit.yaw -= motion.delta.x * ORBIT_PER_PX;
            orbit.pitch = (orbit.pitch + motion.delta.y * ORBIT_PER_PX).clamp(-0.2, 1.5);
        }
        let lines = match scroll.unit {
            MouseScrollUnit::Line => scroll.delta.y,
            MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
        };
        if lines != 0.0 {
            orbit.distance = (orbit.distance * ZOOM_PER_LINE.powf(lines)).clamp(2.0, 4096.0);
        }
    }
    let dir = Vec3::new(orbit.yaw.sin() * orbit.pitch.cos(), orbit.pitch.sin(), orbit.yaw.cos() * orbit.pitch.cos());
    for mut t in &mut camera {
        *t = Transform::from_translation(orbit.focus + dir * orbit.distance).looking_at(orbit.focus, Vec3::Y);
    }
}

fn turn(
    time: Res<Time>,
    layer: Option<NonSend<SlintLayer<AssetViewerUi>>>,
    mut shown: Query<&mut Transform, With<Shown>>,
) {
    if !layer.is_some_and(|l| l.overlay.ui().get_turntable()) {
        return;
    }
    for mut t in &mut shown {
        t.rotate_y(time.delta_secs() * TURN_SPEED);
    }
}

fn strings(items: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.into_iter().map(SharedString::from).collect::<Vec<_>>()))
}

/// Viewer state → panel properties, each only when it changed (a set marks
/// the UI dirty and costs a repaint).
fn push_state(
    layer: Option<NonSend<SlintLayer<AssetViewerUi>>>,
    viewer: Res<Viewer>,
    assets: Res<AssetServer>,
    models: Res<Assets<BbModelAsset>>,
    players: Query<(&BbModel, &BbAnimationPlayer), With<Shown>>,
    mut last_clips: Local<Vec<String>>,
) {
    let Some(layer) = layer else { return };
    let ui = layer.overlay.ui();
    if viewer.is_changed() {
        ui.set_models(strings(viewer.models.iter().map(|m| label(m))));
        ui.set_selected_model(viewer.selected.map_or(-1, |i| i as i32));
    }
    if *last_clips != viewer.clips {
        ui.set_clips(strings(viewer.clips.iter().cloned()));
        *last_clips = viewer.clips.clone();
    }

    let status = match viewer.handle.as_ref().and_then(|h| assets.get_load_state(h.id())) {
        Some(LoadState::Failed(e)) => format!("load failed: {e}"),
        Some(LoadState::Loading) => "loading…".into(),
        _ => String::new(),
    };
    if ui.get_status().as_str() != status {
        ui.set_status(status.into());
    }

    let Ok((model, player)) = players.single() else { return };
    let Some(asset) = models.get(&model.0) else { return };
    let clip = player.animation.as_deref().and_then(|n| asset.skeleton.animation(n));
    let selected = player
        .animation
        .as_deref()
        .and_then(|n| viewer.clips.iter().position(|c| c == n))
        .unwrap_or(0) as i32;
    if ui.get_selected_clip() != selected {
        ui.set_selected_clip(selected);
    }
    let (info, length, shown_time) = match clip {
        Some(a) => {
            let mode = format!("{:?}", a.loop_mode).to_lowercase();
            let t = match a.loop_mode {
                bbmodel::animation::LoopMode::Loop if a.length > 0.0 => player.elapsed.rem_euclid(a.length),
                _ => player.elapsed.min(a.length),
            };
            (format!("{} — {mode}, {:.2} s", a.name, a.length), a.length, t)
        }
        None => ("rest pose".to_string(), 0.0, 0.0),
    };
    if ui.get_clip_info().as_str() != info {
        ui.set_clip_info(info.into());
    }
    if ui.get_length() != length {
        ui.set_length(length);
    }
    if ui.get_time() != shown_time {
        ui.set_time(shown_time);
    }
    if ui.get_paused() != player.paused {
        ui.set_paused(player.paused);
    }
    let speed = format!("×{}", player.speed);
    if ui.get_speed().as_str() != speed {
        ui.set_speed(speed.into());
    }
    let (lo, hi) = asset.bounds.unwrap_or((Vec3::ZERO, Vec3::ZERO));
    let size = hi - lo;
    let mut lines = vec![
        format!("{} nodes, {} parts", asset.nodes.len(), asset.nodes.iter().map(|n| n.parts.len()).sum::<usize>()),
        format!("{:.1} × {:.1} × {:.1} blocks", size.x / 16.0, size.y / 16.0, size.z / 16.0),
        format!("format {} ({})", asset.skeleton.format_version, asset.skeleton.model_format),
    ];
    if !asset.skeleton.warnings.is_empty() {
        lines.push(format!("{} reader warnings (see log)", asset.skeleton.warnings.len()));
    }
    let current: Vec<String> = ui.get_model_info().iter().map(|s| s.to_string()).collect();
    if current != lines {
        ui.set_model_info(strings(lines));
    }
}

fn shoot(
    mut commands: Commands,
    args: Res<Args>,
    loaded: Query<(), (With<Shown>, With<BbModelSpawned>)>,
    mut waited: Local<u32>,
    mut after_load: Local<u32>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(path) = args.shot.as_ref() else { return };
    if loaded.is_empty() {
        *waited += 1;
        if *waited > 1200 {
            error!("model never loaded");
            exit.write(AppExit::error());
        }
        return;
    }
    // Let textures upload, shadows settle and the panels draw.
    *after_load += 1;
    if *after_load == 30 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path.clone()));
    }
    if *after_load == 60 {
        exit.write(AppExit::Success);
    }
}
