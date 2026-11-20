//! The coordinator's replicas (Elastic 2b): several coordinator processes over ONE
//! `coordinator` row, each a cache of the row that writes its transitions with a
//! compare-and-set and, when the row moved under it, rebases onto the newer row and replays
//! what it applied since its last commit (`input.rs`).
//!
//! **Identity and the live set** (Elastic 2b Task 4): a replica registers once per process
//! start — the store issues its id from `coordinator.next_replica`, never reused — and beats
//! its `coordinator_replicas` row every [`REPLICA_BEAT_MS`] from its main loop. The core keeps
//! the DURABLE set of live replicas (`Coordinator::replicas_live`): [`super::Input::ReplicaJoined`]
//! is every replica's first input (it adds itself; `alone` — the store found no other row beaten
//! within [`REPLICA_LEASE_MS`], the total replica crash — drops every other id registered before
//! it), [`super::Input::ReplicaLapsed`] drops one (its row is marked dead in the same transaction,
//! [`super::StoreDecision::EndReplica`]). An attachment names no replica (coordinator HTTP API: a
//! host's mailbox is read from any), so a replica's lapse detaches nobody; a store call another
//! replica handed out is that replica's to answer while it is in the set, and is looked up once
//! it is not. The ticker is the lowest id of the set
//! (`Coordinator::ticker`).
//!
//! A replica not in the set of the core it applies to (a rebase onto a row that ended it) writes
//! nothing and exits fenced; so does one whose beat finds its row dead.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// A coordinator replica: one process start, never reused, nothing derived from its number
/// (Elastic 2b replaces Elastic 2a's process boot with it). The store issues it at the
/// process's start (`register_replica`); tests pick one. A store call handed out by another
/// replica is not in flight here.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct ReplicaId(pub u64);

impl std::fmt::Display for ReplicaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "r{}", self.0)
    }
}

/// How long a replica's row may stand still (its `seen_ms` unchanged over this long of another
/// replica's own reads, on that replica's monotonic clock) before it is ended; a registration
/// counts another replica alive while the database clock minus its `seen_ms` is within it.
pub const REPLICA_LEASE_MS: u64 = 10_000;

/// How often a replica beats its `coordinator_replicas` row (from its main loop).
pub const REPLICA_BEAT_MS: u64 = 2_000;

/// `coordinator_replicas.state` of a live replica.
pub const REPLICA_LIVE: i64 = 0;
/// `coordinator_replicas.state` of an ended one (its lease lapsed, or a registration found it
/// lapsed): its next beat answers "not live" and it exits fenced.
pub const REPLICA_DEAD: i64 = 1;

/// One `coordinator_replicas` row, as a replicas read returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaRow {
    pub replica: ReplicaId,
    /// The database clock (ms) of its last beat.
    pub seen_ms: u64,
    /// `state` is [`REPLICA_LIVE`].
    pub live: bool,
    /// Where it listens for hosts (a label).
    pub addr: String,
}

/// What the watch remembers of one live row.
#[derive(Clone, Debug)]
struct Seen {
    seen_ms: u64,
    /// The monotonic ms of the first read showing this `seen_ms`.
    first_ms: u64,
    /// The monotonic ms of the latest read still showing it.
    last_ms: u64,
}

/// A replica's lapse watch over the `coordinator_replicas` rows, as [`super::LeaseWatch`] is over
/// the `hosts` rows: an observation of this process on its MONOTONIC clock (never the store's),
/// never durable — a new replica counts every lease from zero.
///
/// Each read is stamped with the monotonic ms it was ISSUED at: a row showing one `seen_ms`
/// from a read issued at `first` to a later read issued at least [`REPLICA_LEASE_MS`] after
/// `first` lapsed. Time without a read counts for nothing (a stalled watcher never lapses
/// anyone by its own stall: the read after the stall is issued after it, and shows the beats
/// made meanwhile).
#[derive(Clone, Debug, Default)]
pub struct ReplicaWatch {
    rows: BTreeMap<ReplicaId, Seen>,
    /// The rows the latest successful read showed dead.
    dead: BTreeSet<ReplicaId>,
    /// The highest id the latest successful read returned (`None`: no read, or no row).
    highest: Option<ReplicaId>,
}

impl ReplicaWatch {
    /// A successful read of every `coordinator_replicas` row, issued at monotonic `mono_ms`. A
    /// live row whose `seen_ms` changed starts its count again; a dead row is remembered as dead
    /// until the next read ([`Self::gone`]); a vanished row is forgotten.
    pub fn observe(&mut self, rows: &[ReplicaRow], mono_ms: u64) {
        let mut next = BTreeMap::new();
        for row in rows.iter().filter(|r| r.live) {
            let first_ms = match self.rows.get(&row.replica) {
                Some(s) if s.seen_ms == row.seen_ms => s.first_ms,
                _ => mono_ms,
            };
            next.insert(
                row.replica,
                Seen {
                    seen_ms: row.seen_ms,
                    first_ms,
                    last_ms: mono_ms,
                },
            );
        }
        self.rows = next;
        self.dead = rows.iter().filter(|r| !r.live).map(|r| r.replica).collect();
        self.highest = rows.iter().map(|r| r.replica).max();
    }

    /// A read that failed: every observation is forgotten (an outage is no evidence of anyone's
    /// death).
    pub fn observe_failed(&mut self) {
        self.rows.clear();
        self.dead.clear();
        self.highest = None;
    }

    /// The replicas of `core_live` (the core's live set) other than `me` whose rows the latest
    /// read shows ended without any watch counting their lease — a registration marked them dead
    /// (Elastic 2b Task 4 fix round 1): a row shown dead, or a row absent (pruned) whose id is
    /// below the highest id the read returned (one above it registered after the read was
    /// issued: never gone). By id.
    pub fn gone(&self, core_live: &BTreeSet<ReplicaId>, me: ReplicaId) -> Vec<ReplicaId> {
        let Some(highest) = self.highest else {
            return Vec::new();
        };
        core_live
            .iter()
            .copied()
            .filter(|r| *r != me)
            .filter(|r| self.dead.contains(r) || (!self.rows.contains_key(r) && *r < highest))
            .collect()
    }

    /// The live rows the reads show standing still for [`REPLICA_LEASE_MS`] by the reads issued
    /// up to `now_mono`, by id.
    pub fn lapsed(&self, now_mono: u64) -> Vec<ReplicaId> {
        self.rows
            .iter()
            .filter(|(_, s)| s.last_ms.min(now_mono).saturating_sub(s.first_ms) >= REPLICA_LEASE_MS)
            .map(|(r, _)| *r)
            .collect()
    }

    /// The live rows the latest read showed that have not lapsed by `now_mono`, by id.
    pub fn live(&self, now_mono: u64) -> BTreeSet<ReplicaId> {
        let lapsed: BTreeSet<ReplicaId> = self.lapsed(now_mono).into_iter().collect();
        self.rows
            .keys()
            .copied()
            .filter(|r| !lapsed.contains(r))
            .collect()
    }

    /// Whether `me` is the replica that ends lapsed ones: the lowest id of those this watch shows
    /// live and not lapsed, or — nothing shown yet — never.
    pub fn ends_lapsed(&self, me: ReplicaId, now_mono: u64) -> bool {
        self.live(now_mono).first() == Some(&me)
    }
}
