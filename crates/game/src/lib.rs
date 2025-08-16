//! Game simulation code that is shared between client and server.
// #![deny(missing_docs)]

use log::info;
use parley::FontContext;
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap, VecDeque},
};
use winit::{event::ElementState, keyboard::PhysicalKey};

mod camera;
mod common;
mod data;
mod physics;
mod region;
mod transaction;

use crate::{camera::CameraController, transaction::Transaction};

pub use camera::{Camera, CameraUniform};
pub use common::*;
pub use data::{Entity, EntityType, GameData, GameDataRaw, UpdateGameData};
pub use region::RegionData;

trait Controller {
    fn on_tick<'a>(&mut self, t: &mut Transaction<'a>);
    fn on_keyboard_event<'a>(
        &mut self,
        t: &mut Transaction<'a>,
        key: PhysicalKey,
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
        data.create_entity(cam);

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

/// A region represents an portion of the world that processes game events at
/// its own tick rate separately from other regions.
pub struct Region {
    pub event_log: VecDeque<(GameEvent, Box<dyn Fn(&mut RegionData)>)>,
    input_buffer: BinaryHeap<Reverse<GameEvent>>,
    data: RegionData,
    controllers: Vec<Box<dyn Controller>>,
}

impl Region {
    /// Create new region
    pub fn new(data: RegionData) -> Self {
        Self {
            data,
            event_log: VecDeque::new(),
            input_buffer: BinaryHeap::new(),
            controllers: Vec::from([CameraController::new()]),
        }
    }

    /// Check if event from network matches client event history. rollback the
    /// game state as neccessary and re-simulate to current time.
    pub fn reconcile(&mut self, server_event: GameEvent) -> Result<(), GameError> {
        info!("input buffer {:?}", self.input_buffer);
        info!("incoming server event {:?}", server_event);
        self.input_buffer.push(Reverse(server_event));

        if self.input_buffer.len() > 10 {
            panic!("Server event input buffer too big.");
        }

        if self.event_log.len() == 0 {
            info!("adding to buffer {:?}", server_event);
            return Ok(());
        }

        'outer: while let Some(server_event) = self.input_buffer.pop() {
            info!("comparing server event {:?}", server_event);
            while let Some((event, rollback)) = self.event_log.pop_front() {
                self.event_log.iter().for_each(|e| {
                    info!("rollback log: {:?}", e.0);
                });
                info!("popped event {:?}", event);
                if event.id == server_event.0.id {
                    // Events have the same ID, meaning this event is correctly
                    // next in order.
                    if event == server_event.0 {
                        // Incoming event is the same as event that was
                        // executed. Pure bliss.
                        info!("BLISS");
                        break;
                    } else {
                        rollback(&mut self.data);
                        // TODO: Discrepency detected.
                        // - store all events in rollback log into temp buffer
                        // - must rollback whole log
                        // - handle server event
                        // - re-apply all events from temp buffer.
                        todo!("Rollback is not implemented.");
                    }
                } else {
                    // IDs are incorrect, game event arrived out of order.
                    info!("Adding to input buffer.");
                    self.input_buffer.push(server_event);
                    break 'outer;
                }
            }
        }
        info!("FINISHED RECONCILING");
        Ok(())
    }

    /// Handle a client event.
    pub fn handle_event(&mut self, event: GameEvent) -> Result<(), GameError> {
        let mut t = Transaction::new(event, &mut self.event_log, &mut self.data);
        match event.kind {
            GameEventKind::Tick => {
                for c in self.controllers.iter_mut() {
                    c.on_tick(&mut t);
                }
                t.tick();
            }
            GameEventKind::Quit => todo!(),
            GameEventKind::MouseEvent => {}
            GameEventKind::KeyboardEvent(id, state) => {
                for c in self.controllers.iter_mut() {
                    c.on_keyboard_event(&mut t, id, state);
                }
            }
        }
        //TODO: Temporary while reconciliation is off.
        t.event_log.clear();
        Ok(())
    }
}
