//! The coordinator's store decisions (Elastic 2b Task 3): deciding a world cut, recording a
//! cut's begin, declaring a succession, ending a landless host, rolling the world back,
//! assigning a free super-region (Elastic 3), adding, draining and removing a desired host
//! (Elastic 4). Each
//! is handed out by the core as part of a transition ([`super::Transition::decisions`]) and
//! runs INSIDE the transaction that writes that transition's projection — a savepoint after the
//! compare-and-set of the row (`world_store` `coordinator_tx.rs`). The core and its decisions
//! commit or fail together: the store never holds a decision no committed core knows, and a
//! stale cache issues nothing (issuing IS committing).
//!
//! **Marks.** A decision that needs its answer sets a mark in the projection when it is handed
//! out, stamped with a [`CallStamp`]: this replica and the row version the transition commits
//! as. The answer comes back with the write ([`DecisionAnswer`]) and is applied as a logged
//! input ([`super::Input::Decision`]) — only if its stamp is the mark's; any other answer
//! changes nothing. A mark whose answer never arrives — its write's outcome unknown (a failed
//! write, a lost commit answer), or a mark a restarted or rebased core finds committed with no
//! answer applied — is resolved by a LOOKUP, never by issuing the decision again (a second
//! declaration would meet the first one's open succession and roll the world back):
//! - a cut's decision: the stored decision is read (`cut_decision`; an undecided cut aborted);
//! - a declaration or an end: the open successions are read again, and the next `hosts` read
//!   takes a dead row nobody here declared out of the view (Elastic 2a C1);
//! - a rollback: the world generation is read (the rollback is conditional on its generation);
//! - a cut's begin: nothing (without its record the decision lists every dirty host).
//!
//! An assignment and the three fleet decisions (Elastic 4: [`AddHost`], [`DrainHost`],
//! [`RemoveHost`]) carry no mark: each is idempotent to issue and to apply twice, and a lost
//! answer is repaired by the next read.

use serde::{Deserialize, Serialize};

use super::{CutDecide, Declare, EndLandless, ReplicaId};
use crate::{CutId, Declared, HostId, LandlessEnd};

/// The version a mark holds from its hand-out until its transition is taken: the transition
/// stamps it with the version it commits as ([`super::Coordinator::take_transition`]).
pub const PENDING_VERSION: u64 = u64::MAX;

/// How long (store ms) the core waits after a failed rollback before it hands one out again.
pub const DECISION_RETRY_MS: u64 = 250;

/// Which call a mark (and its answer) is: the replica that handed it out and the row version
/// of the transition that issued it ([`PENDING_VERSION`] until that transition is taken).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallStamp {
    pub replica: ReplicaId,
    pub version: u64,
}

impl CallStamp {
    /// A mark handed out by `replica`, its transition not taken yet.
    pub fn pending(replica: ReplicaId) -> CallStamp {
        CallStamp {
            replica,
            version: PENDING_VERSION,
        }
    }

    pub fn is_pending(&self) -> bool {
        self.version == PENDING_VERSION
    }
}

/// One store decision of a transition, run in the transaction that writes it, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreDecision {
    /// Record the decision on a world cut (`two_phase::decide_in`).
    DecideCut(CutDecide),
    /// Record that world cut `cut` began (`two_phase::begin_in`): the transition that starts it.
    BeginCut { cut: CutId },
    /// Declare successions (`succession::declare_in`).
    Declare(Declare),
    /// End a lapsed landless host (`succession::end_landless_in`).
    EndLandless(EndLandless),
    /// Roll the world back from generation `from` (`succession::rollback_in`; conditional).
    Rollback { from: u64 },
    /// Mark a replica's row dead (Elastic 2b Task 4).
    EndReplica(ReplicaId),
    /// Give a free (or absent) super-region to a live host (Elastic 3; `world_store`
    /// `assign::assign_land_in`).
    AssignLand(AssignLand),
    /// Elastic 4: add a `desired_hosts` row (`world_store` `fleet::add_host_in`).
    AddHost(AddHost),
    /// Elastic 4: mark a `desired_hosts` row draining, and its host's `hosts` row
    /// (`world_store` `fleet::drain_host_in`).
    DrainHost(DrainHost),
    /// Elastic 4: delete a `desired_hosts` row (`world_store` `fleet::remove_host_in`).
    RemoveHost(RemoveHost),
}

/// Elastic 3: super-region `sr` given to host `host` of boot `boot` — only if the row is free
/// or absent and that boot's `hosts` row is live, dirty and not draining.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignLand {
    pub sr: crate::SuperRegion,
    pub host: HostId,
    pub boot: u64,
}

/// What an [`AssignLand`] did. Only `Done` wrote anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Assigned {
    /// The row is the host's now, at `epoch` (1 for an absent row, else the free row's + 1).
    Done { epoch: u64 },
    /// Another host holds the row (or a staged part's release is not committed yet).
    Owned,
    /// The host's row is not that boot's, dead, or cleanly left.
    HostNotLive,
    /// The host's row carries a drain mark: it takes no new land.
    HostDraining,
}

/// Elastic 4: one more `desired_hosts` row, `h<world.next_desired_host>` at the lowest free
/// slot — only if the table holds exactly `expect_wanted` wanted rows (else
/// [`HostAdded::NotNeeded`]: another decision moved it since the core read it) and fewer than
/// `max` rows in all (else [`HostAdded::Full`]: a draining host still holds a slot and a pod).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddHost {
    pub max: u32,
    pub expect_wanted: u32,
}

/// What an [`AddHost`] did. Only `Done` wrote anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostAdded {
    /// The row `name` at `slot` was added, wanted.
    Done { name: String, slot: u32 },
    /// The wanted rows are not `expect_wanted` any more.
    NotNeeded,
    /// `max` rows (wanted or draining) are there already.
    Full,
}

/// Elastic 4: the `desired_hosts` row `name` marked draining; with `host` (its process's id and
/// boot), that `hosts` row's drain mark set to `in_ms` from the store's clock — never later than
/// a mark it has — if it is that boot's live dirty row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainHost {
    pub name: String,
    pub host: Option<(HostId, u64)>,
    pub in_ms: u64,
}

/// What a [`DrainHost`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostDrained {
    /// The row and the host's `hosts` row are marked.
    Marked,
    /// The row is marked; no `hosts` row was (none named, or not that boot's live dirty row).
    RowOnly,
    /// No such row: nothing written.
    Unknown,
}

/// Elastic 4: the `desired_hosts` row `name` deleted (its slot free again, its name never
/// issued again).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveHost {
    pub name: String,
}

/// What a [`RemoveHost`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostRemoved {
    Done,
    /// No such row (removed already).
    Gone,
}

/// The store's answer to one [`StoreDecision`] of a committed transition. An `Err` is the
/// decision's own refusal (a SQL error, a lock timeout, a stale generation): it wrote nothing,
/// and the core still committed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecisionAnswer {
    Cut {
        stamp: CallStamp,
        cut: CutId,
        committed: Result<bool, String>,
    },
    Begin {
        cut: CutId,
        bound: Result<HostId, String>,
    },
    Declared {
        stamp: CallStamp,
        outcome: Result<Declared, String>,
    },
    Ended {
        stamp: CallStamp,
        end: EndLandless,
        outcome: Result<LandlessEnd, String>,
    },
    RolledBack {
        stamp: CallStamp,
        outcome: Result<u64, String>,
    },
    ReplicaEnded {
        replica: ReplicaId,
    },
    Assigned {
        stamp: CallStamp,
        assign: AssignLand,
        outcome: Result<Assigned, String>,
    },
    HostAdded {
        stamp: CallStamp,
        add: AddHost,
        outcome: Result<HostAdded, String>,
    },
    HostDrained {
        stamp: CallStamp,
        drain: DrainHost,
        outcome: Result<HostDrained, String>,
    },
    HostRemoved {
        stamp: CallStamp,
        remove: RemoveHost,
        outcome: Result<HostRemoved, String>,
    },
}
