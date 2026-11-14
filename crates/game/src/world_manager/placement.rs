//! Login placement across hosts (several hosts): the login service pushes its spawn orders
//! and kicks to ONE host (the entry host); the managers relay them to the host that holds
//! the player, over the same reliable peer streams that carry hand-offs and parks — so an
//! order relayed by the host a player left arrives behind the hand-off that took it away.
//!
//! - **Spawn**: [`WorldManager::place_spawn`] names the host (here, or another one). Here:
//!   the server records the pending order ([`WorldManager::take_spawn_here`]) and answers
//!   ([`WorldManager::spawn_answered`]); the acknowledgement names this host's endpoint and
//!   goes back to the order's origin ([`crate::Reliable::SpawnAck`]), which tells the login
//!   service ([`WorldManager::take_login_out`]). Another host: the order is forwarded while
//!   it is reachable and the order has hops left ([`SPAWN_RELAY_MAX_HOPS`]); else refused.
//!   A player is placed here FROM THE STORE (no live state of it here) only once every
//!   expected peer finished its first sync of the current connection — its backlog came
//!   before its `Running` on the one ordered stream — so a peer that may hold the player
//!   live has said so first ("wait, never guess"). Until then the order waits (at most
//!   [`SPAWN_SYNC_WAIT_MS`], then it is refused). Under `claim` an order for land whose owner
//!   left cleanly waits the same way while that land is asked for (R4 drain,
//!   [`crate::HostMsg::LandWanted`]): placed once the heir holds it, else refused at
//!   [`SPAWN_SYNC_WAIT_MS`] — while the coordinator waits out `HEIR_WAIT_MS` a login there is
//!   answered 503 and the client retries. An order taken here keeps an online player
//!   here — no hand-off at a host seam — until its new session connects ([`SPAWN_PIN_MS`]).
//! - **Kick**: applied by the entry host and relayed to every other host; acknowledged to
//!   the login service once every peer applied it. A peer that cannot be reached means no
//!   acknowledgement (the login service resends: kicks stay durable). A session handed
//!   here carries its login ([`crate::PlayerHandoff::login`]), so a kick finds it here too;
//!   a kick is remembered for [`KICK_MEMORY_MS`] for a hand-off still on its way.
//! - **Session end**: reported only when a login session leaves the WORLD (its home session
//!   disconnects or parks), never when its player is handed to another host — the login
//!   service drops a session's pending kick on its end.

use super::*;
use crate::Reliable;

/// How often a spawn order may be forwarded between hosts before it is refused (a loop of
/// stale `departed` pointers ends).
pub const SPAWN_RELAY_MAX_HOPS: u8 = 4;

/// How long a spawn order waits for every peer's first sync before it is refused (the
/// login service has given up by then: `login_api::SPAWN_ACK_TIMEOUT_MS`).
pub const SPAWN_SYNC_WAIT_MS: u64 = 3_000;

/// How long a host remembers a kick it applied, for a hand-off of a revoked session still
/// on its way here (`login_api::SPAWN_PENDING_TTL_SECS`).
pub const KICK_MEMORY_MS: u64 = 120_000;

/// How long a spawn order this host took keeps its player here (no hand-off to another
/// host) while its session has not connected (`login_api::SPAWN_PENDING_TTL_SECS`).
pub const SPAWN_PIN_MS: u64 = 120_000;

/// Where a login's player is placed ([`WorldManager::place_spawn`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Here,
    Host(u32),
    /// The region's super-region has no owner this host knows ([`crate::Owner::Free`] /
    /// [`crate::Owner::Unknown`]): the order waits, as for a peer's sync.
    Wait,
}

/// A spawn order of the login service ([`ServerEvent::SpawnOrder`], [`Reliable::SpawnOrder`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnRequest {
    /// The host the login service pushed it to (it answers the login service).
    pub origin: u32,
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
    pub from: u32,
    pub player_token: String,
    pub session_epoch: u64,
    pub reason: String,
}

/// What the server tells the login service ([`WorldManager::take_login_out`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginOut {
    /// `endpoint`: the host that took the order (`None` when refused).
    SpawnAck { player_token: String, session_epoch: u64, ok: bool, endpoint: Option<crate::HostEndpoint> },
    KickAck { player_token: String, session_epoch: u64 },
    SessionEnded { player_token: String, session_epoch: u64 },
}

/// How often a kick that cannot be acknowledged (a peer out of reach) is warned about; the
/// login service resends it every couple of seconds.
pub const KICK_UNACKED_WARN_MS: u64 = 60_000;

/// A spawn order taken here for an online player ([`SPAWN_PIN_MS`]).
pub(super) struct Pin {
    at_ms: u64,
    epoch: u64,
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
    pub(super) fn ordinal(&self) -> u32 {
        self.ownership.0.ordinal()
    }

    /// The host the login service is linked to (several hosts, `VOXEL_ENTRY_HOST`): a login
    /// session ending on another host is told to it. Unset: this host tells it itself.
    pub fn set_entry_host(&mut self, entry: u32) {
        self.entry_host = Some(entry);
    }

    /// Where `player` of a login is placed, in order: online, parked or parking here →
    /// here; handed to another host (`departed`) → that host; its record's region (off the
    /// map: the spawn region) → its owner; no record → the owner of [`SPAWN_REGION`].
    pub fn place_spawn(&self, player: PlayerId, record_region: Option<RegionCoords>) -> Placement {
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
    fn spawn_region(&self, record_region: Option<RegionCoords>) -> RegionCoords {
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

    /// What the login service is to be told (the entry host; one host: itself).
    pub fn take_login_out(&mut self) -> Vec<LoginOut> {
        std::mem::take(&mut self.login_out)
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
                // Wanted for a claim: the order is placed once an owner is known.
                let region = self.spawn_region(req.record_region);
                self.note_unowned(region);
                self.hold_spawn(req, since_ms, "its region has no owner this host knows")
            }
            Placement::Here => {
                log::info!("[server] spawn order for player {} (epoch {}) placed here", req.player.0, req.session_epoch);
                // Its session connects here: the player stays here until it does.
                let now = self.clock_ms;
                self.spawn_pins.retain(|_, p| now.saturating_sub(p.at_ms) < SPAWN_PIN_MS);
                self.spawn_pins.insert(req.player, Pin { at_ms: now, epoch: req.session_epoch, told: false });
                self.spawn_here.push(req);
            }
            Placement::Host(owner) if self.leave_blocks(owner) && self.ownership.0.directory().is_some() => {
                // R4 drain: the owner left cleanly (or is leaving in a cut not decided here
                // yet). Its land is asked for (`LandWanted`, once it left); the order waits
                // like one for an unknown owner — placed once the heir holds the land (the
                // heir holds every order until it adopted), else refused at
                // `SPAWN_SYNC_WAIT_MS` (the login service answers 503, the client retries).
                let region = self.spawn_region(req.record_region);
                self.note_land_wanted(region);
                self.hold_spawn(req, since_ms, "its region's host left the world; its land is asked for")
            }
            Placement::Host(owner) => {
                if req.hops < SPAWN_RELAY_MAX_HOPS && self.peer_reachable(owner) {
                    log::info!("[server] spawn order for player {} (epoch {}): to host {owner}", req.player.0, req.session_epoch);
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

    fn spawn_refused(&mut self, req: SpawnRequest) {
        self.spawn_answer(req.origin, req.player_token, req.session_epoch, false, None);
    }

    /// The answer to a spawn order, to its `origin` (here: for the login service).
    pub(super) fn spawn_answer(&mut self, origin: u32, player_token: String, session_epoch: u64, ok: bool, endpoint: Option<crate::HostEndpoint>) {
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

    /// Whether a spawn order this host took for `player` waits for its session: the player
    /// is not handed to another host meanwhile (its body is clamped back at a host seam).
    pub(super) fn spawn_pinned(&self, player: PlayerId) -> bool {
        self.spawn_pins.get(&player).is_some_and(|p| self.clock_ms.saturating_sub(p.at_ms) < SPAWN_PIN_MS)
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
    pub(super) fn peer_synced_now(&mut self, from: u32) {
        self.peer_synced.insert(from);
    }

    /// Host `from`'s connection is gone: it syncs again on the next one.
    pub(super) fn peer_unsynced(&mut self, from: u32) {
        self.peer_synced.remove(&from);
    }

    /// The login service's kick, already applied to this host's admission state (the
    /// entry host): a session of the login handed here is parked, and every other host is
    /// told. Acknowledged ([`LoginOut::KickAck`]) once every peer applied it; a peer out of
    /// reach means no acknowledgement (the login service resends).
    pub(super) fn login_kick(&mut self, player_token: String, session_epoch: u64, reason: String) {
        self.kick_here(&player_token, session_epoch, &reason);
        let mut waiting = BTreeSet::new();
        let mut complete = true;
        let ordinals: Vec<u32> =
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

    /// A login session leaves the world here: the login service is told (through the entry
    /// host when this is another one).
    pub(super) fn login_session_ended(&mut self, player_token: String, session_epoch: u64) {
        match self.entry_host {
            Some(entry) if entry != self.ordinal() => {
                self.queue_to_peer(entry, Reliable::SessionEnded { player_token, session_epoch });
            }
            _ => self.login_out.push(LoginOut::SessionEnded { player_token, session_epoch }),
        }
    }

    /// `client`'s session leaves the world: its login (if any) ends.
    pub(super) fn end_login(&mut self, client: ClientId) {
        if let Some((token, epoch)) = self.logins.remove(&client) {
            self.login_session_ended(token, epoch);
        }
    }

    /// A login message host `from` relayed (`peers.rs`).
    pub(super) fn peer_login_msg(&mut self, from: u32, msg: Reliable) {
        match msg {
            Reliable::SpawnOrder { origin, hops, player_token, session_epoch, player, record_region } => {
                self.spawn_order(SpawnRequest { origin, hops, player_token, session_epoch, player, record_region });
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
            Reliable::SessionEnded { player_token, session_epoch } => {
                self.login_out.push(LoginOut::SessionEnded { player_token, session_epoch });
            }
            other => log::error!("[server] host {from}: {other:?} is not a login message"),
        }
    }
}
