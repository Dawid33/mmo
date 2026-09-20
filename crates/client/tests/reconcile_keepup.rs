//! Reconcile convergence + keep-up suite: a headless client<->server harness
//! wiring the real GameInstanceManager to the real WorldManager core via
//! LocalServer (the wasm offline path), driven in deterministic lockstep with
//! fabricated PlayerInput.
//!
//! Covers three regimes (root-caused live 2026-07-08):
//! - convergence: displaced input echoes, latency, one-shot orphans and a
//!   mispredicted crossing (scenarios A/B/C/E/G/H);
//! - keep-up: while the client reconciles every server event each frame, the
//!   prediction log stays bounded under sustained input and drains to empty
//!   when input stops (`keeps_up_under_moderate_input`);
//! - overload: when the client reconciles fewer server events per frame than
//!   it predicts (models rollback+replay cost exceeding the frame budget),
//!   the event log grows without bound (`overload_grows_event_log_unbounded`).
//!   This is a KNOWN failure mode of the overall design — the fix is
//!   optimizing the game/client so the budget is never exceeded, not changing
//!   the reconcile logic. The test documents the signature so it's
//!   recognizable when it recurs.
//!
//! Reconcile tracing is captured by the test runner; run with
//! `-- --nocapture 2>diag.log` to inspect it.
use client::{GameInstanceManager, LocalServer, LOCAL_CLIENT_ID};
use crossbeam::channel::{Receiver, Sender};
use game::{
    ClientPacket, ClientUpdateEvent, GameDataUpdate, GameEventKind, InputEvent, Key, ServerPacket,
};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

struct StderrLog;
impl log::Log for StderrLog {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        eprintln!("[{}] {}", record.level(), record.args());
    }
    fn flush(&self) {}
}
static LOGGER: StderrLog = StderrLog;

struct Diag {
    manager: GameInstanceManager,
    server: LocalServer,
    server_recv: Receiver<ServerPacket>,
    client_recv: Receiver<ClientUpdateEvent>,
    // Keep render-bridge receivers alive so region emits don't hit closed channels.
    bridge_recv: Vec<Receiver<GameDataUpdate>>,
    // Client->server packets flow through this tap so scenarios can drop
    // packets in transit (models a server-side input drop / misroute).
    tap_from_manager: Receiver<ClientPacket>,
    tap_to_server: Sender<ClientPacket>,
    drop_next_input: bool,
}

impl Diag {
    fn new() -> Self {
        let _ = log::set_logger(&LOGGER);
        log::set_max_level(log::LevelFilter::Info);
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded::<ServerPacket>();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager = GameInstanceManager::new(
            game_send.clone(),
            game_recv,
            client_send,
            dummy_addr,
            game::DebugFlags::default(),
        );
        manager.set_lod_table(&[]);
        manager.set_near_radius(0);
        let (tap_to_server, server_side_recv) = crossbeam::channel::unbounded::<ClientPacket>();
        let server = LocalServer::new_with_generator(
            server_side_recv,
            server_send,
            client::local_server::lightweight_test_generator(),
        )
        .unwrap();
        Self {
            tap_from_manager: manager.client_packet_recv(),
            manager,
            server,
            server_recv,
            client_recv,
            bridge_recv: Vec::new(),
            tap_to_server,
            drop_next_input: false,
        }
    }

    /// Forward client->server packets through the tap, dropping one
    /// PlayerInput GameEvent if `drop_next_input` is armed.
    fn forward_packets(&mut self) {
        while let Ok(p) = self.tap_from_manager.try_recv() {
            if self.drop_next_input {
                if let ClientPacket::GameEvent(ev) | ClientPacket::GameEventAfterTick(ev, _) = &p {
                    if matches!(ev.kind, GameEventKind::PlayerInput(..)) {
                        eprintln!("[diag] DROPPING in transit: input event id={}", ev.id);
                        self.drop_next_input = false;
                        continue;
                    }
                }
            }
            self.tap_to_server.send(p).unwrap();
        }
    }

    /// Latency model: hold client->server GameEvent packets for `delay`
    /// frames before delivery (subscription/control packets pass through
    /// immediately so the handshake and window management stay untouched).
    fn forward_packets_delayed(
        &mut self,
        queue: &mut std::collections::VecDeque<(usize, ClientPacket)>,
        now: usize,
        delay: usize,
    ) {
        while let Ok(p) = self.tap_from_manager.try_recv() {
            if matches!(&p, ClientPacket::GameEvent(_) | ClientPacket::GameEventAfterTick(..)) {
                queue.push_back((now + delay, p));
            } else {
                self.tap_to_server.send(p).unwrap();
            }
        }
        while queue.front().is_some_and(|(due, _)| *due <= now) {
            let (_, p) = queue.pop_front().unwrap();
            self.tap_to_server.send(p).unwrap();
        }
    }

    fn stash_bridge_receivers(&mut self) {
        while let Ok(ev) = self.client_recv.try_recv() {
            if let ClientUpdateEvent::NewRegion(_, _, recv) = ev {
                self.bridge_recv.push(recv);
            }
        }
    }

    fn handshake(&mut self) {
        self.manager.start();
        for _ in 0..8 {
            self.forward_packets();
            self.server.pump().unwrap();
            assert!(self.manager.pump(&self.server_recv).unwrap());
            self.stash_bridge_receivers();
        }
        // One authoritative tick so a GameEvent arrives and flips is_caught_up.
        self.server.tick();
        assert!(self.manager.pump(&self.server_recv).unwrap());
        self.stash_bridge_receivers();
    }

    /// One lockstep frame: (optional input) -> client tick+pump -> server
    /// ingest -> server tick -> client reconcile.
    fn frame(&mut self, input: Option<InputEvent>) {
        if let Some(ev) = input {
            self.manager
                .push_game_event(GameEventKind::PlayerInput(LOCAL_CLIENT_ID, ev))
                .unwrap();
        }
        self.manager.send_tick();
        assert!(self.manager.pump(&self.server_recv).unwrap());
        self.forward_packets();
        self.server.pump().unwrap();
        self.server.tick();
        assert!(self.manager.pump(&self.server_recv).unwrap());
        self.stash_bridge_receivers();
    }

    /// Advance the client's prediction only (models the client running ahead
    /// of the server by induced latency).
    fn client_only_tick(&mut self) {
        self.manager.send_tick();
        assert!(self.manager.pump(&self.server_recv).unwrap());
        self.stash_bridge_receivers();
    }

    /// One frame where the client only gets to reconcile `budget` server
    /// GameEvents (control packets pass through un-budgeted). Models the
    /// overload regime: replay cost exceeds the frame budget, so consumption
    /// falls below prediction production. `staging` is the transport backlog
    /// (the live `recv_q`); `slow_lane` is the budgeted delivery channel.
    fn frame_throttled(
        &mut self,
        inputs: &[InputEvent],
        budget: usize,
        staging: &mut std::collections::VecDeque<ServerPacket>,
        slow_lane: &(Sender<ServerPacket>, Receiver<ServerPacket>),
    ) {
        for ev in inputs {
            self.manager
                .push_game_event(GameEventKind::PlayerInput(LOCAL_CLIENT_ID, ev.clone()))
                .unwrap();
        }
        self.manager.send_tick();
        assert!(self.manager.pump(&slow_lane.1).unwrap());
        self.forward_packets();
        self.server.pump().unwrap();
        self.server.tick();
        while let Ok(p) = self.server_recv.try_recv() {
            staging.push_back(p);
        }
        // In-order delivery from the backlog, at most `budget` events.
        let mut delivered = 0;
        while let Some(p) = staging.front() {
            let is_event = matches!(p, ServerPacket::GameEvent(_));
            if is_event && delivered >= budget {
                break;
            }
            let p = staging.pop_front().unwrap();
            if is_event {
                delivered += 1;
            }
            slow_lane.0.send(p).unwrap();
        }
        assert!(self.manager.pump(&slow_lane.1).unwrap());
        self.stash_bridge_receivers();
    }

    /// Unconfirmed-prediction count (event_log depth) in the home region.
    fn home_log_len(&self) -> usize {
        let home = self.manager.home_region();
        self.manager
            .world_ref()
            .regions
            .get(&home)
            .map(|r| r.pending_event_ids().len())
            .unwrap_or(0)
    }

    fn report(&mut self, tag: &str, frame: usize) {
        let home = self.manager.home_region();
        let world = self.manager.world_ref();
        let home_ids = world
            .regions
            .get(&home)
            .map(|r| r.pending_event_ids())
            .unwrap_or_default();
        let total_pending: usize = world
            .regions
            .values()
            .map(|r| r.pending_event_ids().len())
            .sum();
        let server_tick = self.server.region_tick(home);
        let client_tick = world
            .regions
            .get(&home)
            .map(|r| r.current_tick())
            .unwrap_or(0);
        eprintln!(
            "[diag {tag}] frame={frame} home={home:?} home_log_len={} home_ids={:?} total_pending={} client_tick={} server_tick={}",
            home_ids.len(),
            compact(&home_ids),
            total_pending,
            client_tick,
            server_tick,
        );
    }
}

/// Compact id list: first..=last with len, to keep lines readable.
fn compact(ids: &[usize]) -> String {
    match (ids.first(), ids.last()) {
        (Some(f), Some(l)) => format!("{f}..={l} (n={})", ids.len()),
        _ => "empty".to_string(),
    }
}

/// Scenario A: pure lockstep, one keypress. Baseline — must converge.
#[test]
fn diag_a_lockstep_single_input() {
    let mut d = Diag::new();
    d.handshake();
    eprintln!("=== SCENARIO A: lockstep, single input at frame 5 ===");
    for f in 0..20 {
        let input = (f == 5).then_some(InputEvent::Key {
            key: Key::KeyW,
            pressed: true,
        });
        d.frame(input);
        d.report("A", f);
    }
    assert_eq!(
        d.home_log_len(),
        0,
        "lockstep single input must fully confirm"
    );
}

/// Scenario B: client runs 5 ticks ahead (induced-latency model), sparse input.
#[test]
fn diag_b_client_ahead_sparse_input() {
    let mut d = Diag::new();
    d.handshake();
    for _ in 0..5 {
        d.client_only_tick();
    }
    eprintln!("=== SCENARIO B: client 5 ahead, input every 5 frames ===");
    for f in 0..60 {
        let input = (f % 5 == 0).then_some(InputEvent::Key {
            key: Key::KeyW,
            pressed: f % 10 == 0,
        });
        d.frame(input);
        d.report("B", f);
        if f >= 15 {
            assert!(
                d.home_log_len() <= 12,
                "log must stay bounded near the 5-tick lead (frame {f}, len {})",
                d.home_log_len()
            );
        }
    }
}

/// Scenario E: client ahead; ONE input's packet is lost in transit (models a
/// server-side "home not running" drop or a homes-routing misroute during a
/// crossing). The prediction becomes a permanent orphan: measure what that
/// does to reconcile forever after.
#[test]
fn diag_e_one_orphaned_input() {
    let mut d = Diag::new();
    d.handshake();
    for _ in 0..5 {
        d.client_only_tick();
    }
    eprintln!("=== SCENARIO E: one input dropped in transit at frame 10 ===");
    for f in 0..60 {
        if f == 10 {
            d.drop_next_input = true;
        }
        // Sparse legitimate inputs around the orphan, alternating content so
        // no two predictions are byte-identical.
        let input = (f % 7 == 3).then_some(InputEvent::MouseMotion {
            dx: f as f32,
            dy: 0.25,
        });
        let input = if f == 10 {
            Some(InputEvent::Key {
                key: Key::KeyW,
                pressed: true,
            })
        } else {
            input
        };
        d.frame(input);
        d.report("E", f);
    }
    // Documents current (undesired) behavior: the orphaned prediction is
    // pinned in the log forever, on top of the 5-tick lead. If this
    // assertion ever fails LOW, orphan eviction got implemented — update the
    // test to assert draining instead.
    assert!(
        d.home_log_len() >= 5,
        "orphan should keep the log from draining below the lead (len {})",
        d.home_log_len()
    );
}

/// Scenario G: MISPREDICTED CROSSING — the client's predicted pose crosses a
/// region boundary but the authoritative player never does (models pose
/// divergence at the boundary during an input chase window). Client flips
/// home to B and predicts EntityArrived + all subsequent inputs there; the
/// server keeps routing everything to A. Nothing ever confirms B's
/// predictions and no PlayerRegion correction ever comes.
#[test]
fn diag_g_mispredicted_crossing() {
    let mut d = Diag::new();
    d.handshake();
    for _ in 0..5 {
        d.client_only_tick();
    }
    let home = d.manager.home_region();
    // Teleport ONLY the client's predicted body past A's +x boundary.
    {
        let w = d.manager.world_mut();
        let key = *w.data(&home).player_entites.get(&LOCAL_CLIENT_ID).unwrap();
        let iso = game::IsometryReal::from_parts(
            game::na::Translation3::new(
                (game::REGION_SIZE + game::FLIP_HYSTERESIS + 2.0).into(),
                60.0f32.into(),
                128.0f32.into(),
            ),
            game::na::Unit::<game::na::Quaternion<game::parry::math::Real>>::identity(),
        );
        w.regions
            .get_mut(&home)
            .unwrap()
            .with_data(|dd| dd.set_body_pose_safe(key, iso));
    }
    eprintln!("=== SCENARIO G: client-only boundary crossing, then input every frame ===");
    let mut len_mid = 0;
    for f in 0..60 {
        let input = Some(InputEvent::MouseMotion {
            dx: f as f32,
            dy: 0.5,
        });
        d.frame(input);
        d.report("G", f);
        if f == 30 {
            len_mid = d.home_log_len();
        }
    }
    // Documents current (undesired) behavior: sustained home disagreement
    // orphans every input, so the flipped-home log grows without bound. If
    // this ever fails LOW, orphan eviction / home correction got implemented.
    let len_end = d.home_log_len();
    assert!(
        len_end > 40 && len_end > len_mid + 15,
        "mispredicted crossing should grow the log unboundedly (mid {len_mid}, end {len_end})"
    );
}

/// Scenario H: LATENCY COST MODEL — client 5 ahead, and client->server
/// GameEvent packets delayed 5 frames in transit. Every input echo is now
/// displaced ~5 slots from its prediction, so each input should cost ~5
/// mispredicts, each a FULL-log rollback+replay. Continuous input, live
/// gameplay shape. Compare total MISPREDICT count vs scenario C (delay 0,
/// 60 mispredicts for 60 inputs).
#[test]
fn diag_h_latency_chase_cost() {
    let mut d = Diag::new();
    d.handshake();
    for _ in 0..5 {
        d.client_only_tick();
    }
    eprintln!(
        "=== SCENARIO H: client 5 ahead + 5-frame input transit delay, input every frame ==="
    );
    let mut queue = std::collections::VecDeque::new();
    for f in 0..60 {
        d.manager
            .push_game_event(GameEventKind::PlayerInput(
                LOCAL_CLIENT_ID,
                InputEvent::MouseMotion {
                    dx: f as f32,
                    dy: -0.5,
                },
            ))
            .unwrap();
        d.manager.send_tick();
        assert!(d.manager.pump(&d.server_recv).unwrap());
        d.forward_packets_delayed(&mut queue, f, 5);
        d.server.pump().unwrap();
        d.server.tick();
        assert!(d.manager.pump(&d.server_recv).unwrap());
        d.stash_bridge_receivers();
        d.report("H", f);
        if f >= 15 {
            assert!(
                d.home_log_len() <= 14,
                "log must stay bounded at lead + transit window (frame {f}, len {})",
                d.home_log_len()
            );
        }
    }
}

/// Scenario C: client ahead + continuous input (mouse-motion-like, one per
/// frame) — the live gameplay shape.
#[test]
fn diag_c_client_ahead_continuous_input() {
    let mut d = Diag::new();
    d.handshake();
    for _ in 0..5 {
        d.client_only_tick();
    }
    eprintln!("=== SCENARIO C: client 5 ahead, input every frame ===");
    for f in 0..60 {
        let input = Some(InputEvent::MouseMotion {
            dx: 1.0 + f as f32,
            dy: -0.5,
        });
        d.frame(input);
        d.report("C", f);
        if f >= 15 {
            assert!(
                d.home_log_len() <= 12,
                "log must stay bounded under continuous input (frame {f}, len {})",
                d.home_log_len()
            );
        }
    }
}

/// KEEP-UP: with the full server stream reconciled every frame (client has
/// the compute budget), moderate fabricated input — one mouse motion per
/// frame plus a key edge every few frames — must keep the prediction log
/// bounded, and the log must drain once input stops.
#[test]
fn keeps_up_under_moderate_input() {
    let mut d = Diag::new();
    d.handshake();
    let mut max_len = 0;
    for f in 0..80 {
        d.frame(Some(InputEvent::MouseMotion {
            dx: f as f32,
            dy: 1.0,
        }));
        if f % 4 == 0 {
            d.frame(Some(InputEvent::Key {
                key: Key::KeyW,
                pressed: (f / 4) % 2 == 0,
            }));
        }
        if f >= 10 {
            max_len = max_len.max(d.home_log_len());
        }
    }
    eprintln!("[keep-up] max home_log_len after warmup: {max_len}");
    assert!(
        max_len <= 12,
        "prediction log must stay bounded under moderate input (max {max_len})"
    );
    // Input stops: everything in flight confirms and the log drains.
    for _ in 0..10 {
        d.frame(None);
    }
    let final_len = d.home_log_len();
    assert!(
        final_len <= 2,
        "log must drain once input stops (len {final_len})"
    );
}

/// OVERLOAD — KNOWN FAILURE MODE OF THE DESIGN. The client predicts 4 events
/// per frame (1 tick + 3 inputs) but only has budget to reconcile 1 server
/// event per frame (models rollback+replay cost exceeding the frame budget,
/// as measured live: ~8-9ms per replayed tick, passes reaching ~1s). The
/// unconfirmed event log then grows without bound (live: 7 -> 108 in 12s) and
/// the transport backlog piles up behind it. The fix for this regime is
/// optimizing the game/client so the budget is never exceeded — reconcile
/// itself is working as designed here.
#[test]
fn overload_grows_event_log_unbounded() {
    let mut d = Diag::new();
    d.handshake();
    let slow_lane = crossbeam::channel::unbounded::<ServerPacket>();
    let mut staging = std::collections::VecDeque::new();
    let mut len_at = [0usize; 3]; // frames 10, 25, 40
    for f in 0..40 {
        let inputs = [
            InputEvent::MouseMotion {
                dx: f as f32,
                dy: 0.1,
            },
            InputEvent::MouseMotion {
                dx: f as f32,
                dy: 0.2,
            },
            InputEvent::MouseMotion {
                dx: f as f32,
                dy: 0.3,
            },
        ];
        d.frame_throttled(&inputs, 1, &mut staging, &slow_lane);
        match f {
            10 => len_at[0] = d.home_log_len(),
            25 => len_at[1] = d.home_log_len(),
            39 => len_at[2] = d.home_log_len(),
            _ => {}
        }
    }
    eprintln!(
        "[overload] home_log_len at frames 10/25/39: {:?}; transport backlog: {}",
        len_at,
        staging.len()
    );
    // Production 4/frame, consumption 1/frame: ~+3/frame net, monotone.
    assert!(
        len_at[0] < len_at[1] && len_at[1] < len_at[2],
        "event log must grow monotonically under overload: {len_at:?}"
    );
    assert!(
        len_at[2] >= 60,
        "event log must grow without bound under overload (frame 39: {})",
        len_at[2]
    );
    assert!(
        staging.len() >= 20,
        "transport backlog must pile up behind the stalled client ({})",
        staging.len()
    );
}

/// RECOVERY BOUNDARY: a TRANSIENT stall (the live spiral was seeded by
/// ~370ms region-genesis stalls) must be recoverable. Starve the client for
/// 15 frames so a real deficit builds, then restore full budget and assert
/// the log drains back down — distinguishing "transient stall, recovers"
/// from "sustained deficit, spirals" (overload test above).
#[test]
fn transient_stall_recovers() {
    let mut d = Diag::new();
    d.handshake();
    let slow_lane = crossbeam::channel::unbounded::<ServerPacket>();
    let mut staging = std::collections::VecDeque::new();
    // Phase 1 — stall: 2 inputs + 1 tick predicted per frame, 1 event
    // reconciled per frame.
    for f in 0..15 {
        let inputs = [
            InputEvent::MouseMotion {
                dx: f as f32,
                dy: 0.4,
            },
            InputEvent::MouseMotion {
                dx: f as f32,
                dy: 0.8,
            },
        ];
        d.frame_throttled(&inputs, 1, &mut staging, &slow_lane);
    }
    let deficit = d.home_log_len();
    eprintln!(
        "[recovery] log depth after stall: {deficit}, backlog: {}",
        staging.len()
    );
    assert!(
        deficit >= 20,
        "stall phase must build a real deficit (len {deficit})"
    );
    // Phase 2 — full budget restored, light input continues.
    for f in 0..40 {
        let inputs = [InputEvent::MouseMotion {
            dx: 100.0 + f as f32,
            dy: 0.1,
        }];
        d.frame_throttled(&inputs, usize::MAX, &mut staging, &slow_lane);
    }
    // Phase 3 — input stops; everything in flight confirms.
    for _ in 0..10 {
        d.frame_throttled(&[], usize::MAX, &mut staging, &slow_lane);
    }
    let final_len = d.home_log_len();
    eprintln!(
        "[recovery] log depth after recovery: {final_len}, backlog: {}",
        staging.len()
    );
    assert!(
        final_len <= 2,
        "log must drain once the budget is restored (len {final_len})"
    );
    assert!(
        staging.is_empty(),
        "backlog must clear ({} left)",
        staging.len()
    );
}

/// WALL-CLOCK SOAK — perf gate, release only (`just soak`). Runs the real
/// pipeline at true cadence: server ticks every TICK_RATE ms, the client
/// ticks at its adaptive rate and reconciles as fast as it can, fabricated
/// mouse input arrives at ~60/s. If a game change makes region ticks (and
/// therefore rollback+replays) too slow for the frame budget, the prediction
/// log trends up here — the death-spiral signature — and the test fails.
/// In a debug build the client legitimately cannot keep up, so the test
/// measures but does not assert.
#[test]
#[ignore = "wall-clock perf gate; run in release via `just soak`"]
fn soak_realtime_keepup() {
    use std::time::{Duration, Instant};
    /// Highest home-log depth tolerated after warmup (steady state in a
    /// healthy release build sits in the low single digits).
    const SOAK_MAX_LOG: usize = 32;
    const SOAK_INPUT_SECS: u64 = 15;
    const SOAK_DRAIN_SECS: u64 = 3;
    const WARMUP_SAMPLES: usize = 3; // 1s samples skipped before asserting

    let mut d = Diag::new();
    d.handshake();
    let start = Instant::now();
    let mut next_server_tick = start;
    let mut next_client_tick = start;
    let mut next_input = start;
    let mut next_sample = start + Duration::from_secs(1);
    let mut samples: Vec<usize> = Vec::new();
    let input_until = start + Duration::from_secs(SOAK_INPUT_SECS);
    let end = input_until + Duration::from_secs(SOAK_DRAIN_SECS);
    let mut n = 0f32;
    while Instant::now() < end {
        let now = Instant::now();
        if now >= next_server_tick {
            d.forward_packets();
            d.server.pump().unwrap();
            d.server.tick();
            next_server_tick += Duration::from_millis(game::TICK_RATE);
        }
        if now >= next_client_tick {
            d.manager.send_tick();
            next_client_tick += Duration::from_millis(d.manager.tick_rate_ms().max(1));
        }
        if now < input_until && now >= next_input {
            n += 1.0;
            d.manager
                .push_game_event(GameEventKind::PlayerInput(
                    LOCAL_CLIENT_ID,
                    InputEvent::MouseMotion { dx: n, dy: -0.5 },
                ))
                .unwrap();
            next_input += Duration::from_millis(16);
        }
        assert!(d.manager.pump(&d.server_recv).unwrap());
        d.stash_bridge_receivers();
        if now >= next_sample {
            samples.push(d.home_log_len());
            next_sample += Duration::from_secs(1);
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    let final_len = d.home_log_len();
    eprintln!("[soak] 1s log-depth samples: {samples:?}; final after drain: {final_len}");
    if cfg!(debug_assertions) {
        eprintln!("[soak] debug build — client legitimately can't keep up; not asserting");
        return;
    }
    for (i, len) in samples.iter().enumerate().skip(WARMUP_SAMPLES) {
        assert!(
            *len <= SOAK_MAX_LOG,
            "prediction log must stay bounded in real time (sample {i}: {len}, all {samples:?})"
        );
    }
    assert!(
        final_len <= 4,
        "log must drain after input stops (len {final_len})"
    );
}
