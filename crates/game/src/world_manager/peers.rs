//! The peer half of [`WorldManager`]: regions another host owns ("foreign" regions) and
//! the [`PeerSession`] to each other host. An entity crossing into a foreign region goes
//! to its owner as a [`Reliable::Transfer`] — queued whether or not the owner is
//! reachable (it waits there, conserved) — and an inbound one lands through
//! [`WorldManager::accept_arrival`], the path a local crossing takes. Split out of
//! `mod.rs`; the core calls in through the `pub(super)` methods here.
//!
//! Players do not cross hosts yet (Task 7): a player body departing into a foreign region
//! is handed back to its source, and the play paths refuse foreign regions (Task 8).

use super::*;
use crate::{PeerEvent, PeerLink, PeerSession, Reliable};

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
    pub(super) fn cut_peer_rows(&self) -> Vec<InFlightRow> {
        let sessions = self.peers.values().flat_map(|p| p.session.unreceived());
        let waiting = self.peer_waiting.values().flatten();
        sessions
            .chain(waiting)
            .filter_map(|r| match r {
                Reliable::Transfer { target, stamp, bundle, player: None }
                    if !self.peer_in_source_view.contains(&bundle.id) =>
                {
                    Some(InFlightRow::Transfer { target: *target, bundle: (**bundle).clone(), stamp: *stamp })
                }
                _ => None,
            })
            .collect()
    }

    fn peer_event(&mut self, from: u32, ev: PeerEvent) {
        match ev {
            PeerEvent::Up(hello) => log::info!("[server] peer host {from} up (boot {})", hello.boot),
            // May come without an `Up` (a connection broken at the hello).
            PeerEvent::Down => log::info!("[server] peer host {from} down"),
            PeerEvent::HigherGeneration(generation) => {
                log::error!("[server] peer host {from} runs world generation {generation}, newer than ours: this host is stale");
                self.higher_generation = Some(self.higher_generation.map_or(generation, |g| g.max(generation)));
                if let Some(peer) = self.peers.get_mut(&from) {
                    peer.stale = true;
                }
            }
            PeerEvent::Reliable(msg) => self.peer_reliable(from, msg),
            // Halo and region announcements: Task 6.
            PeerEvent::Volatile(v) => log::trace!("[server] peer host {from}: {v:?} ignored"),
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
            other => {
                log::error!("[server] peer host {from}: {} is not handled by this host yet: dropped", reliable_name(&other))
            }
        }
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
