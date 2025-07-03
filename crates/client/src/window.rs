use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use crossbeam::channel::Sender;
use game::{GameEvent, GameEventKind, ServerPacket};
use raylib::{
    camera::Camera2D,
    color::Color,
    math::{Rectangle, Vector2},
    prelude::{KeyboardKey::*, RaylibDraw, RaylibDrawHandle, RaylibMode2D, RaylibMode2DExt},
    RaylibHandle, RaylibThread,
};

const SCREEN_WIDTH: i32 = 800;
const SCREEN_HEIGHT: i32 = 480;

pub struct ClientData {
    player: ClientPlayer,
}

pub struct ClientPlayer {
    position: Vector2,
}

impl ClientData {
    pub fn new() -> Self {
        Self {
            player: ClientPlayer {
                position: Vector2::new(0.0, 0.0),
            },
        }
    }
}

pub struct Window {
    pub rl: RaylibHandle,
    pub camera: Camera2D,
    previous: Option<ClientData>,
    player_moving: (i8, i8),
    thread: RaylibThread,
}

impl Window {
    pub fn new() -> Self {
        let (mut rl, thread) = raylib::init()
            .size(SCREEN_WIDTH, SCREEN_HEIGHT)
            .title("Game")
            .vsync()
            .build();
        rl.set_target_fps(60);

        Self {
            camera: Camera2D {
                target: Vector2::new(0.0, 0.0),
                offset: Vector2::new(
                    SCREEN_WIDTH as f32 / 2.0 - 32.0,
                    SCREEN_HEIGHT as f32 / 2.0 - 32.0,
                ),
                rotation: 0.0,
                zoom: 0.1,
            },
            rl,
            thread,
            player_moving: (0, 0),
            previous: None,
        }
    }

    pub fn run(&mut self) {
        while !self.rl.window_should_close() {
            self.handle_input();
            self.draw();
        }
    }

    fn handle_input(&mut self) {
        self.camera.zoom += self.rl.get_mouse_wheel_move() * 0.05;
        self.camera.zoom = self.camera.zoom.max(0.05).min(2.0);
    }

    fn draw(&mut self) {
        // self.camera.target.x = previous.player.position.x;
        // self.camera.target.y = previous.player.position.y;

        let mut d = self.rl.begin_drawing(&self.thread);
        d.clear_background(Color::RAYWHITE);
        d.draw_fps(0, 0);
        let mut d = d.begin_mode2D(self.camera);
        Self::draw_axis(&mut d);
    }

    #[allow(unused)]
    fn draw_axis(d: &mut RaylibMode2D<RaylibDrawHandle>) {
        const GRID_SIZE: i32 = 100;
        const TILE_SIZE: i32 = 32;
        d.draw_line_ex(
            Vector2 {
                x: (GRID_SIZE * TILE_SIZE) as f32,
                y: 0.0,
            },
            Vector2 {
                x: (-GRID_SIZE * TILE_SIZE) as f32,
                y: 0.0,
            },
            2.0,
            Color::BLACK,
        );
        d.draw_line_ex(
            Vector2 {
                x: 0.0,
                y: (GRID_SIZE * TILE_SIZE) as f32,
            },
            Vector2 {
                x: 0.0,
                y: (-GRID_SIZE * TILE_SIZE) as f32,
            },
            2.0,
            Color::BLACK,
        );
        for i in -GRID_SIZE..GRID_SIZE {
            d.draw_text(
                format!("{}", i * TILE_SIZE).as_str(),
                i * TILE_SIZE + 5,
                5,
                5,
                Color::BLACK,
            );
            d.draw_text(
                format!("{}", i * TILE_SIZE).as_str(),
                5,
                i * TILE_SIZE + 5,
                5,
                Color::BLACK,
            );
        }
    }
}
