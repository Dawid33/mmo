use std::{
    any::Any,
    borrow::BorrowMut,
    cmp::Reverse,
    collections::{BinaryHeap, VecDeque},
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut},
    time::Instant,
};

use borrow::RefCast;
use crossbeam::channel::Sender;
use log::{info, warn};
use parley::{Alignment, AlignmentOptions, FontContext, Layout, LayoutContext, StyleProperty};
use rollback::PlayerKey;
use winit::keyboard::{KeyCode, SmolStr};

use crate::{
    camera::CameraController,
    data::{Ecs, GameData, Rollback, Undo},
    physics::PhysicsController,
    ClientUpdateEvent, Controller, GameDataUpdate, GameError, GameEvent, GameEventKind, RegionId,
    ServerPacket, INDUCED_LATENCY,
};

#[allow(unused)]
pub fn text_layout<'a>(fctx: &mut FontContext, text: &str) -> Layout<()> {
    let mut l: LayoutContext<()> = LayoutContext::new();
    let mut l = l.ranged_builder(fctx, &text, 1.0, true);
    l.push_default(StyleProperty::FontSize(16.0));
    let mut layout = l.build(text);
    layout.align(None, Alignment::Start, AlignmentOptions::default());
    layout.break_all_lines(Some(10000.0));
    layout
}

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
    // font_context: FontContext,
}

impl Region {
    /// Create new region
    pub fn new(
        mut data: Rollback,
        game_update_send: Option<Sender<GameDataUpdate>>,
        id: RegionId,
    ) -> Self {
        data.reinitialize(game_update_send);
        Self {
            data,
            event_log: VecDeque::new(),
            input_buffer: BinaryHeap::new(),
            controllers: Vec::from([CameraController::new(), PhysicsController::new()]),
            id,
            synchronized: false,
        }
    }

    /// Check if event from network matches client event history. rollback the
    /// game state as neccessary and re-simulate to current time.
    pub fn reconcile(&mut self, server_event: GameEvent) -> Result<(), GameError> {
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
                            while let Some(e) = self.event_log.pop_back() {
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
                                let mut len = temp_log.len();
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
                                self.handle_event(event.clone().kind);
                            }
                            self.event_log = temp_log;
                        } else {
                            self.data.forget();
                        }
                    } else {
                        // IDs are incorrect, game event arrived out of order.
                        info!("Arrived out of order, Adding to input buffer.");
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
        self.data.next_game_event_id.undo(|d, _| *d -= 1);
        *self.data.next_game_event_id += 1;
        match &event.kind {
            GameEventKind::Tick => {
                for c in self.controllers.iter_mut() {
                    let data = self.data.as_refs_mut();
                    c.on_tick(data.data);
                }
                let data: &mut GameData = self.data.deref_mut();

                if let Some((p, e)) = data.players.iter().next() {
                    let e = e.clone();
                    let p = data.ecs.player.get_mut(e);
                    if p.input.key_pressed(&winit::keyboard::KeyCode::KeyE) {
                        let old = p.fps_cam_mode;
                        data.ecs.player.undo(move |p, _| {
                            p.get_mut(e).fps_cam_mode = old;
                        });
                        let p = data.ecs.player.get_mut(e);
                        p.fps_cam_mode = !p.fps_cam_mode;
                        let mode = p.fps_cam_mode;
                        data.ecs.send(GameDataUpdate::new(
                            crate::GameDataTransactionKind::Do,
                            crate::GameDataUpdateKind::SetFreeCam(e, mode),
                        ));
                    }

                    let mut players = data.ecs.player.delayed_undo();
                    let p = players.get_mut(e);
                    if let Some(undo_func) = p.input.step() {
                        players.undo(move |player, _| {
                            let p = &mut player.get_mut(e).input;
                            undo_func(p);
                        })
                    }
                }
                self.data.tick.undo(|d, _| *d -= 1);
                *self.data.tick += 1;
            }
            GameEventKind::PlayerWinitEvent(player_key, player_event) => {
                let data: &mut GameData = self.data.deref_mut();
                if let Some((p, e)) = data.players.iter().next() {
                    let e = e.clone();
                    let mut players = data.ecs.player.delayed_undo();
                    let p = players.get_mut(e);
                    if let Some(undo_func) = p.input.update(player_event.clone()) {
                        players.undo(move |player, _| {
                            let p = &mut player.get_mut(e).input;
                            undo_func(p);
                        })
                    }
                }
            }
            GameEventKind::Quit => return Err(GameError::QuitRequested),
        }
        self.event_log.push_back(event.clone());
        Ok(event)
    }

    pub fn build_region_server_packet(&self, region_id: usize, player: PlayerKey) -> ServerPacket {
        ServerPacket::Region(region_id, self.data.clone(), player)
    }

    pub fn current_tick(&self) -> usize {
        *self.data.tick
    }

    pub fn data(&self) -> &Rollback {
        &self.data
    }

    pub fn create_basic(&mut self, create_player: bool) -> Option<PlayerKey> {
        let p = if create_player {
            Some(self.data.create_player_safe())
        } else {
            None
        };
        self.data.create_mesh();
        p
    }

    pub fn forget_last_event(&mut self) {
        self.data.forget();
    }
}
