//! Minimal gizmo-rendering probe: a lit mesh cube (reference) and a red gizmo
//! line in front of a default camera. Screenshots to /tmp and exits, so the
//! result can be inspected without eyes on the window. Diagnostic for
//! "gizmos draw but nothing shows" — compares the gizmo pipeline against the
//! mesh pipeline under this machine's adapter/driver.
use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_systems(Startup, setup)
        .add_systems(Update, (draw_gizmos, screenshot_and_exit))
        .run();
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(0.0, 0.0, 10.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
        MeshMaterial3d(mats.add(Color::srgb(0.2, 0.8, 0.2))),
        Transform::from_xyz(-2.5, 0.0, 0.0),
    ));
    commands.spawn((
        DirectionalLight::default(),
        Transform::default().looking_at(Vec3::new(-1.0, -1.0, -1.0), Vec3::Y),
    ));
}

fn draw_gizmos(mut gizmos: Gizmos) {
    gizmos.line(
        Vec3::new(0.0, -3.0, 0.0),
        Vec3::new(3.0, 3.0, 0.0),
        bevy::color::palettes::css::RED,
    );
    gizmos.cube(Transform::from_xyz(2.5, 0.0, 0.0), bevy::color::palettes::css::YELLOW);
}

fn screenshot_and_exit(
    mut commands: Commands,
    mut frames: Local<u32>,
    mut exit: MessageWriter<AppExit>,
) {
    *frames += 1;
    if *frames == 20 {
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk("/tmp/gizmo_smoke.png"));
    }
    if *frames == 60 {
        exit.write(AppExit::Success);
    }
}
