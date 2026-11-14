//! The peer journal (R4): per peer host, every state message — a [`Reliable::Transfer`]
//! (with or without a player) and a [`Reliable::PlayerMove`] — this host sent to it or
//! received from it since the last committed world cut. A [`super::PeerSession`] forgets
//! a message once it is acknowledged; the journal is what a survivor still knows of the
//! traffic with a host that died, for the heir that takes its land from that cut.
//!
//! Intents and replies are not journalled (their items are conserved by their own
//! refusal path), nor is anything that is not state: orders (`SpawnOrder`,
//! `ObserverOrder`, `Kick`…), `CutMark`, `Owns`.
//!
//! **Truncation is cut ownership read backwards** (`world_manager/world_cut.rs`). When
//! world cut K commits, an entry the K state already explains is no longer "since the
//! last cut": a SENT entry toward a participant that went bare before this host's
//! `CutMark { K }` to it is in the receiver's part of K — dropped; one wrapped
//! `InCut { K }`, or sent after the mark, was still this host's at K — kept. Mirror for a
//! RECEIVED entry with the peer's mark. An abort forgets its marks and keeps every
//! entry; entries toward a host that was not a participant are untouched.
//!
//! Manager state, never saved and never hashed: at boot it starts empty from the store's
//! last committed cut.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::Reliable;
use crate::{CutId, EntityId, PlayerId, Tick};

/// What a journal entry is about: an entity (a transfer's bundle) or a player (a
/// hand-off, with or without a body).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Subject {
    Entity(EntityId),
    Player(PlayerId),
}

/// Which way the message went.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum JournalEvent {
    /// This host sent it (the message itself: never an [`Reliable::InCut`]).
    Sent(Box<Reliable>),
    /// This host received it.
    Received,
}

/// One state message about one subject.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct JournalEntry {
    pub subject: Subject,
    /// A transfer's stamp; for a player move, the world tick it was queued or applied at.
    pub stamp: Tick,
    pub event: JournalEvent,
}

/// An entry with what truncation reads.
#[derive(Debug, Clone)]
struct Row {
    /// Its place among everything journalled with that peer (marks included).
    seq: u64,
    /// The cut its sender accounts for it in (it travelled [`Reliable::InCut`]).
    accounted: Option<CutId>,
    /// A sent entry's id among everything queued to that peer (`None`: received).
    queue_id: Option<u64>,
    entry: JournalEntry,
}

/// What the journal holds for one peer.
#[derive(Debug, Clone, Default)]
struct PeerLog {
    next_seq: u64,
    rows: Vec<Row>,
    /// Where this host's `CutMark` of each cut went in (`seq` of the next row).
    marks_out: BTreeMap<CutId, u64>,
    /// Where the peer's `CutMark` of each cut came in.
    marks_in: BTreeMap<CutId, u64>,
}

impl PeerLog {
    fn push(&mut self, accounted: Option<CutId>, queue_id: Option<u64>, entry: JournalEntry) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.rows.push(Row { seq, accounted, queue_id, entry });
    }
}

/// The state messages exchanged with each peer host since the last committed world cut
/// (the module docs).
#[derive(Debug, Clone)]
pub struct PeerJournal {
    peers: BTreeMap<u32, PeerLog>,
    base: CutId,
}

impl PeerJournal {
    /// An empty journal from committed cut `base` (at boot: the store's last committed
    /// cut, `CutId(0)` before any).
    pub fn new(base: CutId) -> Self {
        Self { peers: BTreeMap::new(), base }
    }

    /// The last committed cut the journal counts from.
    pub fn base(&self) -> CutId {
        self.base
    }

    /// This host queued `msg` (id `queue_id` among everything queued to `peer`) at world
    /// tick `now`; `accounted`: the cut it travels wrapped in. Only a transfer or a player
    /// move is journalled (an `InCut` is unwrapped).
    pub fn sent(&mut self, peer: u32, queue_id: u64, msg: &Reliable, accounted: Option<CutId>, now: Tick) {
        let (msg, accounted) = unwrap(msg, accounted);
        for (subject, stamp) in subjects(msg, now) {
            let event = JournalEvent::Sent(Box::new(msg.clone()));
            self.peers.entry(peer).or_default().push(accounted, Some(queue_id), JournalEntry { subject, stamp, event });
        }
    }

    /// `msg` came from `peer` at world tick `now`; `accounted`: the cut it came wrapped
    /// in. Only a transfer or a player move is journalled.
    pub fn received(&mut self, peer: u32, msg: &Reliable, accounted: Option<CutId>, now: Tick) {
        let (msg, accounted) = unwrap(msg, accounted);
        for (subject, stamp) in subjects(msg, now) {
            let entry = JournalEntry { subject, stamp, event: JournalEvent::Received };
            self.peers.entry(peer).or_default().push(accounted, None, entry);
        }
    }

    /// This host queued its `CutMark { cut }` to `peer`.
    pub fn mark_out(&mut self, peer: u32, cut: CutId) {
        let log = self.peers.entry(peer).or_default();
        let at = log.next_seq;
        log.marks_out.entry(cut).or_insert(at);
    }

    /// `peer`'s `CutMark { cut }` arrived.
    pub fn mark_in(&mut self, peer: u32, cut: CutId) {
        let log = self.peers.entry(peer).or_default();
        let at = log.next_seq;
        log.marks_in.entry(cut).or_insert(at);
    }

    /// World cut `cut` committed with `participants` (the other hosts of it; empty for a
    /// one-phase cut): toward each, what the cut already explains is dropped (the module
    /// docs). A direction whose mark this journal never saw keeps everything (never
    /// guess). The base becomes `cut`; marks of it and earlier cuts are forgotten.
    pub fn committed(&mut self, cut: CutId, participants: &BTreeSet<u32>) {
        self.base = self.base.max(cut);
        for (peer, log) in &mut self.peers {
            if participants.contains(peer) {
                let out = log.marks_out.get(&cut).copied();
                let inn = log.marks_in.get(&cut).copied();
                log.rows.retain(|r| {
                    let mark = if r.queue_id.is_some() { out } else { inn };
                    let explained = r.accounted != Some(cut) && mark.is_some_and(|m| r.seq < m);
                    !explained
                });
            }
            log.marks_out.retain(|c, _| *c > cut);
            log.marks_in.retain(|c, _| *c > cut);
        }
    }

    /// World cut `cut` was aborted: its marks are forgotten, every entry stays.
    pub fn aborted(&mut self, cut: CutId) {
        for log in self.peers.values_mut() {
            log.marks_out.remove(&cut);
            log.marks_in.remove(&cut);
        }
    }

    /// Every entry with `peer`, in order; the journal forgets that peer.
    pub fn take(&mut self, peer: u32) -> Vec<JournalEntry> {
        self.peers.remove(&peer).map(|log| log.rows.into_iter().map(|r| r.entry).collect()).unwrap_or_default()
    }

    /// The entries with `peer`, in order, each with its queue id (a sent entry's id among
    /// everything queued to `peer`; `None` for a received one).
    pub fn entries(&self, peer: u32) -> impl Iterator<Item = (&JournalEntry, Option<u64>)> {
        self.peers.get(&peer).into_iter().flat_map(|log| log.rows.iter().map(|r| (&r.entry, r.queue_id)))
    }

    /// Entries over every peer (the `[server]` line's `journal_entries=`).
    pub fn len(&self) -> usize {
        self.peers.values().map(|log| log.rows.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `msg` without its [`Reliable::InCut`] wrapper (its cut becomes `accounted`).
fn unwrap(msg: &Reliable, accounted: Option<CutId>) -> (&Reliable, Option<CutId>) {
    match msg {
        Reliable::InCut { cut, body } => (body, Some(*cut)),
        other => (other, accounted),
    }
}

/// The subjects a state message is about, each with its stamp: a transfer's entity (and
/// its player, if it hands one over) at the transfer's stamp, a move's player at `now`.
/// Nothing for any other message.
fn subjects(msg: &Reliable, now: Tick) -> Vec<(Subject, Tick)> {
    match msg {
        Reliable::Transfer { stamp, bundle, player, .. } => {
            let mut out = vec![(Subject::Entity(bundle.id), *stamp)];
            out.extend(player.as_ref().map(|h| (Subject::Player(h.player), *stamp)));
            out
        }
        Reliable::PlayerMove { handoff, .. } => vec![(Subject::Player(handoff.player), now)],
        _ => Vec::new(),
    }
}
