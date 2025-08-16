//! Game client
// #![deny(missing_docs)]
use crossbeam::{
    channel::{Receiver, Sender},
    select,
};
use game::{ClientUpdateEvent, GameData, GameError, GameEventKind, Region, RegionData, World};
use log::{info, trace, warn, LevelFilter};
use parley::FontContext;
use simplelog::{FormatItem, SimpleLogger};
use std::{net::SocketAddr, time::Duration};
use winit::event_loop::{ControlFlow, EventLoop};

use crate::window::App;

mod layout;
mod mesh;
mod netcode;
mod state;
mod window;

/// Wrapper struct for coordinating networking / rollback for the game.
pub struct GameInstanceManager {
    game_event_send: Sender<GameEventKind>,
    game_event_recv: Receiver<GameEventKind>,
    client_event_send: Sender<ClientUpdateEvent>,
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
        game_event_send: Sender<GameEventKind>,
        game_event_recv: Receiver<GameEventKind>,
        client_event_send: Sender<ClientUpdateEvent>,
        server: SocketAddr,
    ) -> Self {
        Self {
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
        let tick_sender = self.game_event_send.clone();
        // Generate ticks
        std::thread::spawn(move || loop {
            // TODO: Sync ticks with server.
            tick_sender.send(GameEventKind::Tick).unwrap();
            std::thread::sleep(Duration::from_millis(30));
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

        server_game_send
            .send(game::ClientPacket::RequestRegion)
            .unwrap();

        let mut world: Option<World> = None;
        loop {
            select! {
                recv(server_recv) -> server_msg => {
                    match server_msg.unwrap() {
                        game::ServerPacket::SyncClock => trace!("Recieved syncclock packet"),
                        // TODO: buffer incoming game events until region / world is loaded, then handle all at once
                        // and enable client game events.
                        game::ServerPacket::GameEvent(game_event) => {
                            if let Some(ref mut world) = world {
                                world.reconcile_event(game_event).unwrap();
                            }
                        }
                        game::ServerPacket::Region(id, raw_game_data, last_id) => {
                            let data = RegionData::new(GameData::new(raw_game_data.clone(), Some(self.client_event_send.clone()), id), FontContext::new());
                            self.client_event_send.send(ClientUpdateEvent::NewRegion(raw_game_data)).unwrap();
                            let mut w = World::new();
                            w.load(&id, Region::new(data), last_id);
                            world = Some(w);
                            info!("Region recieved and loaded!");
                        }
                    }
                },
                recv(self.game_event_recv) -> game_event => {
                    if let Some(ref mut world) = world {
                         match game_event {
                            Ok(event) => {
                                match event {
                                    GameEventKind::Quit => return Ok(()),
                                    _ => {
                                        let event = world.handle_event(game_event.unwrap(), 0)?;
                                        server_game_send.send(game::ClientPacket::GameEvent(event)).unwrap();
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
}

/// Event sent from client to game thread.
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

fn start_game_thread() -> Sender<Command> {
    let (command_send, command_recv) = crossbeam::channel::unbounded();
    std::thread::spawn(move || loop {
        match command_recv.recv() {
            Ok(command) => match command {
                Command::ConnectToServerAndScene(sender, receiver, client_sender, server) => {
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
    return command_send;
}

// TODO: Create RenderData to encapsulate GPU state.
// TODO: Think about and improve GameData -> RenderData connection.
// TODO: Make it possible to pan camera with middle mouse button.
// TODO: Create mesh from from world representation
// TODO: Render mesh
// TODO: Profit???

// WINDOW:
// - Update world data based on recieved events.
// - Update GPU buffers based on changes to world data.
// - Accept user input and send it to game simulation.
// GAMESIM:
// - Take in user input and game ticks, resulting in executing code.
// - Executing code causes change events to world data.
// - Send world data change updates to window.
// UI code is part of game simulation, try make editor into a loadable game sim.
// So editor UI code is hardcoded in rust but as a game scene and it is also
// launched like a game scene.
const FORMAT: &'static [FormatItem] = &[FormatItem::Literal("client".as_bytes())];

fn main() {
    let config = simplelog::ConfigBuilder::new()
        .set_time_format_custom(FORMAT)
        .build();
    SimpleLogger::init(LevelFilter::Info, config).unwrap();
    let sender = start_game_thread();

    // Pass scene to window to start game scene or edior scene .
    //
    let event_loop = EventLoop::new().unwrap();

    // When the current loop iteration finishes, immediately begin a new
    // iteration regardless of whether or not new events are available to
    // process. Preferred for applications that want to render as fast as
    // possible, like games.
    event_loop.set_control_flow(ControlFlow::Poll);

    // When the current loop iteration finishes, suspend the thread until
    // another event arrives. Helps keeping CPU utilization low if nothing
    // is happening, which is preferred if the application might be idling in
    // the background.
    // event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(sender);
    event_loop.run_app(&mut app).unwrap();
}
