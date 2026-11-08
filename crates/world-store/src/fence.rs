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
}

/// What the coordinator claimed: the epoch of its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoordinatorFence {
    pub epoch: u64,
}

/// Every super-region a cut writes into: its regions (whose rows also hold
/// its entity rows) and its players' records' regions. A commit checks these
/// and every super-region of its fence.
pub fn touched_super_regions(cut: &WorldCut, s: u32) -> BTreeSet<SuperRegion> {
    let regions = cut.regions.iter().map(|(rc, _)| *rc);
    let players = cut.players.iter().map(|(_, record)| record.region);
    regions.chain(players).map(|rc| super_region_of(rc, s)).collect()
}

/// The super-regions a commit with `fence` must find still held: the cut's
/// touched ones and every one the fence claimed (a host fenced anywhere
/// commits nothing — not even its in-flight rows or the world tick).
pub(crate) fn commit_check_set(cut: &WorldCut, fence: &Fence, s: u32) -> BTreeSet<SuperRegion> {
    let mut set = touched_super_regions(cut, s);
    set.extend(fence.epochs.keys().copied());
    set
}

/// The super-region a fenced region load checks.
pub(crate) fn load_check(rc: RegionCoords, s: u32) -> SuperRegion {
    super_region_of(rc, s)
}
