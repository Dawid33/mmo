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
//! Players do not cross hosts yet (Task 7): a player body departing into a foreign region
//! is handed back to its source, and the play paths refuse foreign regions (Task 8).

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
    pub fn set_peer(&mut self, ordinal: u32, mut session: PeerSession, link: Box<dyn PeerLink + Send>) {
        debug_assert_eq!(session.peer(), ordinal, "a session for another host");
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

    /// `msg` to host `ordinal`: on its session, or kept until it has one.
    pub(super) fn queue_to_peer(&mut self, ordinal: u32, msg: Reliable) {
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
        // Unreachable for a player: the `Departures` arm hands every body that wakes its
        // target (a player, a person without a history) back to its source when the target
        // is foreign, before this is called — a player hand-off is Task 7.
        debug_assert!(bundle.client.is_none(), "players do not cross hosts yet");
        // Task 10: a person with a history crossing hosts (the receiving host's
        // `PersonCrossed`) has no seeded test yet — the rig gets a coordinator there.
        let owner = self.owner_of(target);
        log::debug!("[server] {:?} from {rc:?} into {target:?}: sent to host {owner}", bundle.id);
        // It is on another host now: nothing here routes to it.
        self.locations.remove(&bundle.id);
        if view {
            self.peer_in_source_view.insert(bundle.id);
        } else {
            self.peer_in_source_view.remove(&bundle.id);
        }
        self.queue_to_peer(owner, Reliable::Transfer { target, stamp, bundle: Box::new(bundle), player: None });
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
    pub(super) fn cut_peer_rows(&self) -> Vec<InFlightRow> {
        let sessions = self.peers.values().flat_map(|p| p.session.unreceived());
        let waiting = self.peer_waiting.values().flatten();
        let mut in_view = self.peer_intents_in_view.clone();
        sessions
            .chain(waiting)
            .filter_map(|r| match r {
                Reliable::Transfer { target, stamp, bundle, player: None }
                    if !self.peer_in_source_view.contains(&bundle.id) =>
                {
                    Some(InFlightRow::Transfer { target: *target, bundle: (**bundle).clone(), stamp: *stamp })
                }
                Reliable::Intent(msg) => match in_view.iter().position(|m| m == msg) {
                    Some(i) => {
                        in_view.swap_remove(i);
                        None
                    }
                    None => Some(InFlightRow::Intent { target: peer_intent_target(msg), msg: *msg }),
                },
                _ => None,
            })
            .collect()
    }

    /// `msg` (an intent or a reply) to the region `to` another host owns, reliably.
    /// `view`: sent after its source answered the pending cut (not a row of that cut).
    pub(super) fn intent_to_peer(&mut self, to: RegionCoords, msg: crate::IntentMsg, view: bool) {
        debug_assert_eq!(peer_intent_target(&msg), to, "an intent to a peer goes where its message says");
        if view {
            self.peer_intents_in_view.push(msg);
        }
        let owner = self.owner_of(to);
        log::debug!("[server] {msg:?} for {to:?}: sent to host {owner}");
        self.queue_to_peer(owner, Reliable::Intent(msg));
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

    /// This host's region `rc` started (`up`) or stopped / was lost: told to every host
    /// owning one of its neighbours.
    pub(super) fn announce_region(&mut self, rc: RegionCoords, up: bool) {
        let ordinals: Vec<u32> = self.peers.keys().copied().filter(|&p| self.borders(rc, p)).collect();
        for ordinal in ordinals {
            let Some(peer) = self.peers.get_mut(&ordinal).filter(|p| !p.stale) else { continue };
            peer.session.send_volatile(if up { Volatile::RegionUp(rc) } else { Volatile::RegionDown(rc) });
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
            Volatile::HaloRequest { from: to, of } => {
                // Not running (any more): its `Stopped` reseeds the requester instead.
                if self.owns(of) && self.runs(of) {
                    self.send_to_region(of, RegionInput::HaloRequest(to));
                }
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
            v @ Volatile::Input { .. } => log::trace!("[server] peer host {from}: {v:?} ignored"),
        }
    }

    fn peer_event(&mut self, from: u32, ev: PeerEvent) {
        match ev {
            PeerEvent::Up(hello) => {
                log::info!("[server] peer host {from} up (boot {})", hello.boot);
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
            // As if one of this host's regions had sent it (no cut has seen it here).
            Reliable::Intent(msg) => {
                let emitter = peer_intent_target(&msg);
                self.route_intent(msg, emitter, false);
            }
            other => {
                log::error!("[server] peer host {from}: {} is not handled by this host yet: dropped", reliable_name(&other))
            }
        }
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
