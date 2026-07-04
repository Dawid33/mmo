use bevy::prelude::*;
use crossbeam::channel::{Receiver, Sender};
use game::{ClientId, ClientUpdateEvent, GameEventKind};

mod bridge;
pub mod convert;
mod interpolate;
pub mod meshing;
pub use bridge::*;

#[derive(Resource)]
pub struct ClientUpdates(pub Receiver<ClientUpdateEvent>);

#[derive(Resource)]
pub struct GameEvents(pub Sender<GameEventKind>);

#[derive(Resource, Default)]
pub struct LocalPlayer(pub Option<ClientId>);

pub struct SimBridgePlugin {
    pub client_recv: Receiver<ClientUpdateEvent>,
    pub game_send: Sender<GameEventKind>,
}

impl Plugin for SimBridgePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClientUpdates(self.client_recv.clone()))
            .insert_resource(GameEvents(self.game_send.clone()))
            .init_resource::<LocalPlayer>()
            .init_resource::<bridge::Regions>()
            .init_resource::<bridge::RegionRoots>()
            .init_resource::<bridge::SimEntityMap>()
            .add_systems(
                PreUpdate,
                (bridge::drain_client_updates, bridge::drain_region_updates).chain(),
            )
            .add_systems(Startup, setup_scene)
            .add_systems(Update, (meshing::mesh_chunks, interpolate::interpolate_transforms));
    }
}

fn setup_scene(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(meshing::ChunkMaterial(materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.9,
        ..Default::default()
    })));
    commands.spawn((
        DirectionalLight { color: Color::srgb(0.98, 0.95, 0.82), shadows_enabled: true, ..Default::default() },
        Transform::default().looking_at(Vec3::new(-0.15, -0.1, 0.15), Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.98, 0.95, 0.82),
        brightness: 100.0,
        ..Default::default()
    });
}
