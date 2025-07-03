//! Game client
#![deny(missing_docs)]
use crossbeam::{
    channel::{Receiver, Sender},
    select,
};
use game::{GameError, GameEvent, GameEventKind, GameSnapshotUpdate, Region};
use log::{info, trace, warn};
use quinn::rustls::{
    self,
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use simplelog::SimpleLogger;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use weldr::{parse, FileRefResolver, ResolveError, SourceMap};
use window::Window;

mod netcode;
mod window;

/// Wrapper struct for coordinating networking / rollback for the game.
pub struct GameInstanceManager {
    world: Region,
    game_event_send: Sender<GameEvent>,
    game_event_recv: Receiver<GameEvent>,
    client_event_send: Sender<ClientEvent>,
    server: SocketAddr,
}

impl GameInstanceManager {
    /// Create new GameInstanceManager
    ///
    /// - Talk to ingress to figure out which regions to load for a given player.
    /// - Start listening game events and log them.
    /// - Download game data
    /// - Re-simulate game based on logged events.
    /// - Send spawn player event and enter main loop
    pub fn new(
        game_event_send: Sender<GameEvent>,
        game_event_recv: Receiver<GameEvent>,
        client_event_send: Sender<ClientEvent>,
        server: SocketAddr,
    ) -> Self {
        Self {
            world: Region::new(),
            game_event_recv,
            game_event_send,
            client_event_send,
            server,
        }
    }

    /// Recieve and process game events from the client and network, in that order.
    ///
    /// - If received a client event:
    ///   - Simulate the event and push it on to a buffer.
    ///   - Send it to the players authoritative region.
    ///   - The client buffer should always be larger than the
    ///     network buffer by at least the number of ticks the client is ahead of
    ///     the server.
    /// - If recieved a network event:
    ///   - Check if the network and client event on the front of each buffer match.
    ///   - Keep doing this until the network buffer is empty. Reconcile the region
    ///     each time the comparison succeeds.
    ///   - Any time it doesn't succeed, rollback the client, apply the network
    ///     event and then re-apply the client events that were just rolled back.
    ///
    /// If sync clock event: send client update to window with desired input delay
    /// and update tick time.
    pub fn connect_and_run(&mut self) -> Result<(), GameError> {
        let mut tick = 0;
        let tick_sender = self.game_event_send.clone();
        // Generate ticks
        std::thread::spawn(move || loop {
            // TODO: Sync ticks with server.
            tick_sender
                .send(GameEvent::new(GameEventKind::Tick(tick)))
                .unwrap();
            tick += 1;
            std::thread::sleep(Duration::from_millis(1000));
        });

        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let (server_game_send, server_game_recv) = crossbeam::channel::unbounded();
        let mut conn = netcode::ServerConnection::new(server_send, server_game_recv, self.server);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async { conn.connect_and_handle().await.unwrap() });
        });

        loop {
            select! {
                recv(server_recv) -> server_msg => {
                    // info!("Recieved server packet: {:?}", server_msg.unwrap());
                },
                recv(self.game_event_recv) -> game_event => {
                    match game_event {
                        Ok(game_event) => {
                            match game_event.kind {
                                GameEventKind::Quit => return Ok(()),
                                _ => {
                                    self.world.handle_event(game_event)?;
                                    server_game_send.send(game_event).unwrap();
                                }
                            }
                        },
                        Err(e) => panic!("{}", e),
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
enum Origin {
    Client,
    Server,
}

/// Event sent from game to client
#[derive(Debug)]
pub enum ClientEvent {
    /// Update the game client with new sim info.
    Update(GameSnapshotUpdate),
    /// Notify client of game crashing.
    GameCrash(Origin, GameError),
}

/// Event sent from client to game thread.
pub enum Command {
    /// Connect to a server, sync and start running game sim.
    LaunchGame(
        Sender<GameEvent>,
        Receiver<GameEvent>,
        Sender<ClientEvent>,
        SocketAddr,
    ),
    /// Quit the game thread. Should only be send when quitting the application.
    Quit,
}

fn main() {
    let config = simplelog::ConfigBuilder::new().build();
    SimpleLogger::init(log::LevelFilter::Info, config).unwrap();

    let (command_send, command_recv) = crossbeam::channel::unbounded();
    std::thread::spawn(move || loop {
        match command_recv.recv() {
            Ok(command) => match command {
                Command::LaunchGame(sender, receiver, client_sender, server) => {
                    let mut manager =
                        GameInstanceManager::new(sender, receiver, client_sender, server);
                    loop {
                        if let Err(e) = manager.connect_and_run() {
                            warn!("Game Crashed: {:?}", e);
                        };
                    }
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

    let mut window = Window::new();
    window.run();
}

struct MyCustomResolver {}

fn try_get(filename: &PathBuf) -> Option<Vec<u8>> {
    match std::fs::read_to_string(filename) {
        Ok(data) => Some(data.as_bytes().to_vec()),
        Err(_) => None,
    }
}

impl FileRefResolver for MyCustomResolver {
    fn resolve<P: AsRef<Path>>(&self, filename: P) -> Result<Vec<u8>, ResolveError> {
        let filename = filename.as_ref().to_string_lossy().to_string();
        let filename = filename.replace("\\", "/");
        let dir = std::env::current_dir().unwrap().to_path_buf().join("ldraw");
        let data = if let Some(data) = try_get(&dir.join("models").join(&filename)) {
            data
        } else if let Some(data) = try_get(&dir.join("p").join(&filename)) {
            data
        } else if let Some(data) = try_get(&dir.join("p/48").join(&filename)) {
            data
        } else if let Some(data) = try_get(&dir.join("parts").join(&filename)) {
            data
        } else if let Some(data) = try_get(&dir.join("parts/s").join(&filename)) {
            data
        } else {
            panic!("Could not find file {} in ldraw.", filename);
        };
        Ok(data)
    }
}
