//! Everything the client does to ONE predicted region, as a command in and
//! reports out. The manager never touches a `Region` itself: the inline host
//! calls [`handle`] on the caller's thread, the threaded host on the region's
//! own thread, and both see the same reports in the same order.
use crossbeam::channel::Sender;
use game::{
    ClientId, ClientPacket, ClientUpdateEvent, EntityBundle, GameError, GameEvent, GameEventKind,
    GhostData, ReconcileStats, Region, RegionCoords, RegionId,
};
use log::info;
use std::collections::VecDeque;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

use super::clock::{decide, ClockDecision, ClockInput};

/// One instruction for one region.
#[derive(Debug)]
pub enum RegionCmd {
    /// One predicted tick, or sit it out while the region is stalled.
    Tick,
    /// A server-authoritative event to reconcile, with the server's
    /// `(tick, state hash)` when it came as a `HashedGameEvent`.
    ServerEvent(GameEvent, Option<(game::Tick, u32)>),
    /// A PlayerInput / UiAction / Order from this client: predicted here and
    /// sent to the server stamped with the tick it was predicted after.
    Predict(GameEventKind),
    /// A predicted arrival from a sibling region (isometry already rebased),
    /// stamped with the source's tick after the tick that produced it: held
    /// to the end of that slot here, the server's slot and order (see
    /// [`RunnerState::held_transfers`]).
    Arrival(EntityBundle, game::Tick),
    /// A predicted margin mirror from a sibling region (ditto, same stamp).
    Ghost(GhostData, game::Tick),
    Clock(ClockInput),
    SetMapExtent(Option<game::MapExtent>),
    /// Wireframe the colliders near this viewer WORLD position.
    DebugLines([f32; 3]),
    /// Answered with `Pong` once every earlier command has run.
    Ping(u64),
    /// Answered with `StateHash`: the region's tick and `game::state_hash`
    /// (harnesses compare it with the server's, whichever host runs it).
    StateHash,
    Stop,
}

/// What a region reports back.
#[derive(Debug)]
pub enum RegionReport {
    /// One `Tick` (or alignment tick): this tick's boundary crossings and
    /// margin mirrors, targets absolute, isometries still region-local.
    /// Every host relays them per report, so both see one order.
    Ticked {
        sat_out: bool,
        departures: Vec<(EntityBundle, RegionCoords)>,
        ghosts: Vec<(GhostData, RegionCoords)>,
        /// The region's `current_tick()` right after this tick (T+1 for its
        /// tick T): the slot the targets apply these transfers in, as the
        /// server's `RegionOutput::Departures` stamp.
        stamp: game::Tick,
        #[cfg(not(target_arch = "wasm32"))]
        perf: game::RegionTickPerf,
        /// Wall time of this one forward tick (zero when sat out; replays
        /// are not in it). The F3 Debug tab's per-region tick times.
        #[cfg(not(target_arch = "wasm32"))]
        took: Duration,
    },
    Reconciled {
        stats: ReconcileStats,
        /// How far the local body moved when a misprediction was corrected.
        correction_m: Option<f32>,
        #[cfg(not(target_arch = "wasm32"))]
        took: Duration,
    },
    Clock(ClockDecision),
    /// This region's view of the local client; `None` = it no longer holds it.
    View(Option<HomeView>),
    /// A `Predict` that reached this region after the local player's body
    /// left it — on a region thread, the game thread may send one before it
    /// sees the departure. Re-sent to the new home (where it is held behind
    /// the arrival if that is still held), as the server's region sends one
    /// back as `RegionOutput::Forward`: inputs follow the body.
    Forward(GameEventKind),
    Pong(u64),
    StateHash { tick: game::Tick, hash: u32 },
    Failed(GameError),
}

/// What the game thread needs from the region holding the local client.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeView {
    pub tick: game::Tick,
    /// The local body's region-local translation (`None`: no body).
    pub translation: Option<[f32; 3]>,
    pub fps_cam_mode: bool,
    pub hotbar: game::HotbarView,
    /// The block the local player is mining and how far along (Adventure;
    /// drawn on the crosshair).
    pub mining: Option<game::MiningView>,
    pub mode: game::PlayMode,
    pub inventory_open: bool,
    /// The local player's 36 slots and the stack on its cursor (the
    /// inventory screen).
    pub inventory: game::Inventory,
    pub cursor: Option<game::ItemStack>,
    /// The chest the local player has open (its screen shows these slots).
    pub container: Option<game::ContainerView>,
    /// Unconfirmed predictions (the death-spiral signal).
    pub log_len: usize,
}

/// A region's fixed context: who it runs for and where it reports to.
#[derive(Clone)]
pub struct RunnerCtx {
    pub id: RegionId,
    pub client_id: ClientId,
    pub server_send: Sender<ClientPacket>,
    pub client_event_send: Sender<ClientUpdateEvent>,
    pub debug_flags: game::DebugFlags,
    /// The region runs its own alignment ticks (a region thread). `false` =
    /// the caller sends them one `Tick` at a time (the inline host). Every
    /// tick's transfers ride its `Ticked` report either way. Set by the host.
    pub self_relay: bool,
}

/// A region's runner-side state (the region itself is held beside it).
#[derive(Default)]
pub struct RunnerState {
    /// Ticks still to sit out: the timeline ran ahead of the server's.
    stall: usize,
    /// The local player left this region in a predicted crossing.
    player_departed: bool,
    /// Predicted arrivals / margin mirrors whose slot (the stamp) this
    /// region has not closed, in receipt order: released at the start of
    /// the `Tick` that closes it, right before that tick runs — so they
    /// follow every input this client predicted in the slot, as the server's
    /// `RegionRunner` releases its held inputs for a slot before its held
    /// transfers. A sat-out tick closes nothing and releases nothing.
    held_transfers: VecDeque<(GameEventKind, game::Tick)>,
    /// This client's inputs that reached the region while its body's
    /// arrival was held here: predicted right after the arrival, in order
    /// — where the server's region applies them too (they follow the body).
    held_inputs: Vec<GameEventKind>,
    collider_debug: game::ColliderDebug,
    last_view: Option<HomeView>,
}

/// Run `cmd` on `region`, appending its reports to `out`. Returns false after
/// `Stop`.
pub fn handle(
    region: &mut Region,
    st: &mut RunnerState,
    ctx: &RunnerCtx,
    cmd: RegionCmd,
    out: &mut Vec<RegionReport>,
) -> bool {
    match cmd {
        RegionCmd::Stop => {
            st.held_transfers.clear();
            st.held_inputs.clear();
            return false;
        }
        RegionCmd::Tick => tick(region, st, ctx, out),
        RegionCmd::ServerEvent(event, hash) => {
            #[cfg(not(target_arch = "wasm32"))]
            let start = Instant::now();
            if let Some((tick, hash)) = hash {
                region.expect_state_hash(event.id, tick, hash);
            }
            let before = body_translation(region, ctx.client_id);
            match region.reconcile(event) {
                Ok(stats) => {
                    let correction_m = (stats.mispredictions > 0)
                        .then(|| Some((before?, body_translation(region, ctx.client_id)?)))
                        .flatten()
                        .map(|(a, b)| (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f32>().sqrt());
                    out.push(RegionReport::Reconciled {
                        stats,
                        correction_m,
                        #[cfg(not(target_arch = "wasm32"))]
                        took: start.elapsed(),
                    });
                }
                Err(e) => out.push(RegionReport::Failed(e)),
            }
        }
        RegionCmd::Predict(kind) => {
            let has_body = region.data().player_entites.contains_key(&ctx.client_id);
            if !has_body && local_arrival_held(st, ctx.client_id) {
                st.held_inputs.push(kind);
            } else if st.player_departed && !has_body {
                out.push(RegionReport::Forward(kind));
            } else {
                predict(region, ctx, kind, out);
            }
        }
        RegionCmd::Arrival(bundle, stamp) => {
            // On receipt, not release: input for the returning player is
            // held behind it here from now on, never sent back.
            if bundle.client.as_ref().map(|(c, _)| *c) == Some(ctx.client_id) {
                st.player_departed = false;
            }
            transfer(region, st, ctx, GameEventKind::EntityArrived(bundle), stamp, out);
        }
        RegionCmd::Ghost(data, stamp) => transfer(region, st, ctx, GameEventKind::GhostUpdate(data), stamp, out),
        RegionCmd::Clock(input) => {
            let d = decide(region.current_tick(), &input);
            out.push(RegionReport::Clock(d));
            if let Some((error, ticks)) = d.step.filter(|(_, ticks)| *ticks > 0) {
                if error > 0 {
                    st.stall = ticks;
                } else {
                    st.stall = 0;
                    // Inline, the caller runs them, relaying after each.
                    if ctx.self_relay {
                        for _ in 0..ticks {
                            tick(region, st, ctx, out);
                        }
                    }
                }
            }
        }
        RegionCmd::SetMapExtent(extent) => region.set_map_extent(extent),
        RegionCmd::DebugLines(viewer_world) => {
            if ctx.debug_flags.collider_wireframes() {
                let off = ctx.id.world_offset();
                let viewer_local = [
                    viewer_world[0] - off[0],
                    viewer_world[1] - off[1],
                    viewer_world[2] - off[2],
                ];
                let data = region.data();
                let lines = st.collider_debug.collect(&*data.physics.bodies, &*data.physics.colliders, viewer_local);
                let _ = ctx.client_event_send.send(ClientUpdateEvent::DebugLines(ctx.id, lines));
            }
        }
        RegionCmd::Ping(n) => out.push(RegionReport::Pong(n)),
        RegionCmd::StateHash => out.push(RegionReport::StateHash {
            tick: region.current_tick(),
            hash: game::state_hash(region.data()),
        }),
    }
    let view = home_view(region, ctx.client_id);
    if view != st.last_view {
        st.last_view = view.clone();
        out.push(RegionReport::View(view));
    }
    true
}

/// One `Tick` command: run it, or sit it out while stalled.
fn tick(region: &mut Region, st: &mut RunnerState, ctx: &RunnerCtx, out: &mut Vec<RegionReport>) {
    if st.stall > 0 {
        st.stall -= 1;
        out.push(RegionReport::Ticked {
            sat_out: true,
            departures: Vec::new(),
            ghosts: Vec::new(),
            stamp: region.current_tick(),
            #[cfg(not(target_arch = "wasm32"))]
            perf: game::RegionTickPerf::default(),
            #[cfg(not(target_arch = "wasm32"))]
            took: Duration::ZERO,
        });
        return;
    }
    // The slot this tick closes ends with the transfers held for it, in
    // receipt order — after every input predicted in it, the server's order
    // (its held inputs for a slot, then its held transfers).
    let now = region.current_tick();
    let mut i = 0;
    while i < st.held_transfers.len() {
        if st.held_transfers[i].1 <= now {
            let (kind, _) = st.held_transfers.remove(i).expect("index checked");
            apply_transfer(region, st, ctx, kind, out);
        } else {
            i += 1;
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let start = Instant::now();
    if let Err(e) = region.handle_event(GameEventKind::Tick) {
        log::warn!("region {:?} tick failed: {e:?}", ctx.id);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let took = start.elapsed();
    // Read right after the tick: the buffers hold this tick's scan only
    // (a reconcile replay overwrites them with stale scans, never read).
    let (departures, ghosts) = take_transfers(region, st, ctx);
    let stamp = region.current_tick();
    out.push(RegionReport::Ticked {
        sat_out: false,
        departures,
        ghosts,
        stamp,
        #[cfg(not(target_arch = "wasm32"))]
        perf: region.take_tick_perf(),
        #[cfg(not(target_arch = "wasm32"))]
        took,
    });
}

/// A transfer stamped `stamp`: applied now if its slot has already closed
/// here (this region runs ahead of the source), else held to the end of
/// that slot — released by the `Tick` that closes it.
fn transfer(
    region: &mut Region,
    st: &mut RunnerState,
    ctx: &RunnerCtx,
    kind: GameEventKind,
    stamp: game::Tick,
    out: &mut Vec<RegionReport>,
) {
    if stamp >= region.current_tick() {
        st.held_transfers.push_back((kind, stamp));
    } else {
        apply_transfer(region, st, ctx, kind, out);
    }
}

/// Predict one of this client's inputs and send it to the server, stamped
/// with the tick it was predicted after: the server applies it in that same
/// slot if it gets there first.
fn predict(region: &mut Region, ctx: &RunnerCtx, kind: GameEventKind, out: &mut Vec<RegionReport>) {
    let after_tick = region.current_tick() as u64;
    let is_input = matches!(kind, GameEventKind::PlayerInput(..));
    match region.handle_event(kind) {
        Ok(event) => {
            if is_input {
                info!("[client] predicted PlayerInput id={} in home {:?}; sending to server", event.id, ctx.id);
            }
            let _ = ctx.server_send.send(ClientPacket::GameEventAfterTick(event, after_tick));
        }
        Err(e) => out.push(RegionReport::Failed(e)),
    }
}

/// Is the local player's predicted arrival held here?
fn local_arrival_held(st: &RunnerState, client: ClientId) -> bool {
    st.held_transfers.iter().any(|(k, _)| {
        matches!(k, GameEventKind::EntityArrived(b) if b.client.as_ref().map(|(c, _)| *c) == Some(client))
    })
}

/// Predict a transfer into the region, unless it is an arrival of an entity
/// already living here: the authoritative copy got here first (this region
/// trails the server's, its source trails this one), and a second arrival
/// would never be confirmed — it would re-apply its stale bundle on every
/// replay.
///
/// After the local player's arrival, the inputs held for it are predicted.
fn apply_transfer(
    region: &mut Region,
    st: &mut RunnerState,
    ctx: &RunnerCtx,
    kind: GameEventKind,
    out: &mut Vec<RegionReport>,
) {
    let local = matches!(&kind, GameEventKind::EntityArrived(b) if b.client.as_ref().map(|(c, _)| *c) == Some(ctx.client_id));
    let resident = matches!(&kind, GameEventKind::EntityArrived(b) if region.data().entity_by_id(b.id).is_some());
    if resident {
        if let GameEventKind::EntityArrived(bundle) = &kind {
            log::debug!("[client] predicted arrival of {:?} dropped: already here", bundle.id);
        }
    } else {
        let _ = region.handle_event(kind);
    }
    if local && !local_arrival_held(st, ctx.client_id) {
        for kind in std::mem::take(&mut st.held_inputs) {
            predict(region, ctx, kind, out);
        }
    }
}

/// Empty the region's transfer buffers, noting the local player's departure.
fn take_transfers(
    region: &mut Region,
    st: &mut RunnerState,
    ctx: &RunnerCtx,
) -> (Vec<(EntityBundle, RegionCoords)>, Vec<(GhostData, RegionCoords)>) {
    let (departures, ghosts) = region.take_transfers();
    if departures.iter().any(|(b, _)| b.client.as_ref().map(|(c, _)| *c) == Some(ctx.client_id)) {
        st.player_departed = true;
    }
    (departures, ghosts)
}

/// The local body's region-local translation, if it is here.
fn body_translation(region: &Region, client: ClientId) -> Option<[f32; 3]> {
    let data = region.data();
    let key = data.player_entites.get(&client).copied()?;
    let handle = (*data.ecs.rigidbody.try_get(key))?;
    Some(data.physics.bodies.get(handle)?.translation().to_array())
}

/// The view of `client` this region holds (`None`: no `Client` entry here).
fn home_view(region: &Region, client: ClientId) -> Option<HomeView> {
    let data = region.data();
    let c = data.clients.get(&client)?;
    Some(HomeView {
        tick: region.current_tick(),
        translation: body_translation(region, client),
        fps_cam_mode: c.fps_cam_mode,
        hotbar: game::build_hotbar(data, client),
        mining: game::mining_view(c, region.current_tick() as u64),
        mode: c.mode,
        inventory_open: c.inventory_open,
        inventory: c.inventory.clone(),
        cursor: c.cursor,
        container: game::container_view(data, client),
        log_len: region.pending_event_ids().len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam::channel::{unbounded, Receiver};
    use game::InputEvent;

    const CLIENT: ClientId = 0;

    /// A client-side region (holding `CLIENT`) and a runner context for it.
    fn setup() -> (Region, RunnerState, RunnerCtx, Receiver<ClientPacket>) {
        let rc = RegionCoords::new(0, 0);
        let mut server = Region::from_chunks(rc, Vec::new());
        server.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
        server.commit_server_event();
        let region = Region::new(server.data().clone(), None, rc, Some(CLIENT));
        let (server_send, server_recv) = unbounded();
        let (client_event_send, _) = unbounded();
        let ctx = RunnerCtx {
            id: rc,
            client_id: CLIENT,
            server_send,
            client_event_send,
            debug_flags: game::DebugFlags::default(),
            self_relay: true,
        };
        (region, RunnerState::default(), ctx, server_recv)
    }

    fn run(r: &mut Region, st: &mut RunnerState, ctx: &RunnerCtx, cmd: RegionCmd) -> Vec<RegionReport> {
        let mut out = Vec::new();
        assert!(handle(r, st, ctx, cmd, &mut out));
        out
    }

    fn ticked(out: &[RegionReport]) -> Vec<bool> {
        out.iter()
            .filter_map(|r| match r {
                RegionReport::Ticked { sat_out, .. } => Some(*sat_out),
                _ => None,
            })
            .collect()
    }

    fn clock(server_tick: game::Tick, rtt_ms: u64) -> RegionCmd {
        RegionCmd::Clock(ClockInput { server_tick, server_rate_ms: 50, rtt_ms, is_home: false })
    }

    #[test]
    fn tick_reports_once_and_stall_sits_out() {
        let (mut r, mut st, ctx, _) = setup();
        for _ in 0..6 {
            run(&mut r, &mut st, &ctx, RegionCmd::Tick);
        }
        // 4 ticks ahead, rtt 25 ms: error 100 ms → sit 2 ticks out.
        let t = r.current_tick();
        let out = run(&mut r, &mut st, &ctx, clock(t - 4, 25));
        assert!(matches!(out.first(), Some(RegionReport::Clock(d)) if d.step == Some((100, 2))));
        assert_eq!(ticked(&run(&mut r, &mut st, &ctx, RegionCmd::Tick)), vec![true]);
        assert_eq!(ticked(&run(&mut r, &mut st, &ctx, RegionCmd::Tick)), vec![true]);
        assert_eq!(r.current_tick(), t);
        assert_eq!(ticked(&run(&mut r, &mut st, &ctx, RegionCmd::Tick)), vec![false]);
        assert_eq!(r.current_tick(), t + 1);
    }

    #[test]
    fn trailing_clock_runs_align_ticks() {
        let (mut r, mut st, ctx, _) = setup();
        let t = r.current_tick();
        // 10 ticks behind, rtt 0: error −575 ms → 12 ticks predicted now.
        let out = run(&mut r, &mut st, &ctx, clock(t + 10, 0));
        assert_eq!(ticked(&out), vec![false; 12]);
        assert_eq!(r.current_tick(), t + 12);
    }

    #[test]
    fn predict_stamps_and_sends() {
        let (mut r, mut st, ctx, server_recv) = setup();
        for _ in 0..5 {
            run(&mut r, &mut st, &ctx, RegionCmd::Tick);
        }
        let kind = GameEventKind::PlayerInput(CLIENT, InputEvent::MouseMotion { dx: 1.0, dy: 0.0 });
        run(&mut r, &mut st, &ctx, RegionCmd::Predict(kind.clone()));
        let predicted_id = *r.pending_event_ids().last().unwrap();
        match server_recv.try_recv().unwrap() {
            ClientPacket::GameEventAfterTick(ev, after) => {
                assert_eq!(after, 5);
                assert_eq!(ev.id, predicted_id);
                assert_eq!(ev.kind, kind);
            }
            other => panic!("expected GameEventAfterTick, got {other:?}"),
        }
    }

    #[test]
    fn input_after_departure_is_forwarded() {
        let (mut r, mut st, ctx, server_recv) = setup();
        r.handle_event(GameEventKind::ClientLeft(CLIENT)).unwrap();
        st.player_departed = true;
        let kind = GameEventKind::PlayerInput(CLIENT, InputEvent::MouseMotion { dx: 1.0, dy: 0.0 });
        let out = run(&mut r, &mut st, &ctx, RegionCmd::Predict(kind.clone()));
        assert!(out.iter().any(|o| matches!(o, RegionReport::Forward(k) if *k == kind)));
        assert!(server_recv.try_recv().is_err(), "a forwarded input is not sent from here");
    }

    /// An input for the local player whose arrival is held here waits for
    /// it: predicted (and sent, stamped with the arrival's slot) right after
    /// the arrival, where the server's region applies it too.
    #[test]
    fn local_input_waits_for_its_held_arrival() {
        let (mut r, mut st, ctx, server_recv) = setup();
        let key = *r.data().player_entites.get(&CLIENT).unwrap();
        let out_of_region = game::Pose::translation(game::REGION_SIZE + game::FLIP_HYSTERESIS + 2.0, 26.0, 128.0);
        r.with_data(|d| d.set_body_pose_safe(key, out_of_region));
        let out = run(&mut r, &mut st, &ctx, RegionCmd::Tick);
        let mut bundle = out
            .into_iter()
            .find_map(|o| match o {
                RegionReport::Ticked { departures, .. } => departures.into_iter().next(),
                _ => None,
            })
            .expect("the player departed")
            .0;
        assert!(!r.data().player_entites.contains_key(&CLIENT));
        // It comes straight back, in this slot (as if from a neighbour).
        bundle.body = game::BodyState::kinematic(game::Pose::translation(128.0, 26.0, 128.0));
        let t = r.current_tick();
        run(&mut r, &mut st, &ctx, RegionCmd::Arrival(bundle, t));
        assert_eq!(st.held_transfers.len(), 1, "the arrival is held to the end of slot {t}");
        let n = r.pending_event_ids().len();
        let input = GameEventKind::PlayerInput(CLIENT, InputEvent::MouseMotion { dx: 1.0, dy: 0.0 });
        let out = run(&mut r, &mut st, &ctx, RegionCmd::Predict(input.clone()));
        assert!(!out.iter().any(|o| matches!(o, RegionReport::Forward(_))), "{out:?}");
        assert_eq!(r.pending_event_ids().len(), n, "held, not predicted");
        assert!(server_recv.try_recv().is_err(), "held, not sent");

        run(&mut r, &mut st, &ctx, RegionCmd::Tick);
        assert!(st.held_inputs.is_empty());
        let ids = r.pending_event_ids();
        assert_eq!(ids.len(), n + 3, "arrival, input, tick");
        match server_recv.try_recv().unwrap() {
            ClientPacket::GameEventAfterTick(ev, after) => {
                assert_eq!(ev.kind, input);
                assert_eq!(after, t as u64, "stamped with the arrival's slot");
                assert_eq!(ev.id, ids[n] + 1, "right after the arrival");
            }
            other => panic!("expected GameEventAfterTick, got {other:?}"),
        }
    }

    #[test]
    fn view_changes_only_when_state_changes() {
        let (mut r, mut st, ctx, _) = setup();
        let out = run(&mut r, &mut st, &ctx, RegionCmd::Tick);
        assert!(out.iter().any(|o| matches!(o, RegionReport::View(Some(_)))));
        for n in 0..2 {
            let out = run(&mut r, &mut st, &ctx, RegionCmd::Ping(n));
            assert!(!out.iter().any(|o| matches!(o, RegionReport::View(_))), "{out:?}");
            assert!(out.iter().any(|o| matches!(o, RegionReport::Pong(p) if *p == n)));
        }
    }

    /// A region event error reaches the manager as `Failed` (it ends the
    /// session there, as `?` did). `Quit` is the only kind `handle_event`
    /// refuses; a reconcile that replays one panics inside the rollback
    /// layer first, so the error path is exercised through `Predict`.
    #[test]
    fn failed_event_reports_error() {
        let (mut r, mut st, ctx, server_recv) = setup();
        let out = run(&mut r, &mut st, &ctx, RegionCmd::Predict(GameEventKind::Quit));
        assert!(out.iter().any(|o| matches!(o, RegionReport::Failed(GameError::QuitRequested))), "{out:?}");
        assert!(server_recv.try_recv().is_err(), "a failed prediction is not sent");
    }

    #[test]
    fn state_hash_reports_tick_and_hash() {
        let (mut r, mut st, ctx, _) = setup();
        run(&mut r, &mut st, &ctx, RegionCmd::Tick);
        let out = run(&mut r, &mut st, &ctx, RegionCmd::StateHash);
        let want = (r.current_tick(), game::state_hash(r.data()));
        assert!(
            out.iter().any(|o| matches!(o, RegionReport::StateHash { tick, hash } if (*tick, *hash) == want)),
            "{out:?}"
        );
    }

    fn ghost(stamp: game::Tick) -> RegionCmd {
        RegionCmd::Ghost(
            GhostData {
                id: game::EntityId { birth_region: RegionCoords::new(-1, 0), serial: 0 },
                source_region: RegionCoords::new(-1, 0),
                kind: game::EntityKind::Player,
                isometry: game::Pose::translation(1.0, 26.0, 128.0),
                linvel: game::parry::math::Vector::ZERO,
                collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
            },
            stamp,
        )
    }

    /// A transfer is held to the end of its slot: released by the `Tick`
    /// that closes the slot, after the inputs predicted in it (the server's
    /// order); one whose slot has already closed lands at once.
    #[test]
    fn transfers_land_at_the_end_of_their_slot() {
        let (mut r, mut st, ctx, server_recv) = setup();
        let t = r.current_tick();
        let logged = |r: &Region| r.pending_event_ids();
        let n = logged(&r).len();
        run(&mut r, &mut st, &ctx, ghost(t + 1));
        assert_eq!(logged(&r).len(), n, "held, not applied");
        run(&mut r, &mut st, &ctx, RegionCmd::Tick);
        assert_eq!(logged(&r).len(), n + 1, "tick into slot t+1: still held");
        let input = GameEventKind::PlayerInput(CLIENT, InputEvent::MouseMotion { dx: 1.0, dy: 0.0 });
        run(&mut r, &mut st, &ctx, RegionCmd::Predict(input));
        let input_id = match server_recv.try_recv().unwrap() {
            ClientPacket::GameEventAfterTick(ev, _) => ev.id,
            other => panic!("expected GameEventAfterTick, got {other:?}"),
        };
        assert_eq!(logged(&r).len(), n + 2, "slot t+1 still open: still held");
        run(&mut r, &mut st, &ctx, RegionCmd::Tick);
        let ids = logged(&r);
        assert_eq!(ids.len(), n + 4, "the ghost, then the tick closing slot t+1");
        assert_eq!(ids[n + 2], input_id + 1, "the ghost follows the slot's input");
        assert!(st.held_transfers.is_empty());
        run(&mut r, &mut st, &ctx, ghost(t + 1));
        assert_eq!(logged(&r).len(), n + 5, "slot already closed: applied on receipt");
    }

    /// A sat-out tick closes no slot: the transfer stays held.
    #[test]
    fn stalled_tick_keeps_transfers_held() {
        let (mut r, mut st, ctx, _) = setup();
        let t = r.current_tick();
        run(&mut r, &mut st, &ctx, ghost(t));
        st.stall = 1;
        run(&mut r, &mut st, &ctx, RegionCmd::Tick);
        assert_eq!(st.held_transfers.len(), 1);
        let out = run(&mut r, &mut st, &ctx, RegionCmd::Tick);
        assert!(st.held_transfers.is_empty());
        assert!(
            out.iter().any(|o| matches!(o, RegionReport::Ticked { sat_out: false, stamp, .. } if *stamp == t + 1)),
            "{out:?}"
        );
    }

    /// A predicted arrival of an entity already living in the region (the
    /// authoritative copy got there first) is dropped, never logged.
    #[test]
    fn arrival_of_a_resident_is_dropped() {
        let (mut r, mut st, ctx, _) = setup();
        let key = *r.data().player_entites.get(&CLIENT).unwrap();
        let id = r.data().entity_id(key).expect("players carry an EntityId");
        let bundle = EntityBundle {
            id,
            kind: game::EntityKind::Player,
            body: game::BodyState::kinematic(game::Pose::translation(1.0, 26.0, 128.0)),
            collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
            carried: game::Carried::default(),
            client: None,
            pending_inputs: Vec::new(),
        };
        let t = r.current_tick();
        run(&mut r, &mut st, &ctx, RegionCmd::Tick);
        let n = r.pending_event_ids().len();
        // Slot t is closed: applied (here: dropped) on receipt.
        run(&mut r, &mut st, &ctx, RegionCmd::Arrival(bundle, t));
        assert!(st.held_transfers.is_empty());
        assert_eq!(r.pending_event_ids().len(), n);
    }

    #[test]
    fn stop_returns_false() {
        let (mut r, mut st, ctx, _) = setup();
        let mut out = Vec::new();
        assert!(!handle(&mut r, &mut st, &ctx, RegionCmd::Stop, &mut out));
    }
}
