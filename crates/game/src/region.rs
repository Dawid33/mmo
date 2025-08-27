use std::{
    any::Any,
    cmp::Reverse,
    collections::{BinaryHeap, VecDeque},
};

use crossbeam::channel::Sender;
use log::info;
use parley::{Alignment, AlignmentOptions, FontContext, Layout, LayoutContext, StyleProperty};

use crate::{
    camera::CameraController, data::GameData, physics::PhysicsController, ClientUpdateEvent,
    Controller, GameDataTransaction, GameError, GameEvent, GameEventKind, RegionId,
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
    pub event_log: VecDeque<(GameEvent, Box<dyn Fn(&mut GameData)>)>,
    pub data: GameData,
    id: RegionId,
    input_buffer: BinaryHeap<Reverse<GameEvent>>,
    controllers: Vec<Box<dyn Controller>>,
    font_context: FontContext,
    client_event_send: Option<Sender<ClientUpdateEvent>>,
}

impl Region {
    /// Create new region
    pub fn new(
        data: GameData,
        client_event_send: Option<Sender<ClientUpdateEvent>>,
        id: RegionId,
    ) -> Self {
        Self {
            data,
            event_log: VecDeque::new(),
            input_buffer: BinaryHeap::new(),
            controllers: Vec::from([CameraController::new(), PhysicsController::new()]),
            font_context: FontContext::new(),
            client_event_send,
            id,
        }
    }

    /// Check if event from network matches client event history. rollback the
    /// game state as neccessary and re-simulate to current time.
    pub fn reconcile(&mut self, server_event: GameEvent) -> Result<(), GameError> {
        info!("input buffer {:?}", self.input_buffer);
        info!("incoming server event {:?}", server_event);
        self.input_buffer.push(Reverse(server_event.clone()));

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
        let mut t = GameDataTransaction::new(&mut self.data, &self.client_event_send, self.id);
        match event.kind {
            GameEventKind::Tick => {
                for c in self.controllers.iter_mut() {
                    c.on_tick(&mut t);
                }
                t.tick();
            }
            GameEventKind::PlayerWinitEvent(player, event) => {
                for c in self.controllers.iter_mut() {
                    c.on_player_event(&mut t, player, &event);
                }
                t.update_player_input(player, &event);
            }
            GameEventKind::Quit => (),
        }

        // let log = t.data.log.lock().unwrap();
        // info!("{:?}", log.all.get(0).unwrap());
        Ok(())
    }
}
