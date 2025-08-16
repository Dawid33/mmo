use crossbeam::channel::Sender;
use game::{ClientUpdateEvent, GameData, GameEventKind};
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

use crate::{state::State, Command};

pub struct App {
    state: Option<State>,
    command_sender: Sender<Command>,
    regions: BTreeMap<usize, GameData>,
}

impl App {
    pub fn new(command_sender: Sender<Command>) -> Self {
        Self {
            state: None,
            command_sender,
            regions: BTreeMap::new(),
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes())
                .unwrap(),
        );
        window.set_title("Brick Racer");
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        self.command_sender
            .send(Command::ConnectToServerAndScene(
                game_send.clone(),
                game_recv,
                client_send,
                SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 6466)),
            ))
            .unwrap();
        let state = pollster::block_on(State::new(window.clone(), client_recv, game_send));
        self.state = Some(state);

        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let state = self.state.as_mut().unwrap();

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                while let Ok(event) = state.client_recv.try_recv() {
                    match event {
                        ClientUpdateEvent::NewRegion(data) => {
                            let id = 0;
                            let data = GameData::new(data, None, id);
                            state.add_region(&data);
                            self.regions.insert(id, data);
                        }
                        ClientUpdateEvent::GameCrash(_) => todo!(),
                        ClientUpdateEvent::UpdateRegion(id, event, kind) => {
                            let data = self.regions.get_mut(&id).unwrap();
                            state.update(event, data, kind);
                        }
                    }
                }
                state.render(&self.regions);
                state.get_window().request_redraw();
            }
            WindowEvent::Resized(size) => {
                state.resize(size);
            }
            WindowEvent::KeyboardInput { event, .. } => state
                .game_send
                .send(GameEventKind::KeyboardEvent(
                    event.physical_key,
                    event.state,
                ))
                .unwrap(),
            WindowEvent::MouseInput {
                device_id,
                state: button_state,
                button,
            } => state
                .game_send
                .send(GameEventKind::MouseEvent(button, button_state))
                .unwrap(),
            _ => (),
        }
    }
}
