//! Game client
// #![deny(missing_docs)]
use bevy::prelude::*;
use crossbeam::channel::{Receiver, RecvError, Sender};
#[cfg(not(target_arch = "wasm32"))]
use crossbeam::select;
use game::{ChunkCoords, ClientId, RegionId, Rollback, ServerPacket};
use game::{
    ClientPacket, ClientUpdateEvent, GameError, GameEvent, GameEventKind, Region, INDUCED_LATENCY,
};
#[cfg(not(target_arch = "wasm32"))]
use log::trace;
use log::{info, warn};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
#[cfg(not(target_arch = "wasm32"))]
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

#[cfg(feature = "pyroscope")]
use pyroscope::PyroscopeAgent;
#[cfg(feature = "pyroscope")]
use pyroscope_pprofrs::{pprof_backend, PprofConfig};

#[cfg(not(target_arch = "wasm32"))]
mod netcode;
#[cfg(any(target_arch = "wasm32", test))]
mod local_server;
#[cfg(target_arch = "wasm32")]
mod sim_driver;
mod renderer;

/// Wrapper struct for coordinating networking / rollback for the game.
pub struct GameInstanceManager {
    game_event_send: Sender<GameEventKind>,
    game_event_recv: Receiver<GameEventKind>,
    client_event_send: Sender<ClientUpdateEvent>,
    /// Only read by the native netcode path (`connect_and_run`); kept on wasm
    /// so `new()` has one signature on both targets.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
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
    results_buffer: BTreeMap<RegionId, Result<GameEvent, GameError>>,
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
            results_buffer: BTreeMap::new(),
        }
    }

    /// Kick off the connection handshake: ask the server which region the
    /// player belongs to. Both the native thread loop and the wasm frame
    /// driver call this exactly once before their first pump/select.
    pub fn start(&mut self) {
        self.server_game_send
            .send(ClientPacket::RequestPlayerRegion)
            .unwrap();
    }

    /// Inject a client-side tick (wasm frame driver replaces the native
    /// tick-generator thread with this).
    pub fn send_tick(&self) {
        let _ = self.game_event_send.send(GameEventKind::Tick);
    }

    /// Current adaptive tick interval in milliseconds.
    pub fn tick_rate_ms(&self) -> u64 {
        self.tick_rate.load(Ordering::SeqCst)
    }

    /// The channel the transport (quinn on native, LocalServer on wasm)
    /// reads outgoing ClientPackets from.
    pub fn client_packet_recv(&self) -> Receiver<ClientPacket> {
        self.server_game_recv.clone()
    }

    /// Handle one client-side game event. Returns Ok(false) if the game
    /// should quit. Events arriving before the first region loads are
    /// dropped, matching the pre-refactor select! loop.
    fn handle_game_event(&mut self, event: GameEventKind) -> Result<bool, GameError> {
        if self.world.is_none() {
            return Ok(true);
        }
        match event {
            GameEventKind::Quit => return Ok(false),
            GameEventKind::Tick => {
                self.world
                    .as_mut()
                    .unwrap()
                    .progress_world_one_tick(&mut self.results_buffer);
            }
            GameEventKind::PlayerInput(_, _) if !self.ready && self.is_caught_up => {
                // don't handle player events until sim has caught up with server.
            }
            e @ GameEventKind::PlayerInput(_, _) => {
                if let Some(chunk) = self.player_chunk {
                    let event = self.world.as_mut().unwrap().handle_region_event(e, chunk)?;
                    self.server_game_send
                        .send(game::ClientPacket::GameEvent(event))
                        .unwrap();
                }
            }
            GameEventKind::CreateClient(_) => {
                // Players are created by the server on connection.
                warn!("ignoring locally-originated CreateClient");
            }
        }
        Ok(true)
    }

    /// Drain all pending server packets, then all pending game events,
    /// without blocking. Returns Ok(false) once Quit has been consumed.
    pub fn pump(&mut self, server_recv: &Receiver<ServerPacket>) -> Result<bool, GameError> {
        while let Ok(msg) = server_recv.try_recv() {
            self.handle_server(Ok(msg))?;
        }
        while let Ok(event) = self.game_event_recv.try_recv() {
            if !self.handle_game_event(event)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn handle_server(
        &mut self,
        server_msg: Result<ServerPacket, RecvError>,
    ) -> Result<(), GameError> {
        let new_region =
            |id: RegionId, raw_game_data: Rollback, world: &mut Option<game::World>| {
                let (send, recv) = crossbeam::channel::unbounded();
                let data = Region::new(raw_game_data.clone(), Some(send), id, self.client_id);
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
                if let Err(e) = self.client_event_send.send(ClientUpdateEvent::SetPlayer(client_id)) {
                    // Receiver gone means the render/client-bridge thread has exited;
                    // nothing left to notify, so just log and keep going.
                    warn!("client_event_send closed while sending SetPlayer: {:?}", e);
                }

                let id = id.unwrap_or(ChunkCoords::new(0, 0, 0));
                self.client_id = Some(client_id);
                self.player_chunk = Some(id);
                self.server_game_send
                    .send(ClientPacket::RequestRegionConnection(id))
                    .unwrap();
            }
            game::ServerPacket::Region(id, raw_game_data) => {
                new_region(id, raw_game_data, &mut self.world);
            }
        }
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl GameInstanceManager {
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

        self.start();

        loop {
            select! {
                // Recieve and handle server packets.
                recv(server_recv) -> server_msg => {
                    self.handle_server(server_msg)?;
                },
                // Recieve client game events from either the player or from
                // client-side game tick timer.
                recv(self.game_event_recv) -> game_event => {
                    match game_event {
                        Ok(event) => {
                            if !self.handle_game_event(event)? {
                                return Ok(());
                            }
                        }
                        Err(e) => panic!("{}", e),
                    }
                }
            }
        }
    }
}

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

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(bevy::window::WindowPlugin {
                primary_window: Some(bevy::window::Window {
                    title: "Labour of Love".into(),
                    ..Default::default()
                }),
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

#[cfg(test)]
mod manager_tests {
    use super::*;
    use game::ClientUpdateEvent;

    /// pump() must: load a region from a ServerPacket, then advance the sim
    /// on a Tick game event — all without blocking.
    #[test]
    fn pump_loads_region_and_ticks() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager =
            GameInstanceManager::new(game_send.clone(), game_recv, client_send, dummy_addr);

        // Fake the server side using the same code the real server uses.
        let world = game::World::basic();
        let region_id = ChunkCoords::new(0, 0, 0);
        server_send
            .send(ServerPacket::PlayerRegion(Some(region_id), 0))
            .unwrap();
        server_send
            .send(world.build_region_server_packet(&region_id))
            .unwrap();

        assert!(manager.pump(&server_recv).unwrap());
        let tick_before = manager.world.as_ref().unwrap().current_tick(&region_id);

        manager.send_tick();
        assert!(manager.pump(&server_recv).unwrap());
        let tick_after = manager.world.as_ref().unwrap().current_tick(&region_id);
        assert_eq!(tick_after, tick_before + 1);

        // The render bridge must have been told about the new region + player.
        let mut saw_region = false;
        let mut saw_player = false;
        while let Ok(ev) = client_recv.try_recv() {
            match ev {
                ClientUpdateEvent::NewRegion(..) => saw_region = true,
                ClientUpdateEvent::SetPlayer(..) => saw_player = true,
                _ => {}
            }
        }
        assert!(saw_region && saw_player);

        // Quit terminates the pump.
        game_send.send(GameEventKind::Quit).unwrap();
        assert!(!manager.pump(&server_recv).unwrap());
    }
}
