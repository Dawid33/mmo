use bevy::prelude::*;
use crossbeam::channel::{Receiver, Sender};
use game::{ClientId, ClientUpdateEvent, GameEventKind};

mod bridge;
pub mod convert;
mod interpolate;
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
            .add_systems(Update, interpolate::interpolate_transforms);
    }
}
