//! The peer half of [`WorldManager`]: regions another host owns ("foreign" regions) and
//! the [`PeerSession`] to each other host. An entity crossing into a foreign region goes
//! to its owner as a [`Reliable::Transfer`] — queued whether or not the owner is
//! reachable (it waits there, conserved) — and an inbound one lands through
//! [`WorldManager::accept_arrival`], the path a local crossing takes. Split out of
//! `mod.rs`; the core calls in through the `pub(super)` methods here.
//!
//! Halo and intents cross host seams too. **Volatile** (lost while the link is down):
//! which regions run ([`Volatile::Running`] on every [`PeerEvent::Up`], `RegionUp` /
//! `RegionDown` as a region bordering that host starts and stops — `foreign_running`),
//! halo requests and halo updates / entities, sent only to a region its owner runs (halo
//! never wakes a region). A host learning that a region of the other runs asks that
//! region for the seed of each of its own running neighbours — so a reconnect re-seeds
//! every halo along the seam. A link going down (or a peer of a newer generation) is
//! `HaloSourceGone` for every region of that host. **Reliable**: an intent whose owner
//! is reachable, and every reply (a reply carrying items is never dropped); an intent
//! whose owner is not reachable is refused (`OwnerNotRunning`) as for a parked region.
//!
//! **Players** cross too, when the owner is reachable: the body goes as a
//! [`Reliable::Transfer`] carrying a [`crate::PlayerHandoff`] (claims, a park in flight),
//! and the client's session follows it — its connection to the new host (an
//! [`SessionKind::Observer`] session until then) becomes the home, the one it left an
//! observer. The leaving host remembers where it sent the player (`departed`): input the
//! client still sends there follows the body ([`Volatile::Input`]). A client that never
//! connects to the new host is parked there after [`HANDOFF_SESSION_GRACE_MS`]. A body
//! heading for an unreachable host is handed back to its source. A **move** (Play, embark,
//! return, an adventure, retire) into a region another host owns parks the body here as
//! a local move does, then hands the player over as a record ([`Reliable::PlayerMove`] —
//! queued whether or not that host is reachable: the record waits there, conserved); the
//! receiving host creates its body at the target's landing. A host that parks a player
//! handed to it with no session of the client tells the host it came from
//! ([`Reliable::PlayerParked`]), which ends its observer session of the client. A
//! subscribe to a region another host owns is answered
//! with that host's endpoint and an observer ticket ([`crate::ServerPacket::RegionElsewhere`]),
//! the owner told to expect it ([`Reliable::ObserverOrder`]).

use super::*;
use crate::{PeerEvent, PeerLink, PeerSession, Reliable, Volatile};

/// One other host: the session with it and the link it runs over.
pub(super) struct Peer {
    session: PeerSession,
    link: Box<dyn PeerLink + Send>,
    /// It runs a newer world generation ([`PeerEvent::HigherGeneration`]): this host is
    /// stale and stops using the session (nothing is pumped or sent on it any more).
    stale: bool,
}

impl<S: RegionSpawner> WorldManager<S> {
    /// The session with host `ordinal` and its link. Messages queued for that host before
    /// it had a session go into the session now, in order.
    ///
    /// A session is never replaced: its numbering (both ways) is what makes delivery
    /// exactly-once. For a host that already has one, `session` is dropped and only the
    /// link is swapped; the session's connection is reset
    /// ([`PeerSession::reset_connection`]), so its next pump reports `Down` (that host's
    /// regions are no longer known to run) and resumes over the new link after a hello.
    pub fn set_peer(&mut self, ordinal: u32, mut session: PeerSession, link: Box<dyn PeerLink + Send>) {
        debug_assert_eq!(session.peer(), ordinal, "a session for another host");
        let prefilled = session.unreceived().count();
        if prefilled > 0 {
            log::error!("[server] a session for host {ordinal} came with {prefilled} message(s) already queued");
            debug_assert!(false, "set_peer: a session with messages queued for host {ordinal}");
        }
        if let Some(peer) = self.peers.get_mut(&ordinal) {
            log::info!("[server] the link to host {ordinal} was replaced: its session continues on the new one");
            peer.link = link;
            peer.session.reset_connection();
            return;
        }
        // Already in it: ids of their own (`peer_entries`), ahead of what waited.
        *self.peer_queued.entry(ordinal).or_default() += prefilled as u64;
        for msg in self.peer_waiting.remove(&ordinal).unwrap_or_default() {
            session.queue(msg);
        }
        self.peers.insert(ordinal, Peer { session, link, stale: false });
    }

    /// Drive every peer session: read what the other hosts sent and act on it, then send
    /// what is queued. Called first by [`Self::maintain`].
    pub fn pump_peers(&mut self, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        let ordinals: Vec<u32> = self.peers.keys().copied().collect();
        for ordinal in ordinals {
            let Some(peer) = self.peers.get_mut(&ordinal) else { continue };
            if peer.stale {
                continue;
            }
            let events = peer.session.pump(peer.link.as_mut(), now_ms);
            // A connection the session broke on this pump (it reported `Down` and waits for
            // the next one) is ended here, so the link makes a fresh one with a new hello.
            if peer.session.is_broken() && peer.link.is_up() && events.contains(&PeerEvent::Down) {
                log::warn!("[server] the connection to host {ordinal} broke: closing it for a fresh one");
                peer.link.close();
            }
            for ev in events {
                self.peer_event(ordinal, ev);
            }
        }
        // A person delivered by an inbound transfer was reported to the coordinator.
        self.flush_world();
    }

    /// The host that owns `rc`.
    pub fn owner_of(&self, rc: RegionCoords) -> u32 {
        self.ownership.0.owner_of(rc, self.ownership.1)
    }

    /// Whether another host owns `rc`.
    pub fn is_foreign(&self, rc: RegionCoords) -> bool {
        !self.owns(rc)
    }

    /// Whether something can be put into `rc` now: this host owns it, or its owner's
    /// session is reachable.
    pub fn reaches(&self, rc: RegionCoords) -> bool {
        self.owns(rc)
            || self.peers.get(&self.owner_of(rc)).is_some_and(|p| !p.stale && p.session.is_reachable())
    }

    /// Whether host `ordinal`'s session is reachable now (and it is not of a newer generation).
    pub(super) fn peer_reachable(&self, ordinal: u32) -> bool {
        self.peers.get(&ordinal).is_some_and(|p| !p.stale && p.session.is_reachable())
    }

    /// Reliable messages to other hosts not acknowledged yet, those waiting for a session
    /// included (the `[server]` counters line; 0 once everything arrived).
    pub fn peer_unreceived(&self) -> usize {
        self.peers.values().map(|p| p.session.unreceived().count()).sum::<usize>()
            + self.peer_waiting.values().map(Vec::len).sum::<usize>()
    }

    /// Test support: the entity of every [`Reliable::Transfer`] not acknowledged yet —
    /// `(peer, id, on the wire)`; `false` = never sent (queued, or waiting for a session),
    /// `true` = sent, so the peer may already hold it.
    pub fn peer_unreceived_ids(&self) -> Vec<(u32, crate::EntityId, bool)> {
        let mut out = Vec::new();
        for (&ordinal, peer) in &self.peers {
            let unsent: BTreeSet<crate::EntityId> = peer.session.unsent().filter_map(transfer_id).collect();
            for id in peer.session.unreceived().filter_map(transfer_id) {
                out.push((ordinal, id, !unsent.contains(&id)));
            }
        }
        for (&ordinal, list) in &self.peer_waiting {
            out.extend(list.iter().filter_map(transfer_id).map(|id| (ordinal, id, false)));
        }
        out
    }

    /// Test support: the client and player of every [`Reliable::PlayerMove`] not
    /// acknowledged yet — `(peer, client, player, on the wire)`, as
    /// [`Self::peer_unreceived_ids`].
    pub fn peer_unreceived_moves(&self) -> Vec<(u32, ClientId, PlayerId, bool)> {
        let mut out = Vec::new();
        for (&ordinal, peer) in &self.peers {
            let unsent = peer.session.unsent().count();
            let all: Vec<&Reliable> = peer.session.unreceived().collect();
            let sent = all.len() - unsent;
            for (i, r) in all.into_iter().enumerate() {
                if let Reliable::PlayerMove { handoff, .. } = r.inner() {
                    out.push((ordinal, handoff.client, handoff.player, i < sent));
                }
            }
        }
        for (&ordinal, list) in &self.peer_waiting {
            out.extend(list.iter().filter_map(|r| match r.inner() {
                Reliable::PlayerMove { handoff, .. } => Some((ordinal, handoff.client, handoff.player, false)),
                _ => None,
            }));
        }
        out
    }

    /// Test support: every [`Reliable::Intent`] (an intent or a reply) not acknowledged
    /// yet — `(peer, msg, on the wire)`, as [`Self::peer_unreceived_ids`].
    pub fn peer_unreceived_intents(&self) -> Vec<(u32, crate::IntentMsg, bool)> {
        let mut out = Vec::new();
        for (&ordinal, peer) in &self.peers {
            let unsent = peer.session.unsent().count();
            let all: Vec<&Reliable> = peer.session.unreceived().collect();
            let sent = all.len() - unsent;
            for (i, r) in all.into_iter().enumerate() {
                if let Reliable::Intent(msg) = r.inner() {
                    out.push((ordinal, *msg, i < sent));
                }
            }
        }
        for (&ordinal, list) in &self.peer_waiting {
            out.extend(list.iter().filter_map(|r| match r.inner() {
                Reliable::Intent(msg) => Some((ordinal, *msg, false)),
                _ => None,
            }));
        }
        out
    }

    /// The regions of other hosts known to run and border one of this host's.
    pub fn foreign_running(&self) -> Vec<RegionCoords> {
        self.foreign_running.keys().copied().collect()
    }

    /// The newest world generation a peer reported running ([`PeerEvent::HigherGeneration`]),
    /// once: this host is stale and must stop.
    pub fn take_higher_generation(&mut self) -> Option<u64> {
        self.higher_generation.take()
    }

    /// Where clients reach this host (the first packet of every session,
    /// [`crate::ServerPacket::Host`]), and the seed observer nonces are drawn from (random
    /// per host process start: a nonce must not be guessable).
    pub fn set_endpoints(&mut self, mine: crate::HostEndpoint, nonce_seed: u64) {
        self.endpoint = mine;
        self.nonce_state = Some(nonce_seed);
    }

    /// The observer orders other hosts sent since the last call:
    /// `(client, player, nonce, ttl_secs)` — the server edge admits a
    /// `Credential::Observer` that matches one for `ttl_secs` from when it took it.
    pub fn take_observer_orders(&mut self) -> Vec<(ClientId, PlayerId, u64, u64)> {
        std::mem::take(&mut self.observer_orders)
    }

    /// What `client`'s session here is, if it has one.
    pub fn session_kind(&self, client: ClientId) -> Option<SessionKind> {
        self.sessions.get(&client).map(|s| s.kind)
    }

    /// The region holding (or getting) `client`'s body on this host, if any.
    pub fn home_of(&self, client: ClientId) -> Option<RegionCoords> {
        self.homes.get(&client).copied()
    }

    /// The fortress or adventure `client`'s player plays here, if any.
    pub fn client_claim(&self, client: ClientId) -> Option<Claim> {
        claim_of(self.fortress_of.get(&client).copied(), self.adventurer_of.get(&client).copied())
    }

    /// The region of `site` as this host knows it (the coordinator's view and answers).
    pub fn site_region(&self, site: SiteId) -> Option<RegionCoords> {
        self.site_regions.get(&site).copied()
    }

    /// The players this host handed to another, with that host.
    pub fn departed(&self) -> &BTreeMap<PlayerId, u32> {
        &self.departed
    }

    /// An observer session of `client` (its body is on another host, or on its way here):
    /// subscriptions only. A body handed here before the session came makes it the home.
    pub(super) fn observer_connected(&mut self, client: ClientId, player: PlayerId, now_ms: u64) {
        if self.sessions.contains_key(&client) {
            log::warn!("client {client}: a second session on this host; the observer connection is ignored");
            return;
        }
        let _ = self.out.send((Some(client), ServerPacket::Host(self.endpoint.clone())));
        let home = self.awaiting_session.remove(&client).is_some();
        if home && self.player_of.get(&client) != Some(&player) {
            log::error!("client {client}: an observer session for player {} holds another player's body", player.0);
        }
        let kind = if home { SessionKind::Home } else { SessionKind::Observer };
        self.sessions.insert(client, Session { kind, player: Some(player), ..Session::default() });
        if home {
            log::info!("client {client}: connected after its body was handed here; the session is its home");
            let region = self.homes.get(&client).copied();
            let _ = self.out.send((Some(client), ServerPacket::PlayerRegion(region, client)));
            if let Some(rc) = region {
                self.refresh_keepalive(rc, now_ms);
            }
            // Its `PlayerOnline` went when the body arrived (`accept_handoff`).
        }
    }

    /// The host `client`'s player was handed to, if this host handed it away.
    pub(super) fn departed_host(&self, client: ClientId) -> Option<u32> {
        let player = self.player_of.get(&client).copied().or_else(|| self.sessions.get(&client)?.player)?;
        self.departed.get(&player).copied()
    }

    /// An input of `client`, whose body is on another host now (`departed`), to that host
    /// (volatile: lost with a link that is down, as an input to a dead region is).
    pub(super) fn forward_input(&mut self, client: ClientId, kind: GameEventKind, stamp: crate::Tick) {
        let Some(host) = self.departed_host(client) else { return };
        log::debug!("[server] {kind:?} of client {client}: its body is on host {host}, forwarded");
        if let Some(peer) = self.peers.get_mut(&host).filter(|p| !p.stale) {
            peer.session.send_volatile(Volatile::Input { client, kind, stamp });
        }
    }

    /// Whether `client`'s body departing into `target` (another host's) goes to that host:
    /// it is reachable, the player is known (online, or parking) and no move or restore
    /// here waits on the body. Otherwise it is handed back to where it left.
    pub(super) fn can_hand_off(&self, client: ClientId, target: RegionCoords) -> bool {
        let player = self.player_of.get(&client).copied().or_else(|| self.parking.get(&client).map(|p| p.player));
        let Some(player) = player else { return false };
        self.reaches(target)
            && !self.moving.contains_key(&client)
            && !self.creating.contains_key(&client)
            // A new session of the player waits here for this park.
            && !self.awaiting_park.contains_key(&player)
            // Its claim may change with an answer that comes to this host.
            && !self.claim_pending(player)
    }

    /// `bundle` (in `target`'s frame), `client`'s body, departed from `rc` at `stamp` into
    /// `target`, which another (reachable) host owns: the player goes with it — its claims,
    /// a park in flight — and this host keeps only where it went (`departed`); the
    /// client's session here, if any, becomes an observer. `view`: as [`Self::depart_to_peer`].
    pub(super) fn hand_off_to_peer(
        &mut self,
        rc: RegionCoords,
        target: RegionCoords,
        stamp: crate::Tick,
        bundle: crate::EntityBundle,
        view: bool,
        now_ms: u64,
    ) {
        let client = bundle.client.as_ref().map(|(c, _)| *c).expect("a player's body");
        let owner = self.owner_of(target);
        let (player, claim, park) = match self.parking.remove(&client) {
            // Disconnected, its park on the way to `rc`: parked where it lands.
            Some(p) => (p.player, claim_of(p.fortress, p.adventure), true),
            None => {
                let player = self.player_of.remove(&client).expect("can_hand_off: online or parking");
                let claim = claim_of(self.fortress_of.remove(&client), self.adventurer_of.remove(&client));
                // Not offline: in transit, its claim kept in use until `owner` reports it.
                self.world_handed(player, owner);
                self.forget_claim_reports(player);
                (player, claim, false)
            }
        };
        let hop = self.hop_here(player) + 1;
        log::info!("[server] player {} (client {client}) from {rc:?} into {target:?}: handed to host {owner}", player.0);
        self.homes.remove(&client);
        // Handed here without a session and on again before it came: not awaited here.
        self.awaiting_session.remove(&client);
        if let Some(session) = self.sessions.get_mut(&client) {
            session.kind = SessionKind::Observer;
        }
        self.departed.insert(player, owner);
        self.locations.remove(&bundle.id);
        let handoff = crate::PlayerHandoff { client, player, claim, login: None, park, hop };
        self.send_reliable(owner, Reliable::Transfer { target, stamp, bundle: Box::new(bundle), player: Some(handoff) }, view);
        // Its home may have just lost its keep-alive reason.
        self.refresh_keepalive(rc, now_ms);
    }

    /// A player handed here by host `from` ([`Self::hand_off_to_peer`]): its body arrives at
    /// `target` like any crossing, and the player is this host's now — its home session if
    /// the client is connected here, else awaited ([`HANDOFF_SESSION_GRACE_MS`]); parked
    /// where it lands if its park was on the way.
    fn accept_handoff(
        &mut self,
        from: u32,
        target: RegionCoords,
        stamp: crate::Tick,
        bundle: crate::EntityBundle,
        h: crate::PlayerHandoff,
    ) {
        let now_ms = self.clock_ms;
        if bundle.client.as_ref().map(|(c, _)| *c) != Some(h.client) {
            log::error!("[server] host {from} handed player {} over with another client's body: dropped", h.player.0);
            debug_assert!(false, "a hand-off whose body is not its client's");
            return;
        }
        let c = h.client;
        log::info!("[server] player {} (client {c}) handed over by host {from} into {target:?}", h.player.0);
        self.departed.remove(&h.player);
        if self.holds_player(h.player) {
            return self.drop_arriving_copy(from, h.player, h.hop);
        }
        self.placed_by_handoff(h.player, h.hop);
        let (fortress, adventure) = split_claim(h.claim);
        if self.is_unavailable(target) {
            // Its stored state could not be read (retry not come): the body cannot go
            // there. Parked from the hand-off, as a player crossing into a region whose
            // load fails is (`region_load_failed`); `park_handed` reports it offline.
            let record = handoff_record(target, &bundle, &h).expect("checked above");
            log::error!("[server] player {} handed into unavailable {target:?}: parked", h.player.0);
            self.park_handed(from, c, h.player, record, now_ms);
            return;
        }
        if h.park {
            // `accept_arrival` parks it where it lands (`ParkPlayer` behind the arrival).
            self.parking.insert(c, Parking { player: h.player, region: target, fortress, adventure });
            self.accept_arrival(None, target, stamp, bundle, now_ms);
            return;
        }
        self.player_of.insert(c, h.player);
        if let Some(site) = fortress {
            self.fortress_of.insert(c, site);
        }
        if let Some(site) = adventure {
            self.adventurer_of.insert(c, site);
        }
        match self.sessions.get_mut(&c) {
            Some(session) => session.kind = SessionKind::Home,
            None => {
                self.awaiting_session.insert(c, (now_ms, from));
            }
        }
        // Its home here, `PlayerRegion` to the client.
        self.accept_arrival(None, target, stamp, bundle, now_ms);
        self.claim_changed(h.player);
        self.world_player_online(c);
    }

    /// The move of `client` (`m`) into `m.target`, which another host owns, is answered
    /// (`record` = its park out of `m.from`; `None` = no body came back, the region was
    /// lost): the player goes to that host as a record ([`Reliable::PlayerMove`]: its
    /// `Client`, its claims, standing at the move's landing) — queued whether or not that
    /// host is reachable, so it waits there, conserved. This host keeps only where it went
    /// (`departed`); the client's session here becomes an observer and is sent on to that
    /// host ([`crate::ServerPacket::RegionElsewhere`]).
    pub(super) fn move_to_peer(&mut self, client: ClientId, m: Moving, record: Option<Box<PlayerRecord>>, now_ms: u64) {
        let owner = self.owner_of(m.target);
        // A move belongs to an online player (a disconnect turns it into a park).
        let Some(player) = self.player_of.remove(&client) else {
            log::error!("move of client {client} into {:?}: no player; nothing to hand over", m.target);
            debug_assert!(false, "a move with no player");
            return;
        };
        let claim = claim_of(self.fortress_of.remove(&client), self.adventurer_of.remove(&client));
        // Not offline: in transit, its claim kept in use until `owner` reports it.
        self.world_handed(player, owner);
        self.forget_claim_reports(player);
        let hop = self.hop_here(player) + 1;
        let (mut carried, y) = match record {
            Some(record) => (record.client, record.pose.translation.y),
            None => {
                log::error!("move of client {client} out of {:?}: no body came back; its items are lost", m.from);
                (crate::Client::default(), 0.0)
            }
        };
        // What was to follow its creation there (an adventure start's `SetMode`) is
        // applied to what it carries.
        for kind in m.then {
            carry_event(&mut carried, kind);
        }
        let (fortress, adventure) = split_claim(claim);
        let (x, z) = crate::block_column_xz(m.landing);
        let pose = crate::Pose::from_translation(rapier3d::math::Vector::new(x, y, z));
        let record = PlayerRecord { client: carried, pose, region: m.target, fortress, adventure };
        log::info!(
            "[server] player {} (client {client}) moves from {:?} into {:?}: handed to host {owner}",
            player.0,
            m.from,
            m.target
        );
        self.homes.remove(&client);
        self.awaiting_session.remove(&client);
        if let Some(session) = self.sessions.get_mut(&client) {
            session.kind = SessionKind::Observer;
        }
        self.departed.insert(player, owner);
        let handoff = crate::PlayerHandoff { client, player, claim, login: None, park: false, hop };
        // Parked after `m.from` answered the pending cut: its snapshot holds the body.
        let view = self.in_cut_view(m.from);
        self.send_reliable(owner, Reliable::PlayerMove { handoff, record: Box::new(record), target: m.target }, view);
        self.refresh_keepalive(m.from, now_ms);
        self.redirect(client, m.target);
    }

    /// A player moved here by host `from` ([`Self::move_to_peer`]): this host's now — its
    /// home session if the client is connected here, else awaited
    /// ([`HANDOFF_SESSION_GRACE_MS`]) — its body created at `target`'s landing with what it
    /// carries, as a move within one host creates it. A `target` that cannot be loaded
    /// parks the record here ([`Self::park_handed`]).
    fn accept_move(&mut self, from: u32, h: crate::PlayerHandoff, record: PlayerRecord, target: RegionCoords) {
        let now_ms = self.clock_ms;
        let c = h.client;
        log::info!("[server] player {} (client {c}) moved here by host {from} into {target:?}", h.player.0);
        self.departed.remove(&h.player);
        if self.holds_player(h.player) {
            return self.drop_arriving_copy(from, h.player, h.hop);
        }
        self.placed_by_handoff(h.player, h.hop);
        if self.is_unavailable(target) || !self.owns(target) {
            log::error!("[server] player {} moved into {target:?}, unavailable or not this host's: parked", h.player.0);
            self.park_handed(from, c, h.player, record, now_ms);
            return;
        }
        self.player_of.insert(c, h.player);
        let (fortress, adventure) = split_claim(h.claim);
        if let Some(site) = fortress {
            self.fortress_of.insert(c, site);
        }
        if let Some(site) = adventure {
            self.adventurer_of.insert(c, site);
        }
        match self.sessions.get_mut(&c) {
            Some(session) => session.kind = SessionKind::Home,
            None => {
                self.awaiting_session.insert(c, (now_ms, from));
            }
        }
        // In no region while its creation is on its way to the target.
        self.cut_transit(h.player, &record);
        let landing = self.landing_in(target);
        self.homes.insert(c, target);
        self.creating.insert(c, (target, h.player, record.clone()));
        self.send_play_event(target, GameEventKind::CreateClientAt(c, landing, Some(Box::new(record.client))));
        let _ = self.out.send((Some(c), ServerPacket::PlayerRegion(Some(target), c)));
        self.refresh_keepalive(target, now_ms);
        self.claim_changed(h.player);
        self.world_player_online(c);
    }

    /// Whether this host already holds `player`: online (any session), parked, parking,
    /// moving, its body being created, awaited after a hand-off, or a login of it waiting
    /// for a park.
    fn holds_player(&self, player: PlayerId) -> bool {
        self.player_of.values().any(|p| *p == player)
            || self.parked_players.contains_key(&player)
            || self.parking.values().any(|p| p.player == player)
            || self.creating.values().any(|(_, p, _)| *p == player)
            || self.awaiting_park.contains_key(&player)
    }

    /// A move or hand-off of `player` from host `from` arrives while this host already
    /// holds the player: the arriving copy is dropped — nothing is placed or parked. The
    /// copy here was restored from the store record that `from`'s cut wrote from this very
    /// queued message (it could not be delivered meanwhile), so the two started equal and
    /// the live one here is the newer truth. `from` needs no answer: the reliable message
    /// is acknowledged by its receipt. (`moving` and `awaiting_session` clients are in
    /// `player_of`.)
    ///
    /// The copy here has its own state with the coordinator (online, or parked and
    /// reported so), but `from` reported the player in transit to this host at the
    /// arriving `hop - 1`: the copy here takes the arriving hop if it is higher, and is
    /// reported again at it (online, or offline when it is not online here) — its older
    /// reports would be stale against that transit and could never end it.
    fn drop_arriving_copy(&mut self, from: u32, player: PlayerId, hop: u64) {
        log::warn!("[server] player {} from host {from} is already here; the arriving copy is dropped", player.0);
        if hop <= self.hop_here(player) {
            return;
        }
        self.player_hop.insert(player, hop);
        self.handed.remove(&player);
        let clients: Vec<ClientId> = self.player_of.iter().filter(|(_, p)| **p == player).map(|(c, _)| *c).collect();
        if clients.is_empty() {
            self.world_send(HostMsg::PlayerOffline { player, hop });
        }
        for c in clients {
            // A session waiting for its park reports when it is restored.
            if !self.restore_pending(c) {
                self.world_player_online(c);
            }
        }
    }

    /// `player` (of `client`), handed here by host `from`, cannot go where it was headed:
    /// its record is parked here. A session of the client here is refused as one whose
    /// region cannot be loaded; with none, host `from` is told
    /// ([`Self::tell_parked`]). Never announced online here, it is reported offline: the
    /// sending host reported it in transit to this host (`PlayerHandedTo`), not offline.
    fn park_handed(&mut self, from: u32, client: ClientId, player: PlayerId, record: PlayerRecord, now_ms: u64) {
        let hop = self.hop_here(player);
        self.world_send(HostMsg::PlayerOffline { player, hop });
        self.cut_transit(player, &record);
        self.parked_players.insert(player, record);
        if self.sessions.contains_key(&client) {
            let _ = self.out.send((Some(client), ServerPacket::Refused(REGION_UNAVAILABLE_REFUSAL.to_string())));
            self.client_disconnected(client, now_ms);
        } else {
            self.tell_parked(from, client, player);
        }
    }

    /// Host `from` handed `player` (of `client`) here and this host parked it with no
    /// session of the client: `from` ends the session it may still hold for the client.
    fn tell_parked(&mut self, from: u32, client: ClientId, player: PlayerId) {
        log::info!("[server] player {} (client {client}) parked here with no session: host {from} told", player.0);
        self.queue_to_peer(from, Reliable::PlayerParked { client, player });
    }

    /// [`Self::tell_parked`] for an awaited client (`awaiting_session`) about to be parked.
    pub(super) fn tell_awaited_parked(&mut self, client: ClientId) {
        let Some(&(_, from)) = self.awaiting_session.get(&client) else { return };
        let Some(&player) = self.player_of.get(&client) else { return };
        self.tell_parked(from, client, player);
    }

    /// Host `from` parked `player`, handed there from here, with no session of `client`
    /// there: this host's observer session of the client (if any) ends, the client told
    /// why. `departed` stays (the record is on `from`).
    fn player_parked_elsewhere(&mut self, from: u32, client: ClientId, player: PlayerId) {
        let observing =
            self.sessions.get(&client).is_some_and(|s| s.kind == SessionKind::Observer && s.player == Some(player));
        if !observing {
            log::debug!("[server] host {from} parked player {} (client {client}); no observer session here", player.0);
            return;
        }
        log::info!("[server] host {from} parked player {} (client {client}): its session here ends", player.0);
        let _ = self.out.send((Some(client), ServerPacket::Refused(PLAYER_PARKED_ELSEWHERE.to_string())));
        self.client_disconnected(client, self.clock_ms);
    }

    /// `client` subscribes to `rc`, which another host owns: told where to go, with a
    /// ticket that host expects ([`Self::observer_ticket`]). Nothing while that host is
    /// unreachable.
    pub(super) fn subscribe_elsewhere(&mut self, client: ClientId, rc: RegionCoords) {
        let owner = self.owner_of(rc);
        if !self.reaches(rc) {
            log::debug!("client {client}: {rc:?} is host {owner}'s, which is not reachable; subscribe dropped");
            return;
        }
        self.redirect(client, rc);
    }

    /// `client` is told to go to the host that owns `rc` (its endpoint as last known, a
    /// ticket that host is told to expect — queued like everything to it).
    fn redirect(&mut self, client: ClientId, rc: RegionCoords) {
        let owner = self.owner_of(rc);
        let Some(endpoint) = self.peer_endpoints.get(&owner).cloned() else {
            log::warn!("client {client}: {rc:?} is host {owner}'s, whose endpoint is not known; subscribe dropped");
            return;
        };
        if self.sessions.get(&client).and_then(|s| s.player).is_none() {
            log::warn!("client {client}: no player for its session; subscribe to {rc:?} dropped");
            return;
        }
        if self.nonce_state.is_none() {
            // No seed (`set_endpoints` not called): a ticket would be guessable.
            if !self.unseeded_warned {
                self.unseeded_warned = true;
                log::error!("[server] no nonce seed (set_endpoints): subscribes to other hosts' regions are dropped");
            }
            return;
        }
        let ticket = self.observer_ticket(client, owner, self.clock_ms);
        let _ = self.out.send((Some(client), ServerPacket::RegionElsewhere { region: rc, endpoint, ticket }));
    }

    /// The ticket `client` presents to host `owner`: minted once (a fresh random nonce,
    /// the owner told to expect it — [`Reliable::ObserverOrder`], valid for
    /// [`OBSERVER_ORDER_TTL_SECS`] from its receipt there), handed out again while at
    /// least half its life is left (by this host's `now_ms`), then minted anew.
    fn observer_ticket(&mut self, client: ClientId, owner: u32, now_ms: u64) -> crate::ObserverTicket {
        let reuse_ms = OBSERVER_ORDER_TTL_SECS * 1000 / 2;
        if let Some(&(nonce, minted)) = self.observer_tickets.get(&(client, owner)) {
            if now_ms.saturating_sub(minted) < reuse_ms {
                return crate::ObserverTicket { client, nonce };
            }
        }
        let nonce = self.next_nonce();
        self.observer_tickets.insert((client, owner), (nonce, now_ms));
        match self.sessions.get(&client).and_then(|s| s.player) {
            Some(player) => {
                let ttl_secs = OBSERVER_ORDER_TTL_SECS;
                self.queue_to_peer(owner, Reliable::ObserverOrder { client, player, nonce, ttl_secs });
            }
            None => log::error!("client {client}: an observer ticket with no player; host {owner} is not told"),
        }
        crate::ObserverTicket { client, nonce }
    }

    /// splitmix64 over the seed [`Self::set_endpoints`] gave (callers check it is seeded).
    fn next_nonce(&mut self) -> u64 {
        let state = self.nonce_state.unwrap_or_default().wrapping_add(0x9E37_79B9_7F4A_7C15);
        self.nonce_state = Some(state);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `msg` to host `ordinal` (a state message departing after its source answered goes by
    /// [`Self::send_reliable`]): on its session, or kept until it has one.
    pub(super) fn queue_to_peer(&mut self, ordinal: u32, msg: Reliable) {
        self.send_reliable(ordinal, msg, false);
    }

    /// `msg` to host `ordinal`; `view`: a transfer, intent or move that departed after its
    /// source answered the pending cut (its source's snapshot holds it). A state message to
    /// a participant of a world cut goes bare or wrapped ([`Reliable::InCut`]) by who
    /// accounts for it (`world_cut.rs`, [`Self::world_cut_wrap`]); `view` keeps it out of
    /// a one-phase cut's rows.
    pub(super) fn send_reliable(&mut self, ordinal: u32, msg: Reliable, view: bool) {
        let msg = match self.world_cut_wrap(ordinal, msg, view) {
            (msg, Some(cut)) => Reliable::InCut { cut, body: Box::new(msg) },
            (msg, None) => msg,
        };
        let id = self.queue_now(ordinal, msg);
        if view {
            self.peer_in_source_view.insert((ordinal, id));
        }
    }

    /// `msg` to host `ordinal` now: on its session, or kept until it has one. Its id among
    /// everything queued to that host (`peer_entries`).
    pub(super) fn queue_now(&mut self, ordinal: u32, msg: Reliable) -> u64 {
        let count = self.peer_queued.entry(ordinal).or_default();
        let id = *count;
        *count += 1;
        match self.peers.get_mut(&ordinal) {
            Some(peer) => peer.session.queue(msg),
            None => {
                let waiting = self.peer_waiting.entry(ordinal).or_default();
                // Once per wait, not per message.
                if waiting.is_empty() {
                    log::warn!("[server] no session with host {ordinal} yet: messages to it are kept until it has one");
                }
                waiting.push(msg);
            }
        }
        id
    }

    /// Every reliable message to another host not acknowledged yet (a session's, or
    /// waiting for one), with its host and id. A session's entries (and a waiting list)
    /// leave only from the front — acknowledged, or forgotten through a cut mark — and a
    /// session is never replaced, so they are always the LAST ones queued to that host.
    fn peer_entries(&self) -> Vec<(u32, u64, &Reliable)> {
        let mut out = Vec::new();
        let lists = self.peers.iter().map(|(o, p)| (*o, p.session.unreceived().collect::<Vec<_>>()));
        let waiting = self.peer_waiting.iter().map(|(o, l)| (*o, l.iter().collect::<Vec<_>>()));
        for (ordinal, list) in lists.chain(waiting) {
            let queued = self.peer_queued.get(&ordinal).copied().unwrap_or(0);
            // (Never short; saturating all the same, so a miscount cannot wrap.)
            let first = queued.saturating_sub(list.len() as u64);
            out.extend(list.into_iter().enumerate().map(|(i, r)| (ordinal, first + i as u64, r)));
        }
        out
    }

    /// `bundle` (in `target`'s frame), a body that is not a player, departed from `rc` at
    /// `stamp` into `target`, which another host owns: it goes to that host, reachable or
    /// not. The receiving host decides everything about its arrival (a person's
    /// `PersonCrossed` included). `view`: departed after `rc` answered the pending cut.
    pub(super) fn depart_to_peer(
        &mut self,
        rc: RegionCoords,
        target: RegionCoords,
        stamp: crate::Tick,
        bundle: crate::EntityBundle,
        view: bool,
    ) {
        // Never a player: the `Departures` arm hands a player off (`hand_off_to_peer`) or
        // back to its source, and a person without a history back to its source.
        debug_assert!(bundle.client.is_none(), "a player crosses hosts by hand_off_to_peer");
        // A person with a history: the receiving host reports the crossing
        // (`PersonCrossed`), never this one (`peer_schedule::person_crosses_host_seam_*`).
        let owner = self.owner_of(target);
        log::debug!("[server] {:?} from {rc:?} into {target:?}: sent to host {owner}", bundle.id);
        // It is on another host now: nothing here routes to it.
        self.locations.remove(&bundle.id);
        self.send_reliable(owner, Reliable::Transfer { target, stamp, bundle: Box::new(bundle), player: None }, view);
    }

    /// What this host sent a host that is NOT a participant of the cut (`participants`:
    /// none for a one-phase cut) and that host has not acknowledged: every transfer
    /// without a player is a row of the cut — unless it left its source after the source
    /// answered the cut (the source's snapshot holds it). Intents and replies alike: each
    /// is an [`InFlightRow::Intent`] to the foreign region it went to, unless its source
    /// sent it after answering the cut. Entries are told apart by their per-host id
    /// (`peer_entries`), never by value: an entity departing twice, or two equal replies,
    /// in one cut window each count once.
    ///
    /// To a participant nothing is written here (`world_cut.rs`): a bare message before
    /// this host's mark is the receiver's; a wrapped one ([`Reliable::InCut`]) is in this
    /// host's snapshot or was written as this part's row when it was sent. A participant's
    /// link that is down aborts the cut, so a sender writes an in-transit row only for a
    /// host absent from the cut (a one-phase cut's peer may also hold an entry it received
    /// and has not acknowledged yet).
    pub(super) fn cut_peer_rows(&self, participants: &BTreeSet<u32>) -> Vec<InFlightRow> {
        self.peer_entries()
            .into_iter()
            .filter(|(ordinal, _, _)| !participants.contains(ordinal))
            .filter(|(ordinal, id, _)| !self.peer_in_source_view.contains(&(*ordinal, *id)))
            .filter_map(|(_, _, r)| match r.inner() {
                // A player handed over or moved is a record (`cut_peer_players`), not a row.
                Reliable::Transfer { target, stamp, bundle, player: None } => {
                    Some(InFlightRow::Transfer { target: *target, bundle: (**bundle).clone(), stamp: *stamp })
                }
                Reliable::Intent(msg) => Some(InFlightRow::Intent { target: peer_intent_target(msg), msg: *msg }),
                _ => None,
            })
            .collect()
    }

    /// Every player handed to a host that is NOT a participant of the cut (one-phase: any
    /// host) and not acknowledged yet — a walking hand-off ([`Reliable::Transfer`] with a
    /// player: its body's record, as the receiver would park it) or a move
    /// ([`Reliable::PlayerMove`]: its record) — as its record, in the region it is headed
    /// for, unless it left its source after the source answered the cut (the source's
    /// snapshot holds it). Not a row: a restore requeues nothing for it, and the stored
    /// record places the player at its next login.
    ///
    /// A player handed to a participant follows the same rule as every state message
    /// (`world_cut.rs`): bare, its record is the receiver's; wrapped, this host's snapshot
    /// holds the body or this part wrote its record when it was sent.
    pub(super) fn cut_peer_players(&self, participants: &BTreeSet<u32>) -> Vec<(PlayerId, PlayerRecord)> {
        self.peer_entries()
            .into_iter()
            .filter(|(ordinal, _, _)| !participants.contains(ordinal))
            .filter(|(ordinal, id, _)| !self.peer_in_source_view.contains(&(*ordinal, *id)))
            .filter_map(|(_, _, r)| match r.inner() {
                Reliable::Transfer { target, bundle, player: Some(h), .. } => {
                    handoff_record(*target, bundle, h).map(|record| (h.player, record))
                }
                Reliable::PlayerMove { handoff, record, .. } => Some((handoff.player, (**record).clone())),
                _ => None,
            })
            .collect()
    }

    /// `msg` (an intent or a reply) to the region `to` another host owns, reliably.
    /// `view`: sent after its source answered the pending cut (not a row of that cut).
    pub(super) fn intent_to_peer(&mut self, to: RegionCoords, msg: crate::IntentMsg, view: bool) {
        debug_assert_eq!(peer_intent_target(&msg), to, "an intent to a peer goes where its message says");
        let owner = self.owner_of(to);
        log::debug!("[server] {msg:?} for {to:?}: sent to host {owner}");
        self.send_reliable(owner, Reliable::Intent(msg), view);
    }

    /// `v` to the host that owns `rc`, if its session is up (else dropped: volatile).
    fn volatile_to_owner(&mut self, rc: RegionCoords, v: Volatile) {
        let owner = self.owner_of(rc);
        if let Some(peer) = self.peers.get_mut(&owner).filter(|p| !p.stale) {
            peer.session.send_volatile(v);
        }
    }

    /// Halo traffic for `to`, a region another host owns: sent only when that host runs
    /// it (halo never wakes a region). Whether it was sent.
    pub(super) fn halo_to_peer(&mut self, to: RegionCoords, v: Volatile) -> bool {
        if !self.foreign_running.contains_key(&to) {
            return false;
        }
        self.volatile_to_owner(to, v);
        true
    }

    /// The halo seed of starting `rc` for its neighbour `n`, which another host owns:
    /// asked of that host when it runs `n` (`n` answers with its live chunks, a
    /// `HaloUpdate` to `rc`). `false` when `n` is not known to run (seeded as a parked
    /// region instead). The other way round (`n`'s mirror of `rc`) the owner asks for
    /// itself on this host's `RegionUp`.
    pub(super) fn foreign_halo_request(&mut self, rc: RegionCoords, n: RegionCoords) -> bool {
        self.halo_to_peer(n, Volatile::HaloRequest { from: rc, of: n })
    }

    /// Whether `rc` borders a region host `peer` owns (one of its 8 neighbours).
    fn borders(&self, rc: RegionCoords, peer: u32) -> bool {
        crate::halo::neighbours(rc).any(|n| !self.owns(n) && self.owner_of(n) == peer)
    }

    /// This host's region `rc` started (`Volatile::RegionUp`), parked (`RegionParked`) or
    /// was lost (`RegionDown`): `what(rc)` told to every host owning one of its neighbours.
    pub(super) fn announce_region(&mut self, rc: RegionCoords, what: fn(RegionCoords) -> Volatile) {
        let ordinals: Vec<u32> = self.peers.keys().copied().filter(|&p| self.borders(rc, p)).collect();
        for ordinal in ordinals {
            let Some(peer) = self.peers.get_mut(&ordinal).filter(|p| !p.stale) else { continue };
            peer.session.send_volatile(what(rc));
        }
    }

    /// Foreign region `n` runs: each of this host's running regions next to it asks it
    /// for its halo seed (whatever `n` sent before this host knew was dropped).
    fn request_seeds_of(&mut self, n: RegionCoords) {
        let mine: Vec<RegionCoords> = crate::halo::neighbours(n)
            .filter(|rc| self.regions.get(rc).is_some_and(|l| !l.stopping))
            .collect();
        for rc in mine {
            self.volatile_to_owner(n, Volatile::HaloRequest { from: rc, of: n });
        }
    }

    /// Host `from`'s region `n` no longer runs as far as this host knows: its mirrors here
    /// end (`HaloSourceGone`).
    fn foreign_region_gone(&mut self, n: RegionCoords) {
        if self.foreign_running.remove(&n).is_some() {
            self.halo_source_lost(n);
        }
    }

    /// The link to host `from` is gone (or it runs a newer generation): every region of
    /// it is gone for this host's halos.
    fn peer_lost(&mut self, from: u32) {
        let gone: Vec<RegionCoords> =
            self.foreign_running.iter().filter(|(_, &h)| h == from).map(|(rc, _)| *rc).collect();
        for n in gone {
            self.foreign_region_gone(n);
        }
    }

    /// Whether this host's region `rc` runs and is not stopping.
    fn runs(&self, rc: RegionCoords) -> bool {
        self.regions.get(&rc).is_some_and(|l| !l.stopping)
    }

    fn peer_volatile(&mut self, from: u32, v: Volatile) {
        match v {
            Volatile::Running(list) => {
                // Replaces what was known of that host.
                let list: BTreeSet<RegionCoords> = list.into_iter().collect();
                let stale: Vec<RegionCoords> = self
                    .foreign_running
                    .iter()
                    .filter(|(rc, &h)| h == from && !list.contains(rc))
                    .map(|(rc, _)| *rc)
                    .collect();
                for n in stale {
                    self.foreign_region_gone(n);
                }
                for n in list {
                    self.foreign_running.insert(n, from);
                    self.request_seeds_of(n);
                }
            }
            Volatile::RegionUp(n) => {
                self.foreign_running.insert(n, from);
                self.request_seeds_of(n);
            }
            Volatile::RegionDown(n) => self.foreign_region_gone(n),
            // Parked: its final halo came before this, and its mirrors end as they do for a
            // neighbour on its own host (that final seed); it is just not running now.
            Volatile::RegionParked(n) => {
                self.foreign_running.remove(&n);
            }
            Volatile::HaloRequest { from: to, of } => {
                if !self.owns(of) {
                    log::warn!("[server] host {from} asked this host for the halo of {of:?}, which it does not own");
                } else if self.runs(of) {
                    self.send_to_region(of, RegionInput::HaloRequest(to));
                } else if !self.regions.contains_key(&of) {
                    // Parked (or never run here): from its last terrain, as a starting
                    // region of this host would be seeded; none cached: dropped (the
                    // requester seeds from the store or worldgen).
                    let seed = self.terrain_cache.get(&of).and_then(|snap| crate::halo::halo_seed_from_snapshot(of, to, &snap));
                    if let Some(update) = seed {
                        let stamp = self.spawner.world_tick();
                        self.halo_to_peer(to, Volatile::HaloUpdate { to, update, stamp });
                    }
                }
                // Stopping: its `Stopped` re-seeds the requester from its final terrain.
            }
            Volatile::HaloUpdate { to, update, stamp } => {
                if self.owns(to) && self.runs(to) {
                    self.send_to_region(to, RegionInput::Transfer(GameEventKind::HaloUpdate(update), stamp));
                }
            }
            Volatile::HaloEntity { to, msg, stamp } => {
                if self.owns(to) && self.runs(to) {
                    self.send_to_region(to, RegionInput::Transfer(msg.into_event(), stamp));
                }
            }
            // An input the client sent to that host after its body left for this one.
            Volatile::Input { client, kind, stamp } => {
                if kind.input_client() != Some(client) {
                    log::warn!("[server] peer host {from}: an input of another client as client {client}'s; dropped");
                    return;
                }
                match self.homes.get(&client).copied().filter(|h| self.regions.contains_key(h)) {
                    Some(home) => {
                        self.send_to_region(home, RegionInput::EventAfterTick(kind, stamp));
                    }
                    None => log::debug!("[server] peer host {from}: input of client {client}, whose body is not here; dropped"),
                }
            }
        }
    }

    fn peer_event(&mut self, from: u32, ev: PeerEvent) {
        match ev {
            PeerEvent::Up(hello) => {
                log::info!("[server] peer host {from} up (boot {})", hello.boot);
                // Where its clients connect (`RegionElsewhere`).
                self.peer_endpoints.insert(from, hello.endpoint.clone());
                // Every region (stopping ones too: their `RegionDown` follows) bordering
                // that host; it asks each for the seeds of its own neighbours of them.
                let running: Vec<RegionCoords> =
                    self.regions.keys().copied().filter(|rc| self.borders(*rc, from)).collect();
                if let Some(peer) = self.peers.get_mut(&from) {
                    peer.session.send_volatile(Volatile::Running(running));
                }
            }
            // May come without an `Up` (a connection broken at the hello).
            PeerEvent::Down => {
                log::info!("[server] peer host {from} down");
                self.peer_lost(from);
                self.world_cut_peer_lost(from);
            }
            PeerEvent::HigherGeneration(generation) => {
                log::error!("[server] peer host {from} runs world generation {generation}, newer than ours: this host is stale");
                self.higher_generation = Some(self.higher_generation.map_or(generation, |g| g.max(generation)));
                if let Some(peer) = self.peers.get_mut(&from) {
                    peer.stale = true;
                }
                self.peer_lost(from);
                self.world_cut_peer_lost(from);
            }
            PeerEvent::Reliable(msg) => self.peer_reliable(from, msg),
            PeerEvent::Volatile(v) => self.peer_volatile(from, v),
        }
    }

    /// A participant's link of the pending world cut went down (or it runs a newer
    /// generation): its mark may never come — this host's part fails.
    fn world_cut_peer_lost(&mut self, from: u32) {
        if self.world_cut.as_ref().is_some_and(|r| r.state == super::world_cut::PartState::Pending && r.peers.contains(&from)) {
            log::warn!("[server] world cut: the link to host {from} went down while the cut was pending");
            self.world_cut_fail(crate::CutFailure::Aborted);
        }
    }

    /// Host `from`'s next reliable message (`world_cut.rs`): a mark is noted; a wrapped
    /// state message ([`Reliable::InCut`]) is applied with what it creates noted as
    /// accounted for by its sender in that cut; a bare state message after `from`'s mark of
    /// the pending world cut is a protocol violation — applied all the same (state is never
    /// dropped), and the cut fails.
    pub(super) fn peer_reliable(&mut self, from: u32, msg: Reliable) {
        match msg {
            Reliable::CutMark { cut } => self.peer_cut_mark(from, cut),
            Reliable::InCut { cut, body } => {
                if matches!(*body, Reliable::InCut { .. }) || !body.is_state() {
                    log::error!("[server] host {from}: a wrapped {} for cut {}; applied as is", reliable_name(&body), cut.0);
                }
                self.note_player_accounted(&body, cut);
                let after = self.accounted_now(cut);
                let lineage = self.inbound_accounted.replace(cut);
                self.peer_reliable_in(from, *body, after);
                self.inbound_accounted = lineage;
            }
            msg => {
                if msg.is_state() && self.after_mark(from) {
                    log::error!(
                        "[server] host {from} sent a bare {} after its mark of the pending world cut: \
                         applied, the cut fails",
                        reliable_name(&msg)
                    );
                    self.world_cut_fail(crate::CutFailure::Aborted);
                }
                self.peer_reliable_in(from, msg, false);
            }
        }
    }

    /// Apply host `from`'s reliable message; `after`: its sender accounts for it in the
    /// pending cut (what it creates here is in no part of this host's).
    fn peer_reliable_in(&mut self, from: u32, msg: Reliable, after: bool) {
        match msg {
            Reliable::Transfer { target, stamp, bundle, player: None } => {
                if !self.owns(target) {
                    // Held here all the same (it waits in the mailbox; nothing is lost).
                    // Task 14: bounce it to the owner instead.
                    log::error!("[server] host {from} sent {:?} into {target:?}, which this host does not own", bundle.id);
                    debug_assert!(false, "an inbound transfer into {target:?}, which this host does not own");
                }
                let now_ms = self.clock_ms;
                self.accept_arrival(None, target, stamp, *bundle, now_ms);
            }
            Reliable::Transfer { target, stamp, bundle, player: Some(handoff) } => {
                if !self.owns(target) {
                    // Held here all the same (as a transfer without a player).
                    log::error!("[server] host {from} handed a player into {target:?}, which this host does not own");
                    debug_assert!(false, "an inbound hand-off into {target:?}, which this host does not own");
                }
                self.accept_handoff(from, target, stamp, *bundle, handoff);
            }
            // For the server edge, which admits the ticket (`take_observer_orders`).
            Reliable::ObserverOrder { client, player, nonce, ttl_secs } => {
                self.observer_orders.push((client, player, nonce, ttl_secs));
            }
            // As if one of this host's regions had sent it: no cut has seen it here, unless
            // it came after its sender's mark (that sender's snapshot holds it).
            Reliable::Intent(msg) => {
                let target = peer_intent_target(&msg);
                if self.owns(target) {
                    self.route_intent(msg, target, after);
                    return;
                }
                // Ownership disagrees: never forwarded (it could bounce between hosts).
                // Task 14: bounce it to the owner instead.
                log::error!("[server] host {from} sent {msg:?} for {target:?}, which this host does not own");
                debug_assert!(false, "an inbound intent for {target:?}, which this host does not own");
                match msg {
                    // Refused back to where it came from: its items return.
                    crate::IntentMsg::Intent(s) => {
                        let reply = crate::IntentReply::Refused(s.intent, crate::IntentRefusal::OwnerNotRunning);
                        let back = crate::SentReply { reply, hint: s.from, due: s.due + 1, hops: 0 };
                        self.route_intent(crate::IntentMsg::Reply(back), target, after);
                    }
                    // A reply is held here, in the mailbox of its region (a cut carries it,
                    // and a restore queues it to the owner): never lost.
                    reply @ crate::IntentMsg::Reply(_) => {
                        self.intent_mailbox.entry(target).or_default().push(Mail { msg: reply, in_source_view: after, accounted_in: self.inbound_accounted });
                    }
                }
            }
            Reliable::PlayerMove { handoff, record, target } => {
                if !self.owns(target) {
                    // Parked here (`accept_move`): a region this host does not own is never
                    // started here, so its body could not be created. Never lost.
                    log::error!("[server] host {from} moved a player into {target:?}, which this host does not own");
                    debug_assert!(false, "an inbound move into {target:?}, which this host does not own");
                }
                self.accept_move(from, handoff, *record, target);
            }
            Reliable::PlayerParked { client, player } => self.player_parked_elsewhere(from, client, player),
            other => {
                log::error!("[server] peer host {from}: {} is not handled by this host yet: dropped", reliable_name(&other))
            }
        }
    }
}

/// The claim a player's fortress / adventure is (it holds one at most).
fn claim_of(fortress: Option<SiteId>, adventure: Option<SiteId>) -> Option<Claim> {
    fortress.map(Claim::Fortress).or(adventure.map(Claim::Adventure))
}

/// A claim as the fortress / adventure pair of a record.
fn split_claim(claim: Option<Claim>) -> (Option<SiteId>, Option<SiteId>) {
    match claim {
        Some(Claim::Fortress(s)) => (Some(s), None),
        Some(Claim::Adventure(s)) => (None, Some(s)),
        None => (None, None),
    }
}

/// The record of a player handed over in `bundle` (its body, in `target`'s frame), as the
/// receiving host parks it: `None` if the bundle carries no player.
pub(super) fn handoff_record(target: RegionCoords, bundle: &crate::EntityBundle, h: &crate::PlayerHandoff) -> Option<PlayerRecord> {
    let (_, carried) = bundle.client.as_ref()?;
    let (fortress, adventure) = split_claim(h.claim);
    Some(PlayerRecord { client: crate::parked_client(carried), pose: bundle.body.pose, region: target, fortress, adventure })
}

/// A server-authored event that was to follow a moved player's creation, applied to the
/// `Client` it carries to another host instead: a mode change, as the region applies it
/// (`set_play_mode_safe`). Nothing else follows a creation.
///
/// Leaving survival here differs from a local move in one way: what does not fit back
/// into the inventory stays on the cursor (the record keeps it; the restore stows it
/// again) instead of being dropped at the player's feet. Only an adventure ended while a
/// move is in flight gets here (`end_adventure` otherwise sends its `SetMode` to the home
/// before the move, where the drop happens as locally), and by then the body is parked or
/// being parked: there is no region and no feet to drop at.
fn carry_event(carried: &mut crate::Client, kind: GameEventKind) {
    match kind {
        GameEventKind::Order(_, Order::SetMode { mode, .. }) => {
            if carried.mode != mode {
                carried.mode = mode;
                if mode == PlayMode::Fortress {
                    if let Some(rest) = carried.leave_survival() {
                        carried.cursor = Some(rest);
                    }
                }
            }
        }
        other => log::error!("[server] {other:?} was to follow a player's creation; it moved to another host: dropped"),
    }
}

/// The region an intent or a reply to another host goes to (`route_intent` without a
/// directory: entities on another host are not in this host's): an intent's position
/// target, a region-sourced intent's reply its region, any other reply its hint.
pub(super) fn peer_intent_target(msg: &crate::IntentMsg) -> RegionCoords {
    match msg {
        crate::IntentMsg::Intent(s) => s.target.region().unwrap_or(s.from),
        crate::IntentMsg::Reply(s) if s.reply.to().is_region() => s.reply.to().birth_region,
        crate::IntentMsg::Reply(s) => s.hint,
    }
}

/// The entity a reliable message carries, if it is a transfer.
fn transfer_id(r: &Reliable) -> Option<crate::EntityId> {
    match r.inner() {
        Reliable::Transfer { bundle, .. } => Some(bundle.id),
        _ => None,
    }
}

/// A reliable message's kind, for logs (bundles are large).
fn reliable_name(r: &Reliable) -> &'static str {
    match r {
        Reliable::Transfer { .. } => "Transfer",
        Reliable::Intent(_) => "Intent",
        Reliable::PlayerMove { .. } => "PlayerMove",
        Reliable::ObserverOrder { .. } => "ObserverOrder",
        Reliable::SpawnOrder { .. } => "SpawnOrder",
        Reliable::SpawnAck { .. } => "SpawnAck",
        Reliable::Kick { .. } => "Kick",
        Reliable::KickAck { .. } => "KickAck",
        Reliable::SessionEnded { .. } => "SessionEnded",
        Reliable::CutMark { .. } => "CutMark",
        Reliable::PlayerParked { .. } => "PlayerParked",
        Reliable::InCut { .. } => "InCut",
    }
}
