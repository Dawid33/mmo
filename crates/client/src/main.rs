#![allow(unused)]
//! Game client
// #![deny(missing_docs)]
use crate::window::App;
use crossbeam::{
    channel::{Receiver, RecvError, Sender},
    select,
};
use game::{na::Perspective3, ChunkCoords, ClientId, RegionId, Rollback, ServerPacket};
use game::{
    ClientPacket, ClientUpdateEvent, EntityKey, GameDataTransactionKind, GameDataUpdate, GameError,
    GameEvent, GameEventKind, PlayerKey, Region, INDUCED_LATENCY,
};
use log::{info, trace, warn, LevelFilter};
use rapier3d::math::Isometry;
use simplelog::{FormatItem, SimpleLogger};
use std::{
    collections::BTreeMap,
    error::Error,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    ops::Deref,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use winit::event_loop::{self, ControlFlow, EventLoop};

#[cfg(feature = "pyroscope")]
use pyroscope::PyroscopeAgent;
#[cfg(feature = "pyroscope")]
use pyroscope_pprofrs::{pprof_backend, PprofConfig};

mod layout;
mod netcode;
mod render_world;
mod state;
mod text;
mod window;

/// Wrapper struct for coordinating networking / rollback for the game.
pub struct GameInstanceManager {
    game_event_send: Sender<GameEventKind>,
    game_event_recv: Receiver<GameEventKind>,
    client_event_send: Sender<ClientUpdateEvent>,
    server: SocketAddr,

    server_game_send: Sender<ClientPacket>,
    server_game_recv: Receiver<ClientPacket>,
    world: Option<game::World>,
    buffer: Vec<GameEvent>,
    tick_rate: Arc<AtomicU64>,
    ready: bool,
    is_caught_up: bool,
    client_id: Option<ClientId>,
    player_chunk: Option<ChunkCoords>,
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
        let (server_game_send, server_game_recv) = crossbeam::channel::unbounded();
        Self {
            game_event_recv,
            game_event_send,
            client_event_send,
            server,
            world: None,
            buffer: Vec::new(),
            tick_rate: Arc::new(AtomicU64::new(game::TICK_RATE)),
            ready: false,
            is_caught_up: false,
            server_game_send,
            server_game_recv,
            client_id: None,
            player_chunk: None,
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
        let tick_thread_tick_rate = self.tick_rate.clone();
        // Generate ticks
        std::thread::spawn(move || loop {
            // TODO: Sync ticks with server.
            if let Err(_) = tick_sender.send(GameEventKind::Tick) {
                return;
            };
            let rate = tick_thread_tick_rate.load(Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(rate));
        });

        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let mut conn =
            netcode::ServerConnection::new(server_send, self.server_game_recv.clone(), self.server);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async { conn.connect_and_handle().await.unwrap() });
        });

        self.server_game_send
            .send(ClientPacket::RequestPlayerRegion)
            .unwrap();

        let mut now = Instant::now();
        let mut results_buffer = BTreeMap::new();
        loop {
            select! {
                // Recieve and handle server packets.
                recv(server_recv) -> server_msg => {
                    self.handle_server(server_msg);
                },
                // Recieve client game events from either the player or from
                // client-side game tick timer.
                recv(self.game_event_recv) -> game_event => {
                    // TODO: Check if client needs to request more chunks to be loaded.
                    if let Some(ref mut world) = self.world {
                         match game_event.clone() {
                            Ok(event) => {
                                match event {
                                    GameEventKind::Quit => return Ok(()),
                                    e => {
                                        if let GameEventKind::PlayerInput(_,_) = e {
                                            // don't handle player events until sim has caught up with server.
                                            if !self.ready && self.is_caught_up {
                                                continue;
                                            }
                                        }
                                        match e {
                                            GameEventKind::Tick => {
                                                world.progress_world_one_tick(&mut results_buffer);
                                            },
                                            GameEventKind::Quit | GameEventKind::PlayerInput(_, _) | GameEventKind::CreateClient(_) => {
                                                if let Some(chunk) = self.player_chunk {
                                                    let event = world.handle_region_event(game_event.unwrap(), chunk)?;
                                                    self.server_game_send.send(game::ClientPacket::GameEvent(event)).unwrap();
                                                }
                                            },
                                        }
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

    pub fn handle_server(
        &mut self,
        server_msg: Result<ServerPacket, RecvError>,
    ) -> Result<(), GameError> {
        let new_region =
            |id: RegionId, mut raw_game_data: Rollback, world: &mut Option<game::World>| {
                let (send, recv) = crossbeam::channel::unbounded();
                let mut data = Region::new(raw_game_data.clone(), Some(send), id);
                self.client_event_send
                    .send(ClientUpdateEvent::NewRegion(
                        id,
                        (*raw_game_data.data).clone(),
                        recv,
                    ))
                    .unwrap();
                if let Some(ref mut w) = world {
                    w.load(&id, data);
                } else {
                    let mut w = game::World::new();
                    w.load(&id, data);
                    *world = Some(w);
                }
                info!("Region recieved and loaded!");
            };

        match server_msg.unwrap() {
            game::ServerPacket::SyncClock(region_id, server_tick_rate, server_tick, rtt) => {
                if let Some(ref mut world) = self.world {
                    let client_tick = world.current_tick(&region_id);
                    let diff: isize = client_tick as isize - server_tick as isize;
                    // this is how far behind the server is
                    let milisecond_diff = diff * server_tick_rate as isize;
                    // we subract the rtt to get a more accurate approximation of how far behind the server is.
                    let total_mili_diff = milisecond_diff - rtt.as_millis() as isize;

                    if total_mili_diff < INDUCED_LATENCY {
                        self.ready = false;
                        self.tick_rate
                            .store((server_tick_rate as f32 * 0.9) as u64, Ordering::SeqCst);
                    } else if total_mili_diff > INDUCED_LATENCY {
                        self.ready = true;
                        self.tick_rate
                            .store((server_tick_rate as f32 * 1.1) as u64, Ordering::SeqCst);
                    } else {
                        self.ready = true;
                        self.tick_rate.store(server_tick_rate, Ordering::SeqCst);
                    }
                }
            }
            game::ServerPacket::GameEvent(game_event) => {
                if let Some(ref mut world) = self.world {
                    for event in self.buffer.drain(..) {
                        match world.reconcile_event(event) {
                            Ok(_) => (),
                            Err(e) => {
                                warn!("Failed in catch up. {:?}", e);
                                return Err(e);
                            }
                        };
                    }
                    match world.reconcile_event(game_event) {
                        Ok(_) => (),
                        Err(e) => {
                            warn!("{:?}", e);
                            return Err(e);
                        }
                    };
                    self.is_caught_up = true;
                } else {
                    self.buffer.push(game_event);
                    self.is_caught_up = false;
                }
            }
            game::ServerPacket::PlayerRegion(id, client_id) => {
                self.client_event_send
                    .send(ClientUpdateEvent::SetPlayer(client_id));

                let id = id.unwrap_or(ChunkCoords::new(0, 0, 0));
                self.client_id = Some(client_id);
                self.player_chunk = Some(id);
                self.server_game_send
                    .send(ClientPacket::RequestRegionConnection(id))
                    .unwrap();
            }
            game::ServerPacket::Region(id, mut raw_game_data) => {
                new_region(id, raw_game_data, &mut self.world);
                if let Some(player_chunk) = self.player_chunk {
                    if id == player_chunk && self.client_id.is_some() {
                        self.game_event_send
                            .send(GameEventKind::CreateClient(self.client_id.unwrap()))
                            .unwrap();
                    }
                }
            }
        }
        Ok(())
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

const FORMAT: &'static [FormatItem] = &[FormatItem::Literal("client".as_bytes())];

fn main() {
    let config = simplelog::ConfigBuilder::new()
        .set_time_format_custom(FORMAT)
        .add_filter_ignore_str("winit")
        .add_filter_ignore_str("wgpu")
        .build();
    SimpleLogger::init(LevelFilter::Info, config).unwrap();

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

    let sender = start_game_thread();
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(sender);
    event_loop.run_app(&mut app).unwrap();

    #[cfg(feature = "pyroscope")]
    if let Some(a) = agent_running {
        let agent_ready = a.stop().unwrap();
        agent_ready.shutdown();
    }
}
