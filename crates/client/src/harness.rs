//! Headless, deterministic, tick-by-tick driver: a real LocalServer
//! (WorldManager<InlineSpawner>) + a real GameInstanceManager client, wired
//! over the existing channels and advanced in lockstep. No Bevy, no threads,
//! no wall clock. See docs/superpowers/specs/2026-07-06-headless-sim-test-harness-design.md.

use std::collections::BTreeSet;

use crossbeam::channel::{unbounded, Receiver};
use game::{
    ClientUpdateEvent, GameEventKind, InputEvent, Key, RegionCoords, ServerPacket,
};

use crate::{GameInstanceManager, LocalServer, LOCAL_CLIENT_ID};

pub struct SimHarness {
    server: LocalServer,
    client: GameInstanceManager,
    server_to_client: Receiver<ServerPacket>,
    _bridge_recv: Receiver<ClientUpdateEvent>,
    held: BTreeSet<Key>,
}

impl SimHarness {
    /// Wire a client to a fresh local authoritative server. Mirrors the
    /// local_server.rs test setup.
    pub fn new() -> Self {
        game::set_hash_verification(true); // enforce hash(before)==hash(after undo) along the driven path
        let (game_event_send, game_event_recv) = unbounded::<GameEventKind>();
        let (bridge_send, bridge_recv) = unbounded::<ClientUpdateEvent>();
        // Dummy address: native netcode is never constructed in the harness.
        let addr = "127.0.0.1:0".parse().unwrap();
        let client = GameInstanceManager::new(game_event_send, game_event_recv, bridge_send, addr);

        let client_to_server = client.client_packet_recv(); // Receiver<ClientPacket>
        let (server_to_client_send, server_to_client) = unbounded::<ServerPacket>();
        let server = LocalServer::new(client_to_server, server_to_client_send).expect("local server");

        Self { server, client, server_to_client, _bridge_recv: bridge_recv, held: BTreeSet::new() }
    }

    /// Handshake: client requests its region; pump until the home region
    /// snapshot + 3x3 window have loaded.
    pub fn connect(&mut self) {
        self.client.start();
        // Drive a handful of steps so the RequestPlayerRegion -> PlayerRegion ->
        // Region-snapshot handshake and the initial window settle.
        for _ in 0..8 {
            self.step();
        }
    }

    pub fn press(&mut self, key: Key) {
        self.held.insert(key);
    }
    pub fn release(&mut self, key: Key) {
        self.held.remove(&key);
    }

    /// One deterministic tick: client predict -> server ingest+tick -> client reconcile.
    pub fn step(&mut self) {
        // 1. Client input + predict.
        for &key in &self.held {
            let _ = self.client.push_game_event(GameEventKind::PlayerInput(
                LOCAL_CLIENT_ID,
                InputEvent::Key { key, pressed: true },
            ));
        }
        self.client.send_tick();
        self.client.pump(&self.server_to_client).expect("client pump");

        // 2. Server ingest + advance.
        self.server.pump().expect("server pump");
        self.server.tick();

        // 3. Client reconcile.
        self.client.pump(&self.server_to_client).expect("client reconcile pump");
    }

    pub fn step_n(&mut self, n: usize) {
        for _ in 0..n {
            self.step();
        }
    }

    // --- inspectors used by tests ---
    pub fn client_tick(&self) -> usize {
        let rc = self.client_home();
        self.client.world_ref().current_tick(&rc)
    }
    pub fn client_region_loaded(&self, rc: RegionCoords) -> bool {
        self.client.world_ref().region_exists(&rc)
    }
    fn client_home(&self) -> RegionCoords {
        self.client.home_region()
    }
}

impl Default for SimHarness {
    fn default() -> Self {
        Self::new()
    }
}
