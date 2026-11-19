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
//! heading for an unreachable host is handed back to its source. A **move** (Play, an
//! adventure's start or end) into a region another host owns parks the body here as
//! a local move does, then hands the player over as a record ([`Reliable::PlayerMove`] —
//! queued whether or not that host is reachable: the record waits there, conserved); the
//! receiving host creates its body at the target's landing. A host that parks a player
//! handed to it with no session of the client tells the host it came from
//! ([`Reliable::PlayerParked`]), which ends its observer session of the client. A
//! subscribe to a region another host owns is answered
//! with that host's endpoint and an observer ticket ([`crate::ServerPacket::RegionElsewhere`]),
//! the owner told to expect it ([`Reliable::ObserverOrder`]).

use super::*;
use crate::HostId;
use crate::{PeerEvent, PeerLink, PeerSession, Reliable, Volatile};

/// A message waiting for its region to have an owner this host knows
/// ([`WorldManager::wait_for_owner`]), with the cut flags a mailbox entry has.
pub(super) struct UnownedWait {
    msg: Reliable,
    /// In its source's snapshot of the pending cut (not a row of it).
    pub(super) in_source_view: bool,
    /// The cut its sender accounts for it in (it came wrapped, [`Reliable::InCut`]).
    pub(super) accounted_in: Option<crate::CutId>,
}

/// One other host: the session with it and the link it runs over.
pub(super) struct Peer {
    pub(super) session: PeerSession,
    pub(super) link: Box<dyn PeerLink + Send>,
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
    pub fn set_peer(&mut self, ordinal: HostId, mut session: PeerSession, link: Box<dyn PeerLink + Send>) {
        debug_assert_eq!(session.peer(), ordinal, "a session for another host");
        if self.forgotten_hosts.contains(&ordinal) {
            // A zombie of a forgotten host dialing in: never linked again (its id is never
            // issued twice; the link is dropped).
            log::warn!("[server] a link to host {ordinal}, which is forgotten: dropped");
            return;
        }
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
        // A boot of that host declared dead (a succession) stays refused.
        for &(_, boot) in self.dead_boots.iter().filter(|(o, _)| *o == ordinal) {
            session.refuse_boot(boot);
        }
        for msg in self.peer_waiting.remove(&ordinal).unwrap_or_default() {
            session.queue(msg);
        }
        if self.count_pairs.contains_key(&ordinal) {
            // A pending world cut counts with that host (its count, 0, already went out):
            // the new session stays frozen until the cut is decided.
            session.freeze();
        }
        if self.world_cut_outsider_arrived(ordinal) {
            // A host outside the pending world cut (it registered after the cut began):
            // nothing goes either way until the cut is decided (`world_cut.rs`).
            session.freeze();
        }
        self.peers.insert(ordinal, Peer { session, link, stale: false });
    }

    /// Drive every peer session: read what the other hosts sent and act on it, then send
    /// what is queued. Called first by [`Self::maintain`].
    pub fn pump_peers(&mut self, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        let ordinals: Vec<HostId> = self.peers.keys().copied().collect();
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
        // A peer may have finished its sync: spawn orders waiting for it go on.
        self.retry_held_spawns();
        // Land needed here with no reachable owner: asked of the coordinator (`land.rs`).
        self.ask_for_land(now_ms);
    }

    /// Who simulates `rc`, as this host knows it.
    pub fn owner_of(&self, rc: RegionCoords) -> crate::Owner {
        self.owner_of_sr(crate::super_region_of(rc, self.ownership.1))
    }

    /// Who simulates super-region `sr`, as this host knows it — `Unknown` for one it
    /// inherits by a succession it has not adopted yet (`succession.rs`, `inheriting`), and for
    /// one its pending world-cut part releases (`land.rs`, `releasing`).
    pub(super) fn owner_of_sr(&self, sr: crate::SuperRegion) -> crate::Owner {
        // Land being inherited, and land this host's pending part releases (`land.rs`): no
        // region of it starts until the cut is decided.
        if self.inheriting.contains_key(&sr) || self.releasing.contains_key(&sr) {
            return crate::Owner::Unknown;
        }
        self.ownership.0.owner_of(sr)
    }

    /// The host that simulates `rc` (this one included); `None` while nobody owns it or
    /// this host does not know who does.
    pub fn owner_host(&self, rc: RegionCoords) -> Option<HostId> {
        match self.owner_of(rc) {
            crate::Owner::Me => Some(self.ordinal()),
            crate::Owner::Host(h) => Some(h),
            crate::Owner::Free | crate::Owner::Unknown => None,
        }
    }

    /// The other host that owns `rc`, if another host does.
    pub(super) fn foreign_host(&self, rc: RegionCoords) -> Option<HostId> {
        match self.owner_of(rc) {
            crate::Owner::Host(h) => Some(h),
            _ => None,
        }
    }

    /// Whether another host owns `rc`.
    pub fn is_foreign(&self, rc: RegionCoords) -> bool {
        self.foreign_host(rc).is_some()
    }

    /// Whether something can be put into `rc` now: this host owns it, or its owner's
    /// session is reachable. A region nobody owns, or whose owner this host does not
    /// know, is reached by nobody ("wait, never guess").
    pub fn reaches(&self, rc: RegionCoords) -> bool {
        match self.owner_of(rc) {
            crate::Owner::Me => true,
            crate::Owner::Host(h) => self.peer_reachable(h),
            crate::Owner::Free | crate::Owner::Unknown => false,
        }
    }

    /// Whether host `ordinal`'s session is reachable now (and it is not of a newer
    /// generation, nor joined to this host by a leave: `world_cut.rs`).
    pub(super) fn peer_reachable(&self, ordinal: HostId) -> bool {
        self.peers.get(&ordinal).is_some_and(|p| !p.stale && p.session.is_reachable()) && !self.leave_blocks(ordinal)
    }

    /// Reliable messages to other hosts not acknowledged yet, those waiting for a session
    /// included (the `[server]` counters line; 0 once everything arrived).
    pub fn peer_unreceived(&self) -> usize {
        self.peers.values().map(|p| p.session.unreceived().count()).sum::<usize>()
            + self.peer_waiting.values().map(Vec::len).sum::<usize>()
            + self.leave_pairs.values().map(|p| p.held.len()).sum::<usize>()
            + self.count_pairs.values().map(|p| p.held.len()).sum::<usize>()
    }

    /// What a leave, or a world cut counted across a down link, holds back for each host
    /// (`world_cut.rs`), never sent yet.
    fn leave_held(&self) -> impl Iterator<Item = (HostId, &Reliable)> {
        let counted = self.count_pairs.iter().flat_map(|(o, p)| p.held.iter().map(move |r| (*o, r)));
        self.leave_pairs.iter().flat_map(|(o, p)| p.held.iter().map(move |r| (*o, r))).chain(counted)
    }

    /// Per host, what waits for it outside a session, never sent: waiting for a session
    /// (`peer_waiting`), or held back by a leave or a counted cut.
    fn never_sent(&self) -> impl Iterator<Item = (HostId, Vec<&Reliable>)> {
        let waiting = self.peer_waiting.iter().map(|(o, l)| (*o, l.iter().collect::<Vec<_>>()));
        let counted = self.count_pairs.iter().map(|(o, p)| (*o, p.held.iter().collect::<Vec<_>>()));
        waiting.chain(self.leave_pairs.iter().map(|(o, p)| (*o, p.held.iter().collect::<Vec<_>>()))).chain(counted)
    }

    /// Test support: the entity of every [`Reliable::Transfer`] not acknowledged yet —
    /// `(peer, id, on the wire)`; `false` = never sent (queued, or waiting for a session),
    /// `true` = sent, so the peer may already hold it.
    pub fn peer_unreceived_ids(&self) -> Vec<(HostId, crate::EntityId, bool)> {
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
        out.extend(self.leave_held().filter_map(|(o, r)| transfer_id(r).map(|id| (o, id, false))));
        out
    }

    /// Test support: the client and player of every [`Reliable::PlayerMove`] not
    /// acknowledged yet — `(peer, client, player, on the wire)`, as
    /// [`Self::peer_unreceived_ids`].
    pub fn peer_unreceived_moves(&self) -> Vec<(HostId, ClientId, PlayerId, bool)> {
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
        for (ordinal, list) in self.never_sent() {
            out.extend(list.into_iter().filter_map(|r| match r.inner() {
                Reliable::PlayerMove { handoff, .. } => Some((ordinal, handoff.client, handoff.player, false)),
                _ => None,
            }));
        }
        out
    }

    /// Test support: every [`Reliable::Intent`] (an intent or a reply) not acknowledged
    /// yet — `(peer, msg, on the wire)`, as [`Self::peer_unreceived_ids`].
    pub fn peer_unreceived_intents(&self) -> Vec<(HostId, crate::IntentMsg, bool)> {
        let mut out = Vec::new();
        for (&ordinal, peer) in &self.peers {
            let unsent = peer.session.unsent().count();
            let all: Vec<&Reliable> = peer.session.unreceived().collect();
            let sent = all.len() - unsent;
            for (i, r) in all.into_iter().enumerate() {
                out.extend(intents_in(r).into_iter().map(|msg| (ordinal, msg, i < sent)));
            }
        }
        for (ordinal, list) in self.never_sent() {
            out.extend(list.into_iter().flat_map(|r| intents_in(r).into_iter().map(move |msg| (ordinal, msg, false))));
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
    /// [`crate::ServerPacket::Host`]), and where observer nonces come from: a server passes
    /// the OS's randomness (a nonce must be unpredictable from any number of earlier ones),
    /// a seeded test [`seeded_nonces`].
    pub fn set_endpoints(&mut self, mine: crate::HostEndpoint, nonces: NonceSource) {
        self.endpoint = mine;
        self.nonces = Some(nonces);
    }

    /// The clients whose session here became (or started as) their home since the last
    /// call: the server edge revokes their observer orders, and an observer connection of
    /// theirs may no longer replace the home one.
    pub fn take_observer_revocations(&mut self) -> Vec<ClientId> {
        std::mem::take(&mut self.observer_revocations)
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

    /// The adventure `client`'s player plays here, if any.
    pub fn client_claim(&self, client: ClientId) -> Option<Claim> {
        claim_of(self.adventurer_of.get(&client).copied())
    }

    /// The region of `site` as this host knows it (the coordinator's view and answers).
    pub fn site_region(&self, site: SiteId) -> Option<RegionCoords> {
        self.site_regions.get(&site).copied()
    }

    /// The players this host handed to another, with that host.
    pub fn departed(&self) -> &BTreeMap<PlayerId, HostId> {
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
        self.sessions.insert(client, Session { kind, player: Some(player), idle_since_ms: now_ms, ..Session::default() });
        if home {
            log::info!("client {client}: connected after its body was handed here; the session is its home");
            self.observer_revocations.push(client);
            let region = self.homes.get(&client).copied();
            self.send_home_hop(client, player);
            let _ = self.out.send((Some(client), ServerPacket::PlayerRegion(region, client)));
            self.send_home_state(client, now_ms);
            if let Some(rc) = region {
                self.refresh_keepalive(rc, now_ms);
            }
            // Its `PlayerOnline` went when the body arrived (`accept_handoff`).
        }
    }

    /// The host `client`'s player was handed to, if this host handed it away.
    pub(super) fn departed_host(&self, client: ClientId) -> Option<HostId> {
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
            // A login ordered here waits for its session: an online player stays (one
            // already parking for it goes on as a park, `hand_off_to_peer`).
            && !(self.player_of.contains_key(&client) && self.spawn_pinned(player))
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
        let owner = self.foreign_host(target).expect("can_hand_off: another host owns the target");
        let (player, claim, park) = match self.parking.remove(&client) {
            // Disconnected, its park on the way to `rc`: parked where it lands.
            Some(p) => (p.player, claim_of(p.adventure), true),
            None => {
                let player = self.player_of.remove(&client).expect("can_hand_off: online or parking");
                let claim = claim_of(self.adventurer_of.remove(&client));
                // Not offline: in transit, its claim kept in use until `owner` reports it.
                self.world_handed(player, owner);
                self.forget_claim_reports(player);
                (player, claim, false)
            }
        };
        let hop = self.hop_here(player) + 1;
        // Its login session goes with it (it is still in the world).
        let login = self.logins.remove(&client);
        log::info!("[server] player {} (client {client}) from {rc:?} into {target:?}: handed to host {owner}", player.0);
        self.homes.remove(&client);
        // Handed here without a session and on again before it came: not awaited here.
        self.awaiting_session.remove(&client);
        self.name_new_home(client, target, owner);
        if let Some(session) = self.sessions.get_mut(&client) {
            session.kind = SessionKind::Observer;
            session.idle_since_ms = now_ms;
        }
        self.forget_home_answers(client);
        self.departed.insert(player, owner);
        self.locations.remove(&bundle.id);
        let handoff = crate::PlayerHandoff { client, player, claim, login, park, hop };
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
        from: HostId,
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
        let adventure = split_claim(h.claim);
        let login = h.login.clone();
        if self.is_unavailable(target) || !self.owns(target) || self.leave_parked {
            // Its stored state could not be read (retry not come), the region is not this
            // host's, or this host parked everything to leave: the body cannot go there.
            // Parked from the hand-off, as a player crossing into a region whose load fails
            // is (`region_load_failed`); `park_handed` reports it offline.
            let record = handoff_record(target, &bundle, &h).expect("checked above");
            if self.masked_or_released(target) {
                // A survivor that applied the succession hands it here before this heir
                // adopted the land, or a peer not told yet of a release here: expected,
                // nothing is lost (the player logs in again).
                log::info!("[server] player {} handed into {target:?}, inherited or released here: parked", h.player.0);
            } else {
                log::error!("[server] player {} handed into {target:?}, which cannot take it now: parked", h.player.0);
            }
            self.park_handed(from, c, h.player, record, now_ms);
            self.login_parked_on_arrival(login);
            return;
        }
        if h.park {
            // `accept_arrival` parks it where it lands (`ParkPlayer` behind the arrival).
            self.parking.insert(c, Parking { player: h.player, region: target, adventure });
            self.accept_arrival(None, target, stamp, bundle, now_ms);
            self.login_parked_on_arrival(login);
            return;
        }
        self.player_of.insert(c, h.player);
        if let Some(site) = adventure {
            self.adventurer_of.insert(c, site);
        }
        let promoted = self.promote_session(c, from, now_ms);
        if promoted {
            self.send_home_hop(c, h.player);
        }
        // Its home here, `PlayerRegion` to the client.
        self.accept_arrival(None, target, stamp, bundle, now_ms);
        if promoted {
            self.send_home_state(c, now_ms);
        }
        self.claim_changed(h.player);
        self.world_player_online(c);
        self.login_arrived(c, login);
    }

    /// The move of `client` (`m`) into `m.target`, which another host owns, is answered
    /// (`record` = its park out of `m.from`; `None` = no body came back, the region was
    /// lost): the player goes to that host as a record ([`Reliable::PlayerMove`]: its
    /// `Client`, its claims, standing at the move's landing) — queued whether or not that
    /// host is reachable, so it waits there, conserved. This host keeps only where it went
    /// (`departed`); the client's session here becomes an observer and is sent on to that
    /// host ([`crate::ServerPacket::RegionElsewhere`]).
    pub(super) fn move_to_peer(&mut self, client: ClientId, m: Moving, record: Option<Box<PlayerRecord>>, now_ms: u64) {
        let owner = self.foreign_host(m.target).expect("finish_move: another host owns the target");
        // A move belongs to an online player (a disconnect turns it into a park).
        let Some(player) = self.player_of.remove(&client) else {
            log::error!("move of client {client} into {:?}: no player; nothing to hand over", m.target);
            debug_assert!(false, "a move with no player");
            return;
        };
        let claim = claim_of(self.adventurer_of.remove(&client));
        // Not offline: in transit, its claim kept in use until `owner` reports it.
        self.world_handed(player, owner);
        self.forget_claim_reports(player);
        let hop = self.hop_here(player) + 1;
        let login = self.logins.remove(&client);
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
        let adventure = split_claim(claim);
        let (x, z) = crate::block_column_xz(m.landing);
        let pose = crate::Pose::from_translation(rapier3d::math::Vector::new(x, y, z));
        let record = PlayerRecord { client: carried, pose, region: m.target, adventure };
        log::info!(
            "[server] player {} (client {client}) moves from {:?} into {:?}: handed to host {owner}",
            player.0,
            m.from,
            m.target
        );
        self.homes.remove(&client);
        self.awaiting_session.remove(&client);
        self.name_new_home(client, m.target, owner);
        if let Some(session) = self.sessions.get_mut(&client) {
            session.kind = SessionKind::Observer;
            session.idle_since_ms = now_ms;
        }
        self.forget_home_answers(client);
        self.departed.insert(player, owner);
        let handoff = crate::PlayerHandoff { client, player, claim, login, park: false, hop };
        // Parked after `m.from` answered the pending cut: its snapshot holds the body.
        let view = self.in_cut_view(m.from);
        self.send_reliable(owner, Reliable::PlayerMove { handoff, record: Box::new(record), target: m.target }, view);
        self.refresh_keepalive(m.from, now_ms);
        let _ = self.redirect(client, m.target);
    }

    /// A player moved here by host `from` ([`Self::move_to_peer`]): this host's now — its
    /// home session if the client is connected here, else awaited
    /// ([`HANDOFF_SESSION_GRACE_MS`]) — its body created at `target`'s landing with what it
    /// carries, as a move within one host creates it. A `target` that cannot be loaded
    /// parks the record here ([`Self::park_handed`]).
    fn accept_move(&mut self, from: HostId, h: crate::PlayerHandoff, record: PlayerRecord, target: RegionCoords) {
        let now_ms = self.clock_ms;
        let c = h.client;
        log::info!("[server] player {} (client {c}) moved here by host {from} into {target:?}", h.player.0);
        self.departed.remove(&h.player);
        if self.holds_player(h.player) {
            return self.drop_arriving_copy(from, h.player, h.hop);
        }
        self.placed_by_handoff(h.player, h.hop);
        if self.is_unavailable(target) || !self.owns(target) || self.leave_parked {
            if self.masked_or_released(target) {
                log::info!("[server] player {} moved into {target:?}, inherited or released here: parked", h.player.0);
            } else {
                log::error!("[server] player {} moved into {target:?}, unavailable, not this host's or leaving: parked", h.player.0);
            }
            self.park_handed(from, c, h.player, record, now_ms);
            self.login_parked_on_arrival(h.login);
            return;
        }
        self.player_of.insert(c, h.player);
        if let Some(site) = split_claim(h.claim) {
            self.adventurer_of.insert(c, site);
        }
        let promoted = self.promote_session(c, from, now_ms);
        // In no region while its creation is on its way to the target.
        self.cut_transit(h.player, &record);
        let landing = self.landing_in(target);
        self.homes.insert(c, target);
        self.creating.insert(c, (target, h.player, record.clone()));
        self.send_play_event(target, GameEventKind::CreateClientAt(c, landing, Some(Box::new(record.client))));
        if promoted {
            self.send_home_hop(c, h.player);
        }
        let _ = self.out.send((Some(c), ServerPacket::PlayerRegion(Some(target), c)));
        if promoted {
            self.send_home_state(c, now_ms);
        }
        self.refresh_keepalive(target, now_ms);
        self.claim_changed(h.player);
        self.world_player_online(c);
        self.login_arrived(c, h.login);
    }

    /// `client`'s session here is about to be told (`PlayerRegion`) it became the home of
    /// `player`, handed here from another host: the player's hop here goes first
    /// ([`ServerPacket::HomeHop`]), so a client with connections to several hosts can tell
    /// this newer home from an older one whose `PlayerRegion` arrives after it.
    fn send_home_hop(&self, client: ClientId, player: PlayerId) {
        let _ = self.out.send((Some(client), ServerPacket::HomeHop(self.hop_here(player))));
    }

    /// `c`'s body was handed here by host `from`: its session here (an observer, if any)
    /// becomes the home — whether it did — else the session is awaited
    /// ([`HANDOFF_SESSION_GRACE_MS`]).
    fn promote_session(&mut self, c: ClientId, from: HostId, now_ms: u64) -> bool {
        match self.sessions.get_mut(&c) {
            Some(session) => {
                session.kind = SessionKind::Home;
                self.observer_revocations.push(c);
                true
            }
            None => {
                self.awaiting_session.insert(c, (now_ms, from));
                false
            }
        }
    }

    /// `player`'s record when this host holds the player ONLY as a parked record (not online,
    /// parking, being created or awaiting its park).
    pub(super) fn parked_only(&self, player: PlayerId) -> Option<&PlayerRecord> {
        let record = self.parked_players.get(&player)?;
        let live = self.player_of.values().any(|p| *p == player)
            || self.parking.values().any(|p| p.player == player)
            || self.creating.values().any(|(_, p, _)| *p == player)
            || self.awaiting_park.contains_key(&player);
        (!live).then_some(record)
    }

    /// Whether this host already holds `player`: online (any session), parked, parking,
    /// moving, its body being created, awaited after a hand-off, or a login of it waiting
    /// for a park.
    pub(super) fn holds_player(&self, player: PlayerId) -> bool {
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
    fn drop_arriving_copy(&mut self, from: HostId, player: PlayerId, hop: u64) {
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
    fn park_handed(&mut self, from: HostId, client: ClientId, player: PlayerId, record: PlayerRecord, now_ms: u64) {
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
    fn tell_parked(&mut self, from: HostId, client: ClientId, player: PlayerId) {
        if from == self.ordinal() {
            // Handed here by a host declared dead since (a succession): nobody to tell.
            log::info!("[server] player {} (client {client}) parked here with no session", player.0);
            return;
        }
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
    fn player_parked_elsewhere(&mut self, from: HostId, client: ClientId, player: PlayerId) {
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

    /// `client` subscribes to `rc`, which this host does not own: told where to go — the
    /// host that owns it, with a ticket that host expects ([`Self::observer_ticket`]) — or,
    /// while no owner is known or the owner is unreachable (a link down, a host dead and its
    /// succession not applied, a host that left), remembered (`Session::asked_elsewhere`)
    /// and answered by [`Self::answer_asked_elsewhere`] as soon as it can be ("wait, never
    /// guess"). An observer session is never redirected from here (its client reads no
    /// `RegionElsewhere` from this connection): every subscribe it cannot serve waits for
    /// this host to own the region — the heir of a host whose succession it has not applied
    /// or adopted yet. One that belongs to yet another host waits until the client lets it
    /// go: a client re-asks its home for a region an observer connection has not served
    /// within `OBSERVER_RETRY_MAX_MS`, releasing it there.
    pub(super) fn subscribe_elsewhere(&mut self, client: ClientId, rc: RegionCoords) {
        let Some(kind) = self.sessions.get(&client).map(|s| s.kind) else { return };
        if kind == SessionKind::Observer {
            if !self.reaches(rc) {
                self.note_unowned(rc);
            }
            log::debug!("client {client}: {rc:?} ({:?}) is not served here; its observer subscribe waits", self.owner_of(rc));
            self.remember_asked(client, rc);
            return;
        }
        if !self.reaches(rc) {
            // With no owner known it is wanted for a claim.
            self.note_unowned(rc);
            log::debug!("client {client}: {rc:?}'s owner ({:?}) is not reachable; the subscribe waits", self.owner_of(rc));
            self.remember_asked(client, rc);
            return;
        }
        match self.redirect(client, rc) {
            Redirect::Sent(owner) => self.redirected_to(client, rc, owner),
            Redirect::Never => {}
            Redirect::Later => self.remember_asked(client, rc),
        }
    }

    /// `client`'s body is about to be handed to host `owner`, into `target`: unless this
    /// home session already sent the client there for `target`, it does now — the last
    /// thing it says as the home. Land claimed a moment ago is the case: the client's
    /// subscribe waited here while nobody owned it (`asked_elsewhere`), and the hand-off
    /// that follows the claim forgets what waited ([`Self::forget_home_answers`]); a client
    /// never told has no connection to `owner`, where its player would park for want of a
    /// session (`HANDOFF_SESSION_GRACE_MS`).
    fn name_new_home(&mut self, client: ClientId, target: RegionCoords, owner: HostId) {
        if self.sessions.get(&client).map(|s| s.kind) != Some(SessionKind::Home) {
            return;
        }
        if self.redirected.get(&(client, target)) == Some(&owner) {
            return;
        }
        if let Redirect::Sent(owner) = self.redirect(client, target) {
            self.redirected_to(client, target, owner);
        }
    }

    /// `client`'s subscribe of `rc` waits here (`Session::asked_elsewhere`); the client
    /// takes the region from this connection until it is answered.
    fn remember_asked(&mut self, client: ClientId, rc: RegionCoords) {
        self.redirected.remove(&(client, rc));
        if let Some(session) = self.sessions.get_mut(&client) {
            session.asked_elsewhere.insert(rc);
        }
    }

    /// `client`'s home session here sent it to host `owner` for `rc`.
    fn redirected_to(&mut self, client: ClientId, rc: RegionCoords, owner: HostId) {
        if let Some(session) = self.sessions.get_mut(&client) {
            session.asked_elsewhere.remove(&rc);
        }
        self.redirected.insert((client, rc), owner);
    }

    /// `client`'s session here is not its home any more (its body went to another host): what
    /// it remembered for the client as its home goes (`asked_elsewhere`, `redirected`) — it
    /// does not travel with the body; the client asks its new home for what it still lacks.
    pub(super) fn forget_home_answers(&mut self, client: ClientId) {
        self.redirected.retain(|(c, _), _| *c != client);
        if let Some(session) = self.sessions.get_mut(&client) {
            session.asked_elsewhere.clear();
        }
    }

    /// Every remembered subscribe (`Session::asked_elsewhere`) this host can answer now is
    /// answered: one this host owns now is subscribed here — after a `RegionElsewhere`
    /// naming this host when the client takes the region from another host's connection
    /// (`redirected`: an heir answering for land it inherited) — and a home session's one
    /// whose owner is known and reachable is sent there with a fresh ticket. The others
    /// stay. Logged per batch.
    pub(super) fn answer_asked_elsewhere(&mut self) {
        let waiting: Vec<(ClientId, RegionCoords, SessionKind)> = self
            .sessions
            .iter()
            .flat_map(|(c, s)| s.asked_elsewhere.iter().map(move |rc| (*c, *rc, s.kind)))
            .collect();
        if waiting.is_empty() {
            return;
        }
        let (mut served, mut named_here, mut sent) = (0usize, 0usize, 0usize);
        for (client, rc, kind) in waiting {
            if self.owns(rc) {
                if kind == SessionKind::Home && self.redirected.contains_key(&(client, rc)) {
                    // The client takes it from another host's connection: told to take it
                    // here; its subscribe on this connection follows (`subscribe`).
                    if let Some(session) = self.sessions.get_mut(&client) {
                        session.asked_elsewhere.remove(&rc);
                    }
                    self.redirected.remove(&(client, rc));
                    let endpoint = self.endpoint.clone();
                    // Never presented: a client opens no connection to its home host.
                    let ticket = crate::ObserverTicket { client, nonce: 0 };
                    let _ = self.out.send((Some(client), ServerPacket::RegionElsewhere { region: rc, endpoint, ticket }));
                    named_here += 1;
                } else {
                    self.subscribe(client, rc);
                    served += 1;
                }
                continue;
            }
            if kind != SessionKind::Home || !self.reaches(rc) {
                continue;
            }
            if self.foreign_host(rc).is_some_and(|owner| !self.peer_endpoints.contains_key(&owner)) {
                // Its endpoint comes with its hello (`redirect` would warn every pass).
                continue;
            }
            match self.redirect(client, rc) {
                Redirect::Sent(owner) => {
                    self.redirected_to(client, rc, owner);
                    sent += 1;
                }
                Redirect::Later => {}
                Redirect::Never => {
                    if let Some(session) = self.sessions.get_mut(&client) {
                        session.asked_elsewhere.remove(&rc);
                    }
                }
            }
        }
        if served + named_here + sent > 0 {
            log::info!(
                "[server] remembered subscribes answered: {served} served here, {named_here} named this host (inherited), \
                 {sent} sent to their owner"
            );
        }
    }

    /// Host `pred` was succeeded (its land is the heir's now): every region a home session
    /// here was sent to it for is answered again (`redirected`, the entry kept as where the
    /// client takes the region from until then) — now, or when it can be.
    pub(super) fn reanswer_redirected_to(&mut self, pred: HostId) {
        let to_pred: Vec<(ClientId, RegionCoords)> =
            self.redirected.iter().filter(|(_, o)| **o == pred).map(|(k, _)| *k).collect();
        if to_pred.is_empty() {
            return;
        }
        log::info!("[server] {} region(s) clients were sent to host {pred} for are answered again", to_pred.len());
        for (client, rc) in to_pred {
            match self.sessions.get_mut(&client) {
                Some(session) if session.kind == SessionKind::Home => {
                    session.asked_elsewhere.insert(rc);
                }
                _ => {
                    self.redirected.remove(&(client, rc));
                }
            }
        }
        self.answer_asked_elsewhere();
    }

    /// `client` is told to go to the host that owns `rc` (its endpoint as last known, a
    /// ticket that host is told to expect — queued like everything to it).
    fn redirect(&mut self, client: ClientId, rc: RegionCoords) -> Redirect {
        let Some(owner) = self.foreign_host(rc) else {
            log::warn!("client {client}: {rc:?} is not another host's ({:?}); subscribe dropped", self.owner_of(rc));
            return Redirect::Never;
        };
        let Some(endpoint) = self.peer_endpoints.get(&owner).cloned() else {
            // Per subscribe (a client re-asks): debug only; it waits (`asked_elsewhere`).
            log::debug!("client {client}: {rc:?} is host {owner}'s, whose endpoint is not known yet; the subscribe waits");
            return Redirect::Later;
        };
        if self.sessions.get(&client).and_then(|s| s.player).is_none() {
            log::warn!("client {client}: no player for its session; subscribe to {rc:?} dropped");
            return Redirect::Never;
        }
        if self.nonces.is_none() {
            // No source (`set_endpoints` not called): no ticket.
            if !self.unseeded_warned {
                self.unseeded_warned = true;
                log::error!("[server] no nonce source (set_endpoints): subscribes to other hosts' regions are dropped");
            }
            return Redirect::Never;
        }
        let ticket = self.observer_ticket(client, owner, self.clock_ms);
        let _ = self.out.send((Some(client), ServerPacket::RegionElsewhere { region: rc, endpoint, ticket }));
        Redirect::Sent(owner)
    }

    /// Test support: the regions `client`'s session here waits to answer
    /// (`Session::asked_elsewhere`), and those it sent the client elsewhere for, with the host.
    #[doc(hidden)]
    pub fn client_answers(&self, client: ClientId) -> (Vec<RegionCoords>, Vec<(RegionCoords, HostId)>) {
        let asked = self.sessions.get(&client).map(|s| s.asked_elsewhere.iter().copied().collect()).unwrap_or_default();
        let redirected = self.redirected.iter().filter(|((c, _), _)| *c == client).map(|((_, rc), o)| (*rc, *o)).collect();
        (asked, redirected)
    }

    /// The ticket `client` presents to host `owner`: minted once (a fresh random nonce,
    /// the owner told to expect it — [`Reliable::ObserverOrder`], valid for
    /// [`OBSERVER_ORDER_TTL_SECS`] from its receipt there), handed out again while at
    /// least half its life is left (by this host's `now_ms`), then minted anew.
    fn observer_ticket(&mut self, client: ClientId, owner: HostId, now_ms: u64) -> crate::ObserverTicket {
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
                self.queue_to_peer(owner, Reliable::ObserverOrder { client, player, nonce: crate::Nonce(nonce), ttl_secs });
            }
            None => log::error!("client {client}: an observer ticket with no player; host {owner} is not told"),
        }
        crate::ObserverTicket { client, nonce }
    }

    /// The next nonce of the source [`Self::set_endpoints`] gave (callers check there is one).
    fn next_nonce(&mut self) -> u64 {
        self.nonces.as_mut().map_or(0, |next| next())
    }

    /// `msg` to host `ordinal` (a state message departing after its source answered goes by
    /// [`Self::send_reliable`]): on its session, or kept until it has one.
    pub(super) fn queue_to_peer(&mut self, ordinal: HostId, msg: Reliable) {
        self.send_reliable(ordinal, msg, false);
    }

    /// `msg` to host `ordinal`; `view`: a transfer, intent or move that departed after its
    /// source answered the pending cut (its source's snapshot holds it). A state message to
    /// a participant of a world cut goes bare or wrapped ([`Reliable::InCut`]) by who
    /// accounts for it (`world_cut.rs`, [`Self::world_cut_wrap`]); `view` keeps it out of
    /// a one-phase cut's rows.
    pub(super) fn send_reliable(&mut self, ordinal: HostId, msg: Reliable, view: bool) {
        if self.forgotten_hosts.contains(&ordinal) {
            return self.to_forgotten(ordinal, msg, view);
        }
        let msg = match self.world_cut_wrap(ordinal, msg, view) {
            (msg, Some(cut)) => Reliable::InCut { cut, body: Box::new(msg) },
            (msg, None) => msg,
        };
        // Joined by a leave of the pending world cut: held back until it is decided.
        let Some(msg) = self.hold_for_leave(ordinal, msg) else { return };
        if let Some(id) = self.queue_now(ordinal, msg) {
            if view {
                self.peer_in_source_view.insert((ordinal, id));
            }
        }
    }

    /// `msg` for host `ordinal`, which is forgotten (`forget_host`: dead or departed for good;
    /// its land is another host's in the directory). Never queued. A state message never
    /// vanishes silently:
    /// - a transfer without a player waits for an owner like any transfer whose owner is not
    ///   reachable ([`Self::wait_for_owner`]: this host's cut row meanwhile, delivered to the
    ///   super-region's owner at the next `flush_unowned` — the heir, once the directory says so);
    /// - a hand-off (a transfer with a player), a player move, an intent or a reply has no such
    ///   path (a player is not parked from here, an intent's refusal is its sender region's): it
    ///   is a bug — `log::error!`, counted (`peer_state_to_forgotten=`), `debug_assert!`;
    /// - anything else (orders, acks, notices, marks: no state) is dropped and counted
    ///   (`peer_dropped_for_dead=`).
    pub(super) fn to_forgotten(&mut self, ordinal: HostId, msg: Reliable, view: bool) {
        let (body, accounted) = match msg {
            Reliable::InCut { cut, body } => (*body, Some(cut)),
            other => (other, self.inbound_accounted),
        };
        match body {
            Reliable::Transfer { target, stamp, bundle, player: None } => {
                log::warn!("[server] a transfer of {:?} into {target:?} for host {ordinal}, which is forgotten: it waits for the owner", bundle.id);
                self.wait_for_owner(target, Reliable::Transfer { target, stamp, bundle, player: None }, view, accounted);
                // The directory that routed it here still names the forgotten host: read it
                // again (the heir holds that land now), or nothing would deliver the transfer
                // until the polling backstop.
                self.directory_wanted = true;
            }
            msg if msg.is_state() => {
                log::error!(
                    "[server] a {} for host {ordinal}, which is forgotten: no path takes it (a conservation bug: nothing \
                     should address a forgotten host)",
                    reliable_name(&msg)
                );
                self.peer_state_to_forgotten += 1;
                debug_assert!(false, "a {} for forgotten host {ordinal}", reliable_name(&msg));
            }
            msg => {
                log::debug!("[server] a {} for host {ordinal}, which is forgotten: dropped", reliable_name(&msg));
                self.peer_dropped_for_dead += 1;
            }
        }
    }

    /// `msg` to host `ordinal` now: on its session, or kept until it has one. Its id among
    /// everything queued to that host (`peer_entries`); `None` for a forgotten host — the
    /// message is not queued at all ([`Self::to_forgotten`]).
    pub(super) fn queue_now(&mut self, ordinal: HostId, msg: Reliable) -> Option<u64> {
        if self.forgotten_hosts.contains(&ordinal) {
            self.to_forgotten(ordinal, msg, false);
            return None;
        }
        let count = self.peer_queued.entry(ordinal).or_default();
        let id = *count;
        *count += 1;
        // Everything this host sends a peer is queued here, in order (a leave's held-back
        // messages when they finally go): the journal sees each state message and each
        // mark exactly where it goes on the session.
        if msg.is_state() {
            self.unsaved_traffic.insert(ordinal);
        }
        match &msg {
            Reliable::CutMark { cut } => self.journal.mark_out(ordinal, *cut),
            msg if self.journals() => self.journal.sent(ordinal, id, msg, None, self.spawner.world_tick()),
            _ => {}
        }
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
        Some(id)
    }

    /// The state messages exchanged with each peer host since the last committed world
    /// cut (`peer/journal.rs`).
    pub fn peer_journal(&self) -> &crate::PeerJournal {
        &self.journal
    }

    /// Whether this host journals the state messages it exchanges with its peers: only where
    /// a succession may read them — `CrashPolicy::Succession` (`claim`). Under `Rollback`
    /// (`static`, `allinone`: R3) a crash restarts every host from the last cut and no report
    /// is ever made, while a `Sent` entry keeps a whole bundle for as long as cuts are
    /// blocked: nothing is recorded (marks and cut bookkeeping stay; they hold no state).
    pub(super) fn journals(&self) -> bool {
        self.journal_always || self.ownership.0.policy() == crate::CrashPolicy::Succession
    }

    /// Test support: journal under every crash policy (`on`), or only as the product does —
    /// under `Succession` ([`Self::journals`]).
    #[doc(hidden)]
    pub fn journal_under_any_policy(&mut self, on: bool) {
        self.journal_always = on;
    }

    /// At boot, before any peer traffic: the journal counts from `base`, the store's last
    /// committed world cut (`CutId(0)`, the default, before any).
    pub fn start_peer_journal(&mut self, base: crate::CutId) {
        debug_assert!(self.journal.is_empty(), "the peer journal restarted with entries in it");
        self.journal = crate::PeerJournal::new(base);
    }

    /// Every reliable message to another host not acknowledged yet (a session's, or
    /// waiting for one), with its host and id ([`Self::peer_entries_of`]).
    fn peer_entries(&self) -> Vec<(HostId, u64, &Reliable)> {
        let ordinals: BTreeSet<HostId> = self.peers.keys().chain(self.peer_waiting.keys()).copied().collect();
        ordinals.into_iter().flat_map(|o| self.peer_entries_of(o).into_iter().map(move |(id, r)| (o, id, r))).collect()
    }

    /// What this host owes host `ordinal` and it has not acknowledged (its session's, then
    /// what waits for a session), in order, each with its id among everything queued to that
    /// host. A session's entries (and a waiting list) leave only from the front —
    /// acknowledged, or forgotten through a cut mark or count — and a session is never
    /// replaced, so they are always the LAST ones queued to that host.
    fn peer_entries_of(&self, ordinal: HostId) -> Vec<(u64, &Reliable)> {
        let mut list: Vec<&Reliable> = self.peers.get(&ordinal).map(|p| p.session.unreceived().collect()).unwrap_or_default();
        list.extend(self.peer_waiting.get(&ordinal).into_iter().flatten());
        let queued = self.peer_queued.get(&ordinal).copied().unwrap_or(0);
        // (Never short; saturating all the same, so a miscount cannot wrap.)
        let first = queued.saturating_sub(list.len() as u64);
        list.into_iter().enumerate().map(|(i, r)| (first + i as u64, r)).collect()
    }

    /// `bundle` (in `target`'s frame), a body that is not a player, departed from `rc` at
    /// `stamp` into `target`, which this host does not own: it goes to the host that does,
    /// reachable or not; the receiving host decides everything about its arrival (a
    /// person's `PersonCrossed` included). With no owner this host knows it waits here
    /// ([`Self::wait_for_owner`]). `view`: departed after `rc` answered the pending cut.
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
        // It is on another host now (or waits for one): nothing here routes to it.
        self.locations.remove(&bundle.id);
        let id = bundle.id;
        let msg = Reliable::Transfer { target, stamp, bundle: Box::new(bundle), player: None };
        match self.foreign_host(target) {
            Some(owner) => {
                log::debug!("[server] {id:?} from {rc:?} into {target:?}: sent to host {owner}");
                self.send_reliable(owner, msg, view);
            }
            None => self.wait_for_owner(target, msg, view, None),
        }
    }

    /// `msg` (a transfer) for `target`, whose super-region nobody owns or whose owner this
    /// host does not know: it waits here until it has one, never assumed to be anyone's.
    /// `view` / `accounted_in`: as a mailbox entry's (a cut row unless either says another
    /// snapshot or part holds it).
    pub(super) fn wait_for_owner(
        &mut self,
        target: RegionCoords,
        msg: Reliable,
        view: bool,
        accounted_in: Option<crate::CutId>,
    ) {
        debug_assert!(
            matches!(msg.inner(), Reliable::Transfer { player: None, .. }),
            "only a transfer without a player waits for an owner"
        );
        let sr = crate::super_region_of(target, self.ownership.1);
        let owner = self.owner_of(target);
        log::debug!("[server] {} for {target:?}: super-region {sr:?} is {owner:?} here; it waits", reliable_name(&msg));
        self.unowned_waiting.entry(sr).or_default().push(UnownedWait { msg, in_source_view: view, accounted_in });
        self.note_unowned(target);
    }

    /// Every message waiting for an owner (the `[server]` counters line).
    pub fn unowned_waiting_total(&self) -> usize {
        self.unowned_waiting.values().map(Vec::len).sum()
    }

    // --- land on demand (R4 `PgClaim`; asked of the coordinator since Elastic 3, `land.rs`) ---

    /// Something here needs `rc`, whose super-region has no owner this host knows: it is asked
    /// of the coordinator (`land.rs`, [`Self::ask_for_land`]); an `Unknown` one also asks for a
    /// directory read. Nothing for an owned region (`AllInOne` / `Static` never get here).
    pub(super) fn note_unowned(&mut self, rc: RegionCoords) {
        let sr = crate::super_region_of(rc, self.ownership.1);
        if self.inheriting.contains_key(&sr) {
            // This host's already (a succession not adopted yet): nothing to ask or read.
            return;
        }
        if self.releasing.contains_key(&sr) {
            // Masked for a release: the need waits for the decision (asked once it is free).
            self.land_needed.insert(sr);
            return;
        }
        match self.ownership.0.owner_of(sr) {
            crate::Owner::Free => {
                self.land_needed.insert(sr);
            }
            crate::Owner::Unknown => {
                self.land_needed.insert(sr);
                self.directory_wanted = true;
            }
            crate::Owner::Me | crate::Owner::Host(_) => {}
        }
    }

    /// The hosts marked draining, from the `hosts` rows of a directory read (the server calls
    /// it right before [`Self::directory_read`]): the coordinator assigns none of them land; a
    /// draining host still asks for land something here needs (`land.rs`).
    pub fn set_draining(&mut self, draining: BTreeSet<HostId>) {
        let Some(directory) = self.ownership.0.directory_mut() else { return };
        directory.set_draining(draining);
    }

    /// The server's answer to [`Self::take_assigned`]: `rows` (`(super-region, epoch)`) are
    /// this host's, ALREADY in its fence. Its directory says so, every peer is told
    /// ([`Reliable::Owns`]) and what waited for those super-regions is delivered here.
    pub fn claimed(&mut self, rows: Vec<(crate::SuperRegion, u64)>) {
        let me = self.ordinal();
        let Some(directory) = self.ownership.0.directory_mut() else {
            log::error!("[server] {} super-region(s) assigned, but this host's ownership keeps no directory", rows.len());
            return;
        };
        for &(sr, epoch) in &rows {
            directory.set(sr, Some(me), epoch);
        }
        log::info!("[server] super-region(s) {rows:?} are this host's now");
        for &(sr, _) in &rows {
            self.forget_land_need(sr);
        }
        let peers: Vec<HostId> = self.peers.keys().copied().collect();
        for p in peers {
            self.queue_to_peer(p, Reliable::Owns { rows: rows.clone() });
        }
        self.flush_unowned();
    }

    /// The directory read whole from the store (`(super-region, owner or NO_OWNER, epoch)`)
    /// and the hosts holding their lease (`live`; this host counts as live): at boot, every
    /// `TERRAIN_POLL_SECS` as a backstop, and when [`Self::take_directory_wanted`] asked.
    /// A row older than what a peer already said is kept as said ([`crate::Directory::replace`]).
    /// What waited for a super-region that has an owner now goes on.
    pub fn directory_read(&mut self, rows: Vec<(crate::SuperRegion, i64, u64)>, mut live: BTreeSet<HostId>) {
        live.insert(self.ordinal());
        let Some(directory) = self.ownership.0.directory_mut() else { return };
        directory.replace(rows.into_iter().map(|(sr, owner, epoch)| (sr, u64::try_from(owner).ok().map(HostId), epoch)));
        directory.set_live(live);
        self.directory_wanted = false;
        self.left_land_read();
        self.flush_unowned();
    }

    /// Whether an `Unknown` owner was hit since the last call (the server then reads the
    /// directory, [`Self::directory_read`]).
    pub fn take_directory_wanted(&mut self) -> bool {
        std::mem::take(&mut self.directory_wanted)
    }

    /// Host `from` holds `rows` (`(super-region, epoch)`, its [`Reliable::Owns`]): its
    /// directory says so unless it knows a newer epoch; what waited for them goes on.
    fn peer_owns(&mut self, from: HostId, rows: Vec<(crate::SuperRegion, u64)>) {
        let Some(directory) = self.ownership.0.directory_mut() else {
            log::warn!("[server] host {from} claims {} super-region(s), but this host keeps no directory: ignored", rows.len());
            return;
        };
        // A host back from a leave holds land it left with again (R4 drain).
        let theirs = directory.owned_by(from);
        let reclaimed = rows.iter().any(|(sr, _)| theirs.contains(sr));
        for (sr, epoch) in rows {
            directory.set(sr, Some(from), epoch);
        }
        self.returning_owns(from, reclaimed);
        self.flush_unowned();
    }

    /// Host `from` released `rows` (`(super-region, epoch)`, its [`Reliable::Released`]): each
    /// is free in this host's directory unless it knows a newer epoch (an assignment the
    /// message is late for). From now on nothing is sent to `from` for that land: a need for
    /// it asks the coordinator (`land.rs`), and what arrives for it waits here for its next
    /// owner (`wait_for_owner`, `can_move`, a held reply) — nothing becomes deliverable, so
    /// nothing is flushed.
    fn peer_released(&mut self, from: HostId, rows: Vec<(crate::SuperRegion, u64)>) {
        let Some(directory) = self.ownership.0.directory_mut() else {
            log::warn!("[server] host {from} released {} super-region(s), but this host keeps no directory: ignored", rows.len());
            return;
        };
        log::info!("[server] host {from} released super-region(s) {rows:?}: free");
        for (sr, epoch) in rows {
            directory.set(sr, None, epoch);
        }
    }

    /// Everything [`Self::wait_for_owner`] kept for a super-region that has an owner now
    /// goes there, in order, with its cut flags: delivered here as an inbound arrival
    /// (`Me`), or sent to its owner (`Host`).
    pub(super) fn flush_unowned(&mut self) {
        let ready: Vec<crate::SuperRegion> = self
            .unowned_waiting
            .keys()
            .copied()
            .filter(|sr| matches!(self.owner_of_sr(*sr), crate::Owner::Me | crate::Owner::Host(_)))
            .collect();
        for sr in ready {
            let owner = self.owner_of_sr(sr);
            let list = self.unowned_waiting.remove(&sr).unwrap_or_default();
            log::debug!("[server] super-region {sr:?} is {owner:?}: {} message(s) that waited for it go on", list.len());
            for w in list {
                let lineage = std::mem::replace(&mut self.inbound_accounted, w.accounted_in);
                match (owner, w.msg) {
                    (crate::Owner::Me, Reliable::Transfer { target, stamp, bundle, player: None }) => {
                        let now_ms = self.clock_ms;
                        self.accept_arrival_in_view(None, target, stamp, *bundle, now_ms, w.in_source_view);
                    }
                    (crate::Owner::Host(h), msg) => self.send_reliable(h, msg, w.in_source_view),
                    (owner, msg) => {
                        // `wait_for_owner` keeps only transfers without a player.
                        log::error!("[server] a waiting {} for super-region {sr:?} ({owner:?}): kept", reliable_name(&msg));
                        let keep = UnownedWait { msg, in_source_view: w.in_source_view, accounted_in: w.accounted_in };
                        self.unowned_waiting.entry(sr).or_default().push(keep);
                    }
                }
                self.inbound_accounted = lineage;
            }
        }
        self.forward_mailbox();
    }

    /// Intents and replies held in `intent_mailbox` for a region another host owns now and
    /// this host reaches (land released here — mail held while it was masked — then assigned
    /// to that host; a reply held because its sender's directory and this host's disagreed)
    /// go to that host, in order, with their cut flags (as [`Self::flush_unowned`] sends a
    /// transfer), and leave the mailbox as they are queued: exactly once. Mail for a region
    /// this host owns waits for its next run here (delivered as it starts); for land nobody
    /// owns, or whose owner this host does not reach, it stays.
    fn forward_mailbox(&mut self) {
        let ready: Vec<RegionCoords> = self
            .intent_mailbox
            .keys()
            .copied()
            .filter(|rc| self.foreign_host(*rc).is_some_and(|h| self.peer_reachable(h)))
            .collect();
        for rc in ready {
            let list = self.intent_mailbox.remove(&rc).unwrap_or_default();
            log::debug!("[server] {rc:?} is another host's now: {} intent(s) or reply(ies) held for it go there", list.len());
            for m in list {
                let to = peer_intent_target(&m.msg);
                if !self.foreign_host(to).is_some_and(|h| self.peer_reachable(h)) {
                    // Held under another key than its own target (a reply routed by where
                    // its entity was): kept until that target's owner is reached.
                    self.intent_mailbox.entry(rc).or_default().push(m);
                    continue;
                }
                let lineage = std::mem::replace(&mut self.inbound_accounted, m.accounted_in);
                self.intent_to_peer(to, m.msg, m.in_source_view);
                self.inbound_accounted = lineage;
            }
        }
    }

    /// Test support: the entity of every transfer waiting for an owner
    /// ([`Self::wait_for_owner`]), with its super-region.
    pub fn unowned_waiting_ids(&self) -> Vec<(crate::SuperRegion, crate::EntityId)> {
        let mut out = Vec::new();
        for (sr, list) in &self.unowned_waiting {
            out.extend(list.iter().filter_map(|w| transfer_id(&w.msg)).map(|id| (*sr, id)));
        }
        out
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
    /// host's snapshot or was written as this part's row when it was sent; toward one the
    /// cut counts with across a down link, what is above its count is written by
    /// [`Self::cut_counted`]. (A one-phase cut's peer may also hold an entry it received
    /// and has not acknowledged yet.)
    pub(super) fn cut_peer_rows(&self, participants: &BTreeSet<HostId>) -> Vec<InFlightRow> {
        let to_peers = self
            .peer_entries()
            .into_iter()
            .filter(|(ordinal, _, _)| !participants.contains(ordinal))
            .filter(|(ordinal, id, _)| !self.peer_in_source_view.contains(&(*ordinal, *id)))
            .map(|(_, _, r)| r);
        // Waiting for an owner: no other part can write it (as toward an absent peer).
        let unowned = self.unowned_waiting.values().flatten().filter(|w| !w.in_source_view).map(|w| &w.msg);
        to_peers
            .chain(unowned)
            .filter_map(|r| match in_transit(r, None) {
                InTransit::Row(row) => Some(row),
                _ => None,
            })
            .collect()
    }

    /// What world cut `cut` (at tick `tick`) counted across a down link writes for a peer
    /// `ordinal` whose count of this host's numbering is `count` (the module docs of
    /// `world_cut.rs`, the fourth case): every entry still owed it and numbered above the
    /// count — the peer is frozen, none of it reaches it before the cut is decided — that did
    /// not travel wrapped in `cut` (those are in this part already: its snapshot, case 1, or
    /// its rows, case 3), as for an absent peer ([`in_transit`]), a transfer's stamp or an
    /// intent's due raised to at least `tick` (no receiver region applies it before its
    /// snapshot); and the ids of everything owed above the count (the journal's
    /// `kept_sent`). What waits for a session counts as above any count.
    pub(super) fn cut_counted(&self, ordinal: HostId, count: u64, cut: CutId, tick: crate::Tick) -> CountedPart {
        let mut out = CountedPart::default();
        let mut entries = self.peer_entries_of(ordinal);
        let above = self.peers.get(&ordinal).map_or(0, |p| p.session.owed_above(count).count())
            + self.peer_waiting.get(&ordinal).map_or(0, Vec::len);
        // Owed entries leave only from the front: those above the count are the last ones.
        let above = entries.split_off(entries.len().saturating_sub(above));
        for (id, r) in above {
            out.kept_sent.insert(id);
            if matches!(r, Reliable::InCut { cut: c, .. } if *c == cut) {
                continue;
            }
            match in_transit(r, Some(tick)) {
                InTransit::Row(row) => out.rows.push(row),
                InTransit::Player(player, record) => out.players.push((player, record)),
                InTransit::Nothing => {}
            }
        }
        out
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
    pub(super) fn cut_peer_players(&self, participants: &BTreeSet<HostId>) -> Vec<(PlayerId, PlayerRecord)> {
        self.peer_entries()
            .into_iter()
            .filter(|(ordinal, _, _)| !participants.contains(ordinal))
            .filter(|(ordinal, id, _)| !self.peer_in_source_view.contains(&(*ordinal, *id)))
            .filter_map(|(_, _, r)| match in_transit(r, None) {
                InTransit::Player(player, record) => Some((player, record)),
                _ => None,
            })
            .collect()
    }

    /// `msg` (an intent or a reply) to the region `to` another host owns, reliably.
    /// `view`: sent after its source answered the pending cut (not a row of that cut).
    pub(super) fn intent_to_peer(&mut self, to: RegionCoords, msg: crate::IntentMsg, view: bool) {
        debug_assert_eq!(peer_intent_target(&msg), to, "an intent to a peer goes where its message says");
        let owner = self.foreign_host(to).expect("intent_to_peer: another host owns the target");
        log::debug!("[server] {msg:?} for {to:?}: sent to host {owner}");
        self.send_reliable(owner, Reliable::Intent(msg), view);
    }

    /// `v` to the host that owns `rc`, if its session is up (else dropped: volatile).
    fn volatile_to_owner(&mut self, rc: RegionCoords, v: Volatile) {
        let Some(owner) = self.foreign_host(rc) else { return };
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
    fn borders(&self, rc: RegionCoords, peer: HostId) -> bool {
        crate::halo::neighbours(rc).any(|n| self.foreign_host(n) == Some(peer))
    }

    /// This host's region `rc` started (`Volatile::RegionUp`), parked (`RegionParked`) or
    /// was lost (`RegionDown`): `what(rc)` told to every host owning one of its neighbours.
    pub(super) fn announce_region(&mut self, rc: RegionCoords, what: fn(RegionCoords) -> Volatile) {
        let ordinals: Vec<HostId> = self.peers.keys().copied().filter(|&p| self.borders(rc, p)).collect();
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
            // What it sent last may be lost: the store re-seeds it (`stored_terrain_changed`).
            self.foreign_halo_stale.insert(n);
        }
    }

    /// The link to host `from` is gone (or it runs a newer generation): every region of
    /// it is gone for this host's halos.
    pub(super) fn peer_lost(&mut self, from: HostId) {
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

    fn peer_volatile(&mut self, from: HostId, v: Volatile) {
        match v {
            Volatile::Running(list) => {
                // Sent on our hello, behind everything it had queued for us: its first sync
                // of this connection is done (`placement.rs`).
                self.peer_synced_now(from);
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
                    self.foreign_halo_stale.remove(&n);
                    self.request_seeds_of(n);
                }
            }
            Volatile::RegionUp(n) => {
                self.foreign_running.insert(n, from);
                self.foreign_halo_stale.remove(&n);
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
            // An older host's ask to claim free land (before Elastic 3): no host claims land
            // any more — the coordinator assigns it. Never sent by this build; ignored.
            Volatile::ClaimWanted(sr) => {
                log::debug!("[server] host {from} asked this host to claim {sr:?}: ignored (the coordinator assigns land)");
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

    fn peer_event(&mut self, from: HostId, ev: PeerEvent) {
        match ev {
            PeerEvent::Up(hello) => {
                log::info!("[server] peer host {from} up (boot {})", hello.boot);
                // A host that left and comes back as another boot gets what waited for it.
                self.peer_hello_boot(from, hello.boot);
                // A host succeeded as another boot is a new host now.
                self.succession_hello(from, hello.boot);
                // Where its clients connect (`RegionElsewhere`).
                self.peer_endpoints.insert(from, hello.endpoint.clone());
                // A directory (`PgClaim`): that host runs (it registered before it linked),
                // and it learns every claim of this host — whatever `Owns` it missed.
                let me = self.ordinal();
                if let Some(directory) = self.ownership.0.directory_mut() {
                    directory.note_live(from);
                    let rows: Vec<(crate::SuperRegion, u64)> = directory
                        .owned_by(me)
                        .into_iter()
                        .filter_map(|sr| directory.epoch(sr).map(|epoch| (sr, epoch)))
                        .collect();
                    if !rows.is_empty() {
                        self.queue_to_peer(from, Reliable::Owns { rows });
                    }
                    // And every row this host released that no read showed owned since: a
                    // peer that linked after the release (or missed it) learns it is free.
                    let released: Vec<(crate::SuperRegion, u64)> =
                        self.recently_released.iter().map(|(sr, epoch)| (*sr, *epoch)).collect();
                    if !released.is_empty() {
                        self.queue_to_peer(from, Reliable::Released { rows: released });
                    }
                }
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
                self.peer_unsynced(from);
                self.peer_lost(from);
                // A pending world cut counts with it from here (`world_cut.rs`).
                self.world_cut_count_mode(from, "went down");
            }
            PeerEvent::HigherGeneration(generation) => {
                log::error!("[server] peer host {from} runs world generation {generation}, newer than ours: this host is stale");
                self.higher_generation = Some(self.higher_generation.map_or(generation, |g| g.max(generation)));
                if let Some(peer) = self.peers.get_mut(&from) {
                    peer.stale = true;
                }
                self.peer_unsynced(from);
                self.peer_lost(from);
                self.world_cut_peer_stale(from);
            }
            PeerEvent::Reliable(msg) => self.peer_reliable(from, msg),
            PeerEvent::Volatile(v) => self.peer_volatile(from, v),
            // A boot declared dead (a succession): its connection is closed for a fresh one.
            PeerEvent::Refused(boot) => {
                // The session warned once per boot.
                log::debug!("[server] host {from}'s boot {boot:#x} was declared dead: its connection is closed");
                if let Some(peer) = self.peers.get_mut(&from) {
                    peer.link.close();
                }
            }
        }
    }

    /// A participant of the pending world cut runs a newer generation: this host is stale —
    /// its part fails. (A link that merely went down is counted across instead,
    /// `world_cut.rs`.)
    fn world_cut_peer_stale(&mut self, from: HostId) {
        if self.world_cut.as_ref().is_some_and(|r| r.state == super::world_cut::PartState::Pending && r.peers.contains(&from)) {
            log::warn!("[server] world cut: host {from} runs a newer generation while the cut was pending");
            self.world_cut_fail(crate::CutFailure::Aborted);
        }
    }

    /// Host `from`'s next reliable message (`world_cut.rs`): a mark is noted; a wrapped
    /// state message ([`Reliable::InCut`]) is applied with what it creates noted as
    /// accounted for by its sender in that cut; a bare state message after `from`'s mark of
    /// the pending world cut is a protocol violation — applied all the same (state is never
    /// dropped), and the cut fails.
    pub(super) fn peer_reliable(&mut self, from: HostId, msg: Reliable) {
        if msg.is_state() {
            self.unsaved_traffic.insert(from);
        }
        match msg {
            Reliable::CutMark { cut } => self.peer_cut_mark(from, cut),
            Reliable::InCut { cut, body } => {
                if matches!(*body, Reliable::InCut { .. }) || !body.is_state() {
                    log::error!("[server] host {from}: a wrapped {} for cut {}; applied as is", reliable_name(&body), cut.0);
                }
                if self.journals() {
                    self.journal.received(from, &body, Some(cut), self.spawner.world_tick());
                }
                self.note_wrapper(cut);
                self.note_player_accounted(&body, cut);
                let after = self.accounted_now(cut);
                let lineage = self.inbound_accounted.replace(cut);
                self.peer_reliable_in(from, *body, after);
                self.inbound_accounted = lineage;
            }
            msg => {
                if msg.is_state() && self.after_mark_failed(from) {
                    log::warn!(
                        "[server] host {from} sent a bare {} after its mark of a world cut that failed here; applied",
                        reliable_name(&msg)
                    );
                }
                if msg.is_state() && self.after_mark(from) {
                    log::error!(
                        "[server] host {from} sent a bare {} after its mark of the pending world cut: \
                         applied, the cut fails",
                        reliable_name(&msg)
                    );
                    self.world_cut_fail(crate::CutFailure::Aborted);
                }
                if self.journals() {
                    self.journal.received(from, &msg, None, self.spawner.world_tick());
                }
                // A player that left bare (the receiver's) and comes back bare is this
                // host's again in the pending cut.
                self.note_player_returned(&msg);
                self.peer_reliable_in(from, msg, false);
            }
        }
    }

    /// Apply host `from`'s reliable message; `after`: its sender accounts for it in the
    /// pending cut (what it creates here is in no part of this host's).
    pub(super) fn peer_reliable_in(&mut self, from: HostId, msg: Reliable, after: bool) {
        match msg {
            Reliable::Transfer { target, stamp, bundle, player: None } => {
                if !self.owns(target) && !self.is_foreign(target) {
                    // Sent here, but this host knows no owner of it: it waits here for one
                    // (a cut row), never sent back.
                    // Debug, not warn: one per transfer (counted as `unowned_waiting=`).
                    log::debug!("[server] host {from} sent {:?} into {target:?}, whose owner this host does not know", bundle.id);
                    let msg = Reliable::Transfer { target, stamp, bundle, player: None };
                    self.wait_for_owner(target, msg, after, self.inbound_accounted);
                    return;
                }
                if let Some(owner) = self.foreign_host(target) {
                    log::error!(
                        "[server] host {from} sent {:?} into {target:?}, which this host does not own (host {owner} does)",
                        bundle.id
                    );
                    if owner != from {
                        // Bounced on to its owner (as any crossing into a foreign region).
                        self.send_reliable(owner, Reliable::Transfer { target, stamp, bundle, player: None }, after);
                        return;
                    }
                    // The sender thinks this host owns it and this host thinks the sender
                    // does: never sent back (it could bounce for ever). It waits in the
                    // mailbox here (a cut row; a restore queues it to the owner again).
                }
                let now_ms = self.clock_ms;
                self.accept_arrival(None, target, stamp, *bundle, now_ms);
            }
            Reliable::Transfer { target, stamp, bundle, player: Some(handoff) } => {
                if !self.owns(target) && !self.masked_or_released(target) {
                    // Parked here from the hand-off (`accept_handoff`): a region this host does
                    // not own is never started here. Never lost.
                    log::error!("[server] host {from} handed a player into {target:?}, which this host does not own");
                }
                self.accept_handoff(from, target, stamp, *bundle, handoff);
            }
            // For the server edge, which admits the ticket (`take_observer_orders`).
            // Never for a client whose session here is its home: its ticket must not open a
            // second connection of it (`take_observer_revocations`).
            Reliable::ObserverOrder { client, player, nonce, ttl_secs } => {
                if self.sessions.get(&client).is_some_and(|s| s.kind == SessionKind::Home) {
                    log::info!("[server] host {from}'s observer order for client {client}, at home here: not recorded");
                } else {
                    self.observer_orders.push((client, player, nonce.0, ttl_secs));
                }
            }
            // As if one of this host's regions had sent it: no cut has seen it here, unless
            // it came after its sender's mark (that sender's snapshot holds it).
            Reliable::Intent(msg) => {
                let target = peer_intent_target(&msg);
                if self.owns(target) {
                    self.route_intent(msg, target, after);
                    return;
                }
                // Ownership disagrees: not forwarded on receipt (it could bounce between hosts). An
                // intent is refused back (its items return), a reply held here — sent on by
                // `forward_mailbox` once the directory names another owner this host reaches.
                if self.masked_or_released(target) {
                    log::debug!("[server] host {from} sent {msg:?} for {target:?}, inherited or released here");
                } else {
                    log::error!("[server] host {from} sent {msg:?} for {target:?}, which this host does not own");
                    // One of the two directories is stale: read this one again.
                    self.directory_wanted = true;
                }
                match msg {
                    // Refused back to where it came from: its items return.
                    crate::IntentMsg::Intent(s) => {
                        let reply = crate::IntentReply::Refused(s.intent, crate::IntentRefusal::OwnerNotRunning);
                        let back = crate::SentReply { reply, hint: s.from, due: s.due + 1, hops: 0 };
                        self.route_intent(crate::IntentMsg::Reply(back), target, after);
                    }
                    // A reply is held here, in the mailbox of its region (a cut carries it,
                    // a restore queues it to the owner, `forward_mailbox` sends it to an owner
                    // found since): never lost.
                    reply @ crate::IntentMsg::Reply(_) => {
                        self.intent_mailbox.entry(target).or_default().push(Mail { msg: reply, in_source_view: after, accounted_in: self.inbound_accounted });
                    }
                }
            }
            Reliable::PlayerMove { handoff, record, target } => {
                if !self.owns(target) && !self.masked_or_released(target) {
                    // Parked here (`accept_move`): a region this host does not own is never
                    // started here, so its body could not be created. Never lost.
                    log::error!("[server] host {from} moved a player into {target:?}, which this host does not own");
                }
                self.accept_move(from, handoff, *record, target);
            }
            Reliable::PlayerParked { client, player } => self.player_parked_elsewhere(from, client, player),
            Reliable::Owns { rows } => self.peer_owns(from, rows),
            Reliable::Released { rows } => self.peer_released(from, rows),
            Reliable::SuccessionReport { id, base_cut, entries, owed } => {
                self.peer_succession_report(from, id, base_cut, entries, owed)
            }
            msg @ (Reliable::SpawnOrder { .. }
            | Reliable::SpawnAck { .. }
            | Reliable::Kick { .. }
            | Reliable::KickAck { .. }
            | Reliable::SessionEnded { .. }) => self.peer_login_msg(from, msg),
            other => {
                log::error!("[server] peer host {from}: {} is not handled by this host yet: dropped", reliable_name(&other))
            }
        }
    }
}

/// The claim a player's adventure is.
fn claim_of(adventure: Option<SiteId>) -> Option<Claim> {
    adventure.map(Claim::Adventure)
}

/// A claim as the adventure of a record.
fn split_claim(claim: Option<Claim>) -> Option<SiteId> {
    claim.map(Claim::site)
}

/// The record of a player handed over in `bundle` (its body, in `target`'s frame), as the
/// receiving host parks it: `None` if the bundle carries no player.
pub(super) fn handoff_record(target: RegionCoords, bundle: &crate::EntityBundle, h: &crate::PlayerHandoff) -> Option<PlayerRecord> {
    let (_, carried) = bundle.client.as_ref()?;
    let adventure = split_claim(h.claim);
    Some(PlayerRecord { client: crate::parked_client(carried), pose: bundle.body.pose, region: target, adventure })
}

/// A server-authored event that was to follow a moved player's creation, applied to the
/// `Client` it carries to another host instead: a mode change, as the region applies it
/// (`set_play_mode_safe`). Nothing else follows a creation.
///
/// Leaving survival here differs from a local move in one way: what does not fit back
/// into the inventory stays on the cursor (the record keeps it; the restore stows it
/// again) instead of being dropped at the player's feet. Only a mode order sent while a
/// move is in flight gets here (otherwise it reaches the home
/// before the move, where the drop happens as locally), and by then the body is parked or
/// being parked: there is no region and no feet to drop at.
fn carry_event(carried: &mut crate::Client, kind: GameEventKind) {
    match kind {
        GameEventKind::Order(_, Order::SetMode { mode, .. }) => {
            if carried.mode != mode {
                carried.mode = mode;
                if mode == PlayMode::Overview {
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

/// The intents and replies a reliable message carries: an intent's own, or those a
/// succession report re-routes (`owed`).
pub fn intents_in(r: &Reliable) -> Vec<crate::IntentMsg> {
    match r.inner() {
        Reliable::Intent(msg) => vec![*msg],
        Reliable::SuccessionReport { owed, .. } => owed.iter().flat_map(intents_in).collect(),
        _ => Vec::new(),
    }
}

/// The region a state message is bound for (a transfer's, a move's, an intent's or reply's).
pub(super) fn reliable_target(r: &Reliable) -> Option<RegionCoords> {
    match r.inner() {
        Reliable::Transfer { target, .. } | Reliable::PlayerMove { target, .. } => Some(*target),
        Reliable::Intent(m) => Some(peer_intent_target(m)),
        _ => None,
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
        Reliable::Owns { .. } => "Owns",
        Reliable::SuccessionReport { .. } => "SuccessionReport",
        Reliable::Released { .. } => "Released",
    }
}

/// What a world cut counted across a down link writes for one peer
/// ([`WorldManager::cut_counted`]).
#[derive(Debug, Default)]
pub(super) struct CountedPart {
    pub rows: Vec<InFlightRow>,
    pub players: Vec<(PlayerId, PlayerRecord)>,
    /// The ids of everything owed above the count (the journal keeps these `Sent`).
    pub kept_sent: BTreeSet<u64>,
}

/// What [`WorldManager::redirect`] did with a subscribe.
enum Redirect {
    /// The client was sent to this host (`RegionElsewhere`).
    Sent(HostId),
    /// Not now (the owner's endpoint is not known yet): it may be later.
    Later,
    /// Never from here (no player for the session, no nonce source, not another host's).
    Never,
}

/// What an unacknowledged message to another host is in a cut part that writes it (toward
/// an absent peer, or above a counted peer's count).
enum InTransit {
    /// A transfer without a player (never a person's: the history respawns people), an
    /// intent or a reply.
    Row(InFlightRow),
    /// A player handed over (its body's record, as the receiver would park it) or moved.
    Player(PlayerId, PlayerRecord),
    Nothing,
}

/// `r` (its [`Reliable::InCut`] wrapper aside) as [`InTransit`]; `floor`: a transfer's stamp
/// or an intent's or reply's due raised to at least it.
fn in_transit(r: &Reliable, floor: Option<crate::Tick>) -> InTransit {
    let floor = floor.unwrap_or(0);
    match r.inner() {
        Reliable::Transfer { bundle, .. } if matches!(bundle.kind, EntityKind::Person { .. }) => InTransit::Nothing,
        Reliable::Transfer { target, stamp, bundle, player: None } => {
            InTransit::Row(InFlightRow::Transfer { target: *target, bundle: (**bundle).clone(), stamp: (*stamp).max(floor) })
        }
        Reliable::Intent(msg) => {
            let mut msg = *msg;
            match &mut msg {
                crate::IntentMsg::Intent(s) => s.due = s.due.max(floor),
                crate::IntentMsg::Reply(s) => s.due = s.due.max(floor),
            }
            InTransit::Row(InFlightRow::Intent { target: peer_intent_target(&msg), msg })
        }
        Reliable::Transfer { target, bundle, player: Some(h), .. } => match handoff_record(*target, bundle, h) {
            Some(record) => InTransit::Player(h.player, record),
            None => InTransit::Nothing,
        },
        Reliable::PlayerMove { handoff, record, .. } => InTransit::Player(handoff.player, (**record).clone()),
        _ => InTransit::Nothing,
    }
}
