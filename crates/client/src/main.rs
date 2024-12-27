use game::Region;

/// - Receive client updates from game instance manager.
/// - Update entities to closer approximate simulation snapshot
/// - Render game
pub struct Window {}

/// Only issue client updates for entites based for their authoritative region.
///
/// ## Intial setup
///
/// - Talk to ingress to figure out which regions to load for a given player.
/// - Start listening game events and log them.
/// - Download game data
/// - Re-simulate game based on logged events.
/// - Send spawn player event and enter main loop
///
/// ## Main Loop
/// Recieve and process game events from the client and network, in that order.
///
/// For each region:
/// - If received a client event:
///   - Simulate the event and push it on to a buffer.
///   - Send it to the players authoritative region.
///   - The client buffer should always be larger than the
///     network buffer by at least the number of ticks the client is ahead of
///     the server.
/// - If recieved a network event:
///   - Check if the network and client event on the front of each buffer match.
///   - Keep doing this until the network buffer is empty. Reconcile the region
///     each time the comparison succeeds.
///   - Any time it doesn't succeed, rollback the client, apply the network
///     event and then re-apply the client events that were just rolled back.
///
/// If sync clock event: send client update to window with desired input delay
/// and update tick time.
pub struct GameInstanceManager {
    instances: Vec<Region>,
}

use bevy::{
    pbr::{CascadeShadowConfigBuilder, DirectionalLightShadowMap},
    prelude::*,
};
use std::f32::consts::*;

fn main() {
    App::new()
        .insert_resource(DirectionalLightShadowMap { size: 4096 })
        .add_plugins(DefaultPlugins)
        .add_systems(Startup, setup)
        // .add_systems(Update, animate_light_direction)
        .run();
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    // commands.spawn((
    //     Camera3d::default(),
    //     Transform::from_xyz(0.7, 0.7, 1.0).looking_at(Vec3::new(0.0, 0.3, 0.0), Vec3::Y),
    // ));

    // commands.spawn((
    //     DirectionalLight {
    //         shadows_enabled: true,
    //         ..default()
    //     },
    //     // This is a relatively small scene, so use tighter shadow
    //     // cascade bounds than the default for better quality.
    //     // We also adjusted the shadow map to be larger since we're
    //     // only using a single cascade.
    //     CascadeShadowConfigBuilder {
    //         num_cascades: 1,
    //         maximum_distance: 1.6,
    //         ..default()
    //     }
    //     .build(),
    // ));
    // commands.spawn(SceneRoot(
    //     asset_server.load(GltfAssetLabel::Scene(0).from_asset("region_0.gltf")),
    // ));
}

fn animate_light_direction(
    time: Res<Time>,
    mut query: Query<&mut Transform, With<DirectionalLight>>,
) {
    for mut transform in &mut query {
        transform.rotation = Quat::from_euler(
            EulerRot::ZYX,
            0.0,
            time.elapsed_secs() * PI / 5.0,
            -FRAC_PI_4,
        );
    }
}
