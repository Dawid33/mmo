//! Login placement across hosts: the login service pushes its spawn orders and kicks to the
//! world coordinator, which hands each to ONE live host, the *gate host*
//! ([`crate::Coordinator::gate_host`], [`crate::CoordMsg::Login`]); the server resolves it
//! there ([`WorldManager::take_login_orders`]) and the managers relay it to the host that
//! holds the player, over the same reliable peer streams that carry hand-offs and parks — so
//! an order relayed by the host a player left arrives behind the hand-off that took it away.
//! Every answer goes up its producer's own coordinator link ([`crate::HostMsg::Login`],
//! [`WorldManager::send_login_out`]); the coordinator forwards it to the login service and
//! keeps nothing of it. (Role `all`, one process: its server answers the login service itself.)
//!
//! - **Spawn**: [`WorldManager::place_spawn`] names the host (here, or another one). Here:
//!   the server records the pending order ([`WorldManager::take_spawn_here`]) and answers
//!   ([`WorldManager::spawn_answered`]); the acknowledgement names this host's endpoint and
//!   goes back to the order's origin, the gate host ([`crate::Reliable::SpawnAck`]), which
//!   tells the login service ([`WorldManager::take_login_out`]). Another host: the order is
//!   forwarded while it is reachable and the order has hops left ([`SPAWN_RELAY_MAX_HOPS`]);
//!   else refused. A gate host that dies holding an order answers nothing: the login service
//!   times out (503) and the client's retry reaches the gate host of that moment.
//!   A player is placed here FROM THE STORE (no live state of it here) only once every
//!   expected peer finished its first sync of the current connection — its backlog came
//!   before its `Running` on the one ordered stream — so a peer that may hold the player
//!   live has said so first ("wait, never guess"). Until then the order waits (at most
//!   [`SPAWN_SYNC_WAIT_MS`], then it is refused). Under `claim` an order for land with no
//!   reachable owner — free, or its owner left cleanly — waits the same way while that land is
//!   asked for ([`crate::HostMsg::LandNeeded`], `land.rs`): placed once a host holds it (an
//!   assignment; for a leaver that kept its land, the heir of its `Leave` succession), else
//!   refused at [`SPAWN_SYNC_WAIT_MS`] (the ask, an assignment or a declaration and the heir's
//!   adoption may take longer) — a login there is then answered 503 and the client retries. A
//!   `departed` pointer at a host that left owning no land is dropped (`peers.rs`,
//!   `reroute_held_for_landless_leavers`): the store's record places that player. So is a
//!   pointer whose own forwarded order comes back to this host, from any host: the end of the
//!   player's walk dropped the record after a committed cut wrote it
//!   ([`WorldManager::departed_denied`]). An order taken here keeps an online player
//!   here — no hand-off at a host seam — until its new session connects ([`SPAWN_PIN_MS`]).
//! - **Kick**: applied by the gate host and relayed to every other host; acknowledged to
//!   the login service by the gate host alone, once every peer applied it (a peer's
//!   [`crate::Reliable::KickAck`] comes back to the gate, never up the peer's own link). A
//!   peer that cannot be reached means no acknowledgement (the login service resends: kicks
//!   stay durable; each resend goes to the gate host of that moment). A session handed
//!   here carries its login ([`crate::PlayerHandoff::login`]), so a kick finds it here too;
//!   a kick is remembered for [`KICK_MEMORY_MS`] for a hand-off still on its way.
//! - **Session end**: reported only when a login session leaves the WORLD (its home session
//!   disconnects or parks), never when its player is handed to another host — the login
//!   service drops a session's pending kick on its end. It goes up the coordinator link of
//!   the host where the session ended (queued while that link is down, at most
//!   `LOGIN_ENDS_QUEUED_MAX`, `world_link.rs`).

use super::*;
use crate::HostId;
use crate::Reliable;

/// How often a spawn order may be forwarded between hosts before it is refused (a loop of
/// stale `departed` pointers ends). It must cover the longest chain of stale pointers a walk
/// can leave (Elastic 4 Task 1 fix round 1): refused short of the chain's end, the order
/// drops no pointer and every join of that player at that gate is refused the same way.
/// With `n` pointers, an order goes from its gate along the pointers to the end of the walk,
/// by its record's region to the land's owner, and — the gate mid-chain — along the pointers
/// before the gate back to it, which drops its pointer ([`WorldManager::departed_denied`]) and
/// sends it to the owner once more: at most about `n + 3` hops from any gate (7 for four
/// pointers from a gate off the chain). Each pointer is on a different host, so this covers
/// walks over ~28 hosts; the cap still ends any loop, and a hop is one small reliable message.
pub const SPAWN_RELAY_MAX_HOPS: u8 = 32;

/// How long a spawn order waits for every peer's first sync before it is refused (the
/// login service has given up by then: `login_api::SPAWN_ACK_TIMEOUT_MS`).
pub const SPAWN_SYNC_WAIT_MS: u64 = 3_000;

/// How long a host remembers a kick it applied, for a hand-off of a revoked session still
/// on its way here (`login_api::SPAWN_PENDING_TTL_SECS`).
pub const KICK_MEMORY_MS: u64 = 120_000;

/// How long a spawn order this host took keeps its player here (no hand-off to another
/// host) while its session has not connected (`login_api::SPAWN_PENDING_TTL_SECS`).
pub const SPAWN_PIN_MS: u64 = 120_000;

/// How long a host remembers a spawn order it sent on to the host a `departed` pointer names
/// ([`SentOn`]): the order comes back within the hosts' waits, or the login service gave up.
pub const SPAWN_SENT_ON_MS: u64 = 2 * SPAWN_SYNC_WAIT_MS;

/// A spawn order this host sent on to the host its `departed` pointer names for the player
/// ([`WorldManager::departed_denied`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SentOn {
    pub(super) to: HostId,
    /// The order's origin (its gate host): a copy of the same login pushed to another gate (the
    /// login service resends on another link) is not this order.
    pub(super) origin: HostId,
    pub(super) player_token: String,
    pub(super) session_epoch: u64,
    /// The order's `hops` as it left here.
    pub(super) hops: u8,
    /// This host's hop of the player then ([`WorldManager::hop_here`]): any hand-off or
    /// placement of the player here since changes it.
    pub(super) hop: u64,
    pub(super) at_ms: u64,
}

/// Where a login's player is placed ([`WorldManager::place_spawn`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Here,
    Host(HostId),
    /// The region's super-region has no owner this host knows ([`crate::Owner::Free`] /
    /// [`crate::Owner::Unknown`]): the order waits, as for a peer's sync.
    Wait,
}

/// A spawn order of the login service ([`ServerEvent::SpawnOrder`], [`Reliable::SpawnOrder`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnRequest {
    /// The host the login service pushed it to (it answers the login service).
    pub origin: HostId,
    /// How often it was forwarded so far.
    pub hops: u8,
    pub player_token: String,
    pub session_epoch: u64,
    pub player: PlayerId,
    /// The region of the player's stored record (`None`: no record).
    pub record_region: Option<RegionCoords>,
}

/// A kick another host relayed here, for the server to apply to its admission state
/// ([`WorldManager::take_kicks_in`]), then [`WorldManager::kick_applied`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KickIn {
    pub from: HostId,
    pub player_token: String,
    pub session_epoch: u64,
    pub reason: String,
}

/// What the server tells the login service ([`WorldManager::take_login_out`]; up the
/// coordinator link as [`crate::HostMsg::Login`]). Its `Debug` never shows the player token.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LoginOut {
    /// `endpoint`: the host that took the order (`None` when refused).
    SpawnAck { player_token: String, session_epoch: u64, ok: bool, endpoint: Option<crate::HostEndpoint> },
    KickAck { player_token: String, session_epoch: u64 },
    SessionEnded { player_token: String, session_epoch: u64 },
}

impl std::fmt::Debug for LoginOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const HIDDEN: &str = "<redacted>";
        match self {
            LoginOut::SpawnAck { session_epoch, ok, endpoint, .. } => f
                .debug_struct("SpawnAck")
                .field("player_token", &HIDDEN)
                .field("session_epoch", session_epoch)
                .field("ok", ok)
                .field("endpoint", endpoint)
                .finish(),
            LoginOut::KickAck { session_epoch, .. } => {
                f.debug_struct("KickAck").field("player_token", &HIDDEN).field("session_epoch", session_epoch).finish()
            }
            LoginOut::SessionEnded { session_epoch, .. } => {
                f.debug_struct("SessionEnded").field("player_token", &HIDDEN).field("session_epoch", session_epoch).finish()
            }
        }
    }
}

/// An order of the login service, relayed by the coordinator to the gate host
/// ([`crate::CoordMsg::Login`]); the gate's server resolves it (its player store, its
/// admission state) into [`ServerEvent::SpawnOrder`] / [`ServerEvent::Kick`]. Its `Debug`
/// never shows the player token.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LoginOrder {
    Spawn { player_token: String, session_epoch: u64 },
    Kick { player_token: String, session_epoch: u64, reason: String },
}

impl LoginOrder {
    /// The order's session epoch.
    pub fn session_epoch(&self) -> u64 {
        match self {
            LoginOrder::Spawn { session_epoch, .. } | LoginOrder::Kick { session_epoch, .. } => *session_epoch,
        }
    }
}

impl std::fmt::Debug for LoginOrder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const HIDDEN: &str = "<redacted>";
        match self {
            LoginOrder::Spawn { session_epoch, .. } => {
                f.debug_struct("Spawn").field("player_token", &HIDDEN).field("session_epoch", session_epoch).finish()
            }
            LoginOrder::Kick { session_epoch, reason, .. } => f
                .debug_struct("Kick")
                .field("player_token", &HIDDEN)
                .field("session_epoch", session_epoch)
                .field("reason", reason)
                .finish(),
        }
    }
}

/// How often a kick that cannot be acknowledged (a peer out of reach) is warned about; the
/// login service resends it every couple of seconds.
pub const KICK_UNACKED_WARN_MS: u64 = 60_000;

/// A spawn order taken here for an online player ([`SPAWN_PIN_MS`]).
pub(super) struct Pin {
    at_ms: u64,
    epoch: u64,
    /// The region the player is placed in here (its land is busy while the pin lasts, `land.rs`).
    region: RegionCoords,
    /// Its clamp at a host seam was logged.
    told: bool,
}

/// A kick this host applied: every login of the token up to `up_to` is revoked.
pub(super) struct Kicked {
    up_to: u64,
    reason: String,
    at_ms: u64,
}

impl<S: RegionSpawner> WorldManager<S> {
    /// This host's ordinal.
    pub(super) fn ordinal(&self) -> HostId {
        self.ownership.0.me()
    }

    /// This host's id (its ownership's: the id its registration issued; `HostId::SOLO` alone).
    pub fn host_id(&self) -> HostId {
        self.ordinal()
    }

    /// Where `player` of a login is placed, in order: online, parking or being created here →
    /// here; held here only as a parked record → by its region's owner: this host or another
    /// (here: the session's placement hands the record to that host, `place_player`), nobody
    /// known — land masked for a release (`land.rs`), free or unknown — wait; handed to another
    /// host (`departed`) → that host; its record's region (off the map: the spawn region) → its
    /// owner; no record → the owner of [`SPAWN_REGION`].
    pub fn place_spawn(&self, player: PlayerId, record_region: Option<RegionCoords>) -> Placement {
        if let Some(parked) = self.parked_only(player) {
            return match self.owner_of(self.spawn_region(Some(parked.region))) {
                crate::Owner::Me | crate::Owner::Host(_) => Placement::Here,
                crate::Owner::Free | crate::Owner::Unknown => Placement::Wait,
            };
        }
        if self.holds_player(player) {
            return Placement::Here;
        }
        if let Some(&host) = self.departed.get(&player) {
            return Placement::Host(host);
        }
        let region = self.spawn_region(record_region);
        match self.owner_of(region) {
            crate::Owner::Me => Placement::Here,
            crate::Owner::Host(h) => Placement::Host(h),
            crate::Owner::Free | crate::Owner::Unknown => Placement::Wait,
        }
    }

    /// The region a login with `record_region` is placed in: its record's (off the map: the
    /// spawn region), or [`SPAWN_REGION`] with no record.
    pub(super) fn spawn_region(&self, record_region: Option<RegionCoords>) -> RegionCoords {
        record_region.filter(|rc| !crate::off_map(self.map_extent(), *rc)).unwrap_or(SPAWN_REGION)
    }

    /// Whether every peer that may hold a player live finished its first sync of its current
    /// connection (its `Running`, behind its backlog). A peer that left in a committed cut
    /// holds nobody.
    pub fn peers_synced(&self) -> bool {
        self.peers.keys().all(|o| self.peer_synced.contains(o) || self.left_peers.contains_key(o) || self.peer_succeeded(*o))
    }

    /// Spawn orders this host takes, for the server to record ([`Self::spawn_answered`]).
    pub fn take_spawn_here(&mut self) -> Vec<SpawnRequest> {
        std::mem::take(&mut self.spawn_here)
    }

    /// Kicks other hosts relayed here, for the server to apply ([`Self::kick_applied`]).
    pub fn take_kicks_in(&mut self) -> Vec<KickIn> {
        std::mem::take(&mut self.kicks_in)
    }

    /// What the login service is to be told from this host (the gate host's answers to the
    /// orders it took, and the ends of the sessions that left the world here). Role `host`
    /// sends them up its coordinator link instead ([`Self::send_login_out`]).
    pub fn take_login_out(&mut self) -> Vec<LoginOut> {
        std::mem::take(&mut self.login_out)
    }

    /// Everything the login service is to be told from this host, up this host's own
    /// coordinator link ([`crate::HostMsg::Login`]; the coordinator forwards it unchanged).
    pub fn send_login_out(&mut self) {
        for out in std::mem::take(&mut self.login_out) {
            self.world_send(crate::HostMsg::Login(out));
        }
    }

    /// The server could not take a login order of the coordinator (its player unresolved): the
    /// login service is told so ([`LoginOut::SpawnAck`] `ok: false`; a kick is never refused —
    /// unapplied, it is not answered and the login service resends it).
    pub fn login_order_refused(&mut self, player_token: String, session_epoch: u64) {
        self.login_out.push(LoginOut::SpawnAck { player_token, session_epoch, ok: false, endpoint: None });
    }

    /// The login service's orders the coordinator gave this host (the gate host), in order,
    /// for the server to resolve (its player store and admission state) into
    /// [`ServerEvent::SpawnOrder`] (origin: this host) / [`ServerEvent::Kick`].
    pub fn take_login_orders(&mut self) -> Vec<LoginOrder> {
        std::mem::take(&mut self.login_orders)
    }

    /// Test support: the spawn orders waiting for every peer's first sync.
    pub fn spawn_orders_waiting(&self) -> usize {
        self.spawn_held.len()
    }

    /// The server recorded (`ok`) or refused a spawn order this host took: its origin is
    /// told, with this host's endpoint.
    pub fn spawn_answered(&mut self, order: SpawnRequest, ok: bool) {
        if !ok && self.spawn_pins.get(&order.player).is_some_and(|p| p.epoch == order.session_epoch) {
            // No session comes for a refused order: the player is free to move on.
            self.spawn_pins.remove(&order.player);
        }
        let endpoint = ok.then(|| self.endpoint.clone());
        self.spawn_answer(order.origin, order.player_token, order.session_epoch, ok, endpoint);
    }

    /// The server applied (`applied`) a kick another host relayed: that host is told. Not
    /// applied (its admission state could not be changed): no answer, the login service
    /// resends it.
    pub fn kick_applied(&mut self, kick: KickIn, applied: bool) {
        if applied {
            self.queue_to_peer(kick.from, Reliable::KickAck { player_token: kick.player_token, session_epoch: kick.session_epoch });
        } else {
            log::warn!("[server] a kick from host {} could not be applied here; not acknowledged", kick.from);
        }
    }

    /// A spawn order, first seen now.
    pub(super) fn spawn_order(&mut self, req: SpawnRequest) {
        let now = self.clock_ms;
        self.spawn_order_since(req, now);
    }

    /// A spawn order seen first at `since_ms` (it waits at most [`SPAWN_SYNC_WAIT_MS`]).
    fn spawn_order_since(&mut self, req: SpawnRequest, since_ms: u64) {
        let live = self.holds_player(req.player) || self.departed.contains_key(&req.player);
        if self.leave_requested || self.leave_parked {
            log::warn!("[server] spawn order for player {}: this host is leaving; refused", req.player.0);
            return self.spawn_refused(req);
        }
        if self.heir_not_adopted() {
            // R4: until every report is in and resolved, this host cannot know which players
            // a survivor's journal holds a newer record of — placing any from the store could
            // pick the stale copy ("wait, never guess"). Retried at the adoption.
            return self.hold_spawn(req, since_ms, "this host inherits land it has not adopted yet");
        }
        match self.place_spawn(req.player, req.record_region) {
            Placement::Here if !live && !self.peers_synced() => self.hold_spawn(req, since_ms, "a peer has not synced"),
            Placement::Wait => {
                // Wanted for a claim: the order is placed once an owner is known (a record
                // parked here is placed by its own region).
                let region = self.placed_region(req.player, req.record_region);
                self.note_unowned(region);
                self.hold_spawn(req, since_ms, "its region has no owner this host knows")
            }
            Placement::Here => {
                log::info!("[server] spawn order for player {} (epoch {}) placed here", req.player.0, req.session_epoch);
                // Its session connects here: the player stays here until it does.
                let now = self.clock_ms;
                self.spawn_pins.retain(|_, p| now.saturating_sub(p.at_ms) < SPAWN_PIN_MS);
                let region = self.placed_region(req.player, req.record_region);
                self.spawn_pins.insert(req.player, Pin { at_ms: now, epoch: req.session_epoch, told: false, region });
                self.spawn_here.push(req);
            }
            Placement::Host(owner) if self.leave_blocks(owner) && self.ownership.0.directory().is_some() => {
                // R4 drain: the owner left cleanly (or is leaving in a cut not decided here
                // yet). Its land is asked for (`LandNeeded`, once it left); the order waits
                // like one for an unknown owner — placed once the heir holds the land (the
                // heir holds every order until it adopted), else refused at
                // `SPAWN_SYNC_WAIT_MS` (the login service answers 503, the client retries).
                // Asked at the next pass of `ask_for_land` (`land.rs`).
                let region = self.spawn_region(req.record_region);
                self.note_land_needed(region);
                self.hold_spawn(req, since_ms, "its region's host left the world; its land is asked for")
            }
            Placement::Host(owner) => {
                if req.hops < SPAWN_RELAY_MAX_HOPS && self.peer_reachable(owner) {
                    log::info!("[server] spawn order for player {} (epoch {}): to host {owner}", req.player.0, req.session_epoch);
                    if self.departed.get(&req.player) == Some(&owner) {
                        // Sent on by the pointer (the player is not held here, `place_spawn`):
                        // remembered, in case that host sends it back (`departed_denied`).
                        let now = self.clock_ms;
                        self.spawn_sent_on.retain(|_, s| now.saturating_sub(s.at_ms) < SPAWN_SENT_ON_MS);
                        let sent = SentOn {
                            to: owner,
                            origin: req.origin,
                            player_token: req.player_token.clone(),
                            session_epoch: req.session_epoch,
                            hops: req.hops + 1,
                            hop: self.hop_here(req.player),
                            at_ms: now,
                        };
                        self.spawn_sent_on.insert(req.player, sent);
                    }
                    let SpawnRequest { origin, hops, player_token, session_epoch, player, record_region } = req;
                    let order = Reliable::SpawnOrder { origin, hops: hops + 1, player_token, session_epoch, player, record_region };
                    self.queue_to_peer(owner, order);
                } else {
                    log::warn!(
                        "[server] spawn order for player {}: host {owner} holds it and is not reachable (or {} hops); refused",
                        req.player.0,
                        req.hops
                    );
                    self.spawn_refused(req);
                }
            }
        }
    }

    /// A spawn order that cannot be placed yet (`why`) waits for the next try
    /// ([`Self::retry_held_spawns`]), at most [`SPAWN_SYNC_WAIT_MS`] from `since_ms`; then it
    /// is refused.
    fn hold_spawn(&mut self, req: SpawnRequest, since_ms: u64, why: &str) {
        if self.clock_ms.saturating_sub(since_ms) >= SPAWN_SYNC_WAIT_MS {
            log::warn!("[server] spawn order for player {}: {why} within {SPAWN_SYNC_WAIT_MS} ms; refused", req.player.0);
            return self.spawn_refused(req);
        }
        self.spawn_held.push((since_ms, req));
    }

    /// A spawn order arrives from host `from`. If it is the very order this host sent on to
    /// the host its `departed` pointer names for the player ([`SentOn`]) — come back from ANY
    /// host (Elastic 4 Task 1: a chain of stale pointers) — the end of the player's walk holds
    /// it no more: the pointer is dropped, and the order is placed by its record's region like
    /// any other.
    ///
    /// Why that is safe: an order sent on by a pointer follows the player's walk. Every host
    /// the player left points at the next (`hand_off_to_peer`, `move_to_peer`; a succession
    /// re-points at the heir that parks the player, or at the survivor that holds it), and the
    /// order travels behind every hand-off or move of the player on the same ordered reliable
    /// stream: each host of the walk has applied the hand-off that brought the player before it
    /// reads the order. So the order goes on by pointers until it reaches a host with none,
    /// which places the player if it holds it live or parked (or waits), and otherwise has
    /// nothing of it — it dropped the record after a committed cut wrote it
    /// ([`Self::drop_stored_parked`]), or its own pointer ended the same way — and only then
    /// does the order travel by its record's region. An order that left here by the pointer
    /// and comes back from anywhere was therefore refused by the end of the walk: the store
    /// places the player. An order this host did not send on by the pointer proves none of
    /// this — a hand-off of this host may still be on its way — so the match is exact: the
    /// order's origin (another gate's copy of the same login is not this order), token and
    /// epoch, more hops than it left here with, within [`SPAWN_SENT_ON_MS`], the pointer still
    /// naming the host it was sent to, and this host's hop of the player unchanged (the player
    /// was not handed or placed here, and so not handed on again, since).
    fn departed_denied(&mut self, from: HostId, req: &SpawnRequest) {
        let Some(sent) = self.spawn_sent_on.remove(&req.player) else { return };
        let same_order = sent.origin == req.origin
            && sent.player_token == req.player_token
            && sent.session_epoch == req.session_epoch
            && req.hops > sent.hops
            && self.clock_ms.saturating_sub(sent.at_ms) < SPAWN_SENT_ON_MS;
        if !same_order || sent.hop != self.hop_here(req.player) || self.holds_player(req.player) {
            return;
        }
        if self.departed.get(&req.player) == Some(&sent.to) {
            self.departed.remove(&req.player);
            log::info!(
                "[server] spawn order for player {} sent on to host {} came back from host {from}: the player's walk holds \
                 it no more, placed by its stored record from now on",
                req.player.0,
                sent.to
            );
        }
    }

    fn spawn_refused(&mut self, req: SpawnRequest) {
        self.spawn_answer(req.origin, req.player_token, req.session_epoch, false, None);
    }

    /// The answer to a spawn order, to its `origin` (here: for the login service).
    pub(super) fn spawn_answer(&mut self, origin: HostId, player_token: String, session_epoch: u64, ok: bool, endpoint: Option<crate::HostEndpoint>) {
        if origin == self.ordinal() {
            self.login_out.push(LoginOut::SpawnAck { player_token, session_epoch, ok, endpoint });
        } else {
            self.queue_to_peer(origin, Reliable::SpawnAck { player_token, session_epoch, ok, endpoint });
        }
    }

    /// The spawn orders waiting for a sync, tried again (after the peers were pumped).
    pub(super) fn retry_held_spawns(&mut self) {
        for (since, req) in std::mem::take(&mut self.spawn_held) {
            self.spawn_order_since(req, since);
        }
    }

    /// The region a login of `player` is placed in: its record parked here, else its stored
    /// record's ([`Self::spawn_region`]).
    fn placed_region(&self, player: PlayerId, record_region: Option<RegionCoords>) -> RegionCoords {
        let parked = self.parked_players.get(&player).map(|r| r.region);
        self.spawn_region(parked.or(record_region))
    }

    /// The regions of spawn orders placed here whose session has not come yet (pins not
    /// expired): busy land (`land.rs`).
    pub(super) fn pinned_regions(&self) -> impl Iterator<Item = RegionCoords> + '_ {
        let now = self.clock_ms;
        self.spawn_pins.values().filter(move |p| now.saturating_sub(p.at_ms) < SPAWN_PIN_MS).map(|p| p.region)
    }

    /// Whether a spawn order this host took for `player` waits for its session: the player
    /// is not handed to another host meanwhile (its body is clamped back at a host seam).
    pub(super) fn spawn_pinned(&self, player: PlayerId) -> bool {
        self.spawn_pins.get(&player).is_some_and(|p| self.clock_ms.saturating_sub(p.at_ms) < SPAWN_PIN_MS)
    }

    /// Whether a spawn order for `player` is held, taken or placed here and its session has
    /// not come yet: its parked record stays here meanwhile (`land.rs`, `drop_stored_parked`).
    pub(super) fn spawn_awaited(&self, player: PlayerId) -> bool {
        self.spawn_pinned(player)
            || self.spawn_held.iter().any(|(_, r)| r.player == player)
            || self.spawn_here.iter().any(|r| r.player == player)
    }

    /// `client`'s body was clamped back at a host seam: whether its pin did it (online and
    /// pinned) — logged once per pin, not at every bounce.
    pub(super) fn note_pinned_clamp(&mut self, client: ClientId) -> bool {
        let Some(&player) = self.player_of.get(&client) else { return false };
        if !self.spawn_pinned(player) {
            return false;
        }
        let pin = self.spawn_pins.get_mut(&player).expect("pinned");
        if !pin.told {
            pin.told = true;
            log::info!("client {client}: player {} stays on this host until its new login connects", player.0);
        }
        true
    }

    /// A session of `player` connected here: a spawn order's pin of it ends.
    pub(super) fn spawn_unpin(&mut self, player: PlayerId) {
        self.spawn_pins.remove(&player);
    }

    /// Host `from` finished its first sync of this connection (its `Running`).
    pub(super) fn peer_synced_now(&mut self, from: HostId) {
        self.peer_synced.insert(from);
    }

    /// Host `from`'s connection is gone: it syncs again on the next one.
    pub(super) fn peer_unsynced(&mut self, from: HostId) {
        self.peer_synced.remove(&from);
    }

    /// The login service's kick, already applied to this host's admission state (the
    /// gate host): a session of the login handed here is parked, and every other host is
    /// told. Acknowledged ([`LoginOut::KickAck`]) once every peer applied it; a peer out of
    /// reach means no acknowledgement (the login service resends).
    pub(super) fn login_kick(&mut self, player_token: String, session_epoch: u64, reason: String) {
        self.kick_here(&player_token, session_epoch, &reason);
        let mut waiting = BTreeSet::new();
        let mut complete = true;
        let ordinals: Vec<HostId> =
            self.peers.keys().copied().filter(|o| !self.left_peers.contains_key(o) && !self.peer_succeeded(*o)).collect();
        for o in ordinals {
            if self.peer_reachable(o) {
                let kick = Reliable::Kick { player_token: player_token.clone(), session_epoch, reason: reason.clone() };
                self.queue_to_peer(o, kick);
                waiting.insert(o);
            } else {
                complete = false;
            }
        }
        let key = (player_token, session_epoch);
        if !complete {
            // Resent by the login service every few seconds while the host is away.
            let now = self.clock_ms;
            if self.kick_warned_ms.is_none_or(|at| now.saturating_sub(at) >= KICK_UNACKED_WARN_MS) {
                self.kick_warned_ms = Some(now);
                log::warn!("[server] kick up to epoch {session_epoch}: a host is out of reach; not acknowledged (resent later)");
            }
            self.kicks_pending.remove(&key);
        } else if waiting.is_empty() {
            self.login_out.push(LoginOut::KickAck { player_token: key.0, session_epoch });
        } else {
            self.kicks_pending.insert(key, waiting);
        }
    }

    /// Every session of a login of `token` up to `up_to` this host holds the body of (one
    /// handed here: its login travelled with it) is refused with `reason` and parked; the
    /// kick is remembered for a hand-off still on its way.
    fn kick_here(&mut self, token: &str, up_to: u64, reason: &str) {
        let now = self.clock_ms;
        self.kicked.retain(|_, k| now.saturating_sub(k.at_ms) < KICK_MEMORY_MS);
        let ceiling = self.kicked.get(token).map_or(up_to, |k| k.up_to.max(up_to));
        self.kicked.insert(token.to_string(), Kicked { up_to: ceiling, reason: reason.to_string(), at_ms: now });
        let revoked: Vec<ClientId> =
            self.logins.iter().filter(|(_, (t, e))| t == token && *e <= up_to).map(|(c, _)| *c).collect();
        for c in revoked {
            log::info!("client {c}: its login was revoked ({reason}); parked");
            let _ = self.out.send((Some(c), ServerPacket::Refused(reason.to_string())));
            self.client_disconnected(c, now);
        }
    }

    /// `client`'s login arrived with its player (a hand-off or a move): kept, so it travels
    /// on and its end is reported here — unless a remembered kick revoked it (parked now).
    pub(super) fn login_arrived(&mut self, client: ClientId, login: Option<(String, u64)>) {
        let Some((token, epoch)) = login else { return };
        let now = self.clock_ms;
        let revoked = self
            .kicked
            .get(&token)
            .filter(|k| now.saturating_sub(k.at_ms) < KICK_MEMORY_MS && epoch <= k.up_to)
            .map(|k| k.reason.clone());
        self.logins.insert(client, (token, epoch));
        if let Some(reason) = revoked {
            log::info!("client {client}: handed here with a revoked login ({reason}); parked");
            let _ = self.out.send((Some(client), ServerPacket::Refused(reason)));
            self.client_disconnected(client, now);
        }
    }

    /// A player handed here with its login is parked on arrival (its session never plays
    /// here): its login session ends.
    pub(super) fn login_parked_on_arrival(&mut self, login: Option<(String, u64)>) {
        if let Some((token, epoch)) = login {
            self.login_session_ended(token, epoch);
        }
    }

    /// A login session leaves the world here: the login service is told, by this host (up
    /// its own coordinator link; never through a peer — [`Reliable::SessionEnded`] is no
    /// longer sent).
    pub(super) fn login_session_ended(&mut self, player_token: String, session_epoch: u64) {
        self.login_out.push(LoginOut::SessionEnded { player_token, session_epoch });
    }

    /// `client`'s session leaves the world: its login (if any) ends.
    pub(super) fn end_login(&mut self, client: ClientId) {
        if let Some((token, epoch)) = self.logins.remove(&client) {
            self.login_session_ended(token, epoch);
        }
    }

    /// A login message host `from` relayed (`peers.rs`).
    pub(super) fn peer_login_msg(&mut self, from: HostId, msg: Reliable) {
        match msg {
            Reliable::SpawnOrder { origin, hops, player_token, session_epoch, player, record_region } => {
                let req = SpawnRequest { origin, hops, player_token, session_epoch, player, record_region };
                self.departed_denied(from, &req);
                self.spawn_order(req);
            }
            Reliable::SpawnAck { player_token, session_epoch, ok, endpoint } => {
                self.login_out.push(LoginOut::SpawnAck { player_token, session_epoch, ok, endpoint });
            }
            Reliable::Kick { player_token, session_epoch, reason } => {
                self.kick_here(&player_token, session_epoch, &reason);
                self.kicks_in.push(KickIn { from, player_token, session_epoch, reason });
            }
            Reliable::KickAck { player_token, session_epoch } => {
                let key = (player_token, session_epoch);
                let done = self.kicks_pending.get_mut(&key).is_some_and(|waiting| {
                    waiting.remove(&from);
                    waiting.is_empty()
                });
                if done {
                    self.kicks_pending.remove(&key);
                    self.login_out.push(LoginOut::KickAck { player_token: key.0, session_epoch });
                }
            }
            // No longer sent (Elastic 1: each host tells its own coordinator link): kept only
            // because `Reliable` is append-only.
            Reliable::SessionEnded { player_token, session_epoch } => {
                self.login_out.push(LoginOut::SessionEnded { player_token, session_epoch });
            }
            // Redacted: a transfer or move may carry a login's player token.
            other => log::error!("[server] host {from}: {:?} is not a login message", other.redacted()),
        }
    }
}
