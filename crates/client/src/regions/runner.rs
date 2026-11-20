//! Everything the client does to ONE predicted region, as a command in and
//! reports out. The manager never touches a `Region` itself: the inline host
//! calls [`handle`] on the caller's thread, the threaded host on the region's
//! own thread, and both see the same reports in the same order.
use crossbeam::channel::Sender;
use game::{
    ClientId, ClientPacket, ClientUpdateEvent, EntityBundle, GameError, GameEvent, GameEventKind,
    HaloEntityMsg, ReconcileStats, Region, RegionCoords, RegionId, SlotInbox,
};
use log::info;
use std::time::Duration;

use super::clock::ticks_due;
use crate::perf_clock::MonotonicInstant;

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
    /// Wireframe the colliders near the viewer at this position of this
    /// (home) region's frame.
    DebugLines(game::RegionCoords, [f32; 3]),
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
        /// are not in it). The F3 debug pane's per-region tick times.
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
    StateHash {
        tick: game::Tick,
        hash: u32,
    },
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
    /// Every block a client of this region is mining or left damaged (the
    /// renderer's cracks and wobble).
    pub damage: Vec<game::BlockDamageView>,
    /// Per damaged tree block (the lead): the blocks a chop there brings
    /// down (`Rollback::shake_blocks`), shaken with it. Shared with
    /// [`RunnerState`]'s cache, so comparing two views is a pointer check.
    pub shake: Vec<(game::edit::BlockPos, ShakeSet)>,
    pub mode: game::PlayMode,
    pub editor: bool,
    pub editor_allowed: bool,
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

/// The blocks that shake with one chopped block.
pub type ShakeSet = std::sync::Arc<Vec<(game::edit::BlockPos, game::Block)>>;

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
    /// arrival was buffered here (`push_after_arrival`): predicted directly
    /// after that arrival, in order, before the slot's other messages — where
    /// the server's region applies them too (they follow the body, as the
    /// inputs a crossing bundle carries do).
    inbox: SlotInbox,
    collider_debug: game::ColliderDebug,
    last_view: Option<HomeView>,
    /// The shake set of each damaged tree block, by (lead, its block): a
    /// whole-tree flood, so computed once per lead and kept while it is
    /// listed. A tree that changes meanwhile shakes by the old set (what
    /// goes when it is felled is the sim's `TreeFelled`, not this).
    shake_cache: Vec<((game::edit::BlockPos, game::Block), ShakeSet)>,
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
            let start = std::time::Instant::now();
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
            let arrives = if has_body {
                None
            } else {
                local_arrival_held(st, ctx.client_id)
            };
            if let Some((stamp, id)) = arrives {
                st.inbox.push_after_arrival(kind, stamp, id);
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
        RegionCmd::DebugLines(home, at) => {
            if ctx.debug_flags.collider_wireframes() {
                // The home's origin in this region's frame, by the nearest image.
                let (dx, dz) = game::region_delta(region.map_extent(), ctx.id, home);
                let viewer_local = [
                    at[0] + dx as f32 * game::REGION_SIZE,
                    at[1],
                    at[2] + dz as f32 * game::REGION_SIZE,
                ];
                let data = region.data();
                let lines = st.collider_debug.collect(
                    &*data.physics.bodies,
                    &*data.physics.colliders,
                    viewer_local,
                );
                let _ = ctx
                    .client_event_send
                    .send(ClientUpdateEvent::DebugLines(ctx.id, lines));
            }
        }
        RegionCmd::Ping(n) => out.push(RegionReport::Pong(n)),
        RegionCmd::StateHash => out.push(RegionReport::StateHash {
            tick: region.current_tick(),
            hash: game::state_hash(region.data()),
        }),
    }
    let view = home_view(region, ctx.client_id, &mut st.shake_cache);
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
        took: Duration::ZERO,
    });
}

/// One region tick.
fn tick(region: &mut Region, st: &mut RunnerState, ctx: &RunnerCtx, out: &mut Vec<RegionReport>) {
    // The slot this tick closes ends with its neighbour messages (and any
    // late ones), in canonical order — after every input predicted in it, as
    // on the server. A client input among them is this client's, queued
    // directly behind the buffered arrival of its body: predicted (and sent).
    let now = region.current_tick();
    for kind in st.inbox.drain_due(now) {
        if let Some(client) = kind.input_client() {
            debug_assert_eq!(
                client, ctx.client_id,
                "only the local client's inputs are queued here"
            );
            predict(region, ctx, kind, out);
        } else {
            apply_transfer(region, kind);
        }
    }
    let start = MonotonicInstant::now();
    if let Err(e) = region.handle_event(GameEventKind::Tick) {
        log::warn!("region {:?} tick failed: {e:?}", ctx.id);
    }
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
                info!(
                    "[client] predicted PlayerInput id={} in home {:?}; sending to server",
                    event.id, ctx.id
                );
            }
            let packet = ClientPacket::GameEventAfterTick(event, after_tick);
            let _ = ctx
                .server_send
                .send(crate::connections::Outgoing::Home(packet));
        }
        Err(e) => out.push(RegionReport::Failed(e)),
    }
}

/// The stamp and entity of the local player's predicted arrival, if it is
/// buffered here.
fn local_arrival_held(st: &RunnerState, client: ClientId) -> Option<(game::Tick, game::EntityId)> {
    let (stamp, kind) = st
        .inbox
        .holds_entry(|k| matches!(k, GameEventKind::EntityArrived(b) if b.client.as_ref().map(|(c, _)| *c) == Some(client)))?;
    match kind {
        GameEventKind::EntityArrived(b) => Some((stamp, b.id)),
        _ => None,
    }
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
            log::debug!(
                "[client] predicted arrival of {:?} dropped: already here",
                bundle.id
            );
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
) -> (
    Vec<(EntityBundle, RegionCoords)>,
    Vec<(HaloEntityMsg, RegionCoords)>,
) {
    let (departures, halo_entities) = region.take_transfers();
    if departures
        .iter()
        .any(|(b, _)| b.client.as_ref().map(|(c, _)| *c) == Some(ctx.client_id))
    {
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
fn home_view(
    region: &Region,
    client: ClientId,
    shake_cache: &mut Vec<((game::edit::BlockPos, game::Block), ShakeSet)>,
) -> Option<HomeView> {
    let data = region.data();
    let c = data.clients.get(&client)?;
    let damage = data.block_damage_views(region.current_tick() as u64);
    shake_cache.retain(|(k, _)| damage.iter().any(|v| (v.target, v.block) == *k));
    let mut shake = Vec::new();
    for v in damage.iter().filter(|v| v.block.tree_id() != 0) {
        let key = (v.target, v.block);
        let set = match shake_cache.iter().find(|(k, _)| *k == key) {
            Some((_, set)) => set.clone(),
            None => {
                let set: ShakeSet = std::sync::Arc::new(data.shake_blocks(v.target));
                shake_cache.push((key, set.clone()));
                set
            }
        };
        if !set.is_empty() {
            shake.push((v.target, set));
        }
    }
    Some(HomeView {
        tick: region.current_tick(),
        translation: body_translation(region, client),
        fps_cam_mode: c.fps_cam_mode,
        hotbar: game::build_hotbar(data, client),
        mining: game::mining_view(c, region.current_tick() as u64),
        damage,
        shake,
        mode: c.mode,
        editor: c.editor,
        editor_allowed: c.editor_allowed,
        inventory_open: c.inventory_open,
        inventory: c.inventory.clone(),
        cursor: c.cursor,
        container: game::container_view(data, client),
        log_len: region.pending_event_ids().len(),
    })
}
