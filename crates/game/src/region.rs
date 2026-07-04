use std::{
    cmp::Reverse,
    collections::{BinaryHeap, VecDeque},
    ops::DerefMut,
};

use crossbeam::channel::Sender;
use log::info;
use crate::input::Key;
use crate::{Client, ClientId, GameData, Rollback};

use crate::{
    camera::CameraController, physics::PhysicsController, ChunkCoords,
    Controller, GameDataUpdate, GameError, GameEvent, GameEventKind, RegionId, ServerPacket,
};

/// A region represents an portion of the world that processes game events at
/// its own tick rate separately from other regions.
#[allow(unused)]
pub struct Region {
    event_log: VecDeque<GameEvent>,
    data: Rollback,
    id: RegionId,
    input_buffer: BinaryHeap<Reverse<GameEvent>>,
    controllers: Vec<Box<dyn Controller>>,
    synchronized: bool,
    /// `Some` on a client (which client's predictions live in `event_log`);
    /// `None` on the server, which never reconciles.
    local_client_id: Option<ClientId>,
    /// `next_game_event_id` of the snapshot this region was built from.
    /// Server events below this id are already baked into the state.
    base_event_id: usize,
}

impl Region {
    /// Create new region
    pub fn new(
        mut data: Rollback,
        game_update_send: Option<Sender<GameDataUpdate>>,
        id: ChunkCoords,
        local_client_id: Option<ClientId>,
    ) -> Self {
        data.reinitialize(game_update_send);
        let base_event_id = *data.next_game_event_id;
        Self {
            data,
            event_log: VecDeque::new(),
            input_buffer: BinaryHeap::new(),
            controllers: Vec::from([CameraController::new(), PhysicsController::new()]),
            id,
            synchronized: false,
            local_client_id,
            base_event_id,
        }
    }

    /// Check if event from network matches client event history. rollback the
    /// game state as neccessary and re-simulate to current time.
    pub fn reconcile(&mut self, server_event: GameEvent) -> Result<(), GameError> {
        // Events older than the snapshot this region was constructed from
        // are already baked into its state.
        if server_event.id < self.base_event_id {
            return Ok(());
        }
        self.input_buffer.push(Reverse(server_event.clone()));

        if self.input_buffer.len() > 1000 {
            panic!("Server event input buffer too big.");
        }

        if self.event_log.len() == 0 {
            return Ok(());
        }

        'outer: while !self.input_buffer.is_empty() && !self.event_log.is_empty() {
            if let Some(server_event) = self.input_buffer.pop() {
                if let Some(event) = self.event_log.pop_front() {
                    if event.id == server_event.0.id {
                        if event != server_event.0 {
                            self.event_log.push_front(event);
                            let mut temp_log = self.event_log.clone();

                            // rollback whole event log
                            while let Some(_e) = self.event_log.pop_back() {
                                self.data.rollback();
                            }

                            // apply server event that was different from expected.
                            self.handle_event(server_event.clone().0.kind)?;
                            self.data.forget();

                            // Remove the corresponding event from event log, it must
                            // have happened later.
                            // TODO: DO NOT DO THIS FOR EVENTS THAT ORIGINATE FROM
                            // OTHER CLIENTS
                            {
                                let len = temp_log.len();
                                let mut iter = temp_log.iter_mut().enumerate();
                                while let Some((i, event)) = iter.next() {
                                    if event.kind == server_event.0.kind {
                                        drop(iter);
                                        temp_log.remove(i);
                                        break;
                                    }
                                    event.id += 1;
                                }
                                if len == temp_log.len() {
                                    info!(
                                        "Client didn't have event recieved from server. Client must be behind."
                                    );
                                }
                            }

                            // TODO: if from other client, increase event id' as well as self.next_game_event_id
                            for event in &mut temp_log {
                                let _ = self.handle_event(event.clone().kind);
                            }
                            self.event_log = temp_log;
                        } else {
                            self.data.forget();
                        }
                    } else {
                        // IDs are incorrect, game event arrived out of order.
                        info!("Arrived out of order, Adding to input buffer.");
                        info!("{:?} != {:?}", server_event, event);
                        self.input_buffer.push(server_event);
                        self.event_log.push_front(event);
                        break 'outer;
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle a client event.
    pub fn handle_event(&mut self, event: GameEventKind) -> Result<GameEvent, GameError> {
        let event = GameEvent::new(event, *self.data.next_game_event_id, self.id);
        self.data.new_transaction();
        self.data.next_game_event_id.update(|n| *n += 1);
        let _b = &event.kind;
        match event.kind.clone() {
            GameEventKind::Tick => {
                for c in self.controllers.iter_mut() {
                    let data = self.data.as_refs_mut();
                    c.on_tick(data.data);
                }
                let data: &mut GameData = self.data.deref_mut();
                let keys: Vec<ClientId> = data.clients.keys().cloned().collect();
                for client_id in keys {
                    // get_mut logs the prior Client, so the toggle and the
                    // input step are both covered by one delta.
                    let toggled = {
                        let client = data.clients.get_mut(&client_id).unwrap();
                        let toggle = client.input.key_pressed(&Key::KeyE);
                        if toggle {
                            client.fps_cam_mode = !client.fps_cam_mode;
                        }
                        let _ = client.input.step();
                        toggle.then_some(client.fps_cam_mode)
                    };
                    if let Some(mode) = toggled {
                        data.ecs.send(GameDataUpdate::new(
                            crate::GameDataTransactionKind::Do,
                            crate::GameDataUpdateKind::SetFreeCam(client_id, mode),
                        ));
                    }
                }
                self.data.tick.update(|t| *t += 1);
            }
            GameEventKind::PlayerInput(client_id, player_event) => {
                let data: &mut GameData = self.data.deref_mut();
                if let Some(c) = data.clients.get_mut(&client_id) {
                    let _ = c.input.update(player_event.clone());
                }
            }
            GameEventKind::Quit => return Err(GameError::QuitRequested),
            GameEventKind::CreateClient(client_id) => {
                info!("{:?}", event);
                self.data.clients.insert(client_id, Client::default());
                self.data.create_player_safe(client_id);
            }
        }
        self.event_log.push_back(event.clone());
        Ok(event)
    }

    pub fn build_region_server_packet(&self, region_id: &RegionId) -> ServerPacket {
        ServerPacket::Region(*region_id, self.data.clone())
    }

    pub fn current_tick(&self) -> usize {
        *self.data.tick
    }

    /// Ids of locally-predicted events awaiting server confirmation.
    /// Exposed for tests.
    pub fn pending_event_ids(&self) -> Vec<usize> {
        self.event_log.iter().map(|e| e.id).collect()
    }

    pub fn data(&self) -> &Rollback {
        &self.data
    }

    pub fn create_basic(&mut self, coords: ChunkCoords) {
        self.data.create_mesh(coords);
    }

    pub fn forget_last_event(&mut self) {
        self.data.forget();
    }
}
