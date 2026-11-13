//! Everything the client does to ONE predicted region, as a command in and
//! reports out. The manager never touches a `Region` itself: the inline host
//! calls [`handle`] on the caller's thread, the threaded host on the region's
//! own thread, and both see the same reports in the same order.
use crossbeam::channel::Sender;
use game::{
    ClientId, ClientPacket, ClientUpdateEvent, EntityBundle, GameError, GameEvent, GameEventKind,
    HaloEntityMsg, ReconcileStats, Region, RegionCoords, RegionId, SlotClass, SlotInbox,
};
use log::info;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

use super::clock::ticks_due;

/// One instruction for one region.
#[derive(Debug)]
pub enum RegionCmd {
    /// One tick of the client's world clock: run to this predicted world
    /// tick (`regions::clock`) — one tick in step, a bounded burst while
    /// behind (a fresh snapshot), none while ahead (sat out).
    Tick(game::Tick),
    /// A server-authoritative event to reconcile, with the server's
    /// `(tick, state hash)` when it came as a `HashedGameEvent`.
    ServerEvent(GameEvent, Option<(game::Tick, u32)>),
    /// A PlayerInput / UiAction / Order from this client: predicted here and
    /// sent to the server stamped with the tick it was predicted after.
    Predict(GameEventKind),
    /// A predicted arrival from a sibling region (isometry already rebased),
    /// stamped with the source's tick after the tick that produced it:
    /// buffered to the end of that slot here, the server's slot and order (see
    /// [`RunnerState::inbox`]).
    Arrival(EntityBundle, game::Tick),
    /// A predicted entity-halo message from a sibling region (ditto, same
    /// stamp): an update of its mirror here, or its explicit end.
    HaloEntity(HaloEntityMsg, game::Tick),
    /// A predicted terrain halo update from a sibling region (ditto, same
    /// stamp), like the server's `RegionInput::Transfer(HaloUpdate)`.
    Halo(game::HaloUpdate, game::Tick),
    /// A predicted intent or reply from a sibling region (`game::intents`),
    /// stamped with its due tick: buffered to the end of that slot like the
    /// server's `RegionInput::Transfer(Intent)`, then queued for the opening
    /// of the tick.
    Intent(game::IntentMsg),
    /// The world's extent and coarse-route map (`Region::set_route_map`).
    SetRouteMap(Option<std::sync::Arc<game::RouteMap>>),
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
    /// One tick (one of a `Tick`'s burst), or a sat-out `Tick`: this tick's
    /// boundary crossings and entity-halo messages, targets absolute,
    /// isometries still region-local.
    /// Every host relays them per report, so both see one order.
    Ticked {
        sat_out: bool,
        departures: Vec<(EntityBundle, RegionCoords)>,
        halo_entities: Vec<(HaloEntityMsg, RegionCoords)>,
        /// Terrain halo updates for the neighbours (`game::halo`): band
        /// chunks this tick changed. Targets absolute.
        halo: Vec<(game::HaloUpdate, RegionCoords)>,
        /// Intents and replies sent to other regions since the last tick
        /// (`game::intents`), each stamped with its own due tick.
        intents: Vec<game::IntentMsg>,
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
    /// This region's view of the local client; `None` = it no longer holds it.
    View(Option<HomeView>),
    /// A `Predict` that reached this region after the local player's body
    /// left it — on a region thread, the game thread may send one before it
    /// sees the departure. Re-sent to the new home (where it queues behind
    /// the arrival if that is still buffered), as the server's region sends one
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
    /// Where its predictions go: the session's home connection (`Outgoing::Home`).
    pub server_send: Sender<crate::connections::Outgoing>,
    pub client_event_send: Sender<ClientUpdateEvent>,
    pub debug_flags: game::DebugFlags,
}

/// A region's runner-side state (the region itself is held beside it).
#[derive(Default)]
pub struct RunnerState {
    /// The local player left this region in a predicted crossing.
    player_departed: bool,
    /// Every predicted message from a sibling region (arrival, entity halo,
    /// terrain halo, intent or reply), never applied on receipt: right
    /// before the `Tick` that closes the slot `now`, every one stamped
    /// `<= now` is applied in `SlotInbox`'s canonical order — the server's
    /// `RegionRunner` order, whatever order the host routed them in (thread
    /// timing on the threaded host). So a slot is: `Tick`, this client's
    /// inputs, the slot's neighbour messages sorted, the next `Tick`. A
    /// sat-out tick closes nothing and applies nothing.
    ///
    /// Also this client's inputs that reached the region while its body's
    /// arrival was buffered here (`push_behind`, class `Other`, the arrival's
    /// stamp): predicted after every message of that slot, in order — where
    /// the server's region applies them too (they follow the body).
    inbox: SlotInbox,
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
            st.inbox = SlotInbox::new();
            return false;
        }
        RegionCmd::Tick(target) => {
            let due = ticks_due(region.current_tick(), target);
            if due == 0 {
                sat_out(region, out);
            }
            for _ in 0..due {
                tick(region, st, ctx, out);
            }
        }
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
            let arrives = if has_body { None } else { local_arrival_held(st, ctx.client_id) };
            if let Some(arrives) = arrives {
                st.inbox.push_behind(kind, arrives);
            } else if st.player_departed && !has_body {
                out.push(RegionReport::Forward(kind));
            } else {
                predict(region, ctx, kind, out);
            }
        }
        RegionCmd::Arrival(bundle, stamp) => {
            // On receipt, not when applied: input for the returning player
            // queues behind it here from now on, never sent back.
            if bundle.client.as_ref().map(|(c, _)| *c) == Some(ctx.client_id) {
                st.player_departed = false;
            }
            transfer(st, GameEventKind::EntityArrived(bundle), stamp);
        }
        RegionCmd::HaloEntity(msg, stamp) => transfer(st, msg.into_event(), stamp),
        RegionCmd::Halo(update, stamp) => transfer(st, GameEventKind::HaloUpdate(update), stamp),
        RegionCmd::Intent(msg) => transfer(st, GameEventKind::Intent(msg), msg.due()),
        RegionCmd::SetRouteMap(route) => region.set_route_map(route),
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

/// A `Tick` that found the region at (or past) the predicted world tick:
/// nothing runs, and no slot closes (the inbox keeps its messages).
fn sat_out(region: &Region, out: &mut Vec<RegionReport>) {
    out.push(RegionReport::Ticked {
        sat_out: true,
        departures: Vec::new(),
        halo_entities: Vec::new(),
        halo: Vec::new(),
        intents: Vec::new(),
        stamp: region.current_tick(),
        #[cfg(not(target_arch = "wasm32"))]
        perf: game::RegionTickPerf::default(),
        #[cfg(not(target_arch = "wasm32"))]
        took: Duration::ZERO,
    });
}

/// One region tick.
fn tick(region: &mut Region, st: &mut RunnerState, ctx: &RunnerCtx, out: &mut Vec<RegionReport>) {
    // The slot this tick closes ends with its neighbour messages (and any
    // late ones), in canonical order — after every input predicted in it, as
    // on the server. Class `Other` entries are this client's inputs queued
    // behind a buffered arrival of its body: no message pushed by `transfer`
    // is of that class.
    let now = region.current_tick();
    for kind in st.inbox.drain_due(now) {
        if game::slot_class(&kind) == SlotClass::Other {
            predict(region, ctx, kind, out);
        } else {
            apply_transfer(region, kind);
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
    let (departures, halo_entities) = take_transfers(region, st, ctx);
    // Band chunks written since the last tick (this tick's edits, and any a
    // predicted event made in the slot before it).
    let halo = region.take_halo_updates();
    // Sent by this tick and the events predicted before it.
    let intents = region.take_intents();
    let stamp = region.current_tick();
    out.push(RegionReport::Ticked {
        sat_out: false,
        departures,
        halo_entities,
        halo,
        intents,
        stamp,
        #[cfg(not(target_arch = "wasm32"))]
        perf: region.take_tick_perf(),
        #[cfg(not(target_arch = "wasm32"))]
        took,
    });
}

/// A message from a sibling region stamped `stamp`: always buffered, applied
/// right before the `Tick` that closes slot `stamp` — or, late (its slot has
/// closed here), the slot that is open — in canonical order.
fn transfer(st: &mut RunnerState, kind: GameEventKind, stamp: game::Tick) {
    st.inbox.push(kind, stamp);
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
            let packet = ClientPacket::GameEventAfterTick(event, after_tick);
            let _ = ctx.server_send.send(crate::connections::Outgoing::Home(packet));
        }
        Err(e) => out.push(RegionReport::Failed(e)),
    }
}

/// The stamp of the local player's predicted arrival, if it is buffered here.
fn local_arrival_held(st: &RunnerState, client: ClientId) -> Option<game::Tick> {
    st.inbox
        .holds(|k| matches!(k, GameEventKind::EntityArrived(b) if b.client.as_ref().map(|(c, _)| *c) == Some(client)))
}

/// Predict a transfer into the region, unless it is an arrival of an entity
/// already living here: the authoritative copy got here first (this region
/// trails the server's, its source trails this one), and a second arrival
/// would never be confirmed — it would re-apply its stale bundle on every
/// replay.
fn apply_transfer(region: &mut Region, kind: GameEventKind) {
    let resident = matches!(&kind, GameEventKind::EntityArrived(b) if region.data().entity_by_id(b.id).is_some());
    if resident {
        if let GameEventKind::EntityArrived(bundle) = &kind {
            log::debug!("[client] predicted arrival of {:?} dropped: already here", bundle.id);
        }
    } else {
        let _ = region.handle_event(kind);
    }
}

/// Empty the region's transfer buffers, noting the local player's departure.
fn take_transfers(
    region: &mut Region,
    st: &mut RunnerState,
    ctx: &RunnerCtx,
) -> (Vec<(EntityBundle, RegionCoords)>, Vec<(HaloEntityMsg, RegionCoords)>) {
    let (departures, halo_entities) = region.take_transfers();
    if departures.iter().any(|(b, _)| b.client.as_ref().map(|(c, _)| *c) == Some(ctx.client_id)) {
        st.player_departed = true;
    }
    (departures, halo_entities)
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
        mining: data.mining_view(client, region.current_tick() as u64),
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
    fn setup() -> (Region, RunnerState, RunnerCtx, Receiver<crate::connections::Outgoing>) {
        let rc = RegionCoords::new(0, 0);
        let mut server = Region::empty(rc);
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

    /// One tick of the clock with the region in step: it runs one tick.
    fn tick1(r: &mut Region, st: &mut RunnerState, ctx: &RunnerCtx) -> Vec<RegionReport> {
        let target = r.current_tick() + 1;
        run(r, st, ctx, RegionCmd::Tick(target))
    }

    #[test]
    fn tick_reports_once_and_ahead_sits_out() {
        let (mut r, mut st, ctx, _) = setup();
        for _ in 0..6 {
            tick1(&mut r, &mut st, &ctx);
        }
        let t = r.current_tick();
        // At or past the predicted tick: held.
        assert_eq!(ticked(&run(&mut r, &mut st, &ctx, RegionCmd::Tick(t))), vec![true]);
        assert_eq!(ticked(&run(&mut r, &mut st, &ctx, RegionCmd::Tick(t - 2))), vec![true]);
        assert_eq!(r.current_tick(), t);
        assert_eq!(ticked(&run(&mut r, &mut st, &ctx, RegionCmd::Tick(t + 1))), vec![false]);
        assert_eq!(r.current_tick(), t + 1);
    }

    /// Behind the predicted tick (a fresh snapshot): bursts of at most
    /// `MAX_CATCH_UP_BURST` ticks, each reported, never past it.
    #[test]
    fn behind_runs_bounded_bursts() {
        let (mut r, mut st, ctx, _) = setup();
        let t = r.current_tick();
        let burst = game::MAX_CATCH_UP_BURST as usize;
        let out = run(&mut r, &mut st, &ctx, RegionCmd::Tick(t + 7));
        assert_eq!(ticked(&out), vec![false; burst]);
        assert_eq!(r.current_tick(), t + burst as game::Tick);
        let out = run(&mut r, &mut st, &ctx, RegionCmd::Tick(t + 7));
        assert_eq!(ticked(&out), vec![false; 7 - burst]);
        assert_eq!(r.current_tick(), t + 7);
    }

    #[test]
    fn predict_stamps_and_sends() {
        let (mut r, mut st, ctx, server_recv) = setup();
        for _ in 0..5 {
            tick1(&mut r, &mut st, &ctx);
        }
        let kind = GameEventKind::PlayerInput(CLIENT, InputEvent::MouseMotion { dx: 1.0, dy: 0.0 });
        run(&mut r, &mut st, &ctx, RegionCmd::Predict(kind.clone()));
        let predicted_id = *r.pending_event_ids().last().unwrap();
        match server_recv.try_recv().unwrap() {
            crate::connections::Outgoing::Home(ClientPacket::GameEventAfterTick(ev, after)) => {
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

    /// The local player leaves the region in one tick: its departing bundle,
    /// set back inside (to come straight back, as if from a neighbour).
    fn departed_and_back(r: &mut Region, st: &mut RunnerState, ctx: &RunnerCtx) -> EntityBundle {
        let key = *r.data().player_entites.get(&CLIENT).unwrap();
        let out_of_region = game::Pose::translation(game::REGION_SIZE + game::FLIP_HYSTERESIS + 2.0, 26.0, 128.0);
        r.with_data(|d| d.set_body_pose_safe(key, out_of_region));
        let out = tick1(r, st, ctx);
        let mut bundle = out
            .into_iter()
            .find_map(|o| match o {
                RegionReport::Ticked { departures, .. } => departures.into_iter().next(),
                _ => None,
            })
            .expect("the player departed")
            .0;
        assert!(!r.data().player_entites.contains_key(&CLIENT));
        bundle.body = game::BodyState::kinematic(game::Pose::translation(128.0, 26.0, 128.0));
        bundle
    }

    /// An input for the local player whose arrival is buffered here waits
    /// for it: predicted (and sent, stamped with the arrival's slot) after
    /// the arrival, where the server's region applies it too.
    #[test]
    fn local_input_waits_for_its_held_arrival() {
        let (mut r, mut st, ctx, server_recv) = setup();
        let bundle = departed_and_back(&mut r, &mut st, &ctx);
        // It comes straight back, in this slot.
        let t = r.current_tick();
        run(&mut r, &mut st, &ctx, RegionCmd::Arrival(bundle, t));
        assert_eq!(st.inbox.len(), 1, "the arrival is buffered to the end of slot {t}");
        let n = r.pending_event_ids().len();
        let input = GameEventKind::PlayerInput(CLIENT, InputEvent::MouseMotion { dx: 1.0, dy: 0.0 });
        let out = run(&mut r, &mut st, &ctx, RegionCmd::Predict(input.clone()));
        assert!(!out.iter().any(|o| matches!(o, RegionReport::Forward(_))), "{out:?}");
        assert_eq!(r.pending_event_ids().len(), n, "held, not predicted");
        assert!(server_recv.try_recv().is_err(), "held, not sent");

        tick1(&mut r, &mut st, &ctx);
        assert!(st.inbox.is_empty());
        let ids = r.pending_event_ids();
        assert_eq!(ids.len(), n + 3, "arrival, input, tick");
        match server_recv.try_recv().unwrap() {
            crate::connections::Outgoing::Home(ClientPacket::GameEventAfterTick(ev, after)) => {
                assert_eq!(ev.kind, input);
                assert_eq!(after, t as u64, "stamped with the arrival's slot");
                assert_eq!(ev.id, ids[n] + 1, "right after the arrival");
            }
            other => panic!("expected GameEventAfterTick, got {other:?}"),
        }
    }

    /// An input queued behind the local player's buffered arrival follows
    /// every message of the arrival's slot, not just the arrival — as the
    /// server's region queues it (`push_behind`, the client twin of its
    /// `behind_held_body`).
    #[test]
    fn local_input_follows_every_message_of_its_arrivals_slot() {
        let (mut r, mut st, ctx, server_recv) = setup();
        let bundle = departed_and_back(&mut r, &mut st, &ctx);
        let t = r.current_tick();
        let n = r.pending_event_ids().len();
        run(&mut r, &mut st, &ctx, RegionCmd::Arrival(bundle, t));
        let input = GameEventKind::PlayerInput(CLIENT, InputEvent::MouseMotion { dx: 1.0, dy: 0.0 });
        run(&mut r, &mut st, &ctx, RegionCmd::Predict(input.clone()));
        // A neighbour's mirror of the same slot, routed after the input.
        run(&mut r, &mut st, &ctx, ghost(t));
        assert!(server_recv.try_recv().is_err(), "held, not sent");
        tick1(&mut r, &mut st, &ctx);
        let kinds: Vec<&GameEventKind> = r.pending_events().skip(n).map(|e| &e.kind).collect();
        assert!(
            matches!(
                kinds.as_slice(),
                [GameEventKind::EntityArrived(_), GameEventKind::HaloEntity(_), GameEventKind::PlayerInput(..), GameEventKind::Tick]
            ),
            "arrival, mirror, input, tick: {kinds:?}"
        );
        match server_recv.try_recv().unwrap() {
            crate::connections::Outgoing::Home(ClientPacket::GameEventAfterTick(ev, after)) => {
                assert_eq!(ev.kind, input);
                assert_eq!(after, t as u64, "stamped with the arrival's slot");
            }
            other => panic!("expected GameEventAfterTick, got {other:?}"),
        }
    }

    #[test]
    fn view_changes_only_when_state_changes() {
        let (mut r, mut st, ctx, _) = setup();
        let out = tick1(&mut r, &mut st, &ctx);
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
        tick1(&mut r, &mut st, &ctx);
        let out = run(&mut r, &mut st, &ctx, RegionCmd::StateHash);
        let want = (r.current_tick(), game::state_hash(r.data()));
        assert!(
            out.iter().any(|o| matches!(o, RegionReport::StateHash { tick, hash } if (*tick, *hash) == want)),
            "{out:?}"
        );
    }

    fn ghost(stamp: game::Tick) -> RegionCmd {
        ghost_from(RegionCoords::new(-1, 0), 0, stamp)
    }

    /// A mirror update from `source` of its entity `serial`.
    fn ghost_from(source: RegionCoords, serial: u64, stamp: game::Tick) -> RegionCmd {
        RegionCmd::HaloEntity(
            HaloEntityMsg::Update(game::HaloEntityDelta {
                id: game::EntityId { birth_region: source, serial },
                source_region: source,
                kind: game::EntityKind::Player,
                isometry: game::Pose::translation(1.0, 26.0, 128.0),
                linvel: game::parry::math::Vector::ZERO,
                collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
            }),
            stamp,
        )
    }

    /// A transfer is buffered to the end of its slot: applied right before
    /// the `Tick` that closes the slot, after the inputs predicted in it (the
    /// server's order); one whose slot has already closed is not applied on
    /// receipt either, but at the end of the slot that is open.
    #[test]
    fn transfers_land_at_the_end_of_their_slot() {
        let (mut r, mut st, ctx, server_recv) = setup();
        let t = r.current_tick();
        let logged = |r: &Region| r.pending_event_ids();
        let n = logged(&r).len();
        run(&mut r, &mut st, &ctx, ghost(t + 1));
        assert_eq!(logged(&r).len(), n, "held, not applied");
        tick1(&mut r, &mut st, &ctx);
        assert_eq!(logged(&r).len(), n + 1, "tick into slot t+1: still held");
        let input = GameEventKind::PlayerInput(CLIENT, InputEvent::MouseMotion { dx: 1.0, dy: 0.0 });
        run(&mut r, &mut st, &ctx, RegionCmd::Predict(input));
        let input_id = match server_recv.try_recv().unwrap() {
            crate::connections::Outgoing::Home(ClientPacket::GameEventAfterTick(ev, _)) => ev.id,
            other => panic!("expected GameEventAfterTick, got {other:?}"),
        };
        assert_eq!(logged(&r).len(), n + 2, "slot t+1 still open: still held");
        tick1(&mut r, &mut st, &ctx);
        let ids = logged(&r);
        assert_eq!(ids.len(), n + 4, "the mirror, then the tick closing slot t+1");
        assert_eq!(ids[n + 2], input_id + 1, "the mirror follows the slot's input");
        assert!(st.inbox.is_empty());
        run(&mut r, &mut st, &ctx, ghost(t + 1));
        assert_eq!(logged(&r).len(), n + 4, "slot already closed: buffered all the same");
        tick1(&mut r, &mut st, &ctx);
        assert_eq!(logged(&r).len(), n + 6, "the late mirror, then the tick closing the open slot");
        assert!(st.inbox.is_empty());
    }

    /// Two neighbours' messages of one slot apply in the server's canonical
    /// order (`game::SlotInbox`) whatever order the host routed them in: the
    /// region's events after the tick that closes the slot are the same.
    #[test]
    fn routed_messages_apply_in_canonical_order_whatever_the_report_order() {
        let events = |west_first: bool| {
            let (mut r, mut st, ctx, _) = setup();
            let t = r.current_tick();
            let west = ghost_from(RegionCoords::new(-1, 0), 1, t);
            let north = ghost_from(RegionCoords::new(0, -1), 2, t);
            for cmd in if west_first { [west, north] } else { [north, west] } {
                run(&mut r, &mut st, &ctx, cmd);
            }
            tick1(&mut r, &mut st, &ctx);
            r.pending_events().map(|e| (e.id, e.kind.clone())).collect::<Vec<_>>()
        };
        let (one, two) = (events(true), events(false));
        assert_eq!(one.iter().filter(|(_, k)| matches!(k, GameEventKind::HaloEntity(_))).count(), 2, "{one:?}");
        assert_eq!(one, two);
    }

    /// A sat-out tick closes no slot: the transfer stays held.
    #[test]
    fn stalled_tick_keeps_transfers_held() {
        let (mut r, mut st, ctx, _) = setup();
        let t = r.current_tick();
        run(&mut r, &mut st, &ctx, ghost(t));
        run(&mut r, &mut st, &ctx, RegionCmd::Tick(t));
        assert_eq!(st.inbox.len(), 1);
        let out = tick1(&mut r, &mut st, &ctx);
        assert!(st.inbox.is_empty());
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
        tick1(&mut r, &mut st, &ctx);
        let n = r.pending_event_ids().len();
        // Slot t is closed: buffered, then dropped at the end of the open slot.
        run(&mut r, &mut st, &ctx, RegionCmd::Arrival(bundle, t));
        assert_eq!(st.inbox.len(), 1);
        tick1(&mut r, &mut st, &ctx);
        assert!(st.inbox.is_empty());
        assert_eq!(r.pending_event_ids().len(), n + 1, "only the tick is logged");
    }

    #[test]
    fn stop_returns_false() {
        let (mut r, mut st, ctx, _) = setup();
        let mut out = Vec::new();
        assert!(!handle(&mut r, &mut st, &ctx, RegionCmd::Stop, &mut out));
    }
}
