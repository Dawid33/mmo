//! Look at a Blockbench model: `just model-view <path> [--shot out.png]`.
//!
//! The path is relative to the repo root (e.g. `assets/models/soldier.bbmodel`
//! or `crates/bbmodel/tests/fixtures/hephaestus/dragon.bbmodel`). The model
//! turns slowly on a stone-grey floor one block (16 units) per grid square,
//! framed from its bounds. Edit and save the file while it runs: it reloads.
//! `--shot <png>` saves a screenshot after the model has loaded and exits
//! (a headless-ish check that it renders).
//!
//! Animations: `--anim <name>` plays one from the start (default: the
//! first); `--time <seconds>` freezes it at that moment (for screenshots).
//! In the window, N plays the next animation, Space pauses, R restarts.
use bevy::app::AppExit;
use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use client::renderer::model::{BbAnimationPlayer, BbModel, BbModelAsset, BbModelPlugin, BbModelSpawned};

#[derive(Resource)]
struct Args {
    path: String,
    shot: Option<String>,
    anim: Option<String>,
    time: Option<f32>,
}

#[derive(Component)]
struct Turntable;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut shot = None;
    let mut anim = None;
    let mut time = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--shot" => shot = args.next(),
            "--anim" => anim = args.next(),
            "--time" => time = args.next().and_then(|t| t.parse().ok()),
            other => path = Some(other.to_string()),
        }
    }
    let path = path.unwrap_or_else(|| {
        eprintln!("usage: model_view <path relative to the repo root> [--shot out.png]");
        std::process::exit(2)
    });
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(AssetPlugin { file_path: "../..".into(), watch_for_changes_override: Some(true), ..default() })
                .set(WindowPlugin {
                    primary_window: Some(Window { title: format!("model_view: {path}"), ..default() }),
                    ..default()
                })
                .set(client::renderer::render_plugin()),
        )
        .add_plugins(BbModelPlugin)
        .insert_resource(ClearColor(Color::srgb(0.55, 0.68, 0.82)))
        .insert_resource(Args { path, shot, anim, time })
        .add_systems(Startup, setup)
        .add_systems(Update, (frame_camera, turn, shoot, animation_keys))
        .run();
}

fn setup(
    mut commands: Commands,
    args: Res<Args>,
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let model: Handle<BbModelAsset> = assets.load(args.path.clone());
    commands.spawn((BbModel(model), Turntable, BbAnimationPlayer::default()));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(256.0, 256.0))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.45, 0.45, 0.42), ..default() })),
    ));
    commands.spawn((
        DirectionalLight { illuminance: 8_000.0, shadow_maps_enabled: true, ..default() },
        Transform::default().looking_at(Vec3::new(-0.4, -1.0, -0.6), Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight { brightness: 500.0, ..default() });
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(40.0, 30.0, 40.0).looking_at(Vec3::ZERO, Vec3::Y),
        client::renderer::UiCamera,
    ));
}

/// Point the camera at the model once its bounds are known.
fn frame_camera(
    args: Res<Args>,
    models: Res<Assets<BbModelAsset>>,
    mut loaded: Query<(&BbModel, &mut BbAnimationPlayer), Added<BbModelSpawned>>,
    mut camera: Query<&mut Transform, With<Camera3d>>,
) {
    for (model, mut player) in &mut loaded {
        let Some(asset) = models.get(&model.0) else { continue };
        let (lo, hi) = asset.bounds.unwrap_or((Vec3::splat(-8.0), Vec3::splat(8.0)));
        let centre = (lo + hi) / 2.0;
        let radius = (hi - lo).length().max(4.0);
        info!(
            "model {:?}: {} nodes, {} parts, bounds {lo} .. {hi} (units; 16 = 1 block), animations {:?}",
            asset.name,
            asset.nodes.len(),
            asset.nodes.iter().map(|n| n.parts.len()).sum::<usize>(),
            asset.animation_names().collect::<Vec<_>>()
        );
        let first = asset.animation_names().next().map(str::to_owned);
        if let Some(name) = args.anim.clone().or(first) {
            info!("playing {name:?} (N: next, Space: pause, R: restart)");
            player.play(&name);
            if let Some(t) = args.time {
                player.elapsed = t;
                player.paused = true;
            }
        }
        for mut t in &mut camera {
            *t = Transform::from_translation(centre + Vec3::new(1.0, 0.7, 1.0).normalize() * radius * 1.3)
                .looking_at(centre, Vec3::Y);
        }
    }
}

fn turn(time: Res<Time>, args: Res<Args>, mut q: Query<&mut Transform, With<Turntable>>) {
    if args.shot.is_some() {
        return; // a still frame for the screenshot
    }
    for mut t in &mut q {
        t.rotate_y(time.delta_secs() * 0.5);
    }
}

fn shoot(
    mut commands: Commands,
    args: Res<Args>,
    loaded: Query<(), With<BbModelSpawned>>,
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
    // Let textures upload and shadows settle, then shoot once and exit.
    *after_load += 1;
    if *after_load == 30 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path.clone()));
    }
    if *after_load == 60 {
        exit.write(AppExit::Success);
    }
}

fn animation_keys(
    keys: Res<ButtonInput<KeyCode>>,
    models: Res<Assets<BbModelAsset>>,
    mut players: Query<(&BbModel, &mut BbAnimationPlayer)>,
) {
    for (model, mut player) in &mut players {
        let Some(asset) = models.get(&model.0) else { continue };
        if keys.just_pressed(KeyCode::Space) {
            player.paused = !player.paused;
        }
        if keys.just_pressed(KeyCode::KeyR) {
            player.elapsed = 0.0;
        }
        if keys.just_pressed(KeyCode::KeyN) {
            let names: Vec<&str> = asset.animation_names().collect();
            if names.is_empty() {
                continue;
            }
            let next = player
                .animation
                .as_deref()
                .and_then(|cur| names.iter().position(|n| *n == cur))
                .map_or(0, |i| (i + 1) % names.len());
            info!("playing {:?}", names[next]);
            player.crossfade(names[next], 0.25);
        }
    }
}
