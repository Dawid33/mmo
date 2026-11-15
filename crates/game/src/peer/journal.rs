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
//! RECEIVED entry with the peer's mark. A pair the cut counted across a down link
//! (`world_cut.rs`, the fourth case) truncates by the counts instead
//! ([`PeerJournal::counted`]): a SENT entry stays iff it went wrapped in K or numbered above
//! the peer's count (this host's part of K holds it); a RECEIVED one goes iff it is inside
//! this host's own count and not wrapped in K. Every participant of K must be recorded by
//! [`PeerJournal::cut_begun`] (`begin_world_cut`, the only path that joins a cut — count mode
//! included). An abort forgets its marks and counts and keeps every entry; entries toward a
//! host that was not a participant are untouched.
//!
//! Manager state, never saved and never hashed: at boot it starts empty from the store's
//! last committed cut.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::Reliable;
use crate::HostId;
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
    /// What orders a subject's events: an entity's transfer stamp; for a player, the
    /// hand-off's hop (its placement count — never a host's own world tick).
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
    /// Cuts this pair took part in by counts ([`PeerJournal::counted`]).
    counted: BTreeMap<CutId, Counted>,
}

/// A pair's truncation by counts in one cut ([`PeerJournal::counted`]).
#[derive(Debug, Clone, Default)]
struct Counted {
    kept_sent: BTreeSet<u64>,
    received_before: usize,
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
    peers: BTreeMap<HostId, PeerLog>,
    /// The other participants of each world cut begun here and not decided yet
    /// ([`Self::cut_begun`]): whichever path reports the commit, it truncates toward them.
    cuts: BTreeMap<CutId, BTreeSet<HostId>>,
    base: CutId,
    /// Peers whose entries were taken for a succession report ([`Self::take`]), with the base
    /// the journal counted from then: the report is read until a cut after it commits (the
    /// succession's settle cut), so the peer is not forgotten before ([`Self::forget`]).
    taken: BTreeMap<HostId, CutId>,
    /// Test support: the marks the last truncating commit read, per peer `(out, in)`.
    last_marks: Option<(CutId, BTreeMap<HostId, (Option<u64>, Option<u64>)>)>,
    /// Test support: the pairs the last truncating commit read by counts.
    last_counted: Option<(CutId, BTreeMap<HostId, CountedRead>)>,
}

/// Test support: how a commit truncated toward a peer it counted with
/// ([`PeerJournal::last_counted`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountedRead {
    /// Rows from here on (their place in the order) came after that commit.
    pub after: u64,
    /// The sent entries (queue ids) kept as this host's part of the cut.
    pub kept_sent: BTreeSet<u64>,
}

impl PeerJournal {
    /// An empty journal from committed cut `base` (at boot: the store's last committed
    /// cut, `CutId(0)` before any).
    pub fn new(base: CutId) -> Self {
        Self { peers: BTreeMap::new(), cuts: BTreeMap::new(), base, taken: BTreeMap::new(), last_marks: None, last_counted: None }
    }

    /// This host takes part in world cut `cut` with `participants` (the other hosts of it):
    /// remembered until the cut is decided, so a commit learned from the store (no
    /// `CutDone`) truncates toward them as one learned from the coordinator does.
    pub fn cut_begun(&mut self, cut: CutId, participants: &BTreeSet<HostId>) {
        self.cuts.insert(cut, participants.clone());
    }

    /// The last committed cut the journal counts from.
    pub fn base(&self) -> CutId {
        self.base
    }

    /// This host queued `msg` (id `queue_id` among everything queued to `peer`) at world
    /// tick `_now` (no entry is stamped with it: see `subjects`); `accounted`: the cut it
    /// travels wrapped in. Only a transfer or a player move is journalled (an `InCut` is
    /// unwrapped).
    pub fn sent(&mut self, peer: HostId, queue_id: u64, msg: &Reliable, accounted: Option<CutId>, _now: Tick) {
        let (msg, accounted) = unwrap(msg, accounted);
        for (subject, stamp) in subjects(msg) {
            let event = JournalEvent::Sent(Box::new(msg.clone()));
            self.peers.entry(peer).or_default().push(accounted, Some(queue_id), JournalEntry { subject, stamp, event });
        }
    }

    /// `msg` came from `peer` at world tick `_now` (stamps nothing, as in [`Self::sent`]);
    /// `accounted`: the cut it came wrapped in. Only a transfer or a player move is journalled.
    pub fn received(&mut self, peer: HostId, msg: &Reliable, accounted: Option<CutId>, _now: Tick) {
        let (msg, accounted) = unwrap(msg, accounted);
        for (subject, stamp) in subjects(msg) {
            let entry = JournalEntry { subject, stamp, event: JournalEvent::Received };
            self.peers.entry(peer).or_default().push(accounted, None, entry);
        }
    }

    /// This host queued its `CutMark { cut }` to `peer`.
    pub fn mark_out(&mut self, peer: HostId, cut: CutId) {
        let log = self.peers.entry(peer).or_default();
        let at = log.next_seq;
        log.marks_out.entry(cut).or_insert(at);
    }

    /// `peer`'s `CutMark { cut }` arrived.
    pub fn mark_in(&mut self, peer: HostId, cut: CutId) {
        let log = self.peers.entry(peer).or_default();
        let at = log.next_seq;
        log.marks_in.entry(cut).or_insert(at);
    }

    /// World cut `cut` runs with `peer` in count mode (`world_manager/world_cut.rs`: their
    /// link was down): at its commit the pair truncates by the counts, not by marks (a mark
    /// recorded for it is ignored) — a `Sent` entry stays iff it travelled wrapped in `cut`
    /// or its queue id is in `kept_sent` (numbered above the peer's relayed count: this
    /// host's part of the cut holds it); of the `Received` entries the first
    /// `received_before` (inside this host's own count: in its part) go unless they came
    /// wrapped in `cut` (the sender's part holds those). Called when this host's part
    /// completes; a later call replaces an earlier one.
    pub fn counted(&mut self, peer: HostId, cut: CutId, kept_sent: &BTreeSet<u64>, received_before: usize) {
        let log = self.peers.entry(peer).or_default();
        log.counted.insert(cut, Counted { kept_sent: kept_sent.clone(), received_before });
    }

    /// The `Received` entries with `peer` now.
    pub fn received_len(&self, peer: HostId) -> usize {
        self.peers.get(&peer).map_or(0, |log| log.rows.iter().filter(|r| r.queue_id.is_none()).count())
    }

    /// World cut `cut` committed with `participants` (the other hosts of it, joined with
    /// those [`Self::cut_begun`] recorded; none for a one-phase cut): toward each, what the
    /// cut already explains is dropped (the module docs). A direction whose mark this
    /// journal never saw keeps everything (never guess). The base becomes `cut`; marks and
    /// participants of it and earlier cuts are forgotten, so a second report of the same
    /// commit (the store's poll, then the coordinator's `CutDone`) changes nothing.
    pub fn committed(&mut self, cut: CutId, participants: &BTreeSet<HostId>) {
        self.base = self.base.max(cut);
        let mut participants = participants.clone();
        participants.extend(self.cuts.get(&cut).into_iter().flatten().copied());
        self.cuts.retain(|c, _| *c > cut);
        let mut read = BTreeMap::new();
        let mut read_counted = BTreeMap::new();
        for (peer, log) in &mut self.peers {
            if let Some(c) = log.counted.get(&cut).filter(|_| participants.contains(peer)).cloned() {
                read_counted.insert(*peer, CountedRead { after: log.next_seq, kept_sent: c.kept_sent.clone() });
                let mut received = 0usize;
                log.rows.retain(|r| {
                    if r.accounted == Some(cut) {
                        return true;
                    }
                    match r.queue_id {
                        Some(id) => c.kept_sent.contains(&id),
                        None => {
                            received += 1;
                            received > c.received_before
                        }
                    }
                });
            } else if participants.contains(peer) {
                let out = log.marks_out.get(&cut).copied();
                let inn = log.marks_in.get(&cut).copied();
                read.insert(*peer, (out, inn));
                log.rows.retain(|r| {
                    let mark = if r.queue_id.is_some() { out } else { inn };
                    let explained = r.accounted != Some(cut) && mark.is_some_and(|m| r.seq < m);
                    !explained
                });
            }
            log.marks_out.retain(|c, _| *c > cut);
            log.marks_in.retain(|c, _| *c > cut);
            log.counted.retain(|c, _| *c > cut);
        }
        // A second report of the same commit read no marks (they are gone): keep the first.
        if !participants.is_empty() && self.last_marks.as_ref().is_none_or(|(c, _)| *c < cut) {
            self.last_marks = Some((cut, read));
            self.last_counted = Some((cut, read_counted));
        }
    }

    /// World cut `cut` was aborted: its marks and counts are forgotten, every entry stays.
    pub fn aborted(&mut self, cut: CutId) {
        self.cuts.remove(&cut);
        for log in self.peers.values_mut() {
            log.marks_out.remove(&cut);
            log.marks_in.remove(&cut);
            log.counted.remove(&cut);
        }
    }

    /// Every entry with `peer`, in order (a succession's report); the journal drops them and
    /// remembers the base it counted from, so the peer is forgotten ([`Self::forget`]) only
    /// once a later cut — the succession's settle — committed.
    pub fn take(&mut self, peer: HostId) -> Vec<JournalEntry> {
        self.taken.insert(peer, self.base);
        self.peers.remove(&peer).map(|log| log.rows.into_iter().map(|r| r.entry).collect()).unwrap_or_default()
    }

    /// The journal forgets `peer` for good (a dead or departed host the manager forgets,
    /// `WorldManager::forget_host`): its marks, counts and every trace of it. Refused while
    /// anything of it is still needed — entries not reported or explained by a cut (a
    /// succession reads them), a report taken against a base no later cut has committed past
    /// yet (the settle cut is the one that makes it unnecessary), or a cut not decided yet
    /// that it takes part in. A peer never journalled is forgotten at once.
    pub fn forget(&mut self, peer: HostId) -> Result<(), ForgetRefused> {
        let rows = self.peers.get(&peer).map_or(0, |log| log.rows.len());
        if rows > 0 {
            return Err(ForgetRefused { peer, why: format!("{rows} journal entries with it are not reported or saved") });
        }
        if let Some(&at) = self.taken.get(&peer).filter(|at| **at >= self.base) {
            return Err(ForgetRefused {
                peer,
                why: format!("its report was taken against cut {} and no later cut has committed (the settle cut)", at.0),
            });
        }
        if let Some((cut, _)) = self.cuts.iter().find(|(_, p)| p.contains(&peer)) {
            return Err(ForgetRefused { peer, why: format!("world cut {} with it is not decided", cut.0) });
        }
        self.peers.remove(&peer);
        self.taken.remove(&peer);
        if let Some((_, read)) = self.last_marks.as_mut() {
            read.remove(&peer);
        }
        if let Some((_, read)) = self.last_counted.as_mut() {
            read.remove(&peer);
        }
        Ok(())
    }

    /// Whether the journal holds anything of `peer` (entries, marks, counts, a report taken).
    pub fn mentions(&self, peer: HostId) -> bool {
        self.peers.contains_key(&peer)
            || self.taken.contains_key(&peer)
            || self.cuts.values().any(|p| p.contains(&peer))
    }

    /// The entries with `peer`, in order, each with its queue id (a sent entry's id among
    /// everything queued to `peer`; `None` for a received one).
    pub fn entries(&self, peer: HostId) -> impl Iterator<Item = (&JournalEntry, Option<u64>)> {
        self.peers.get(&peer).into_iter().flat_map(|log| log.rows.iter().map(|r| (&r.entry, r.queue_id)))
    }

    /// Test support: the entries with `peer` as truncation reads them — `(entry, place
    /// in the order, accounted cut)` — and the marks the last truncating commit read
    /// (`(cut, peer → (this host's mark, the peer's mark))`, places in the same order).
    #[doc(hidden)]
    pub fn rows(&self, peer: HostId) -> impl Iterator<Item = (&JournalEntry, u64, Option<CutId>)> {
        self.peers.get(&peer).into_iter().flat_map(|log| log.rows.iter().map(|r| (&r.entry, r.seq, r.accounted)))
    }

    #[doc(hidden)]
    pub fn last_marks(&self) -> Option<&(CutId, BTreeMap<HostId, (Option<u64>, Option<u64>)>)> {
        self.last_marks.as_ref()
    }

    /// Test support: the pairs the last truncating commit read by counts (with the marks of
    /// [`Self::last_marks`], of the same commit).
    #[doc(hidden)]
    pub fn last_counted(&self) -> Option<&(CutId, BTreeMap<HostId, CountedRead>)> {
        self.last_counted.as_ref()
    }


    /// Entries over every peer (the `[server]` line's `journal_entries=`).
    pub fn len(&self) -> usize {
        self.peers.values().map(|log| log.rows.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Why [`PeerJournal::forget`] refused to forget a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetRefused {
    pub peer: HostId,
    pub why: String,
}

impl std::fmt::Display for ForgetRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the peer journal still needs host {}: {}", self.peer, self.why)
    }
}

impl std::error::Error for ForgetRefused {}

/// `msg` without its [`Reliable::InCut`] wrapper (its cut becomes `accounted`).
fn unwrap(msg: &Reliable, accounted: Option<CutId>) -> (&Reliable, Option<CutId>) {
    match msg {
        Reliable::InCut { cut, body } => (body, Some(*cut)),
        other => (other, accounted),
    }
}

/// The subjects a state message is about, each with its stamp: a transfer's entity at the
/// transfer's stamp (causal: applied at stamp + 1 on the receiver, one tick numbering for
/// every host); a PLAYER — handed over with a body or moved without one — at the hand-off's
/// hop ([`crate::PlayerHandoff::hop`]: the sender's + 1, so every later placement of the
/// player counts higher on whichever host it happens). Never a host's own world tick: two
/// machines' clocks may differ by a few ticks, and a fast S1 → D → S2 chain stamped by tick
/// could resolve to the older `Sent`. Nothing for any other message.
fn subjects(msg: &Reliable) -> Vec<(Subject, Tick)> {
    match msg {
        Reliable::Transfer { stamp, bundle, player, .. } => {
            let mut out = vec![(Subject::Entity(bundle.id), *stamp)];
            out.extend(player.as_ref().map(|h| (Subject::Player(h.player), h.hop)));
            out
        }
        Reliable::PlayerMove { handoff, .. } => vec![(Subject::Player(handoff.player), handoff.hop)],
        _ => Vec::new(),
    }
}
