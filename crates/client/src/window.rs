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
    dpi::PhysicalSize,
    event_loop::ActiveEventLoop,
    window::{CursorGrabMode, Window, WindowId},
};

use crate::{state::State, Command};

pub struct App {
    state: Option<State>,
    command_sender: Sender<Command>,
    last_mouse_delta: (f32, f32),
    last_event_was_new_events: bool,
    mouse_motion_buffer: Option<(f64, f64)>,
    mouse_motion_buffer_sent: bool,
    cursor_on_window: bool,
}

impl App {
    pub fn new(command_sender: Sender<Command>) -> Self {
        Self {
            state: None,
            command_sender,
            last_mouse_delta: (0.0, 0.0),
            last_event_was_new_events: false,
            mouse_motion_buffer: None,
            cursor_on_window: false,
            mouse_motion_buffer_sent: false,
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
                // Send accumulated mouse motion events.
                if let Some(player) = &state.player {
                    if let Some(buf) = self.mouse_motion_buffer.take() {
                        state
                            .game_send
                            .send(GameEventKind::PlayerWinitEvent(
                                *player,
                                WinitEvent::DeviceEvent(game::DeviceEvent::MouseMotion {
                                    delta: buf,
                                }),
                            ))
                            .unwrap();
                    }
                }

                while let Ok(event) = state.client_recv.try_recv() {
                    match event {
                        ClientUpdateEvent::NewRegion(data, player, receiver) => {
                            let id = 0;
                            if player.is_some() {
                                state.player = player;
                            }
                            state.add_region(id, data, receiver);
                        }
                        ClientUpdateEvent::GameCrash(_) => todo!(),
                    }
                }
                state.update();
                state.render();
                state.get_window().request_redraw();
                return;
            }
            _ => (),
        }

        let player = if let Some(player) = state.player {
            player
        } else {
            return;
        };

        let mut sent_event = true;
        match event {
            winit::event::WindowEvent::Resized(size) => {
                state.resize(size);
                self.last_event_was_new_events = false;
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
            } => {
                self.last_event_was_new_events = false;
                if self.cursor_on_window {
                    state.window.focus_window();
                }
                state
                    .game_send
                    .send(GameEventKind::PlayerWinitEvent(
                        player,
                        WinitEvent::WindowEvent(WindowEvent::MouseInput {
                            state: button_state,
                            button: button,
                        }),
                    ))
                    .unwrap()
            }
            winit::event::WindowEvent::MouseWheel { delta, phase, .. } => {
                self.last_event_was_new_events = false;
                state
                    .game_send
                    .send(GameEventKind::PlayerWinitEvent(
                        player,
                        WinitEvent::WindowEvent(WindowEvent::MouseWheel { delta, phase }),
                    ))
                    .unwrap()
            }
            winit::event::WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                self.last_event_was_new_events = false;
                if !event.repeat {
                    state
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
                        .unwrap()
                }
            }
            winit::event::WindowEvent::Focused(focused) => {
                self.last_event_was_new_events = false;
                state
                    .game_send
                    .send(GameEventKind::PlayerWinitEvent(
                        player,
                        WinitEvent::WindowEvent(game::WindowEvent::Focused(focused)),
                    ))
                    .unwrap();
            }
            winit::event::WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.last_event_was_new_events = false;
                state
                    .game_send
                    .send(GameEventKind::PlayerWinitEvent(
                        player,
                        WinitEvent::WindowEvent(game::WindowEvent::ScaleFactorChanged {
                            scale_factor,
                        }),
                    ))
                    .unwrap()
            }
            winit::event::WindowEvent::DroppedFile(path) => {
                self.last_event_was_new_events = false;
                state
                    .game_send
                    .send(GameEventKind::PlayerWinitEvent(
                        player,
                        WinitEvent::WindowEvent(game::WindowEvent::DroppedFile(path)),
                    ))
                    .unwrap()
            }
            winit::event::WindowEvent::Destroyed => {
                self.last_event_was_new_events = false;
                state
                    .game_send
                    .send(GameEventKind::PlayerWinitEvent(
                        player,
                        WinitEvent::WindowEvent(game::WindowEvent::Destroyed),
                    ))
                    .unwrap()
            }
            winit::event::WindowEvent::CloseRequested => {
                state
                    .game_send
                    .send(GameEventKind::PlayerWinitEvent(
                        player,
                        WinitEvent::WindowEvent(game::WindowEvent::CloseRequested),
                    ))
                    .unwrap();
                self.last_event_was_new_events = false;
                event_loop.exit();
            }
            winit::event::WindowEvent::Moved(_) => self.cursor_on_window = true,
            winit::event::WindowEvent::CursorEntered { device_id } => self.cursor_on_window = true,
            winit::event::WindowEvent::CursorLeft { device_id } => self.cursor_on_window = false,
            _ => sent_event = false,
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
            winit::event::DeviceEvent::Added => {
                self.last_event_was_new_events = false;
                send(game::DeviceEvent::Added)
            }
            winit::event::DeviceEvent::Removed => {
                self.last_event_was_new_events = false;
                send(game::DeviceEvent::Removed)
            }
            winit::event::DeviceEvent::MouseMotion { delta } => {
                self.last_event_was_new_events = false;
                if let Some(buf) = &mut self.mouse_motion_buffer {
                    buf.0 += delta.0;
                    buf.1 += delta.1;
                } else {
                    self.mouse_motion_buffer = Some(delta);
                }
            }
            winit::event::DeviceEvent::MouseWheel { delta } => {
                self.last_event_was_new_events = false;
                send(game::DeviceEvent::MouseWheel { delta })
            }
            _ => (),
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        if let Some(state) = self.state.as_mut() {
            let player = if let Some(player) = state.player {
                player
            } else {
                return;
            };
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(state) = self.state.as_mut() {
            let player = if let Some(player) = state.player {
                player
            } else {
                return;
            };
        }
    }
}
