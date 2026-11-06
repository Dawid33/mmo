//! Headless, deterministic, tick-by-tick driver: a real LocalServer
//! (WorldManager<InlineSpawner>) + a real GameInstanceManager client, wired
//! over the existing channels and advanced in lockstep. No Bevy, no threads,
//! no wall clock. See docs/superpowers/specs/2026-07-06-headless-sim-test-harness-design.md.

use std::collections::{BTreeSet, HashMap};

use crossbeam::channel::{unbounded, Receiver, Sender};
use game::{
    state_hash, ClientUpdateEvent, GameEventKind, InputEvent, Key, RegionCoords, ServerPacket,
};

use crate::{GameInstanceManager, LocalServer, LOCAL_CLIENT_ID};

pub struct SimHarness {
    server: LocalServer,
    client: GameInstanceManager,
    server_to_client: Receiver<ServerPacket>,
    bridge_recv: Receiver<ClientUpdateEvent>,
    /// Debug toggles shared with the manager — tests flip these the way the
    /// render thread's F3 dev panel would.
    pub debug: game::DebugFlags,
    held: BTreeSet<Key>,
    /// Keys released since the last `step()`, awaiting their one-shot key-up
    /// edge. The sim's `InputState` only leaves `Held` on a `pressed:false`
    /// event, so a release MUST send that edge or the key stays held forever
    /// (movement reads `key_held`).
    released: BTreeSet<Key>,
    /// Authoritative server region state hashes, keyed by (region, that
    /// region's tick), each taken at the START of the slot — right after the
    /// Tick event, before the slot's held inputs and transfers (the state the
    /// desync check hashes, and what a client region holds at that tick: it
    /// predicts a slot's inputs and releases its held transfers only on its
    /// way to the next tick). Recorded every `step()` by `server.tick()`. The
    /// client is a bit-exact but ~1-tick-delayed mirror of the server in this
    /// lockstep model (the server snapshots a region then immediately ticks it
    /// forward before the client observes the snapshot), so convergence is
    /// checked tick-aligned: the client's `(region, tick)` must equal the
    /// server's authoritative hash recorded FOR THAT SAME TICK — not the
    /// server's live (already-advanced) hash. (That describes a client that
    /// trails; one that leads is aligned by `catch_up`.)
    server_hashes: HashMap<(RegionCoords, game::Tick), u32>,
    /// What the client actually reads: `server_to_client` forwarded through
    /// `forward()`, which can withhold `SyncClock` (see `catch_up`).
    client_in_send: Sender<ServerPacket>,
    client_in: Receiver<ServerPacket>,
    /// The client's regions run on their own threads (`threaded`); every
    /// pump then waits for them (`settle_regions`), so a step stays lockstep.
    threaded: bool,
}

/// Most server-only ticks `catch_up` runs — comfortably more than the
/// client's lead band (`CLIENT_LEAD_MS` in instance.rs).
const CATCH_UP_MAX_TICKS: usize = 8;

/// Ticks `cross_boundary` spends ascending before it heads for the seam
/// (20 ticks of creative flight ≈ 7.5 blocks).
const CROSSING_CLIMB_TICKS: usize = 20;

impl SimHarness {
    /// Wire a client to a fresh local authoritative server. Mirrors the
    /// local_server.rs test setup. LOD ring subscriptions are OFF (empty
    /// table): every key the client requests costs the LocalServer a
    /// synchronous full-region build, minutes of wall clock at the
    /// production table's radius. LOD-focused tests opt back in with
    /// `set_lod_table`.
    pub fn new() -> Self {
        Self::with_config(&[], 0)
    }

    /// `new`, with an explicit LOD ring table (see `new` for why the
    /// default is empty).
    pub fn with_lod_table(lod_table: &'static [(i32, u32)]) -> Self {
        Self::with_config(lod_table, 0)
    }

    /// Build a harness with an explicit near-region radius. Crossing tests
    /// opt into radius 1; home-only tests keep the one-region default.
    pub fn with_near_radius(near_radius: i32) -> Self {
        Self::with_config(&[], near_radius)
    }

    /// `with_near_radius`, with the client's regions on their own threads
    /// as in native play. Region state is then only reachable through the
    /// regions themselves (`client_home_data` is unavailable).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn threaded(near_radius: i32) -> Self {
        let mut h = Self::with_config(&[], near_radius);
        h.client.use_region_threads();
        h.threaded = true;
        h
    }

    fn with_config(lod_table: &'static [(i32, u32)], near_radius: i32) -> Self {
        game::set_hash_mode(game::HashMode::Full); // enforce hash(before)==hash(after undo) along the driven path
        let (game_event_send, game_event_recv) = unbounded::<GameEventKind>();
        let (bridge_send, bridge_recv) = unbounded::<ClientUpdateEvent>();
        // Dummy address: native netcode is never constructed in the harness.
        let addr = "127.0.0.1:0".parse().unwrap();
        let debug = game::DebugFlags::default();
        let mut client = GameInstanceManager::new(
            game_event_send,
            game_event_recv,
            bridge_send,
            addr,
            debug.clone(),
        );
        client.set_lod_table(lod_table);
        client.set_near_radius(near_radius);

        let client_to_server = client.client_packet_recv(); // Receiver<ClientPacket>
        let (server_to_client_send, server_to_client) = unbounded::<ServerPacket>();
        let (client_in_send, client_in) = unbounded::<ServerPacket>();
        let server = LocalServer::new_with_generator(
            client_to_server,
            server_to_client_send,
            crate::local_server::lightweight_test_generator(),
        )
        .expect("local server");
        let mut server = server;
        server.record_slot_start_hashes();

        Self {
            server,
            client,
            server_to_client,
            bridge_recv,
            debug,
            held: BTreeSet::new(),
            released: BTreeSet::new(),
            server_hashes: HashMap::new(),
            client_in_send,
            client_in,
            threaded: false,
        }
    }

    /// Handshake: client requests its region and pumps until the configured
    /// near window has loaded (home-only by default).
    pub fn connect(&mut self) {
        self.client.start();
        // Drive a handful of steps so the RequestPlayerRegion -> PlayerRegion ->
        // Region-snapshot handshake and the initial window settle.
        for _ in 0..8 {
            self.step();
        }
    }

    /// Drain everything the manager has sent to the (absent) render bridge.
    pub fn drain_bridge_events(&mut self) -> Vec<ClientUpdateEvent> {
        self.bridge_recv.try_iter().collect()
    }

    /// Jitter the server's region scheduling from a seed, standing in for
    /// real region threads. Use `InlineSchedule::late` (or `shuffled`): they
    /// keep every region's tick count in lockstep with the client, which the
    /// tick-aligned `assert_converged` needs. `jittery` makes regions miss
    /// ticks, and this harness's client cannot pace itself against that.
    pub fn set_schedule(&mut self, schedule: game::InlineSchedule) {
        self.server.set_schedule(Some(schedule));
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
                InputEvent::Key {
                    key,
                    pressed: false,
                },
            ));
        }
        self.released.clear();
        self.client.send_tick();
        self.pump_client(true);

        // 2. Server ingest + advance.
        self.tick_server();

        // 3. Client reconcile.
        self.pump_client(true);
    }

    /// Server ingest + one authoritative tick, recording each region's
    /// slot-start hash at every tick it ran so convergence can be checked
    /// tick-aligned (see `server_hashes` / `assert_converged`).
    fn tick_server(&mut self) {
        self.server.pump().expect("server pump");
        self.server.tick();
        for (rc, t, hash) in self.server.take_slot_start_hashes() {
            self.server_hashes.insert((rc, t), hash);
        }
    }

    /// Deliver the server's packets and let the client process them (plus any
    /// queued `Tick`). `with_clock: false` withholds `SyncClock`, so the
    /// client reconciles without re-pacing itself against the server.
    fn pump_client(&mut self, with_clock: bool) {
        for packet in self.server_to_client.try_iter() {
            if with_clock || !matches!(packet, ServerPacket::SyncClock(..)) {
                let _ = self.client_in_send.send(packet);
            }
        }
        self.client.pump(&self.client_in).expect("client pump");
        if self.threaded {
            self.client.settle_regions().expect("client regions");
            // What settling routed may have queued more (events, ticks).
            self.client.pump(&self.client_in).expect("client pump");
        }
    }

    pub fn step_n(&mut self, n: usize) {
        for _ in 0..n {
            self.step();
        }
    }

    // --- inspectors used by tests ---
    pub fn client_tick(&self) -> game::Tick {
        if self.threaded {
            return self.client.home_tick().expect("home region reports a tick");
        }
        let rc = self.client_home();
        self.client.world_ref().current_tick(&rc)
    }
    pub fn client_region_count(&self) -> usize {
        self.client.loaded_regions().len()
    }
    pub fn client_region_loaded(&self, rc: RegionCoords) -> bool {
        self.client.loaded_regions().contains(&rc)
    }

    /// Every client region's `(tick, state hash)`: read directly on the
    /// inline host, asked of the regions on threads.
    fn client_regions(&mut self) -> HashMap<RegionCoords, (game::Tick, u32)> {
        if self.threaded {
            return self.client.region_probe().expect("client regions").into_iter().collect();
        }
        let world = self.client.world_ref();
        world
            .loaded_regions()
            .into_iter()
            .map(|rc| (rc, (world.current_tick(&rc), state_hash(world.data(&rc)))))
            .collect()
    }
    /// Local player's body translation (region-local). Panics if called before
    /// the player exists (i.e. before `connect`).
    pub fn player_pos(&self) -> [f32; 3] {
        self.client
            .local_player_translation()
            .expect("player exists after connect")
    }
    fn client_home(&self) -> RegionCoords {
        self.client.home_region()
    }

    /// Step with no new input until the client has drained all buffered server
    /// events (no snapshot/event still in flight). Client and server ticks
    /// never line up by construction (the client trails a fresh snapshot by a
    /// tick, then holds a lead over the server), so we settle on "nothing
    /// pending", not on tick equality.
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
    ///
    /// Right after a boundary crossing a region can be
    /// bit-inexact for a FEW ticks (the just-flipped home; or a neighbour
    /// whose server copy got a transfer a slot late, which the client's
    /// prediction for that slot cannot know until the server closes it) — the spec-accepted transient flip-tick
    /// rubber-band (docs/superpowers/2026-07-06-seamless-local-crossing-followup.md),
    /// which reconciles within ~1 tick once input stops. `settle()` returns as
    /// soon as `pending_events_empty()` (true at the flip tick), so before the
    /// strict check we step idle up to a small bound to let that transient
    /// heal. This does NOT weaken the check — it still demands bit-exact
    /// agreement; it only permits the documented, bounded reconciliation the
    /// authority guarantees. Callers must not hold input across this call (the
    /// heal steps are meant to be idle); the tests release before asserting.
    /// A genuinely non-healing divergence (e.g. input held indefinitely across
    /// a seam) still exhausts the bound and fails loudly below.
    pub fn assert_converged(&mut self) {
        self.settle();
        const HEAL_BOUND: usize = 8;
        let home = self.client.home_region();
        let mut aligned = self.catch_up();
        for _ in 0..HEAL_BOUND {
            // Every aligned region healed (the home among them); none is
            // skipped by the strict check below either.
            if aligned.contains_key(&home) && aligned.values().all(|(_, client, server)| client == server) {
                break;
            }
            self.step();
            aligned = self.catch_up();
        }
        for rc in self.client.loaded_regions() {
            match aligned.get(&rc) {
                Some(&(t, client_hash, server_hash)) => {
                    assert_eq!(
                        client_hash, server_hash,
                        "client region {:?} @tick {} diverges from the server's authoritative state at that tick",
                        rc, t
                    );
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
                        "home region {:?} @tick {} could not be tick-aligned with the server within {} catch-up ticks — out of sync with the authority (convergence unverified)",
                        rc,
                        self.client.world_ref().current_tick(&rc),
                        CATCH_UP_MAX_TICKS
                    );
                }
            }
        }
        assert!(
            aligned.contains_key(&home),
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
        assert!(
            self.client_tick() > t0,
            "sim tick did not advance while holding {:?}",
            key
        );
        assert!(
            self.player_pos() != p0,
            "player did not move while holding {:?} (input frozen?)",
            key
        );
    }

    pub fn player_region(&self) -> RegionCoords {
        self.client.home_region()
    }

    /// Authoritative teleport for scenario setup (server-side, undo-safe).
    pub fn teleport_player(&mut self, pos: [f32; 3]) {
        self.server.teleport_local_player(pos);
        self.settle();
    }

    /// Fly in `dir` and step (bounded) until the client's
    /// home region changes AND the player entity is actually queryable there
    /// — i.e. the player crossed a seam and the crossing has fully landed.
    ///
    /// `home_region` flips as soon as the authoritative `PlayerRegion` packet
    /// is handled, which can land one tick before the predicted
    /// `apply_local_transfers` path creates the player entity in the new
    /// region's data (`GameInstanceManager::local_player_translation` briefly
    /// returns `None` in that window). Waiting on both conditions makes
    /// "crossed" mean what callers actually need (`player_pos()` etc. are
    /// safe to call immediately after), without loosening any assertion.
    ///
    /// The final `settle()` gives the just-crossed region's authoritative
    /// confirmation one more round to land: the exact tick this loop breaks
    /// on can be transiently one step behind reconciliation (same class of
    /// lag as the home-flip/entity-queryable gap above), which otherwise
    /// makes an `assert_converged()` called immediately after `cross_boundary`
    /// spuriously see a same-tick hash mismatch that self-heals on the very
    /// next `step()`.
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
        // Fly there: a walking body stops at the first ledge taller than a
        // step. Double-tap Space enters flight; holding the second tap climbs
        // clear of the terrain around the spawn.
        self.press(Key::Space);
        self.step();
        self.release(Key::Space);
        self.step();
        self.press(Key::Space);
        self.step_n(CROSSING_CLIMB_TICKS);
        self.release(Key::Space);
        self.step();
        self.press(key);
        for _ in 0..400 {
            self.step();
            if self.player_region() != start && self.client.local_player_translation().is_some() {
                break;
            }
        }
        self.release(key);
        assert_ne!(
            self.player_region(),
            start,
            "player never crossed a boundary"
        );
        assert!(
            self.client.local_player_translation().is_some(),
            "player entity not queryable in the new home region after crossing"
        );
        self.settle();
    }

    pub fn pending_events_empty(&self) -> bool {
        self.client.pending_events_empty()
    }

    /// The client's home region's full rollback state, for determinism checks
    /// (`state_hash(h.client_home_data())`).
    pub fn client_home_data(&self) -> &game::Rollback {
        self.client.world_ref().data(&self.client.home_region())
    }

    /// Does the client's home region currently match the server's authoritative
    /// hash recorded at the client's home tick? `Some(true|false)`, or `None` if
    /// the server never recorded that region at that tick.
    /// Tick-align client and server and return, per client region,
    /// `(tick, client hash, server hash)` AT THE CLIENT'S CURRENT TICK.
    ///
    /// The client holds a lead over the server (`CLIENT_LEAD_MS` in
    /// instance.rs; window regions are stepped into the lead band), so a
    /// region's client tick is usually one the server has not reached. The
    /// client's past states can't be read back, and a hash noted when a tick
    /// was first predicted is no substitute — the server's per-tick events (a
    /// ghost mirror refreshing while the player stands in a seam margin)
    /// rewrite every predicted tick on arrival. So the CLIENT is frozen while
    /// the server alone ticks up to it, the client reconciling what arrives;
    /// `SyncClock` is withheld for the duration, or the client would step
    /// itself forward to win its lead back. Both sides then hold final state
    /// for the same tick, and the comparison is as strict as it ever was.
    /// Afterwards the client predicts the same number of ticks alone, which
    /// restores the lead it had — the run continues as if uninterrupted.
    fn catch_up(&mut self) -> HashMap<RegionCoords, (game::Tick, u32, u32)> {
        let mut steps = 0;
        let mut client = self.client_regions();
        while steps < CATCH_UP_MAX_TICKS
            && client.iter().any(|(rc, (t, _))| {
                !self.server_hashes.contains_key(&(*rc, *t)) && self.server.region_tick(*rc) < *t
            })
        {
            self.tick_server();
            self.pump_client(false);
            steps += 1;
            client = self.client_regions();
        }

        let aligned = client
            .into_iter()
            .filter_map(|(rc, (t, hash))| {
                let server = *self.server_hashes.get(&(rc, t))?;
                Some((rc, (t, hash, server)))
            })
            .collect();

        for _ in 0..steps {
            self.client.send_tick();
            self.pump_client(false);
        }
        aligned
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
