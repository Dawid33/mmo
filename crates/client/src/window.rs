use crossbeam::channel::Sender;
use game::{ClientUpdateEvent, GameData, GameEventKind, WindowEvent, WinitEvent};
#[allow(unused)]
use log::info;
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};
use winit::{
    application::ApplicationHandler,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

use crate::{input::WinitInputHelper, state::State, Command};

pub struct App {
    state: Option<State>,
    helper: WinitInputHelper,
    command_sender: Sender<Command>,
}

impl App {
    pub fn new(command_sender: Sender<Command>) -> Self {
        Self {
            state: None,
            command_sender,
            helper: WinitInputHelper::new(),
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
        window.set_title("Dems the Bricks");
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

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: winit::event::WindowEvent,
    ) {
        let state = self.state.as_mut().unwrap();

        match event {
            winit::event::WindowEvent::RedrawRequested => {
                while let Ok(event) = state.client_recv.try_recv() {
                    match event {
                        ClientUpdateEvent::NewRegion(data, player) => {
                            let id = 0;
                            if player.is_some() {
                                state.player = player;
                            }
                            state.add_region(id, data);
                        }
                        ClientUpdateEvent::GameCrash(_) => todo!(),
                        ClientUpdateEvent::UpdateRegion(id, event, kind) => {
                            // let now = std::time::Instant::now();
                            state.update(id, event, kind);
                            // info!("{:?}", now.elapsed());
                        }
                    }
                }
                state.render();
                state.get_window().request_redraw();
                state.lerp();
            }
            _ => (),
        }

        let player = if let Some(player) = state.player {
            player
        } else {
            return;
        };

        match event {
            winit::event::WindowEvent::Resized(size) => {
                state.resize(size);
                state
                    .game_send
                    .send(GameEventKind::PlayerWinitEvent(
                        player,
                        WinitEvent::WindowEvent(game::WindowEvent::Resized(size)),
                    ))
                    .unwrap();
            }
            winit::event::WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => state
                .game_send
                .send(GameEventKind::PlayerWinitEvent(
                    player,
                    WinitEvent::WindowEvent(WindowEvent::MouseInput {
                        state: button_state,
                        button: button,
                    }),
                ))
                .unwrap(),
            winit::event::WindowEvent::MouseWheel { delta, phase, .. } => state
                .game_send
                .send(GameEventKind::PlayerWinitEvent(
                    player,
                    WinitEvent::WindowEvent(WindowEvent::MouseWheel { delta, phase }),
                ))
                .unwrap(),
            winit::event::WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => state
                .game_send
                .send(GameEventKind::PlayerWinitEvent(
                    player,
                    WinitEvent::WindowEvent(WindowEvent::KeyboardInput {
                        physical_key: event.physical_key,
                        logical_key: event.logical_key,
                        location: event.location,
                        state: event.state,
                        repeat: event.repeat,
                        is_synthetic,
                    }),
                ))
                .unwrap(),
            winit::event::WindowEvent::Focused(focused) => state
                .game_send
                .send(GameEventKind::PlayerWinitEvent(
                    player,
                    WinitEvent::WindowEvent(game::WindowEvent::Focused(focused)),
                ))
                .unwrap(),
            winit::event::WindowEvent::ScaleFactorChanged { scale_factor, .. } => state
                .game_send
                .send(GameEventKind::PlayerWinitEvent(
                    player,
                    WinitEvent::WindowEvent(game::WindowEvent::ScaleFactorChanged { scale_factor }),
                ))
                .unwrap(),
            winit::event::WindowEvent::DroppedFile(path) => state
                .game_send
                .send(GameEventKind::PlayerWinitEvent(
                    player,
                    WinitEvent::WindowEvent(game::WindowEvent::DroppedFile(path)),
                ))
                .unwrap(),
            winit::event::WindowEvent::Destroyed => state
                .game_send
                .send(GameEventKind::PlayerWinitEvent(
                    player,
                    WinitEvent::WindowEvent(game::WindowEvent::Destroyed),
                ))
                .unwrap(),
            winit::event::WindowEvent::CloseRequested => {
                state
                    .game_send
                    .send(GameEventKind::PlayerWinitEvent(
                        player,
                        WinitEvent::WindowEvent(game::WindowEvent::CloseRequested),
                    ))
                    .unwrap();
                event_loop.exit();
            }
            _ => (),
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, _cause: winit::event::StartCause) {
        if let Some(state) = self.state.as_mut() {
            if let Some(player) = state.player {
                state
                    .game_send
                    .send(GameEventKind::PlayerWinitEvent(
                        player,
                        WinitEvent::NewEvents,
                    ))
                    .unwrap();
            }
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        let state = self.state.as_mut().unwrap();
        let player = if let Some(player) = state.player {
            player
        } else {
            return;
        };
        let send = move |event| {
            state
                .game_send
                .send(GameEventKind::PlayerWinitEvent(
                    player,
                    WinitEvent::DeviceEvent(event),
                ))
                .unwrap();
        };
        match event {
            winit::event::DeviceEvent::Added => send(game::DeviceEvent::Added),
            winit::event::DeviceEvent::Removed => send(game::DeviceEvent::Removed),
            winit::event::DeviceEvent::MouseMotion { delta } => {
                send(game::DeviceEvent::MouseMotion { delta })
            }
            winit::event::DeviceEvent::MouseWheel { delta } => {
                send(game::DeviceEvent::MouseWheel { delta })
            }
            winit::event::DeviceEvent::Motion { axis, value } => {
                send(game::DeviceEvent::Motion { axis, value })
            }
            winit::event::DeviceEvent::Button { button, state } => {
                send(game::DeviceEvent::Button { button, state })
            }
            winit::event::DeviceEvent::Key(raw_key_event) => {
                send(game::DeviceEvent::Key(raw_key_event))
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        let state = self.state.as_mut().unwrap();
        if let Some(player) = state.player {
            state
                .game_send
                .send(GameEventKind::PlayerWinitEvent(
                    player,
                    WinitEvent::AboutToWait,
                ))
                .unwrap();
        }
    }
}
