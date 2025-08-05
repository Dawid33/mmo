use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{Arc, Mutex, MutexGuard},
};

use crossbeam::channel::Sender;
use game::{GameData, GameEventKind};
use raylib::{
    camera::Camera3D,
    ffi::{CameraMode, MouseButton},
    math::{Vector2, Vector3},
    prelude::{
        RaylibDraw, RaylibDraw3D, RaylibDrawHandle, RaylibMode2D, RaylibMode2DExt, RaylibMode3DExt,
    },
    RaylibHandle, RaylibThread,
};

use crate::Command;
use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache};

const SCREEN_WIDTH: i32 = 800;
const SCREEN_HEIGHT: i32 = 480;

pub struct Window {
    // pub rl: RaylibHandle,
    // pub camera: raylib::camera::Camera3D,
    // last_key_pressed: Option<u32>,
    // thread: RaylibThread,
    // command_send: Sender<Command>,
    // font_system: FontSystem,
    // swash_cache: SwashCache,
}

impl Window {
    pub fn new(command_send: Sender<Command>) -> Self {
        // let (mut rl, thread) = raylib::init()
        //     .size(SCREEN_WIDTH, SCREEN_HEIGHT)
        //     .title("Brick Racer")
        //     .vsync()
        //     .build();
        // rl.set_target_fps(60);
        // let camera = Camera3D::perspective(
        //     Vector3::new(10.0, 10.0, 10.0),
        //     Vector3::new(0.0, 0.0, 0.0),
        //     Vector3::new(0.0, 1.0, 0.0),
        //     45.0,
        // );

        Self {
            // camera,
            // rl,
            // thread,
            // command_send,
            // last_key_pressed: None,
            // font_system: FontSystem::new(),
            // swash_cache: SwashCache::new(),
        }
    }

    pub fn run(&mut self) {}
}
//     pub fn run(&mut self) {
//         let (mut game_send, game_recv) = crossbeam::channel::unbounded();
//         let (client_send, client_recv) = crossbeam::channel::unbounded();
//         self.command_send
//             .send(Command::ConnectToServerAndScene(
//                 game_send.clone(),
//                 game_recv,
//                 client_send,
//                 SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 6466)),
//             ))
//             .unwrap();

//         let mut data: Option<Arc<Mutex<GameData>>> = None;
//         self.rl.disable_cursor();

//         while !self.rl.window_should_close() {
//             self.handle_input(&mut game_send);
//             while let Ok(event) = client_recv.try_recv() {
//                 // Update game world based on client update events.
//                 match event {
//                     game::ClientUpdateEvent::GameCrash(_) => todo!(),
//                     game::ClientUpdateEvent::Region(mutex) => {
//                         data = Some(mutex);
//                     }
//                 }
//             }

//             // Draw to the canvas
//             if let Some(ref data) = data {
//                 let data = data.lock().unwrap();
//                 self.draw_region(data);
//             }
//         }
//     }

//     fn handle_input(&mut self, game_send: &mut Sender<GameEventKind>) {
//         // self.camera += self.rl.get_mouse_wheel_move() * 0.05;
//         // self.camera= self.camera.zoom.max(0.05).min(2.0);

//         let buttons = &[
//             (MouseButton::MOUSE_BUTTON_LEFT, game::MouseButton::Left),
//             (MouseButton::MOUSE_BUTTON_RIGHT, game::MouseButton::Right),
//             (MouseButton::MOUSE_BUTTON_MIDDLE, game::MouseButton::Middle),
//         ];
//         for (rl_button, my_button) in buttons {
//             if self.rl.is_mouse_button_pressed(*rl_button) {
//                 let pos = self.rl.get_mouse_position();
//                 let ray = self.rl.get_screen_to_world_ray(pos, self.camera);
//                 game_send
//                     .send(GameEventKind::MouseEvent(
//                         *my_button,
//                         game::MouseButtonAction::Pressed,
//                         (pos.x, pos.y),
//                         (
//                             (ray.position.x, ray.position.y, ray.position.z),
//                             (ray.direction.x, ray.direction.y, ray.direction.z),
//                         ),
//                     ))
//                     .unwrap();
//             }
//         }

//         for (rl_button, my_button) in buttons {
//             if self.rl.is_mouse_button_released(*rl_button) {
//                 let pos = self.rl.get_mouse_position();
//                 let ray = self.rl.get_screen_to_world_ray(pos, self.camera);
//                 game_send
//                     .send(GameEventKind::MouseEvent(
//                         *my_button,
//                         game::MouseButtonAction::Released,
//                         (pos.x, pos.y),
//                         (
//                             (ray.position.x, ray.position.y, ray.position.z),
//                             (ray.direction.x, ray.direction.y, ray.direction.z),
//                         ),
//                     ))
//                     .unwrap();
//             }
//         }

//         if self.rl.is_key_pressed(raylib::ffi::KeyboardKey::KEY_TAB) {
//             self.rl.enable_cursor();
//         }

//         if let Some(key) = self.rl.get_key_pressed_number() {
//             self.rl.set_window_focused();
//             if !self.last_key_pressed.is_some_and(|last| key == last) {
//                 self.last_key_pressed = Some(key);
//                 game_send.send(GameEventKind::KeyboardEvent(key)).unwrap();
//             }
//         }
//     }

//     fn draw_region(&mut self, data: MutexGuard<GameData>) {
//         self.rl
//             .update_camera(&mut self.camera, CameraMode::CAMERA_CUSTOM);
//         let mut d = self.rl.begin_drawing(&self.thread);
//         d.clear_background(raylib::color::Color::RAYWHITE);
//         d.draw_mode3D(self.camera, |mut d, _| {
//             d.draw_cube(
//                 Vector3::new(0.0, 0.0, 0.0),
//                 1.0,
//                 1.0,
//                 1.0,
//                 raylib::color::Color::RED,
//             );
//         });

//         d.draw_fps(0, 0);
//         for e in &data.entities {
//             match &e.kind {
//                 game::EntityType::Text(text) => {
//                     // d.draw_text_codepoints(
//                     //     font,
//                     //     &text,
//                     //     Vector2::new(e.position.x, e.position.y),
//                     //     14.0,
//                     //     20.0raylib re,
//                     //     raylib::color::Color::BLACK,
//                     // );
//                 }
//                 game::EntityType::TaffyTree => (),
//                 game::EntityType::Default => (),
//             }
//         }
//         drop(data)
//     }

//     #[allow(unused)]
//     fn draw_axis(d: &mut RaylibMode2D<RaylibDrawHandle>) {
//         const GRID_SIZE: i32 = 100;
//         const TILE_SIZE: i32 = 32;
//         d.draw_line_ex(
//             Vector2 {
//                 x: (GRID_SIZE * TILE_SIZE) as f32,
//                 y: 0.0,
//             },
//             Vector2 {
//                 x: (-GRID_SIZE * TILE_SIZE) as f32,
//                 y: 0.0,
//             },
//             2.0,
//             raylib::color::Color::BLACK,
//         );
//         d.draw_line_ex(
//             Vector2 {
//                 x: 0.0,
//                 y: (GRID_SIZE * TILE_SIZE) as f32,
//             },
//             Vector2 {
//                 x: 0.0,
//                 y: (-GRID_SIZE * TILE_SIZE) as f32,
//             },
//             2.0,
//             raylib::color::Color::BLACK,
//         );
//         for i in -GRID_SIZE..GRID_SIZE {
//             d.draw_text(
//                 format!("{}", i * TILE_SIZE).as_str(),
//                 i * TILE_SIZE + 5,
//                 5,
//                 5,
//                 raylib::color::Color::BLACK,
//             );
//             d.draw_text(
//                 format!("{}", i * TILE_SIZE).as_str(),
//                 5,
//                 i * TILE_SIZE + 5,
//                 5,
//                 raylib::color::Color::BLACK,
//             );
//         }
//     }
// }
