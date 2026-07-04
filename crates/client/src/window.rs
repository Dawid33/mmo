use crossbeam::channel::Sender;
use game::{ClientUpdateEvent, GameData, GameEventKind, InputEvent, Rollback};
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
        window.set_title("Labour of Love");
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
        match event {
            winit::event::WindowEvent::CloseRequested => {
                self.state.as_mut().inspect(|value| {
                    value.game_send.send(GameEventKind::Quit).unwrap();
                });
                self.command_sender.send(Command::Quit).unwrap();
                drop(self.state.take());
                return;
            }
            winit::event::WindowEvent::Destroyed => {
                event_loop.exit();
                return;
            }
            _ => (),
        }

        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };

        match event {
            winit::event::WindowEvent::RedrawRequested => {
                // Send accumulated mouse motion events.
                if let Some(player) = &state.player {
                    if let Some(buf) = self.mouse_motion_buffer.take() {
                        state
                            .game_send
                            .send(GameEventKind::PlayerInput(
                                *player,
                                InputEvent::MouseMotion {
                                    dx: buf.0 as f32,
                                    dy: buf.1 as f32,
                                },
                            ))
                            .unwrap();
                    }
                }

                while let Ok(event) = state.client_recv.try_recv() {
                    println!("{:?}", event);
                    match event {
                        ClientUpdateEvent::NewRegion(id, data, receiver) => {
                            state.add_region(id, data, receiver);
                        }
                        ClientUpdateEvent::GameCrash(_) => todo!(),
                        ClientUpdateEvent::SetPlayer(player_key) => {
                            state.player = Some(player_key);
                        }
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
                    .send(GameEventKind::PlayerInput(
                        player,
                        InputEvent::Resized {
                            width: size.width,
                            height: size.height,
                        },
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
                    state.get_window().focus_window();
                }
                state
                    .game_send
                    .send(GameEventKind::PlayerInput(
                        player,
                        InputEvent::MouseButton {
                            button: map_mouse_button(button),
                            pressed: button_state.is_pressed(),
                        },
                    ))
                    .unwrap()
            }
            winit::event::WindowEvent::MouseWheel { delta, .. } => {
                self.last_event_was_new_events = false;
                let event = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => {
                        InputEvent::MouseWheel { x, y }
                    }
                    winit::event::MouseScrollDelta::PixelDelta(p) => InputEvent::MouseWheel {
                        x: p.x as f32 / 20.0,
                        y: p.y as f32 / 20.0,
                    },
                };
                state
                    .game_send
                    .send(GameEventKind::PlayerInput(player, event))
                    .unwrap()
            }
            winit::event::WindowEvent::KeyboardInput { event, .. } => {
                self.last_event_was_new_events = false;
                if !event.repeat {
                    if let winit::keyboard::PhysicalKey::Code(code) = event.physical_key {
                        if let Some(key) = map_key(code) {
                            state
                                .game_send
                                .send(GameEventKind::PlayerInput(
                                    player,
                                    InputEvent::Key {
                                        key,
                                        pressed: event.state.is_pressed(),
                                    },
                                ))
                                .unwrap()
                        }
                    }
                }
            }
            winit::event::WindowEvent::Focused(focused) => {
                self.last_event_was_new_events = false;
                state
                    .game_send
                    .send(GameEventKind::PlayerInput(
                        player,
                        InputEvent::Focused(focused),
                    ))
                    .unwrap();
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
        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };
        if state.player.is_none() {
            return;
        }
        match event {
            winit::event::DeviceEvent::MouseMotion { delta } => {
                self.last_event_was_new_events = false;
                if let Some(buf) = &mut self.mouse_motion_buffer {
                    buf.0 += delta.0;
                    buf.1 += delta.1;
                } else {
                    self.mouse_motion_buffer = Some(delta);
                }
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

fn map_key(code: winit::keyboard::KeyCode) -> Option<game::Key> {
    use game::Key;
    use winit::keyboard::KeyCode as K;
    Some(match code {
        K::KeyW => Key::KeyW,
        K::KeyA => Key::KeyA,
        K::KeyS => Key::KeyS,
        K::KeyD => Key::KeyD,
        K::KeyE => Key::KeyE,
        K::Space => Key::Space,
        K::ControlLeft => Key::ControlLeft,
        K::ShiftLeft => Key::ShiftLeft,
        K::Escape => Key::Escape,
        _ => return None,
    })
}

fn map_mouse_button(b: winit::event::MouseButton) -> game::MouseButton {
    use winit::event::MouseButton as M;
    match b {
        M::Left => game::MouseButton::Left,
        M::Right => game::MouseButton::Right,
        M::Middle => game::MouseButton::Middle,
        M::Back => game::MouseButton::Other(3),
        M::Forward => game::MouseButton::Other(4),
        M::Other(n) => game::MouseButton::Other(n),
    }
}
