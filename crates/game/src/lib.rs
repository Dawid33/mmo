//! Game simulation code that is shared between client and server.
// #![deny(missing_docs)]

use parley::FontContext;
use std::collections::BTreeMap;
use winit::{
    event::{ElementState, MouseButton},
    keyboard::PhysicalKey,
};

mod camera;
mod common;
mod data;
mod physics;
mod region;
mod transaction;

use crate::transaction::Transaction;

pub use camera::{Camera, CameraUniform};
pub use common::*;
pub use data::{
    Entity, EntityType, GameData, GameDataRaw, GameDataTransactionKind, UpdateGameData,
};
pub use region::{Region, RegionData};

trait Controller {
    fn on_tick<'a>(&mut self, t: &mut Transaction<'a>);
    fn on_keyboard_event<'a>(
        &mut self,
        t: &mut Transaction<'a>,
        key: PhysicalKey,
        state: ElementState,
    );
    fn on_mouse_event<'a>(
        &mut self,
        t: &mut Transaction<'a>,
        key: MouseButton,
        state: ElementState,
    );
}

/// A word is a collection of regions that communicate with one another
/// via IPC.
pub struct World {
    regions: BTreeMap<usize, Region>,
    last_game_event_id: usize,
}

impl World {
    pub fn new() -> Self {
        return Self {
            regions: BTreeMap::new(),
            last_game_event_id: 0,
        };
    }

    pub fn editor() -> Self {
        let id = 0;
        let mut cam = Entity::default();
        cam.kind = EntityType::Camera(Camera::new());
        let mut data = GameData::new(GameDataRaw::new(), None, id);
        data.change().create_entity(cam);

        let data = RegionData::new(data, FontContext::new());
        return Self {
            regions: BTreeMap::from([(id, Region::new(data))]),
            last_game_event_id: 0,
        };
    }

    pub fn clone_game_data(&self, id: &usize) -> GameDataRaw {
        self.regions.get(id).unwrap().data.data.raw().clone()
    }

    pub fn load(&mut self, id: &usize, region: Region, last_game_event_id: usize) {
        self.last_game_event_id = last_game_event_id;
        self.regions.insert(*id, region);
    }

    pub fn handle_event(
        &mut self,
        event: GameEventKind,
        region_id: usize,
    ) -> Result<GameEvent, GameError> {
        let event = GameEvent::new(event, self.last_game_event_id, region_id);
        self.last_game_event_id += 1;
        let region = self.regions.get_mut(&event._region_id).unwrap();
        region.handle_event(event)?;
        Ok(event)
    }

    pub fn reconcile_event(&mut self, _event: GameEvent) -> Result<(), GameError> {
        // self.regions
        //     .get_mut(&event._region_id)
        //     .unwrap()
        //     .reconcile(event)
        Ok(())
    }

    pub fn build_region_server_packet(&self, region_id: usize) -> ServerPacket {
        ServerPacket::Region(
            region_id,
            self.clone_game_data(&region_id),
            self.last_game_event_id,
        )
    }
}
