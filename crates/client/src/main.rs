//! Game client
// #![deny(missing_docs)]
use bevy::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use crossbeam::channel::{Receiver, Sender};
use game::GameEventKind;
#[cfg(not(target_arch = "wasm32"))]
use game::ClientUpdateEvent;
#[cfg(not(target_arch = "wasm32"))]
use log::trace;
#[cfg(not(target_arch = "wasm32"))]
use log::warn;
#[cfg(not(target_arch = "wasm32"))]
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

#[cfg(feature = "pyroscope")]
use pyroscope::PyroscopeAgent;
#[cfg(feature = "pyroscope")]
use pyroscope_pprofrs::{pprof_backend, PprofConfig};

use client::renderer;
#[cfg(target_arch = "wasm32")]
use client::sim_driver;
#[cfg(not(target_arch = "wasm32"))]
use client::GameInstanceManager;

/// Event sent from client to game thread.
#[cfg(not(target_arch = "wasm32"))]
pub enum Command {
    /// Connect to a server, sync and start running game sim.
    ConnectToServerAndScene(
        Sender<GameEventKind>,
        Receiver<GameEventKind>,
        Sender<ClientUpdateEvent>,
        SocketAddr,
    ),
    /// Quit the game thread. Should only be send when quitting the application.
    Quit,
}

#[cfg(not(target_arch = "wasm32"))]
fn start_game_thread() -> Sender<Command> {
    let (command_send, command_recv) = crossbeam::channel::unbounded();
    std::thread::spawn(move || loop {
        match command_recv.recv() {
            Ok(command) => match command {
                Command::ConnectToServerAndScene(sender, receiver, client_sender, server) => {
                    let mut manager =
                        GameInstanceManager::new(sender, receiver, client_sender, server);
                    if let Err(e) = manager.connect_and_run() {
                        warn!("Game Crashed: {:?}", e);
                    };
                }
                Command::Quit => {
                    trace!("Game thread recieved quit command.");
                    break;
                }
            },
            Err(_e) => {
                warn!(
                    "Game thread stoped receiving command events, stopping game thread. Client probably crashed or was closed incorrectly."
                );
                break;
            }
        }
    });
    return command_send;
}

fn main() {
    // Debug builds keep per-transaction hash self-verification (the rollback
    // bar); release skips the O(state) walk — state restore is identical.
    #[cfg(not(debug_assertions))]
    game::set_hash_verification(false);

    #[cfg(feature = "pyroscope")]
    let agent_running = if let Ok(p) = std::env::var("PYROSCOPE") {
        let agent = PyroscopeAgent::builder("http://localhost:4040", "client")
            .backend(pprof_backend(PprofConfig::new().sample_rate(100)))
            .build()
            .unwrap();
        Some(agent.start().unwrap())
    } else {
        None
    };

    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();

    #[cfg(not(target_arch = "wasm32"))]
    let (command_send, game_send, client_recv) = {
        let command_send = start_game_thread();
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        command_send
            .send(Command::ConnectToServerAndScene(
                game_send.clone(),
                game_recv,
                client_send,
                SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 6466)),
            ))
            .unwrap();
        (command_send, game_send, client_recv)
    };

    #[cfg(target_arch = "wasm32")]
    let (sim, game_send, client_recv) = sim_driver::start_wasm_sim();

    // Bevy's default asset root is CARGO_MANIFEST_DIR/assets (crates/client/assets,
    // a local, gitignored scratch dir left over from earlier prototyping). The
    // workspace's actual tracked asset tree (assets/blocks, assets/shaders/...) lives
    // two levels up at the repo root, so point the file-asset source there instead.
    // On wasm, assets are fetched over HTTP relative to the served root, and
    // wasm-server-runner serves ./assets from the working directory.
    #[cfg(not(target_arch = "wasm32"))]
    let asset_path = "../../assets";
    #[cfg(target_arch = "wasm32")]
    let asset_path = "assets";

    #[allow(unused_mut)]
    let mut primary_window = bevy::window::Window {
        title: "Labour of Love".into(),
        ..Default::default()
    };
    // Track the canvas's parent (the full-viewport <body> in index.html) so the
    // render buffer resizes with the browser window instead of CSS-stretching.
    #[cfg(target_arch = "wasm32")]
    {
        primary_window.fit_canvas_to_parent = true;
    }

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(bevy::window::WindowPlugin {
                primary_window: Some(primary_window),
                ..Default::default()
            })
            .set(bevy::log::LogPlugin {
                filter: "wgpu=error,naga=warn".into(),
                ..Default::default()
            })
            .set(bevy::asset::AssetPlugin {
                file_path: asset_path.into(),
                ..Default::default()
            }),
    )
    .add_plugins(renderer::SimBridgePlugin {
        client_recv,
        game_send: game_send.clone(),
    });

    #[cfg(target_arch = "wasm32")]
    app.insert_resource(sim)
        .add_systems(Update, sim_driver::drive_sim);

    app.run();

    // Window closed: shut the sim and game threads down.
    let _ = game_send.send(GameEventKind::Quit);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = command_send.send(Command::Quit);

    #[cfg(feature = "pyroscope")]
    if let Some(a) = agent_running {
        let agent_ready = a.stop().unwrap();
        agent_ready.shutdown();
    }
}
