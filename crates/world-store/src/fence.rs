//! Owner-epoch fencing: a host claims super-regions (`super_regions` rows:
//! owner + an epoch bumped by every claim) and the coordinator claims its one
//! row; every fenced write or load then checks, in its own transaction, that
//! the epochs it claimed are still the stored ones. A second process claiming
//! the same super-region (or the coordinator) bumps the epoch, so the first
//! one's next write is refused with [`crate::StoreError::Fenced`] and writes
//! nothing. The epoch replaces Postgres' old advisory "world lock": many
//! processes may open one database, each writes only what it holds.

use std::collections::{BTreeMap, BTreeSet};

use game::{RegionCoords, WorldCut, super_region_of};

/// A super-region's coordinates (`game::super_region_of`).
pub type SuperRegion = (i32, i32);

/// What a host claimed: its owner id and, per super-region, the epoch the
/// claim set. Passed to every fenced region load and cut commit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fence {
    pub owner: u32,
    pub epochs: BTreeMap<SuperRegion, u64>,
    /// The process runs every host's regions (`Ownership::AllInOne`): it adopts every
    /// host's in-flight rows ([`Fence::in_flight_scope`]) and its cuts replace them all.
    /// Otherwise a host adopts and replaces only the rows of `owner` — never another
    /// host's, which that host still re-commits (adopting them would deliver them twice).
    pub all_hosts: bool,
}

impl Fence {
    /// Whose in-flight rows this process adopts at start
    /// ([`crate::StoreHandle::load_in_flight`]): `None` = every host's.
    pub fn in_flight_scope(&self) -> Option<u32> {
        (!self.all_hosts).then_some(self.owner)
    }

    /// The claim's epoch as one number: the highest owner epoch it set (0 with no
    /// super-regions). A later claim of the same super-regions bumps every epoch, so the
    /// process that claimed last has the higher one (its service-link hello carries it).
    pub fn claim_epoch(&self) -> u64 {
        self.epochs.values().copied().max().unwrap_or(0)
    }
}

/// What the coordinator claimed: the epoch of its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoordinatorFence {
    pub epoch: u64,
}

/// Every super-region a cut writes into: its regions (whose rows also hold
/// its entity rows). A commit checks these and every super-region of its fence.
///
/// A player record's region is not one: a host writes the records of the
/// players it holds, wherever they are headed (a record of a player handed to
/// another host's region rides in the cut until that host has it). The commit
/// is still fenced by every super-region the host claimed, so a fenced-out host
/// writes no record.
pub fn touched_super_regions(cut: &WorldCut, s: u32) -> BTreeSet<SuperRegion> {
    cut.regions.iter().map(|(rc, _)| super_region_of(*rc, s)).collect()
}

/// The super-regions a commit with `fence` must find still held: the cut's
/// touched ones and every one the fence claimed (a host fenced anywhere
/// commits nothing — not even its in-flight rows or the world tick). An
/// empty fence with a cut that touches no super-region checks nothing: that
/// commit (in-flight rows, the world tick) is unfenced.
pub(crate) fn commit_check_set(cut: &WorldCut, fence: &Fence, s: u32) -> BTreeSet<SuperRegion> {
    let mut set = touched_super_regions(cut, s);
    set.extend(fence.epochs.keys().copied());
    set
}

/// The super-region a fenced region load checks.
pub(crate) fn load_check(rc: RegionCoords, s: u32) -> SuperRegion {
    super_region_of(rc, s)
}
