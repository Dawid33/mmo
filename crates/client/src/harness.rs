//! Headless, deterministic, tick-by-tick driver: a real LocalServer
//! (WorldManager<InlineSpawner>) + a real GameInstanceManager client, wired
//! over the existing channels and advanced in lockstep. No Bevy, no threads,
//! no wall clock. See docs/superpowers/specs/2026-07-06-headless-sim-test-harness-design.md.

use std::collections::{BTreeSet, HashMap};

use crossbeam::channel::{unbounded, Receiver};
use game::{
    state_hash, ClientUpdateEvent, GameEventKind, InputEvent, Key, RegionCoords, ServerPacket,
};

use crate::{GameInstanceManager, LocalServer, LOCAL_CLIENT_ID};

pub struct SimHarness {
    server: LocalServer,
    client: GameInstanceManager,
    server_to_client: Receiver<ServerPacket>,
    _bridge_recv: Receiver<ClientUpdateEvent>,
    held: BTreeSet<Key>,
    /// Keys released since the last `step()`, awaiting their one-shot key-up
    /// edge. The sim's `InputState` only leaves `Held` on a `pressed:false`
    /// event, so a release MUST send that edge or the key stays held forever
    /// (movement reads `key_held`).
    released: BTreeSet<Key>,
    /// Authoritative server region state hashes, keyed by (region, that
    /// region's tick), recorded every `step()` after `server.tick()`. The
    /// client is a bit-exact but ~1-tick-delayed mirror of the server in this
    /// lockstep model (the server snapshots a region then immediately ticks it
    /// forward before the client observes the snapshot), so convergence is
    /// checked tick-aligned: the client's `(region, tick)` must equal the
    /// server's authoritative hash recorded FOR THAT SAME TICK — not the
    /// server's live (already-advanced) hash.
    server_hashes: HashMap<(RegionCoords, usize), u32>,
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

        Self {
            server,
            client,
            server_to_client,
            _bridge_recv: bridge_recv,
            held: BTreeSet::new(),
            released: BTreeSet::new(),
            server_hashes: HashMap::new(),
        }
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
        // Only queue an up-edge if it was actually held, so release() without a
        // prior press() is a no-op (matches real edge-triggered input).
        if self.held.remove(&key) {
            self.released.insert(key);
        }
    }

    /// One deterministic tick: client predict -> server ingest+tick -> client reconcile.
    pub fn step(&mut self) {
        // 1. Client input + predict. Held keys re-send `pressed:true` each tick
        // (idempotent: InputState treats repeated press on a Held key as a
        // no-op); released keys send exactly one `pressed:false` up-edge so the
        // sim actually stops treating them as held.
        for &key in &self.held {
            let _ = self.client.push_game_event(GameEventKind::PlayerInput(
                LOCAL_CLIENT_ID,
                InputEvent::Key { key, pressed: true },
            ));
        }
        for &key in &self.released {
            let _ = self.client.push_game_event(GameEventKind::PlayerInput(
                LOCAL_CLIENT_ID,
                InputEvent::Key { key, pressed: false },
            ));
        }
        self.released.clear();
        self.client.send_tick();
        self.client.pump(&self.server_to_client).expect("client pump");

        // 2. Server ingest + advance.
        self.server.pump().expect("server pump");
        self.server.tick();

        // Record the server's authoritative per-region hash at its new tick, so
        // convergence can be checked tick-aligned against the ~1-tick-delayed
        // client (see `server_hashes` / `assert_converged`).
        for rc in self.server.running_regions() {
            let t = self.server.region_tick(rc);
            if let Some(hash) = self.server.region_hash(rc) {
                self.server_hashes.insert((rc, t), hash);
            }
        }

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
    /// Local player's body translation (region-local). Panics if called before
    /// the player exists (i.e. before `connect`).
    pub fn player_pos(&self) -> [f32; 3] {
        self.client.local_player_translation().expect("player exists after connect")
    }
    fn client_home(&self) -> RegionCoords {
        self.client.home_region()
    }

    /// Step with no new input until the client has drained all buffered server
    /// events (no snapshot/event still in flight). The client stays ~1 tick
    /// behind the server by construction, so we settle on "nothing pending",
    /// not on tick equality (which can never hold in this lockstep).
    pub fn settle(&mut self) {
        for _ in 0..64 {
            self.step();
            if self.pending_events_empty() {
                return;
            }
        }
        // Not fatal on its own; assert_converged surfaces any real divergence.
    }

    /// Bit-exact convergence: after settling, each region the client holds must
    /// match the server's authoritative state RECORDED AT THE CLIENT'S TICK for
    /// that region (the client is an exact but ~1-tick-delayed mirror — see
    /// `server_hashes`). Fails if a held region diverges, or if nothing could
    /// be tick-aligned at all (convergence would otherwise be vacuous).
    pub fn assert_converged(&mut self) {
        self.settle();
        let home = self.client.home_region();
        let mut home_checked = false;
        for rc in self.client.world_ref().loaded_regions() {
            let t = self.client.world_ref().current_tick(&rc);
            let client_hash = state_hash(self.client.world_ref().data(&rc));
            match self.server_hashes.get(&(rc, t)) {
                Some(&server_hash) => {
                    assert_eq!(
                        client_hash, server_hash,
                        "client region {:?} @tick {} diverges from the server's authoritative state at that tick",
                        rc, t
                    );
                    if rc == home {
                        home_checked = true;
                    }
                }
                None => {
                    // The home region is the region under test — it MUST be
                    // tick-alignable, or convergence is unverified there. A
                    // window neighbour we can't yet align (just-subscribed,
                    // its tick not yet recorded) is tolerated; the home region
                    // is not. Without this, a static neighbour matching would
                    // let a genuinely diverging fresh home region pass silently
                    // (e.g. right after cross_boundary / teleport_player).
                    assert!(
                        rc != home,
                        "home region {:?} @tick {} has no matching server-authoritative tick after settle — client is ahead of / out of sync with the authority (convergence unverified)",
                        rc, t
                    );
                }
            }
        }
        assert!(
            home_checked,
            "home region {:?} was not tick-aligned+verified against the server — convergence is vacuous",
            home
        );
    }

    /// Liveness: holding `key` advances the sim tick AND moves the player body.
    pub fn assert_progresses(&mut self, key: Key) {
        let t0 = self.client_tick();
        let p0 = self.player_pos();
        self.press(key);
        self.step_n(4);
        self.release(key);
        assert!(self.client_tick() > t0, "sim tick did not advance while holding {:?}", key);
        assert!(self.player_pos() != p0, "player did not move while holding {:?} (input frozen?)", key);
    }

    pub fn player_region(&self) -> RegionCoords {
        self.client.home_region()
    }

    /// Authoritative teleport for scenario setup (server-side, undo-safe).
    pub fn teleport_player(&mut self, pos: [f32; 3]) {
        self.server.teleport_local_player(pos);
        self.settle();
    }

    /// Hold the movement key for `dir` and step (bounded) until the client's
    /// home region changes — i.e. the player crossed a seam.
    pub fn cross_boundary(&mut self, dir: Dir) {
        let start = self.player_region();
        let key = match dir {
            Dir::North => Key::KeyW,
            Dir::South => Key::KeyS,
            Dir::East => Key::KeyD,
            Dir::West => Key::KeyA,
        };
        // fps-cam must be on for movement; toggle it first.
        self.press(Key::KeyE);
        self.step();
        self.release(Key::KeyE);
        self.step();
        self.press(key);
        for _ in 0..400 {
            self.step();
            if self.player_region() != start {
                break;
            }
        }
        self.release(key);
        assert_ne!(self.player_region(), start, "player never crossed a boundary");
    }

    pub fn pending_events_empty(&self) -> bool {
        self.client.pending_events_empty()
    }
}

/// Cardinal directions for `SimHarness::cross_boundary`.
#[derive(Copy, Clone, Debug)]
pub enum Dir {
    East,
    West,
    North,
    South,
}

impl Default for SimHarness {
    fn default() -> Self {
        Self::new()
    }
}
