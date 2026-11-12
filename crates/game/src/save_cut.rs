//! The world-tick save cut (world store R0+R1, Task 5; plan Decisions
//! 10–12): how the [`crate::WorldManager`] saves the world without stopping
//! it.
//!
//! **The cut is a world tick in the future.** [`crate::WorldManager::begin_cut`]
//! picks `T = now + lead` ([`SAVE_CUT_LEAD_TICKS`]) and asks every running
//! region for its portable snapshot at the opening of slot T
//! ([`crate::RegionInput::SaveAt`]): after slot T−1 closed, before any
//! slot-T input. No region waits — each reaches T on its own clock. A region
//! already past T answers [`crate::RegionOutput::CutMissed`]; the cut is
//! aborted and the next one doubles the lead (up to
//! [`SAVE_CUT_LEAD_MAX_TICKS`]). A region that parks before T answers with
//! its `Stopped` snapshot (it is parked at T); one that starts at or before T
//! while the cut is pending is asked too.
//!
//! **In-flight rule.** A crossing or intent stamped at sender tick `s` is in
//! the cut iff `s < T` and it is absent from its target's snapshot at T. The
//! manager decides it by the output channel's order, which is exact: at the
//! snapshot point a region first flushes the Tick's departures and the
//! intents sent so far, then sends its `CutSnapshot`. So an item routed
//! before its source answered left the source before the snapshot (not in
//! the source's view), and one confirmed by its target before the target
//! answered is in the target's view. An item in neither view is an
//! [`InFlightRow`] of the cut — a player's crossing a [`PlayerRecord`]
//! instead, a person's dropped (the history owns persons).
//!
//! A region that parks before T answers from `Shutdown`: what is held for
//! slots before T is applied, the snapshot taken, then the rest. A body the
//! manager creates or restores (a move, a reconnect) in a region that has not
//! answered is held to slot T+1 — its park may have landed after its old
//! region's snapshot — and a parked record keeps its state at T while the cut
//! is pending (an item reply folded into it after its region answered goes
//! to the next cut). A cut still waiting [`CUT_DEADLINE_LEADS`] leads past
//! T is aborted.
//!
//! **Dirty tracking survives failures.** A region's snapshot carries only
//! the chunks changed since its previous one; the manager merges every
//! snapshot (a cut's or a `Stopped`'s, aborted cuts included) into an
//! uncommitted entry per region, and a cut carries those entries. An entry is
//! cleared only when a cut carrying it commits
//! ([`crate::WorldManager::cut_committed`]) and nothing newer was merged
//! since; a failed commit ([`crate::WorldManager::cut_failed`]) keeps it.
//!
//! **Players are written only in cuts.** `players` holds the records of the
//! bodies in the snapshots, of the players crossing a seam in the cut, and of
//! the parked players (`WorldManager`'s `parked_players`).
use std::collections::{BTreeMap, BTreeSet};

use crate::{EntityBundle, IntentMsg, PlayerId, PlayerRecord, PortableRegion, RegionCoords, Tick};

/// How far ahead of the world tick a cut is placed: every region reaches it
/// on its own clock (one second at 50 ms ticks).
pub const SAVE_CUT_LEAD_TICKS: Tick = 20;
/// The longest lead a retry after a missed cut doubles up to.
pub const SAVE_CUT_LEAD_MAX_TICKS: Tick = 640;
/// A pending cut still waiting for a region this many leads past its tick
/// (world ticks) is aborted, logged with the regions it waited for: a region
/// thread that died while stopping never answers. Such a stopping region is
/// left out of later cuts until it parks.
pub const CUT_DEADLINE_LEADS: Tick = 10;

/// One save cut's id, fresh per [`crate::WorldManager::begin_cut`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct CutId(pub u64);

/// Something on its way between regions at the cut tick, in no region's
/// snapshot: restored, it is put back on its way
/// ([`crate::WorldManager::restore_in_flight`]).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum InFlightRow {
    /// A crossing (never a player's or a person's): the bundle in `target`'s
    /// frame and its stamp.
    Transfer { target: RegionCoords, bundle: EntityBundle, stamp: Tick },
    /// An intent or a reply for `target`.
    Intent { target: RegionCoords, msg: IntentMsg },
}

/// The world at one tick, for the store: every region snapshot not committed
/// yet (each the merge of its uncommitted snapshots), what was in flight and the
/// players.
#[derive(Debug, Clone)]
pub struct WorldCut {
    pub id: CutId,
    /// The region host that cut it (0 = the only one); scopes the in-flight rows it replaces.
    pub host: u32,
    pub tick: Tick,
    pub regions: Vec<(RegionCoords, PortableRegion)>,
    pub in_flight: Vec<InFlightRow>,
    pub players: Vec<(PlayerId, PlayerRecord)>,
    /// The R1 history row. A host's cut never carries it any more (`None`): the world
    /// coordinator is the only writer of history state (its own `coordinator` row); an R1
    /// world's row is read once, to migrate it.
    pub history: Option<Vec<u8>>,
}

/// A region's snapshots not committed yet, merged ([`PortableRegion::merge`]);
/// `seq` grows with every merge, so a commit clears only what it carried.
#[derive(Debug)]
pub(crate) struct Uncommitted {
    pub region: PortableRegion,
    pub seq: u64,
}

/// A cut asked for and not complete yet (`WorldManager::pending_cut`).
#[derive(Debug)]
pub(crate) struct PendingCut {
    pub id: CutId,
    pub tick: Tick,
    /// The world tick past which it is aborted ([`CUT_DEADLINE_LEADS`]).
    pub deadline: Tick,
    /// Regions whose answer (a `CutSnapshot`, or a `Stopped`) is still to
    /// come. Every other region's view is taken: answered, or not running
    /// (its uncommitted entry, or the store).
    pub waiting: BTreeSet<RegionCoords>,
    /// Each answered region's uncommitted entry right after its answer, and
    /// that entry's `seq`; regions that were not running at `begin_cut` are
    /// here from the start.
    pub regions: BTreeMap<RegionCoords, (PortableRegion, u64)>,
    /// The players of each region's answer: `(order, player, record)`.
    pub region_players: BTreeMap<RegionCoords, Vec<(u64, PlayerId, PlayerRecord)>>,
    /// Players crossing a seam that the cut caught (confirmed by a target
    /// after its answer): `(order, player, record)`.
    pub crossing_players: Vec<(u64, PlayerId, PlayerRecord)>,
    /// Records that passed through the manager while the cut was pending
    /// (parks, restores, moves): the fallback for a player in no snapshot.
    pub transit_players: BTreeMap<PlayerId, PlayerRecord>,
    /// Parked records as they were before something sent after its region's
    /// answer was folded into them (that region's snapshot still holds it).
    pub parked_at_t: BTreeMap<PlayerId, PlayerRecord>,
    /// Crossings and intents a target confirmed after its answer.
    pub rows: Vec<InFlightRow>,
    /// A world cut (several hosts): players whose record another part writes — handed to
    /// a participant before this host's mark (`None`: the receiver's), or received from a
    /// participant after its mark (the sender's snapshot holds them; `Some` keeps the
    /// record this part already had for the player at the cut tick, from before that
    /// arrival). This part's fallbacks (parked, passing through, unreceived hand-offs)
    /// never write them; a region snapshot or a crossing still does.
    pub not_ours: BTreeMap<PlayerId, Option<PlayerRecord>>,
}

impl PendingCut {
    pub fn new(id: CutId, tick: Tick, deadline: Tick) -> Self {
        Self {
            id,
            tick,
            deadline,
            waiting: BTreeSet::new(),
            regions: BTreeMap::new(),
            region_players: BTreeMap::new(),
            crossing_players: Vec::new(),
            transit_players: BTreeMap::new(),
            parked_at_t: BTreeMap::new(),
            rows: Vec::new(),
            not_ours: BTreeMap::new(),
        }
    }
}
