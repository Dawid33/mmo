//! Game client
// #![deny(missing_docs)]
use bevy::prelude::*;
use crossbeam::channel::{Receiver, RecvError, Sender};
#[cfg(not(target_arch = "wasm32"))]
use crossbeam::select;
use game::{ClientId, RegionCoords, RegionId, Rollback, ServerPacket};
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
mod netcode_web;
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
    home_region: Option<RegionCoords>,
    /// Regions we have requested (and not released) — the desired window.
    subscribed: std::collections::BTreeSet<RegionCoords>,
    /// Events for subscribed regions whose snapshot hasn't arrived yet.
    pending_events: BTreeMap<RegionCoords, Vec<GameEvent>>,
    results_buffer: BTreeMap<RegionId, Result<GameEvent, GameError>>,
}

/// The client's desired subscription set: the 3x3 window around the viewer,
/// plus the home region. Home must NEVER be released while the free-cam
/// roams — PlayerInput routes to it (World::handle_region_event unwraps on a
/// missing region) and it anchors viewer_region's pose read; this also
/// mirrors the server's keep-alive rule, which pins a client's home region
/// regardless of its window.
fn desired_window(
    center: RegionCoords,
    home: Option<RegionCoords>,
) -> std::collections::BTreeSet<RegionCoords> {
    let mut desired: std::collections::BTreeSet<RegionCoords> =
        center.window_3x3().into_iter().collect();
    if let Some(home) = home {
        desired.insert(home);
    }
    desired
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
            home_region: None,
            subscribed: std::collections::BTreeSet::new(),
            pending_events: BTreeMap::new(),
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
                self.update_window();
            }
            GameEventKind::PlayerInput(_, _) if !self.is_caught_up => {
                // Don't handle player events until the sim has caught up with
                // the server (join/replay window). `ready` deliberately does
                // NOT gate input: it flaps with SyncClock sampling jitter
                // (diff of ±1 tick at rtt≈0) and its job is tuning the
                // adaptive tick rate, not input admission — gating on it ate
                // a 500ms window of keypresses per flap (261:1 measured in
                // the browser).
            }
            e @ GameEventKind::PlayerInput(_, _) => {
                if let Some(home) = self.home_region {
                    // Belt-and-braces: desired_window pins home in the
                    // subscription set, so it should always be loaded — but
                    // handle_region_event unwraps on a missing region, so
                    // never route input into a hole.
                    if !self.world.as_ref().unwrap().region_exists(&home) {
                        warn!("dropping PlayerInput: home region {:?} not loaded", home);
                        return Ok(true);
                    }
                    let event = self.world.as_mut().unwrap().handle_region_event(e, home)?;
                    self.server_game_send
                        .send(game::ClientPacket::GameEvent(event))
                        .unwrap();
                }
            }
            GameEventKind::CreateClient(_) => {
                // Players are created by the server on connection.
                warn!("ignoring locally-originated CreateClient");
            }
            // Transfer events are relayed into regions directly (server) or
            // client-predicted (later task); never locally-originated here.
            GameEventKind::EntityArrived(_) | GameEventKind::GhostUpdate(_) => {}
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

    /// Build render-bridge + local-world state for a freshly received region
    /// snapshot. Hoisted out of a closure (was `new_region` inline) so
    /// `handle_server` can call `drop_region` first without fighting the
    /// borrow checker over `self`.
    fn new_region(&mut self, id: RegionId, raw_game_data: Rollback) {
        let (send, recv) = crossbeam::channel::unbounded();
        let data = Region::new(raw_game_data.clone(), Some(send), id, self.client_id);
        self.client_event_send
            .send(ClientUpdateEvent::NewRegion(
                id,
                (*raw_game_data.data).clone(),
                recv,
            ))
            .unwrap();
        if let Some(ref mut w) = self.world {
            w.load(&id, data);
        } else {
            let mut w = game::World::new();
            w.load(&id, data);
            self.world = Some(w);
        }
        info!("Region recieved and loaded!");
    }

    /// The viewer's current region, from the local player's sim body in the
    /// home region (region-local pose + home offset). Free-cam local coords
    /// run past the region bounds on purpose; from_world floor-divides them
    /// into the right neighbour. Falls back to home when the player entity
    /// isn't readable yet.
    fn viewer_region(&self) -> Option<RegionCoords> {
        let home = self.home_region?;
        let world = self.world.as_ref()?;
        if !world.region_exists(&home) {
            return Some(home);
        }
        let data = world.data(&home);
        let Some(client_id) = self.client_id else { return Some(home) };
        let Some(key) = data.player_entites.get(&client_id).copied() else {
            return Some(home);
        };
        let Some(handle) = *data.ecs.rigidbody.try_get(key) else {
            return Some(home);
        };
        let Some(body) = data.physics.bodies.get(handle) else {
            return Some(home);
        };
        let t = body.translation();
        let off = home.world_offset();
        // Real = OrderedFloat<f32>; .0 unwraps to f32 (same as convert.rs).
        Some(RegionCoords::from_world(t.x.0 + off[0], t.z.0 + off[2]))
    }

    /// Diff the desired window against current subscriptions; request
    /// the new, release the stale, and tear stale regions out of the local
    /// world + render bridge. Cheap when nothing changed (set compare).
    fn update_window(&mut self) {
        let Some(center) = self.viewer_region() else { return };
        let desired = desired_window(center, self.home_region);
        if desired == self.subscribed {
            return;
        }
        for rc in desired.difference(&self.subscribed) {
            self.server_game_send
                .send(ClientPacket::RequestRegionConnection(*rc))
                .unwrap();
        }
        let stale: Vec<RegionCoords> = self.subscribed.difference(&desired).copied().collect();
        for rc in stale {
            self.server_game_send
                .send(ClientPacket::ReleaseRegionConnection(rc))
                .unwrap();
            self.drop_region(rc);
        }
        self.subscribed = desired;
    }

    /// Remove a region from the local world and the render bridge.
    fn drop_region(&mut self, rc: RegionCoords) {
        self.pending_events.remove(&rc);
        if let Some(ref mut world) = self.world {
            if world.remove_region(&rc).is_some() {
                let _ = self.client_event_send.send(ClientUpdateEvent::RemoveRegion(rc));
            }
        }
    }

    /// Route one server-authoritative game event to the region it targets:
    /// reconcile if loaded, buffer if the snapshot is still in flight,
    /// drop if the region was released/never wanted.
    fn route_server_game_event(&mut self, event: GameEvent) -> Result<(), GameError> {
        let world = self.world.as_mut().expect("routed only when world exists");
        if world.region_exists(&event.region_id) {
            match world.reconcile_event(event) {
                Ok(()) => Ok(()),
                Err(e) => {
                    warn!("reconcile failed: {:?}", e);
                    Err(e)
                }
            }
        } else if self.subscribed.contains(&event.region_id) {
            // Snapshot still in flight: hold the event for replay.
            self.pending_events.entry(event.region_id).or_default().push(event);
            Ok(())
        } else {
            // Released/never-wanted region: steady-state noise.
            log::debug!("dropping event for region {:?}", event.region_id);
            Ok(())
        }
    }

    pub fn handle_server(
        &mut self,
        server_msg: Result<ServerPacket, RecvError>,
    ) -> Result<(), GameError> {
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
                if self.world.is_none() {
                    self.buffer.push(game_event);
                    self.is_caught_up = false;
                    return Ok(());
                }
                for event in self.buffer.drain(..).collect::<Vec<_>>() {
                    self.route_server_game_event(event)?;
                }
                self.route_server_game_event(game_event)?;
                self.is_caught_up = true;
            }
            game::ServerPacket::PlayerRegion(id, client_id) => {
                if let Err(e) = self.client_event_send.send(ClientUpdateEvent::SetPlayer(client_id)) {
                    // Receiver gone means the render/client-bridge thread has exited;
                    // nothing left to notify, so just log and keep going.
                    warn!("client_event_send closed while sending SetPlayer: {:?}", e);
                }
                self.client_id = Some(client_id);

                let home = id.unwrap_or(RegionCoords::new(0, 0));
                self.home_region = Some(home);
                // Ask for the whole 3x3 window up front; update_window keeps
                // it in sync from then on.
                for rc in home.window_3x3() {
                    self.server_game_send
                        .send(ClientPacket::RequestRegionConnection(rc))
                        .unwrap();
                    self.subscribed.insert(rc);
                }
            }
            game::ServerPacket::Region(id, raw_game_data) => {
                if !self.subscribed.contains(&id) {
                    // Window moved on while the snapshot was in flight.
                    self.server_game_send
                        .send(ClientPacket::ReleaseRegionConnection(id))
                        .unwrap();
                    return Ok(());
                }
                // Capture events that raced ahead of the snapshot BEFORE
                // drop_region — its first act is to clear this region's
                // pending buffer, which would silently discard them.
                let pending = self.pending_events.remove(&id);
                // Replace-on-re-receipt: crash-respawn/resubscribe resyncs.
                self.drop_region(id);
                self.new_region(id, raw_game_data);
                // Replay events that raced ahead of the snapshot; reconcile
                // skips anything already baked in (base_event_id).
                if let Some(pending) = pending {
                    if let Some(ref mut world) = self.world {
                        for ev in pending {
                            let _ = world.reconcile_event(ev);
                        }
                    }
                }
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
        let region_id = RegionCoords::new(0, 0);
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

    /// `ready` only tunes the adaptive tick rate; it must NOT gate input.
    /// Regression test for the browser input-drop: SyncClock sampling flaps
    /// `ready` to false whenever the client samples one tick behind (rtt=0),
    /// and the old `!ready && is_caught_up` gate then ate every keypress for
    /// the following 500ms window — measured live at 261 dropped : 1 sent.
    /// Only a sim that has not yet caught up with the server may drop input.
    #[test]
    fn player_input_flows_while_not_ready_once_caught_up() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager =
            GameInstanceManager::new(game_send.clone(), game_recv, client_send, dummy_addr);
        let outgoing = manager.client_packet_recv();

        let world = game::World::basic();
        let region_id = RegionCoords::new(0, 0);
        server_send
            .send(ServerPacket::PlayerRegion(Some(region_id), 0))
            .unwrap();
        server_send
            .send(world.build_region_server_packet(&region_id))
            .unwrap();
        assert!(manager.pump(&server_recv).unwrap());
        // Keep the render-bridge receiver alive so region emits don't hit a
        // closed channel (see offline_handshake_loads_region).
        let mut bridge_recv = None;
        while let Ok(ev) = client_recv.try_recv() {
            if let ClientUpdateEvent::NewRegion(_, _, recv) = ev {
                bridge_recv = Some(recv);
            }
        }
        // Drain the handshake packets the manager itself sent.
        while outgoing.try_recv().is_ok() {}

        // The flapping state observed live: caught up, but sampled behind.
        manager.is_caught_up = true;
        manager.ready = false;

        game_send
            .send(GameEventKind::PlayerInput(
                0,
                game::InputEvent::Key { key: game::Key::KeyE, pressed: true },
            ))
            .unwrap();
        assert!(manager.pump(&server_recv).unwrap());
        assert!(
            matches!(outgoing.try_recv(), Ok(ClientPacket::GameEvent(_))),
            "PlayerInput must be predicted + sent while ready=false once caught up"
        );

        // The join/replay window is still protected: before catch-up, drop.
        manager.is_caught_up = false;
        game_send
            .send(GameEventKind::PlayerInput(
                0,
                game::InputEvent::Key { key: game::Key::KeyE, pressed: false },
            ))
            .unwrap();
        assert!(manager.pump(&server_recv).unwrap());
        assert!(
            outgoing.try_recv().is_err(),
            "PlayerInput must be dropped until the sim has caught up"
        );
        drop(bridge_recv);
    }

    fn manager_with_player() -> (
        GameInstanceManager,
        crossbeam::channel::Sender<ServerPacket>,
        Receiver<ServerPacket>,
        Receiver<ClientUpdateEvent>,
        crossbeam::channel::Sender<GameEventKind>,
    ) {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let manager =
            GameInstanceManager::new(game_send.clone(), game_recv, client_send, dummy_addr);
        (manager, server_send, server_recv, client_recv, game_send)
    }

    /// The desired window must keep the home region subscribed even when
    /// the free-cam viewer roams outside home's 3x3 neighbourhood: input
    /// routing and viewer-pose reads both anchor on home.
    #[test]
    fn desired_window_pins_home_outside_3x3() {
        let home = RegionCoords::new(0, 0);
        let far = RegionCoords::new(5, -3);
        let desired = desired_window(far, Some(home));
        assert!(desired.contains(&home), "home must never leave the window");
        assert_eq!(desired.len(), 10, "3x3 around viewer plus distant home");
        for rc in far.window_3x3() {
            assert!(desired.contains(&rc));
        }
    }

    /// Home inside (or at the centre of) the 3x3 must not duplicate, and no
    /// home yields the plain 3x3.
    #[test]
    fn desired_window_home_inside_does_not_duplicate() {
        let home = RegionCoords::new(0, 0);
        // Home adjacent to centre: inside the 3x3.
        let desired = desired_window(RegionCoords::new(1, 0), Some(home));
        assert_eq!(desired.len(), 9);
        assert!(desired.contains(&home));
        // Home at centre.
        let desired = desired_window(home, Some(home));
        assert_eq!(desired.len(), 9);
        // No home yet: plain 3x3.
        assert_eq!(desired_window(RegionCoords::new(2, 2), None).len(), 9);
    }

    /// PlayerRegion must trigger subscription requests for the full 3x3
    /// window around home, not just home.
    #[test]
    fn player_region_requests_full_window() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        assert!(manager.pump(&server_recv).unwrap());

        let mut requested = std::collections::BTreeSet::new();
        while let Ok(p) = manager.client_packet_recv().try_recv() {
            if let ClientPacket::RequestRegionConnection(rc) = p {
                requested.insert(rc);
            }
        }
        let expected: std::collections::BTreeSet<_> = home.window_3x3().into_iter().collect();
        assert_eq!(requested, expected);
    }

    /// Events for a subscribed-but-not-yet-loaded region are buffered and
    /// applied when the snapshot arrives, not dropped.
    #[test]
    fn events_for_pending_region_are_buffered_until_snapshot() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        manager.pump(&server_recv).unwrap();

        // Load home so `world` exists.
        let world = game::World::basic();
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        manager.pump(&server_recv).unwrap();

        // A neighbour's tick arrives before its snapshot.
        let neighbour = RegionCoords::new(1, 0);
        let tick_from_neighbour =
            GameEvent::new(GameEventKind::Tick, 0, neighbour);
        server_send.send(ServerPacket::GameEvent(tick_from_neighbour)).unwrap();
        manager.pump(&server_recv).unwrap();
        assert!(!manager.world.as_ref().unwrap().region_exists(&neighbour));

        // Snapshot arrives: region loads, then the buffered event must be
        // replayed through reconcile — Tick id 0 lands in the region's
        // server-input buffer, waiting for the matching local prediction.
        let mut neighbour_world = game::World::new();
        neighbour_world.load(&neighbour, Region::from_chunks(neighbour, Vec::new()));
        server_send.send(neighbour_world.build_region_server_packet(&neighbour)).unwrap();
        manager.pump(&server_recv).unwrap();
        assert!(manager.world.as_ref().unwrap().region_exists(&neighbour));

        // Prove the replay actually happened (not just that the region
        // loaded): tick locally once — the neighbour predicts Tick id 0 —
        // then deliver server Tick id 1. Reconcile pops the *replayed* id 0
        // from the input buffer, matches it against the prediction, and
        // forgets it. If the replay was dropped, id 1 mismatches prediction
        // id 0 and the prediction stays stuck in the event log.
        manager.send_tick();
        manager.pump(&server_recv).unwrap();
        server_send
            .send(ServerPacket::GameEvent(GameEvent::new(
                GameEventKind::Tick,
                1,
                neighbour,
            )))
            .unwrap();
        manager.pump(&server_recv).unwrap();
        let unconfirmed = manager
            .world
            .as_ref()
            .unwrap()
            .regions
            .get(&neighbour)
            .unwrap()
            .pending_event_ids();
        assert!(
            unconfirmed.is_empty(),
            "buffered event was not replayed: prediction(s) {:?} never confirmed",
            unconfirmed
        );
    }

    /// A snapshot for an already-loaded region replaces it (crash-respawn /
    /// resubscribe path), emitting RemoveRegion then NewRegion to the bridge.
    #[test]
    fn region_re_receipt_replaces_and_signals_bridge() {
        let (mut manager, server_send, server_recv, client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        let world = game::World::basic();
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        manager.pump(&server_recv).unwrap();
        while client_recv.try_recv().is_ok() {} // clear initial events

        server_send.send(world.build_region_server_packet(&home)).unwrap();
        manager.pump(&server_recv).unwrap();

        let events: Vec<ClientUpdateEvent> = client_recv.try_iter().collect();
        assert!(
            matches!(events[0], ClientUpdateEvent::RemoveRegion(rc) if rc == home),
            "teardown precedes rebuild, got {:?}", events
        );
        assert!(matches!(events[1], ClientUpdateEvent::NewRegion(rc, _, _) if rc == home));
    }

    /// A snapshot for a region outside the desired window is released
    /// immediately, never loaded (window moved on before it arrived).
    #[test]
    fn stale_snapshot_is_released_not_loaded() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        let world = game::World::basic();
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        manager.pump(&server_recv).unwrap();
        while manager.client_packet_recv().try_recv().is_ok() {}

        let far = RegionCoords::new(50, 50);
        let mut far_world = game::World::new();
        far_world.load(&far, Region::from_chunks(far, Vec::new()));
        server_send.send(far_world.build_region_server_packet(&far)).unwrap();
        manager.pump(&server_recv).unwrap();

        assert!(!manager.world.as_ref().unwrap().region_exists(&far));
        let released: Vec<ClientPacket> = manager.client_packet_recv().try_iter().collect();
        assert!(released.iter().any(|p| matches!(p, ClientPacket::ReleaseRegionConnection(rc) if *rc == far)));
    }
}
