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
//! heading for an unreachable host is handed back to its source; the play paths still
//! refuse foreign regions (Task 8). A subscribe to a region another host owns is answered
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

    /// Test support: every [`Reliable::Intent`] (an intent or a reply) not acknowledged
    /// yet — `(peer, msg, on the wire)`, as [`Self::peer_unreceived_ids`].
    pub fn peer_unreceived_intents(&self) -> Vec<(u32, crate::IntentMsg, bool)> {
        let mut out = Vec::new();
        for (&ordinal, peer) in &self.peers {
            let unsent = peer.session.unsent().count();
            let all: Vec<&Reliable> = peer.session.unreceived().collect();
            let sent = all.len() - unsent;
            for (i, r) in all.into_iter().enumerate() {
                if let Reliable::Intent(msg) = r {
                    out.push((ordinal, *msg, i < sent));
                }
            }
        }
        for (&ordinal, list) in &self.peer_waiting {
            out.extend(list.iter().filter_map(|r| match r {
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
        self.nonce_state = nonce_seed;
    }

    /// The observer orders other hosts sent since the last call:
    /// `(client, player, nonce, expires_unix_secs)` — the server edge admits a
    /// `Credential::Observer` that matches one.
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
            self.world_player_online(client);
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
                self.world_send(HostMsg::PlayerOffline { player });
                (player, claim, false)
            }
        };
        log::info!("[server] player {} (client {client}) from {rc:?} into {target:?}: handed to host {owner}", player.0);
        self.homes.remove(&client);
        if let Some(session) = self.sessions.get_mut(&client) {
            session.kind = SessionKind::Observer;
        }
        self.departed.insert(player, owner);
        self.locations.remove(&bundle.id);
        let handoff = crate::PlayerHandoff { client, player, claim, login: None, park };
        let id = self.queue_to_peer(owner, Reliable::Transfer { target, stamp, bundle: Box::new(bundle), player: Some(handoff) });
        if view {
            self.peer_in_source_view.insert((owner, id));
        }
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
        let (fortress, adventure) = match h.claim {
            Some(Claim::Fortress(s)) => (Some(s), None),
            Some(Claim::Adventure(s)) => (None, Some(s)),
            None => (None, None),
        };
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
                self.awaiting_session.insert(c, now_ms);
            }
        }
        // Its home here, `PlayerRegion` to the client.
        self.accept_arrival(None, target, stamp, bundle, now_ms);
        self.world_player_online(c);
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
        let Some(endpoint) = self.peer_endpoints.get(&owner).cloned() else {
            log::warn!("client {client}: {rc:?} is host {owner}'s, whose endpoint is not known; subscribe dropped");
            return;
        };
        if self.sessions.get(&client).and_then(|s| s.player).is_none() {
            log::warn!("client {client}: no player for its session; subscribe to {rc:?} dropped");
            return;
        }
        let ticket = self.observer_ticket(client, owner, self.clock_ms);
        let _ = self.out.send((Some(client), ServerPacket::RegionElsewhere { region: rc, endpoint, ticket }));
    }

    /// The ticket `client` presents to host `owner`: minted once (a fresh random nonce,
    /// the owner told to expect it — [`Reliable::ObserverOrder`], valid for
    /// [`OBSERVER_ORDER_TTL_SECS`]), handed out again while at least half its life is
    /// left, then minted anew.
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
                let expires_unix_secs = unix_now_secs() + OBSERVER_ORDER_TTL_SECS;
                self.queue_to_peer(owner, Reliable::ObserverOrder { client, player, nonce, expires_unix_secs });
            }
            None => log::error!("client {client}: an observer ticket with no player; host {owner} is not told"),
        }
        crate::ObserverTicket { client, nonce }
    }

    /// splitmix64 over the seed [`Self::set_endpoints`] gave.
    fn next_nonce(&mut self) -> u64 {
        self.nonce_state = self.nonce_state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.nonce_state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `msg` to host `ordinal`: on its session, or kept until it has one. Its id among
    /// everything queued to that host (`peer_entries`).
    pub(super) fn queue_to_peer(&mut self, ordinal: u32, msg: Reliable) -> u64 {
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
        // Task 10: a person with a history crossing hosts (the receiving host's
        // `PersonCrossed`) has no seeded test yet — the rig gets a coordinator there.
        let owner = self.owner_of(target);
        log::debug!("[server] {:?} from {rc:?} into {target:?}: sent to host {owner}", bundle.id);
        // It is on another host now: nothing here routes to it.
        self.locations.remove(&bundle.id);
        let id = self.queue_to_peer(owner, Reliable::Transfer { target, stamp, bundle: Box::new(bundle), player: None });
        if view {
            self.peer_in_source_view.insert((owner, id));
        }
    }

    /// One-phase cuts (until Task 13): every transfer to another host not acknowledged
    /// yet, without a player, is a row of the cut — unless it left its source after the
    /// source answered the cut (the source's snapshot holds it).
    ///
    /// Task 13 replaces this with in-band cut marks: today a transfer the peer received
    /// but has not acknowledged is ALSO in the peer's state, so its cut may hold it too.
    ///
    /// Intents and replies alike: each not acknowledged yet is an
    /// [`InFlightRow::Intent`] to the foreign region it went to, unless its source sent it
    /// after answering the cut.
    /// Entries are told apart by their per-host id (`peer_entries`), never by value: an
    /// entity departing twice, or two equal replies, in one cut window each count once.
    pub(super) fn cut_peer_rows(&self) -> Vec<InFlightRow> {
        self.peer_entries()
            .into_iter()
            .filter(|(ordinal, id, _)| !self.peer_in_source_view.contains(&(*ordinal, *id)))
            .filter_map(|(_, _, r)| match r {
                Reliable::Transfer { target, stamp, bundle, player: None } => {
                    Some(InFlightRow::Transfer { target: *target, bundle: (**bundle).clone(), stamp: *stamp })
                }
                Reliable::Intent(msg) => Some(InFlightRow::Intent { target: peer_intent_target(msg), msg: *msg }),
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
        let id = self.queue_to_peer(owner, Reliable::Intent(msg));
        if view {
            self.peer_in_source_view.insert((owner, id));
        }
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
            }
            PeerEvent::HigherGeneration(generation) => {
                log::error!("[server] peer host {from} runs world generation {generation}, newer than ours: this host is stale");
                self.higher_generation = Some(self.higher_generation.map_or(generation, |g| g.max(generation)));
                if let Some(peer) = self.peers.get_mut(&from) {
                    peer.stale = true;
                }
                self.peer_lost(from);
            }
            PeerEvent::Reliable(msg) => self.peer_reliable(from, msg),
            PeerEvent::Volatile(v) => self.peer_volatile(from, v),
        }
    }

    fn peer_reliable(&mut self, from: u32, msg: Reliable) {
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
            Reliable::ObserverOrder { client, player, nonce, expires_unix_secs } => {
                self.observer_orders.push((client, player, nonce, expires_unix_secs));
            }
            // As if one of this host's regions had sent it (no cut has seen it here).
            Reliable::Intent(msg) => {
                let target = peer_intent_target(&msg);
                if self.owns(target) {
                    self.route_intent(msg, target, false);
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
                        self.route_intent(crate::IntentMsg::Reply(back), target, false);
                    }
                    // A reply is held here, in the mailbox of its region (a cut carries it,
                    // and a restore queues it to the owner): never lost.
                    reply @ crate::IntentMsg::Reply(_) => {
                        self.intent_mailbox.entry(target).or_default().push(Mail { msg: reply, in_source_view: false });
                    }
                }
            }
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

/// Seconds since the Unix epoch: when an observer order expires at the edge, which
/// compares it with its own wall clock. No wall clock on wasm (no peers there either): 0.
fn unix_now_secs() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
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
    match r {
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
    }
}
