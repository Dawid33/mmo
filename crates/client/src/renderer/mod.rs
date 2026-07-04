use bevy::prelude::*;
use crossbeam::channel::{Receiver, Sender};
use game::{ClientId, ClientUpdateEvent, GameEventKind};

pub mod convert;

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
            .add_systems(PreUpdate, drain_client_updates);
    }
}

fn drain_client_updates(updates: Res<ClientUpdates>, mut player: ResMut<LocalPlayer>) {
    while let Ok(event) = updates.0.try_recv() {
        match event {
            ClientUpdateEvent::NewRegion(id, _data, _receiver) => {
                info!("bridge: new region {:?}", id);
            }
            ClientUpdateEvent::SetPlayer(client_id) => {
                info!("bridge: local player {:?}", client_id);
                player.0 = Some(client_id);
            }
            ClientUpdateEvent::GameCrash(e) => {
                error!("bridge: game thread crashed: {:?}", e);
            }
        }
    }
}
