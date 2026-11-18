//! The world coordinator: the services that do not partition by region — the history and
//! its calendar, claims (one adventure per player), adventure validation, legends, person
//! info, the daily calendar / site broadcasts and notices. A host talks to it only through
//! [`HostMsg`] / [`CoordMsg`] over a [`WorldLink`]. Threadless and IO-free: time arrives as
//! `now_ms` — the store's clock in milliseconds (`clock.rs`: [`StoreClock`],
//! [`CALENDAR_GAP_MS`]) — and writing is the caller's ([`Coordinator::take_transition`]).
//!
//! **One durable projection (Elastic 2a, `state.rs`):** everything the core holds but its
//! observations and its outgoing messages is the projection the runtime writes with a
//! compare-and-set; a core [`Coordinator::load`]ed from it behaves as the one that wrote it.
//! **Every output is durable before it leaves:** a message the core decides waits in its
//! outgoing queue until [`Coordinator::take_transition`] hands it out with the projection that
//! decided it (the runtime sends it once that write committed), or — when nothing durable
//! changed — [`Coordinator::take_out`] releases it. The outgoing stream keeps its order, so a
//! host sees the coordinator's messages in the order they were decided (a later `SpawnPeople`
//! never overtakes an earlier directive for that region).
//!
//! **Replicas over one row (Elastic 2b, `input.rs`):** every input arrives through
//! [`Coordinator::apply`], which logs it until its transition commits; a write that meets a
//! newer row is followed by [`Coordinator::rebase`]: the newer projection, the log applied
//! again onto it. Attachments are stamped with the [`ReplicaId`], store calls with a
//! [`CallStamp`]: every store decision runs inside the transaction of the transition that
//! decided it (`decisions.rs`), its answer an input of the next pass. ONE replica, the ticker
//! ([`Coordinator::is_ticker`]: the lowest live id), runs the timed and lease work — the upkeep,
//! the `hosts` and successions reads, every store-call hand-out (Elastic 2b Task 6, `input.rs`).
//!
//! **Many hosts:** every outgoing message carries its [`Target`]: a region's messages go to
//! the host that announced the region (`RegionStarted`, its `Attach`, a delivered
//! `PersonCrossed`), a reply or claim answer to the host that asked, the world's view to
//! every host. Which host runs which region (`region_host`) and where each player is
//! online (`online_on`) are part of the projection; after a restart every host's `Attach`
//! corrects them (its unlisted regions park, its unlisted players go offline), and a host that
//! never comes back is ended by its succession.

mod clock;
mod decisions;
mod input;
mod link;
mod msg;
mod replicas;
mod state;
mod succession;
mod world_cut;

pub use clock::*;
pub use decisions::*;
pub use input::*;
pub use link::*;
pub use msg::*;
pub use replicas::*;
pub use state::*;
pub use succession::*;
pub use world_cut::*;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use state::{decode_core, raw_core, seal_core};

use crate::HostId;
use crate::history::persist::HistoryLoadError;
use crate::history::{
    AdventureRefusal, CalendarTick, Directive, History, SiteId, SocietyGrid, TICKS_PER_DAY,
};
use crate::{CutId, PlayerId, RegionCoords};

/// Answers of state-changing requests remembered for replay past which the coordinator
/// logs an error (at most once per [`MEMO_WARN_SECS`]): answers are forgotten only when
/// their host acknowledges them ([`HostMsg::Settled`], an `Attach`'s watermark or new
/// boot), never by count — a forgotten answer would re-execute a resent request.
pub const ANSWERED_MEMO_MAX: usize = 4096;
/// Seconds between two errors about a memo over [`ANSWERED_MEMO_MAX`].
pub const MEMO_WARN_SECS: u64 = 60;
/// Most times one host renumbers one player's placement on a [`CoordMsg::HopIs`] (per
/// placement: a new login or arrival starts over): two hosts that both hold the player (a duplicate body)
/// would otherwise outbid each other for ever. Past it the host logs an error once and
/// stops renumbering that player.
pub const HOP_RENUMBER_MAX_PER_PLAYER: u32 = 8;

/// A host's count of [`CoordMsg::HopIs`] renumberings per player
/// ([`HOP_RENUMBER_MAX_PER_PLAYER`]).
#[derive(Debug, Default)]
pub struct RenumberBudget {
    used: BTreeMap<PlayerId, u32>,
    errors: u32,
}

impl RenumberBudget {
    /// Whether `player` may be renumbered once more (counted if so). The first refusal
    /// logs an error.
    pub fn take(&mut self, player: PlayerId) -> bool {
        let n = self.used.entry(player).or_default();
        if *n < HOP_RENUMBER_MAX_PER_PLAYER {
            *n += 1;
            return true;
        }
        if *n == HOP_RENUMBER_MAX_PER_PLAYER {
            *n += 1;
            self.errors += 1;
            log::error!(
                "{player:?}: renumbered {HOP_RENUMBER_MAX_PER_PLAYER} times; another host holds it too — no more renumbering"
            );
        }
        false
    }

    /// `player` is placed on the host anew (a login, an arrival): its budget starts over.
    /// Renumbering itself is no new placement, so two holders still stop outbidding.
    pub fn reset(&mut self, player: PlayerId) {
        self.used.remove(&player);
    }

    /// Errors logged (players whose budget ran out).
    pub fn errors_logged(&self) -> u32 {
        self.errors
    }
}
/// The sites as last broadcast, bucketed: a broadcast goes out when a site is founded or
/// abandoned, or a population crosses a multiple of 10.
fn sites_key(history: &History) -> Vec<(SiteId, bool, u32)> {
    history.site_markers().into_iter().map(|m| (m.site, m.abandoned, m.population / 10)).collect()
}

/// The world coordinator's core.
///
/// Every field is classified in [`projection!`] (below): durable — in the projection the
/// runtime writes (`state.rs`) — or the process's own. The struct literals of
/// [`Coordinator::new_for`] and [`Coordinator::load`] name every field, so a new one does not
/// compile until it is classified.
pub struct Coordinator {
    history: History,
    /// What each player plays (one adventure at a time).
    player_claims: BTreeMap<PlayerId, Claim>,
    /// Each region's latest incarnation seen start (`RegionStarted`, `Attach`): the stamp
    /// on its directives, and the bar a bounce must meet to be deferred.
    incarnations: BTreeMap<RegionCoords, u64>,
    /// Answers of state-changing requests, replayed for a repeated id.
    memo: BTreeMap<RequestId, WorldReply>,
    /// `memo`'s ids, oldest first.
    memo_order: VecDeque<RequestId>,
    /// Per host, the boots its `Attach`es named: a new one forgets the host's answers of
    /// every other boot.
    boots_seen: BTreeMap<HostId, BTreeSet<u64>>,
    /// `clock_ms` of the last error about an oversized memo (the process's: a log rate limit).
    memo_warned_ms: Option<u64>,
    /// Decided and not handed out yet, in order: they leave with the transition that holds
    /// the state deciding them ([`Self::take_transition`]), or with [`Self::take_out`] when
    /// nothing durable changed.
    out: Vec<(Target, CoordMsg)>,
    /// Which host runs each region, as announced (`RegionStarted`, `Attach`, a delivered
    /// `PersonCrossed`); a park forgets it. Kept beside the history's running set: it also
    /// names the host of a region a delivered crossing announced before the history runs it,
    /// and the history's set is the people's truth while this one routes (Ruling, Elastic 2a
    /// Task 5). After a restart a host's `Attach` corrects it (`attach`).
    region_host: BTreeMap<RegionCoords, HostId>,
    /// The host each online player is online on (`PlayerOnline`, an `Attach`'s online
    /// list, the host that was granted its claim). A player in transit to a host
    /// (`PlayerHandedTo`) stays online on that host until it reports.
    online_on: BTreeMap<PlayerId, HostId>,
    /// The highest hop ([`HostMsg`]'s hop rule) of each player's presence messages
    /// applied: a message with a lower one is stale. Unknown = 0; cleared by an `Attach` of
    /// a newer world generation (every host starts its hops over).
    hop_of: BTreeMap<PlayerId, u64>,
    /// The highest world generation an `Attach` named.
    generation_seen: u64,
    /// The world generation of the latest `Attach` that was the first of its generation
    /// ([`Self::first_of_generation`]): the first host attaching at a higher one finds every
    /// host of the older generation gone.
    attached_generation: u64,
    /// For a player online on a host by that host's own report (not in transit to it):
    /// the host and the hop it reported. A message of another host that moves the player
    /// away from it tells it ([`CoordMsg::HopIs`]): if it still holds the player it
    /// re-asserts above the new hop.
    online_reported: BTreeMap<PlayerId, (HostId, u64)>,
    /// Displaced-host notifications sent, `(player, host, reported hop)`: at most one each.
    /// A player's go when it goes offline on its host; all go at a newer generation.
    displaced_told: BTreeSet<(PlayerId, HostId, u64)>,
    /// The latest `now_ms` seen (store ms, `clock.rs`). The process's: a loaded core starts
    /// at the store time it was loaded at; a rebase keeps it.
    clock_ms: u64,
    /// `(now_ms, calendar tick)` the calendar was anchored at (store ms).
    calendar_anchor: (u64, CalendarTick),
    /// `now_ms` of the calendar's last sign of life — a day turn, a view handed out, or its
    /// anchor: a `now_ms` more than [`CALENDAR_GAP_MS`] past it is downtime, and re-anchors the
    /// calendar where it last stood ([`Self::calendar_now`], [`Self::calendar_resume`]).
    calendar_seen_ms: u64,
    /// The calendar day last broadcast.
    calendar_day: u64,
    /// The sites as last broadcast ([`sites_key`]).
    sites_key: Vec<(SiteId, bool, u32)>,
    /// The world cuts of the `claim` hosts, any number (`world_cut.rs`).
    cuts: WorldCuts,
    /// Which hosts lost their lease (`succession.rs`), fed by the runtime's `hosts` reads.
    /// An observation: a restart counts every lease from zero again.
    leases: LeaseWatch,
    /// The successions declared and not settled yet (`succession.rs`).
    successions: SuccessionRuns,
    /// The generation of the last world rollback this coordinator ordered: a host attaching
    /// with an older one is told it ([`CoordMsg::Generation`]) and not applied.
    rolled_back_to: Option<u64>,
    /// What the hosts told the login service ([`HostMsg::Login`]), in arrival order, for the
    /// runtime to forward ([`Self::take_login_out`]). Nothing about an order is kept.
    login_out: Vec<crate::LoginOut>,
    /// This replica: what its attachments and store calls are stamped with.
    replica: ReplicaId,
    /// Every input applied since the last commit, with the store time it was applied at
    /// (`input.rs`): what a rebase applies again onto a newer row.
    log: Vec<(u64, Input)>,
    /// `log`'s length when the transition in flight was taken: its commit drops the log up to
    /// here ([`Self::transition_written`]).
    log_boundary: usize,
    /// The row version this core holds (as loaded, last committed, or rebased onto).
    version: u64,
    /// This replica acted as the ticker at its last ticker work ([`Self::ticking_now`]): a
    /// change of role restarts what the ticker observes (Elastic 2b Task 6).
    ticking: bool,
    /// The cut period the runtime configured ([`Self::set_world_cuts`]; `None`: never): it holds
    /// across a rebase onto a row written with another.
    cut_every_config: Option<u64>,
    /// The cut ids' high half the runtime configured ([`Self::set_cut_epoch`]; 0: never): a
    /// rebase onto a row written before it was committed raises the row's to it again.
    cut_epoch_config: u64,
    /// The ticker's successions reads applied so far ([`Input::SuccessionsOpen`]), the count when
    /// the transition in flight was taken, and the count its last commit (or a release with
    /// nothing durable pending) covered: a rebase that drops an answer not covered owes a read
    /// (Elastic 2b Task 6).
    reads_applied: u64,
    reads_taken: u64,
    reads_committed: u64,
    /// The hosts the latest successful `hosts` read showed marked draining (`worldctl drain`):
    /// never the gate host ([`Self::takes_logins`]). Durable, so every replica's gate knows the
    /// marks the ticker read (a non-ticker reads no `hosts` rows); a failed read keeps them (a
    /// mark is never lifted).
    draining: BTreeSet<HostId>,
    /// [`Self::take_transition`] just found the projection as committed, and nothing was
    /// applied since: [`Self::take_out`] needs no encode of its own.
    clean_checked: bool,
    /// The bincode of the projection as last handed out ([`Self::take_transition`]) or
    /// loaded: a transition is due when the projection's bytes differ from it (or the history
    /// changed).
    committed_raw: Vec<u8>,
    /// No store stands behind this core ([`Self::commit_unstored`], the inline link): its
    /// outputs are free at once, nothing is encoded.
    unstored: bool,
    /// A transition was handed out ([`Self::take_transition`]) and its write is not answered
    /// yet ([`Self::transition_written`] / [`Self::transition_failed`]): nothing leaves by
    /// [`Self::take_out`] meanwhile — a message decided after it may depend on its state.
    writing: bool,
    /// Store decisions handed out and not taken yet, in order (`decisions.rs`): they leave with
    /// the next transition ([`Self::take_transition`]), never on their own.
    decisions: Vec<StoreDecision>,
    /// The stamp of the transition in flight: the row version it commits as, by this replica.
    /// The marks it carries hold it ([`Self::transition_failed`] resolves them).
    writing_stamp: Option<CallStamp>,
    /// The live coordinator replicas (Elastic 2b Task 4, `replicas.rs`): an attachment counts
    /// only while its replica is here; the lowest is the ticker ([`Self::ticker`]).
    replicas: BTreeSet<ReplicaId>,
    /// This replica's own [`Input::ReplicaJoined`] was applied: from then on a core lacking its
    /// id is [`Self::evicted`].
    joined: bool,
    /// The version check of the outputs free to leave with several live replicas (Elastic 2b
    /// departs 5): handed out ([`Self::version_check_wanted`]), confirmed or not.
    version_check: Option<VersionCheck>,
    /// Messages for hosts attached to another live replica ([`Self::route_out`]), in order: they
    /// leave with the next transition, through the store's outbox (Elastic 2b Task 5).
    outbox: Vec<OutboxMsg>,
    /// How many of `out`'s first messages [`Self::route_out`] routed already (a transition handed
    /// back: its `Every`s were fanned out to `outbox` once — never twice).
    routed: usize,
    /// The hosts whose `Attach` was applied since the last transition was taken: that transition
    /// discards their outbox rows (decided before the `Attach`, which reconciles them).
    attached_now: BTreeSet<HostId>,
    /// Hosts' messages about a world cut this cache cannot know yet — an id above its last one
    /// ([`Self::take_refresh_wanted`]): the row moved past this cache (a host got the `BeginCut`
    /// of a transition another replica committed). Never looked up while the cache may be stale
    /// (a lookup would abort the cut, audit H3): the cache is refreshed and the log replayed.
    stale_cuts: Vec<(HostId, CutId)>,
    /// A relay of such a cut was dropped ([`HostMsg::CutPeerCount`]): the cache is stale.
    stale_seen: bool,
    /// [`Self::take_refresh_wanted`] answered `true` covering this many of `stale_cuts` (and
    /// `stale_seen`): what [`Self::refresh_not_newer`] may resolve.
    refresh_covers: Option<(usize, bool)>,
    /// Test support ([`Self::record_inputs`]): the inputs that left the log by a commit or a
    /// release, in that order.
    recorded: Option<Vec<(u64, Input)>>,
}

/// A version check handed out ([`Coordinator::version_check_wanted`]): the cache's `version`
/// then, and how many outputs and logged inputs it covers (those decided before it was asked).
#[derive(Clone, Copy, Debug)]
struct VersionCheck {
    version: u64,
    out_len: usize,
    log_len: usize,
    /// The row was read at `version` ([`Coordinator::version_checked`]).
    confirmed: bool,
}

/// The classification of every [`Coordinator`] field: the projection's (durable, written by
/// the runtime, `state.rs`) with their types — the `history` travels as its own blob — and
/// the process's own, with what a loaded core starts them at: `kept` by a rebase (the
/// process's observations, its relay, its log) or `reset` by it (what belonged to the
/// projection it replaced).
macro_rules! projection {
    (
        durable { $($d:ident: $dty:ty,)* }
        process($proc:ident, $now:ident) {
            kept { $($k:ident = $kinit:expr,)* }
            reset { $($r:ident = $rinit:expr,)* }
        }
    ) => {
        /// The projection, borrowed for encoding.
        #[derive(Serialize, Debug)]
        struct ProjectionRef<'a> {
            $($d: &'a $dty,)*
        }

        /// The projection, decoded.
        #[derive(Deserialize)]
        struct Projection {
            $($d: $dty,)*
        }

        impl Coordinator {
            fn projection(&self) -> ProjectionRef<'_> {
                ProjectionRef { $($d: &self.$d,)* }
            }

            /// A core from a decoded projection and history: the process's own fields as a
            /// loaded core of replica `$proc` starts them at store time `$now`.
            fn from_projection(p: Projection, history: History, $proc: ReplicaId, $now: u64) -> Self {
                Coordinator { history, $($d: p.$d,)* $($k: $kinit,)* $($r: $rinit,)* }
            }

            /// A rebase's swap ([`Self::rebase`]): the projection (and the history, if given)
            /// replaced, the process's `kept` fields untouched, its `reset` ones as a load
            /// starts them.
            fn replace_projection(&mut self, p: Projection, history: Option<History>) {
                $(self.$d = p.$d;)*
                if let Some(history) = history {
                    self.history = history;
                }
                $(self.$r = $rinit;)*
            }
        }
    };
}

projection! {
    durable {
        player_claims: BTreeMap<PlayerId, Claim>,
        incarnations: BTreeMap<RegionCoords, u64>,
        memo: BTreeMap<RequestId, WorldReply>,
        memo_order: VecDeque<RequestId>,
        boots_seen: BTreeMap<HostId, BTreeSet<u64>>,
        region_host: BTreeMap<RegionCoords, HostId>,
        online_on: BTreeMap<PlayerId, HostId>,
        hop_of: BTreeMap<PlayerId, u64>,
        generation_seen: u64,
        attached_generation: u64,
        online_reported: BTreeMap<PlayerId, (HostId, u64)>,
        displaced_told: BTreeSet<(PlayerId, HostId, u64)>,
        calendar_anchor: (u64, CalendarTick),
        calendar_seen_ms: u64,
        calendar_day: u64,
        sites_key: Vec<(SiteId, bool, u32)>,
        cuts: WorldCuts,
        successions: SuccessionRuns,
        rolled_back_to: Option<u64>,
        replicas: BTreeSet<ReplicaId>,
        draining: BTreeSet<HostId>,
    }
    process(replica, now_ms) {
        kept {
            memo_warned_ms = None,
            clock_ms = now_ms,
            leases = LeaseWatch::default(),
            login_out = Vec::new(),
            replica = replica,
            log = Vec::new(),
            unstored = false,
            joined = false,
            recorded = None,
            ticking = false,
            cut_every_config = None,
            cut_epoch_config = 0,
            reads_applied = 0,
            reads_taken = 0,
            reads_committed = 0,
        }
        reset {
            out = Vec::new(),
            log_boundary = 0,
            version = 0,
            clean_checked = false,
            committed_raw = Vec::new(),
            writing = false,
            decisions = Vec::new(),
            writing_stamp = None,
            version_check = None,
            outbox = Vec::new(),
            routed = 0,
            attached_now = BTreeSet::new(),
            stale_cuts = Vec::new(),
            stale_seen = false,
            refresh_covers = None,
        }
    }
}

impl Coordinator {
    /// A coordinator over `history`, its calendar running from `history.now()` at
    /// `now_ms`, of replica 0 (tests, tools, the inline link: [`Self::new_for`] names one).
    /// Its first outgoing message is a [`CoordMsg::View`].
    pub fn new(history: History, now_ms: u64) -> Self {
        Self::new_for(history, now_ms, ReplicaId::default())
    }

    /// A new coordinator (a bootstrap: nothing of it is stored yet — its first transition
    /// writes all of it, the history included) over `history`, its calendar anchored at
    /// store time `now_ms` at `history.now()`, of replica `replica`. Its first outgoing
    /// message is a [`CoordMsg::View`].
    pub fn new_for(mut history: History, now_ms: u64, replica: ReplicaId) -> Self {
        history.mark_content_dirty();
        let calendar_anchor = (now_ms, history.now());
        let mut c = Coordinator {
            calendar_day: history.now().0 / TICKS_PER_DAY,
            sites_key: sites_key(&history),
            history,
            player_claims: BTreeMap::new(),
            incarnations: BTreeMap::new(),
            memo: BTreeMap::new(),
            memo_order: VecDeque::new(),
            boots_seen: BTreeMap::new(),
            memo_warned_ms: None,
            out: Vec::new(),
            region_host: BTreeMap::new(),
            online_on: BTreeMap::new(),
            hop_of: BTreeMap::new(),
            generation_seen: 0,
            attached_generation: 0,
            online_reported: BTreeMap::new(),
            displaced_told: BTreeSet::new(),
            clock_ms: now_ms,
            calendar_anchor,
            calendar_seen_ms: now_ms,
            cuts: WorldCuts::default(),
            leases: LeaseWatch::default(),
            successions: SuccessionRuns::default(),
            rolled_back_to: None,
            login_out: Vec::new(),
            replica,
            log: Vec::new(),
            log_boundary: 0,
            version: 0,
            // The replica that bootstraps the row is its ticker from the start: nothing to read
            // yet (a new world has no successions, no host lapsed).
            ticking: true,
            cut_every_config: None,
            cut_epoch_config: 0,
            reads_applied: 0,
            reads_taken: 0,
            reads_committed: 0,
            draining: BTreeSet::new(),
            clean_checked: false,
            committed_raw: Vec::new(),
            unstored: false,
            writing: false,
            decisions: Vec::new(),
            writing_stamp: None,
            // The replica that bootstraps the row is live: its first transition writes it so.
            replicas: [replica].into_iter().collect(),
            joined: false,
            version_check: None,
            outbox: Vec::new(),
            routed: 0,
            attached_now: BTreeSet::new(),
            stale_cuts: Vec::new(),
            stale_seen: false,
            refresh_covers: None,
            recorded: None,
        };
        c.push_view(Target::Every);
        c
    }

    /// A coordinator restarted (or a cache reloaded) from its written projection: `core` and
    /// `history` as [`Self::take_transition`] made them, for replica `replica`, at store
    /// time `now_ms` — everything exactly as written (the calendar continues from its stored
    /// anchor: a quick restart skips no day and repeats no day turn; a longer silence than
    /// [`CALENDAR_GAP_MS`] is downtime, re-anchored at the load where the calendar last stood,
    /// never earlier than a host was told — [`Self::calendar_resume`]), but:
    /// - an attachment counts only while its replica is in the stored live set (Elastic 2b Task
    ///   4): the runtime's first input is this replica's [`Input::ReplicaJoined`] — `alone`, the
    ///   total replica crash, it drops every replica registered before it, whose hosts attach
    ///   again; each `Attach` corrects what the projection holds of its host (its unlisted
    ///   regions park, its unlisted players go offline); a host that never comes back is ended by
    ///   its succession;
    /// - a store decision a replica that is not in the live set handed out (its mark,
    ///   `decisions.rs`) committed with the transition holding it, and its answer died with that
    ///   process: it is LOOKED UP, never issued again ([`Self::orphan_marks`]); a live replica's
    ///   mark is that replica's to answer (a replica dropped later has its marks looked up then);
    /// - a stored deadline more than [`CALENDAR_GAP_MS`] in the past restarts its count
    ///   ([`rebase_deadline`] / [`rebase_since`]): never earlier than stored.
    ///
    /// Nothing goes out at a load (the hosts get the view with their `Attach`). The runtime
    /// sets its configuration again (the cut period, the crash policy, …) and the row version it
    /// read ([`Self::set_version`]). An older core format is `OldVersion` (worlds are re-forged).
    pub fn load(
        core: &[u8],
        history: &[u8],
        grid: SocietyGrid,
        replica: ReplicaId,
        now_ms: u64,
    ) -> Result<Self, HistoryLoadError> {
        let p: Projection = decode_core(core)?;
        let history = crate::history::persist::decode(history, grid)?;
        let mut c = Coordinator::from_projection(p, history, replica, now_ms);
        c.committed_raw = raw_core(&c.projection());
        let live = c.replicas.clone();
        // Outside the log: a rebase before the next commit orphans them again (its own rule).
        c.orphan_marks(|stamp| !live.contains(&stamp.replica), "it was handed out by a replica that is not live");
        c.rebase_deadlines(now_ms);
        if c.calendar_gap(now_ms) {
            // Downtime: re-anchored at once where the calendar last stood (never the stored
            // history's `now`, which lags it: a calendar advance alone is not written), and the
            // history moved there at once (no step is due before it: a step that ran was
            // written) — not one pass later, at the first `maintain`.
            c.reanchor_calendar(now_ms);
            let resumed = c.calendar_resume();
            c.history.advance(resumed);
        }
        Ok(c)
    }

    /// The row version a loaded core was read at (the runtime, the rig; a commit and a rebase
    /// set it themselves).
    pub fn set_version(&mut self, version: u64) {
        self.version = version;
    }

    /// The row was bootstrapped (Elastic 2b Task 3) with this core's projection exactly as
    /// [`Self::encode_projection`] encodes it now, at row `version`: that is its last commit —
    /// what it holds is not written again (its outputs still wait for the next transition, or
    /// leave with [`Self::take_out`]).
    pub fn bootstrapped(&mut self, version: u64) {
        self.version = version;
        self.committed_raw = raw_core(&self.projection());
        self.history.clear_content_dirty();
        self.clean_checked = false;
    }

    /// The row version this core holds: as loaded ([`Self::set_version`]), last committed
    /// ([`Self::transition_written`]) or rebased onto ([`Self::rebase`]).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// [`Self::load`]'s re-base of every stored deadline and period start at store time `now_ms`.
    fn rebase_deadlines(&mut self, now_ms: u64) {
        self.rebase_cut_deadlines(now_ms);
        self.rebase_succession_deadlines(now_ms);
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    /// The history, to change directly (tests, tools): a change outside the log — a rebase
    /// before the next commit drops it. What it decides goes out at the next [`Self::apply`].
    /// Never call its region lifecycle methods through it: the hosts' messages own them.
    pub fn history_mut(&mut self) -> &mut History {
        self.clean_checked = false;
        self.history.mark_content_dirty();
        &mut self.history
    }

    /// What `player` plays, as the coordinator has it.
    pub fn claim_of(&self, player: PlayerId) -> Option<Claim> {
        self.player_claims.get(&player).copied()
    }

    /// The host `player` is online on, as the coordinator has it.
    pub fn online_on(&self, player: PlayerId) -> Option<HostId> {
        self.online_on.get(&player).copied()
    }

    /// The highest hop of `player`'s presence messages applied (0: none known).
    pub fn hop_of(&self, player: PlayerId) -> u64 {
        self.hop_of.get(&player).copied().unwrap_or(0)
    }

    /// The host that runs `rc`, as announced to the coordinator.
    pub fn region_host(&self, rc: RegionCoords) -> Option<HostId> {
        self.region_host.get(&rc).copied()
    }

    /// The calendar at `now_ms` (store ms): the anchor's tick plus one per `TICK_RATE` ms
    /// since — unless `now_ms` is more than [`CALENDAR_GAP_MS`] past the calendar's last sign
    /// of life (`calendar_seen_ms`: a day turn, a view handed out): that was downtime, not
    /// play, and is not caught up — the calendar stands where it last stood
    /// ([`Self::calendar_resume`], where the next [`Input::Maintain`] re-anchors it).
    pub fn calendar_now(&self, now_ms: u64) -> CalendarTick {
        if self.calendar_gap(now_ms) {
            return self.calendar_resume();
        }
        let (ms, tick) = self.calendar_anchor;
        CalendarTick(tick.0 + now_ms.saturating_sub(ms) / crate::TICK_RATE)
    }

    /// Where the calendar last stood: its tick at its last sign of life (`calendar_seen_ms` on
    /// the anchor), or the history's own `now` if that is later. Never earlier than anything
    /// a host was told — every day turn and every view moves `calendar_seen_ms`, durable before
    /// the message leaves — while the stored history's `now` may lag it (a calendar advance
    /// alone writes no history, Elastic 2a decision 7; final review I1).
    fn calendar_resume(&self) -> CalendarTick {
        let (ms, tick) = self.calendar_anchor;
        let seen = CalendarTick(tick.0 + self.calendar_seen_ms.saturating_sub(ms) / crate::TICK_RATE);
        seen.max(self.history.now())
    }

    /// The calendar re-anchored at store time `now_ms` where it last stood
    /// ([`Self::calendar_resume`]): downtime is not caught up, and time never runs backwards.
    fn reanchor_calendar(&mut self, now_ms: u64) {
        log::info!(
            "coordinator: the calendar was silent {} ms (over {CALENDAR_GAP_MS}): re-anchored, not caught up",
            now_ms.saturating_sub(self.calendar_seen_ms)
        );
        self.calendar_anchor = (now_ms, self.calendar_resume());
        self.calendar_seen_ms = now_ms;
    }

    /// Whether `now_ms` lies more than [`CALENDAR_GAP_MS`] past the calendar's last day turn.
    fn calendar_gap(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.calendar_seen_ms) > CALENDAR_GAP_MS
    }

    /// What host `host` tells the login service ([`HostMsg::Login`]): into the process's login
    /// relay as it is — never logged — unless it comes from a succeeded boot (a zombie: dropped).
    fn forward_login(&mut self, host: HostId, out: crate::LoginOut) {
        if self.from_succeeded_boot(host, None) {
            log::debug!("coordinator: a login message of host {host}'s succeeded boot dropped: {out:?}");
            return;
        }
        self.login_out.push(out);
    }

    /// Apply one message of host `host` (the ordinal its link is for): [`Input::Host`].
    fn on_host(&mut self, host: HostId, msg: HostMsg, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        // A zombie of a succeeded boot (R4) changes nothing: its state-changing requests are
        // refused (memoised), everything else is dropped.
        let attach_boot = match &msg {
            HostMsg::Attach { boot, .. } => Some(*boot),
            _ => None,
        };
        if self.from_succeeded_boot(host, attach_boot) {
            return self.zombie_message(host, msg);
        }
        // A host of a generation older than the coordinator's rollback: told, not applied.
        if let HostMsg::Attach { generation, .. } = &msg {
            if let Some(g) = self.rolled_back_to.filter(|g| generation < g) {
                log::warn!("coordinator: host {host} attaches with generation {generation}, the world rolled back to {g}");
                return self.push(Target::Host(host), CoordMsg::Generation(g));
            }
        }
        // For the login service, forwarded as it is.
        if let HostMsg::Login(out) = msg {
            self.login_out.push(out);
            return;
        }
        match msg {
            HostMsg::Attach {
                host: named,
                epoch,
                running,
                online,
                generation,
                boot,
                settled_below,
                handed,
                pending_cut,
                successions,
                // Checked by the runtime against its own before the core sees the Attach.
                policy: _,
            } => {
                if named != host {
                    log::warn!("coordinator: host {host}'s Attach names host {named}; taken as host {host}'s");
                }
                if generation > self.generation_seen {
                    // A newer world generation: every host started its hops over.
                    if !self.hop_of.is_empty() {
                        log::info!("coordinator: world generation {generation}: player hops start over");
                    }
                    self.generation_seen = generation;
                    self.hop_of.clear();
                    self.online_reported.clear();
                    self.displaced_told.clear();
                    // A rollback this coordinator did not order (`worldctl rollback`), or a
                    // total crash's first boot: the store's open successions may differ.
                    self.reread_successions();
                }
                self.forget_settled(host, boot, settled_below, true);
                if generation > self.attached_generation {
                    self.first_of_generation(host, generation);
                }
                self.attach(host, epoch, running, online, handed);
                self.cut_attach(host, pending_cut, boot);
                self.succession_attach(host, successions);
                // Its outbox rows, decided before this `Attach`, go with the transition holding it.
                self.attached_now.insert(host);
            }
            HostMsg::Settled { boot, below } => self.forget_settled(host, boot, below, false),
            HostMsg::RegionStarted { rc, incarnation } => {
                if let Some(other) = self.region_host.insert(rc, host).filter(|h| *h != host) {
                    // A region runs on one host: `other`'s incarnation is gone without its
                    // park (R4: its land moved — the heir may start a moved region before this
                    // coordinator has heard its own declaration's answer and parked the dead
                    // host's regions, `host_succeeded`). Lost, as that park would say: the
                    // history takes back everyone it had there, and this start embodies them
                    // again — never left embodied in a region whose host holds no body of theirs.
                    log::warn!(
                        "coordinator: {rc:?} started on host {host}; host {other} had announced it: that incarnation is lost"
                    );
                    self.history.region_parked(rc, Vec::new());
                }
                self.incarnations.insert(rc, incarnation);
                let events = self.history.region_started(rc);
                self.push(Target::Host(host), CoordMsg::SpawnPeople { rc, incarnation, events });
            }
            HostMsg::RegionStopping { rc } => {
                if !self.runs_elsewhere(rc, host) {
                    self.history.region_stopping(rc);
                }
            }
            HostMsg::RegionParked { rc, people } => {
                if !self.runs_elsewhere(rc, host) {
                    self.region_host.remove(&rc);
                    self.history.region_parked(rc, people);
                }
            }
            HostMsg::PersonCrossed { person, to, delivered } => {
                if delivered {
                    // The host put the body in `to`, which it runs.
                    self.region_host.entry(to).or_insert(host);
                }
                let decided = self.history.person_crossed(person, to);
                if decided != delivered {
                    log::error!(
                        "coordinator: {person:?} crossed into {to:?}: the host {} the body, the history wanted it {}",
                        if delivered { "delivered" } else { "dropped" },
                        if decided { "delivered" } else { "absorbed" },
                    );
                }
                if delivered && !decided {
                    // The host's view of running wins: the body stands in `to`.
                    if let Some(d) = self.history.adopt_body(to, person) {
                        self.push_directive(d);
                    }
                }
            }
            HostMsg::Positions(people) => self.history.note_positions(people),
            HostMsg::DirectiveBounced { directive, incarnation } => {
                let latest = self.incarnations.get(&directive.region).copied().unwrap_or(0);
                if incarnation < latest {
                    log::debug!(
                        "coordinator: bounce for {:?} incarnation {incarnation} (latest {latest}) dropped: {:?}",
                        directive.region,
                        directive.event
                    );
                } else {
                    self.history.defer_directive(directive);
                }
            }
            HostMsg::Request { id, player, hop, req } => self.request(host, id, player, hop, req),
            HostMsg::Settle { id, req } => self.settle(host, id, req),
            HostMsg::PlayerOnline(p) => self.player_online(host, p),
            HostMsg::PlayerOffline { player, hop } => self.player_offline(host, player, hop),
            HostMsg::PlayerHandedTo { player, to, hop } => self.player_handed(host, player, to, hop),
            HostMsg::ReleaseClaim { player } => self.release_claim(player),
            HostMsg::CutPrepared { cut } => self.cut_prepared(host, cut),
            HostMsg::CutFailed { cut, why } => self.cut_failed(host, cut, why),
            HostMsg::LeaveRequest => self.leave_request(host),
            HostMsg::SuccessionApplied { id } => self.succession_applied(host, id),
            HostMsg::SuccessionAdopted { id } => self.succession_adopted(host, id),
            HostMsg::LandWanted { sr } => self.land_asked(host, sr),
            HostMsg::CutPeerCount { cut, peer, received_through, boot } => {
                self.cut_peer_count(host, cut, peer, received_through, boot)
            }
            HostMsg::Login(_) => unreachable!("handled above"),
        }
        self.push_directives();
    }

    /// A message of a succeeded boot of `host` (a zombie): a state-changing request is
    /// refused `WorldUnavailable` (memoised, as a settle of it would be); anything else is
    /// dropped — its presence, regions and attaches would point the coordinator's view at a
    /// dead host again.
    fn zombie_message(&mut self, host: HostId, msg: HostMsg) {
        match msg {
            HostMsg::Request { id, req, .. } | HostMsg::Settle { id, req } if req.changes_state() => {
                log::warn!("coordinator: a request of host {host}'s succeeded boot: refused");
                self.settle(host, id, req);
            }
            other => log::debug!("coordinator: a message of host {host}'s succeeded boot dropped: {other:?}"),
        }
    }

    /// A lifecycle message of `host` about `rc`, which another host announced since: stale,
    /// ignored (the other host's own messages run the region in the history).
    fn runs_elsewhere(&self, rc: RegionCoords, host: HostId) -> bool {
        match self.region_host.get(&rc) {
            Some(&other) if other != host => {
                log::warn!("coordinator: host {host}'s message about {rc:?} ignored: host {other} runs it");
                true
            }
            _ => false,
        }
    }

    /// Move the calendar to `now_ms` ([`Input::Maintain`]): the history advances, its directives
    /// go to the running regions, a day's turn broadcasts the calendar (and the sites if they
    /// changed), and the notices go to their regions. `WorldManager::maintain`'s world
    /// services, moved here.
    fn on_maintain(&mut self, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        if self.calendar_gap(now_ms) {
            // Downtime: the calendar goes on from where it last stood.
            self.reanchor_calendar(now_ms);
        }
        let to = self.calendar_now(now_ms);
        self.history.advance(to);
        self.push_directives();
        self.broadcast_daily(to, now_ms);
        for (region, line) in self.history.take_notices() {
            let to = self.region_host.get(&region).map_or(Target::Every, |h| Target::Host(*h));
            self.push(to, CoordMsg::Notice { region, line });
        }
        // Successions listed by `Attach`es while this replica did not know the open ones (on any
        // replica: a non-ticker never does), answered once the ticker does.
        self.drain_listed_early();
        self.maintain_successions(now_ms);
        self.maintain_cuts(now_ms);
    }

    /// The messages free to leave now, in order, each with the host(s) it is for: everything
    /// decided while nothing durable changed since the last transition (a query's answer) —
    /// or, with a durable change pending, nothing: those wait for [`Self::take_transition`],
    /// which hands them out with the projection that decided them. A core with no store
    /// ([`Self::commit_unstored`]) frees them at once. Released, the inputs logged since the
    /// last commit changed nothing durable: they are dropped from the log (the row holds all
    /// they did). Right after a [`Self::take_transition`] that found nothing to write, the
    /// projection is not encoded again: that verdict (`clean_checked`) holds only while nothing
    /// changed since — call this right after that [`Self::take_transition`], with no input,
    /// hand-out or answer in between (each of those clears the verdict anyway: every input
    /// through [`Self::apply`], every store decision handed out through `push_decision`; a
    /// lookup handed out between the two is released with what it took, as it would be by the
    /// next transition).
    ///
    /// With more than one live replica (Elastic 2b departs 5) a cache may be stale: outputs
    /// leave only once the row was read at this cache's version — the ones decided before
    /// [`Self::version_check_wanted`] handed the check out, after [`Self::version_checked`]
    /// confirmed it; until then nothing leaves (a check that found the row moved: the runtime
    /// rebases first). An evicted core ([`Self::evicted`]) releases nothing.
    pub fn take_out(&mut self) -> Vec<(Target, CoordMsg)> {
        if !self.unstored {
            if self.writing || self.evicted() {
                return Vec::new();
            }
            if self.is_stale() {
                // A host spoke of a cut this cache cannot know yet (Elastic 2b Task 5): the row
                // moved past it. Nothing leaves and the log stays whole until it is refreshed.
                return Vec::new();
            }
            let clean = std::mem::take(&mut self.clean_checked) || !self.durable_pending();
            if !clean {
                return Vec::new();
            }
            debug_assert!(self.decisions.is_empty(), "a store decision waits, yet nothing durable is pending");
            if self.replicas.len() > 1 && !self.out.is_empty() {
                let Some(check) = self.version_check.filter(|c| c.confirmed && c.version == self.version) else {
                    return Vec::new();
                };
                self.version_check = None;
                let n = check.out_len.min(self.out.len());
                let logged = check.log_len.min(self.log.len());
                let drained: Vec<(u64, Input)> = self.log.drain(..logged).collect();
                self.record(drained);
                self.reads_committed = self.reads_applied;
                self.routed = self.routed.saturating_sub(n);
                return self.out.drain(..n).collect();
            }
        } else {
            // No store: nothing to decide in (an unstored core runs no world cut nor succession),
            // and no other replica (nothing routed through an outbox).
            self.decisions.clear();
            self.attached_now.clear();
        }
        let drained = std::mem::take(&mut self.log);
        self.record(drained);
        self.reads_committed = self.reads_applied;
        self.routed = 0;
        std::mem::take(&mut self.out)
    }

    /// Split what waits in `out` by attachment (Elastic 2b Task 5): a message for a host attached
    /// to this replica (or to none: its link, if any, is here) stays in `out`, sent on this
    /// replica's links; one for a host attached to ANOTHER live replica goes to the outbox, with
    /// that host's boot (the store inserts it with the transition; the replica linking the host
    /// relays it in commit order); one for a host attached to a replica no longer live is dropped
    /// — as a detached host's: its next `Attach` (to a live replica) reconciles what it would
    /// have carried. [`Target::Every`] stays in `out` for this replica's links and is fanned out
    /// to the outbox once per host attached to another live replica. An answer to an `Attach`
    /// on this replica's link ([`CoordMsg::Attached`], [`CoordMsg::Generation`]) always stays.
    /// One live replica, every host attached to it (or to none): nothing changes. Run by
    /// [`Self::take_transition`] over everything decided since the last one, at the attachments
    /// its compare-and-set commits with.
    fn route_out(&mut self) {
        let me = self.replica;
        let from = self.routed.min(self.out.len());
        self.routed = self.out.len();
        if self.unstored || self.attachments().values().all(|(_, r)| *r == me) {
            return;
        }
        let elsewhere: BTreeMap<HostId, u64> = self
            .attachments()
            .iter()
            .filter(|(_, (_, r))| *r != me && self.replicas.contains(r))
            .map(|(h, (boot, _))| (*h, *boot))
            .collect();
        let out = self.out.split_off(from);
        for (to, msg) in out {
            let answers_link = matches!(msg, CoordMsg::Attached { .. } | CoordMsg::Generation(_));
            match to {
                Target::Host(h) if !answers_link => match self.attachments().get(&h).copied() {
                    Some((boot, r)) if r != me && self.replicas.contains(&r) => self.outbox.push(OutboxMsg { host: h, boot, msg }),
                    Some((_, r)) if r != me => {
                        log::debug!("coordinator: host {h} is attached to replica {r}, not live: a message for it dropped");
                    }
                    _ => self.out.push((to, msg)),
                },
                Target::Host(_) => self.out.push((to, msg)),
                Target::Every => {
                    for (&host, &boot) in &elsewhere {
                        self.outbox.push(OutboxMsg { host, boot, msg: msg.clone() });
                    }
                    self.out.push((to, msg));
                }
            }
        }
        self.routed = self.out.len();
    }

    /// Whether a host spoke of a world cut this cache cannot know yet: it is stale (the row moved
    /// past it) until refreshed ([`Self::take_refresh_wanted`]).
    fn is_stale(&self) -> bool {
        !self.stale_cuts.is_empty() || self.stale_seen
    }

    /// Whether the runtime should refresh this cache now (Elastic 2b Task 5): a host's message
    /// named a world cut above the last this cache made (a cut answer, a peer count) — another
    /// replica committed it. `true` once per such finding; the runtime reads the row and rebases
    /// onto it ([`Self::refresh`]: the message is applied again there, never looked up — a lookup
    /// would abort the cut), or, found not newer, answers [`Self::refresh_not_newer`].
    pub fn take_refresh_wanted(&mut self) -> bool {
        if !self.is_stale() || self.refresh_covers.is_some() {
            return false;
        }
        self.refresh_covers = Some((self.stale_cuts.len(), self.stale_seen));
        true
    }

    /// The refresh [`Self::take_refresh_wanted`] asked for read the row at no newer version than
    /// this cache's: the cuts it covered do not exist (a read after the host's message sees every
    /// cut that host could have been told of) — each is looked up as an unknown cut is (the
    /// store's decision, an undecided one aborted), and a dropped relay stays dropped.
    pub fn refresh_not_newer(&mut self) {
        let Some((cuts, seen)) = self.refresh_covers.take() else { return };
        let covered: Vec<(HostId, CutId)> = self.stale_cuts.drain(..cuts.min(self.stale_cuts.len())).collect();
        if seen {
            self.stale_seen = false;
        }
        for (host, cut) in covered {
            log::warn!("coordinator: host {host} named cut {}, which the row does not hold either: looked up", cut.0);
            self.clean_checked = false;
            self.queue_cut_lookup(host, cut);
        }
    }

    /// Test support: the inputs leaving the log from now on (by a commit, or a release) are kept
    /// for [`Self::take_recorded`].
    #[doc(hidden)]
    pub fn record_inputs(&mut self) {
        self.recorded.get_or_insert_with(Vec::new);
    }

    /// Test support: the inputs that left the log since the last call ([`Self::record_inputs`]),
    /// in order.
    #[doc(hidden)]
    pub fn take_recorded(&mut self) -> Vec<(u64, Input)> {
        self.recorded.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// Test support: this core applies its next inputs as replica `r` (a reference core fed every
    /// replica's committed inputs in commit order, Elastic 2b Task 5).
    #[doc(hidden)]
    pub fn set_replica_for_test(&mut self, r: ReplicaId) {
        self.replica = r;
        // No process changes role here: what the ticker observed stays (a reference core).
        self.ticking = self.is_ticker();
    }

    /// Inputs leaving the log, kept if recording.
    fn record(&mut self, inputs: Vec<(u64, Input)>) {
        if let Some(r) = self.recorded.as_mut() {
            r.extend(inputs);
        }
    }

    /// The version check the outputs free to leave wait for (Elastic 2b departs 5): with more
    /// than one live replica, nothing durable pending, nothing being written, outputs waiting and
    /// no check out yet — this cache's version, for the runtime to compare with the row's
    /// (`SELECT version`) and answer with [`Self::version_checked`]. It covers the outputs
    /// decided so far. `None`: no check wanted (one replica: none at all).
    pub fn version_check_wanted(&mut self) -> Option<u64> {
        if self.unstored || self.writing || self.evicted() || self.replicas.len() <= 1 || self.out.is_empty() || self.is_stale() {
            return None;
        }
        if self.version_check.is_some_and(|c| c.version == self.version) || self.durable_pending() {
            return None;
        }
        self.version_check = Some(VersionCheck { version: self.version, out_len: self.out.len(), log_len: self.log.len(), confirmed: false });
        Some(self.version)
    }

    /// The row's version as the check handed out read it: at this cache's version, the outputs
    /// it covers may leave ([`Self::take_out`]) — `true`; anything else (the row moved: this
    /// cache is stale, the runtime rebases; or no such check out) — `false`, nothing leaves.
    pub fn version_checked(&mut self, row_version: u64) -> bool {
        match self.version_check.take() {
            Some(mut c) if c.version == self.version && row_version == self.version => {
                c.confirmed = true;
                self.version_check = Some(c);
                true
            }
            _ => false,
        }
    }

    /// The live replicas, as the projection holds them (Elastic 2b Task 4).
    pub fn replicas_live(&self) -> &BTreeSet<ReplicaId> {
        &self.replicas
    }

    /// The ticker: the lowest live replica (`None`: none live).
    pub fn ticker(&self) -> Option<ReplicaId> {
        self.replicas.first().copied()
    }

    /// This replica joined ([`Input::ReplicaJoined`]) and the projection no longer holds it:
    /// another replica ended it. It writes nothing and lets nothing leave; the runtime exits
    /// fenced.
    pub fn evicted(&self) -> bool {
        !self.unstored && self.joined && !self.replicas.contains(&self.replica)
    }

    /// Replica `me` joined (its first input): it is live; `alone` (the store found no other
    /// replica beaten within the lease, every one marked dead) drops every replica registered
    /// before it — ids are issued in order, so a replica that registered after it, and joined
    /// first, stays.
    fn on_replica_joined(&mut self, me: ReplicaId, alone: bool) {
        if me == self.replica {
            self.joined = true;
        }
        if alone {
            let gone: Vec<ReplicaId> = self.replicas.iter().copied().filter(|r| *r < me).collect();
            for r in gone {
                log::warn!("coordinator: replica {me} registered alone: replica {r} is gone");
                self.drop_replica(r);
            }
        }
        if self.replicas.insert(me) {
            log::info!("coordinator: replica {me} joined; live {:?}", self.replicas);
        }
    }

    /// Replica `r` lapsed (another replica's watch saw its row stand still for a lease): it is
    /// dropped, and its row is ended in the same transaction ([`StoreDecision::EndReplica`]).
    /// Not live any more (a replay onto a row that dropped it already): nothing.
    fn on_replica_lapsed(&mut self, r: ReplicaId) {
        if r == self.replica {
            log::warn!("coordinator: replica {r} is told it lapsed itself: ignored (its beat decides)");
            return;
        }
        if self.replicas.contains(&r) {
            log::warn!("coordinator: replica {r} lapsed: ended");
            self.drop_replica(r);
            self.push_decision(StoreDecision::EndReplica(r));
        }
    }

    /// Replica `r` is no longer live: the hosts attached to it count detached from here on (their
    /// attachments stay in the projection, as a restart's did, until each host attaches again or
    /// a `hosts` read no longer shows it — a cut they take part in aborts at their re-`Attach` or
    /// its deadline), the store calls it handed out and never answered are looked up.
    fn drop_replica(&mut self, r: ReplicaId) {
        self.replicas.remove(&r);
        self.orphan_marks(|s| s.replica == r, "its replica is no longer live");
    }

    /// The world's gate host: the lowest host attached to any live replica that neither left
    /// nor was succeeded, and is neither leaving nor draining ([`Self::takes_logins`]). `None`:
    /// no such host (none attached, or every one draining). A replica relays the login
    /// service's orders to its OWN gate host, [`Self::gate_host_here`] (Elastic 2b Task 8).
    pub fn gate_host(&self) -> Option<HostId> {
        self.attached_live().into_iter().filter(|h| self.takes_logins(*h)).min()
    }

    /// This replica's gate host (Elastic 2b Task 8): [`Self::gate_host`] among the hosts
    /// attached to THIS replica only — the one its login orders go to ([`Self::login_order`]).
    /// `None`: none of its hosts takes logins (another replica's may): the runtime answers the
    /// login service `NoGate` at once, and it tries its next link.
    pub fn gate_host_here(&self) -> Option<HostId> {
        let here = self.hosts_attached_to(self.replica());
        self.attached_live().into_iter().filter(|h| here.contains(h) && self.takes_logins(*h)).min()
    }

    /// Whether attached live host `host` may be the gate host: not leaving (its leave asked
    /// for: it refuses every spawn order) and not draining (the latest successful hosts read
    /// showed its row marked, `worldctl drain`; a failed read keeps the marks — durable, so a
    /// replica that reads no `hosts` rows knows them too).
    fn takes_logins(&self, host: HostId) -> bool {
        !self.leave_asked(host) && !self.draining.contains(&host)
    }

    /// An order of the login service: for this replica's gate host of this moment
    /// ([`Self::gate_host_here`]), as the message to send it — the runtime sends it; nothing is
    /// stored or logged here (process-local, never replayed: a resend of the order, or of a
    /// kick, goes to the gate host of ITS moment, and the hosts' answers come back as
    /// [`HostMsg::Login`]). `None`: this replica has no gate host — the runtime answers the
    /// login service `NoGate`, and it tries another replica (none: the join answers 503; a kick
    /// is resent).
    pub fn login_order(&mut self, order: crate::LoginOrder) -> Option<(HostId, CoordMsg)> {
        let gate = self.gate_host_here()?;
        Some((gate, CoordMsg::Login(order)))
    }

    /// What the hosts told the login service since the last call ([`HostMsg::Login`]), in
    /// order, untouched: the runtime forwards it.
    pub fn take_login_out(&mut self) -> Vec<crate::LoginOut> {
        std::mem::take(&mut self.login_out)
    }

    /// Whether the projection differs from the one last handed out (or loaded), or the
    /// history changed since.
    fn durable_pending(&self) -> bool {
        !self.decisions.is_empty()
            || !self.outbox.is_empty()
            || !self.attached_now.is_empty()
            || self.history.content_dirty()
            || raw_core(&self.projection()) != self.committed_raw
    }

    /// The pending change as bytes plus the messages it releases; `None` when nothing durable
    /// changed (what was decided then leaves with [`Self::take_out`]). The runtime writes
    /// `core` (and `history`, present only when the history changed) in one compare-and-set
    /// and sends `out` only after that write committed — or hands the transition back
    /// ([`Self::transition_failed`]). One at a time: the next is taken only once this one is
    /// answered, so whatever is decided meanwhile waits for it.
    ///
    /// The history's directives are always drained into the outgoing queue by the step that
    /// made them ([`Self::apply`]), so a projection never holds one
    /// undrained: a restart cannot re-send a directive outside the truth check of the hosts'
    /// `Attach` (`reattach_regions`).
    pub fn take_transition(&mut self) -> Option<Transition> {
        if self.writing {
            // One at a time (final review G1): the one handed out is not answered yet.
            return None;
        }
        if self.evicted() {
            // Ended by another replica: nothing is written (the runtime exits fenced).
            return None;
        }
        // The marks handed out since the last transition are this one's: it commits, if at
        // all, as the next version of the row it was read at (the compare-and-set).
        let stamp = CallStamp { replica: self.replica, version: self.version + 1 };
        self.stamp_pending_marks(stamp);
        // Routed at the attachments this transition commits with (Elastic 2b Task 5): a message
        // for another replica's host leaves through the outbox, written with it.
        self.route_out();
        let raw = raw_core(&self.projection());
        let history_changed = self.history.content_dirty();
        if !history_changed
            && raw == self.committed_raw
            && self.decisions.is_empty()
            && self.outbox.is_empty()
            && self.attached_now.is_empty()
        {
            self.clean_checked = true;
            return None;
        }
        let history = history_changed.then(|| crate::history::persist::encode(&self.history));
        self.history.clear_content_dirty();
        let core = seal_core(&raw);
        self.committed_raw = raw;
        self.writing = true;
        self.writing_stamp = Some(stamp);
        self.clean_checked = false;
        self.log_boundary = self.log.len();
        self.reads_taken = self.reads_applied;
        // Every output leaves with this transition: its compare-and-set is their version check.
        self.version_check = None;
        self.routed = 0;
        Some(Transition {
            core,
            history,
            out: std::mem::take(&mut self.out),
            decisions: std::mem::take(&mut self.decisions),
            outbox: std::mem::take(&mut self.outbox),
            attached_now: std::mem::take(&mut self.attached_now).into_iter().collect(),
        })
    }

    /// The write of the transition last taken committed as row version `version`: its messages
    /// may leave (the caller sends them), the inputs it holds leave the log, and
    /// [`Self::take_out`] frees again what was decided with nothing durable pending. Its store
    /// decisions' answers come next, as inputs ([`Input::Decision`]); an answer that never comes
    /// leaves its mark to a lookup (a rebase, a restart: [`Self::orphan_marks`]).
    pub fn transition_written(&mut self, version: u64) {
        debug_assert!(
            self.writing_stamp.is_none_or(|s| s.version == version),
            "the transition was stamped {:?} and committed as version {version}",
            self.writing_stamp
        );
        self.writing = false;
        self.writing_stamp = None;
        self.version = version;
        let boundary = std::mem::take(&mut self.log_boundary).min(self.log.len());
        let drained: Vec<(u64, Input)> = self.log.drain(..boundary).collect();
        self.record(drained);
        self.reads_committed = self.reads_taken;
    }

    /// The write of transition `t` failed, and whether it committed is not known (not
    /// fenced, not a conflict: a store error, a lost answer): its messages go back to the head
    /// of the outgoing queue, in order, and the next [`Self::take_transition`] writes the whole
    /// projection again (the history too if `t` carried it). Nothing is decided twice: the
    /// inputs were applied once, and their state is in the next transition; they stay in the
    /// log. Its store decisions may have committed with it: never issued again — each mark it
    /// carries is looked up ([`Self::orphan_marks`]).
    pub fn transition_failed(&mut self, t: Transition) {
        let stamp = self.writing_stamp;
        // An end of a replica's row carries no mark (no lookup would find it lost) and is
        // idempotent: issued again with the next transition (Elastic 2b Task 4).
        let ends: Vec<StoreDecision> =
            self.hand_back(t).into_iter().filter(|d| matches!(d, StoreDecision::EndReplica(_))).collect();
        let later = std::mem::replace(&mut self.decisions, ends);
        self.decisions.extend(later);
        if let Some(stamp) = stamp {
            self.orphan_marks(|s| *s == stamp, "its write failed and may have committed");
        }
    }

    /// The write of transition `t` certainly did not commit (a `Conflict` before the row is
    /// rebased, or a write refused before it reached the store): as [`Self::transition_failed`],
    /// but its store decisions were never issued — they go back, ahead of any handed out since,
    /// their marks pending again, and leave with the next transition.
    pub fn transition_refused(&mut self, t: Transition) {
        let stamp = self.writing_stamp;
        let decisions = self.hand_back(t);
        if let Some(stamp) = stamp {
            self.stamp_marks(|s| *s == stamp, CallStamp::pending(self.replica));
        }
        let later = std::mem::replace(&mut self.decisions, decisions);
        self.decisions.extend(later);
    }

    /// `t` back in the core (not committed as far as this replica knows): its messages ahead of
    /// what was decided since, the whole projection (and the history if `t` carried it) to be
    /// written again. Its decisions, to the caller.
    fn hand_back(&mut self, t: Transition) -> Vec<StoreDecision> {
        self.writing = false;
        self.writing_stamp = None;
        self.log_boundary = 0;
        self.clean_checked = false;
        if t.history.is_some() {
            self.history.mark_content_dirty();
        }
        self.committed_raw.clear();
        self.routed = t.out.len();
        let later = std::mem::replace(&mut self.out, t.out);
        self.out.extend(later);
        // Routed already (an `Every` fanned out): back as they were, ahead of anything routed since.
        let later = std::mem::replace(&mut self.outbox, t.outbox);
        self.outbox.extend(later);
        self.attached_now.extend(t.attached_now);
        t.decisions
    }

    /// A store decision handed out (its mark set by the caller, stamped pending): it leaves
    /// with the next transition.
    fn push_decision(&mut self, d: StoreDecision) {
        self.clean_checked = false;
        self.decisions.push(d);
    }

    /// Every mark stamped pending takes `stamp` (the transition being taken).
    fn stamp_pending_marks(&mut self, stamp: CallStamp) {
        let replica = self.replica;
        self.stamp_marks(|s| s.is_pending() && s.replica == replica, stamp);
    }

    /// Every mark whose stamp `matches` takes `to`.
    fn stamp_marks(&mut self, matches: impl Fn(&CallStamp) -> bool, to: CallStamp) {
        self.cuts_stamp_marks(&matches, to);
        self.successions_stamp_marks(&matches, to);
    }

    /// Every mark whose stamp `matches` lost its answer (`why`, for the log): resolved by a
    /// lookup, never issued again (`decisions.rs`) — a cut's decision is read back, a
    /// declaration or an end makes the open successions be read again (and the next `hosts`
    /// read takes a dead row nobody here declared out of the view), a rollback reads the world
    /// generation. Whether any mark matched.
    pub(super) fn orphan_marks(&mut self, matches: impl Fn(&CallStamp) -> bool, why: &str) -> bool {
        let cut = self.cuts_orphan_marks(&matches, why);
        let successions = self.successions_orphan_marks(&matches, why);
        cut || successions
    }

    /// Whether `answer` is the one a mark here awaits — what applying it ([`Input::Decision`])
    /// would act on; any other changes nothing. The runtime takes an answer's side effects
    /// (counters, closing a succeeded host's connection, retry pacing) only for these. A cut's
    /// begin and a replica's end carry no mark: always.
    pub fn answer_awaited(&self, answer: &DecisionAnswer) -> bool {
        match answer {
            DecisionAnswer::Cut { stamp, cut, .. } => self.cut_mark_is(*cut, *stamp),
            DecisionAnswer::Declared { stamp, .. } => self.successions_declaring() == Some(*stamp),
            DecisionAnswer::Ended { stamp, end, .. } => self.successions_ending() == Some((end.host, end.boot, *stamp)),
            DecisionAnswer::RolledBack { stamp, .. } => self.successions_rolling() == Some(*stamp),
            DecisionAnswer::Begin { .. } | DecisionAnswer::ReplicaEnded { .. } => true,
        }
    }

    /// Whether the cache's history changed since it was last written or loaded (a refresh then
    /// reads the row's history whole).
    pub fn history_dirty(&self) -> bool {
        self.history.content_dirty()
    }

    /// One answer of a store decision ([`Input::Decision`]): applied only if its stamp is the
    /// mark's (`decisions.rs`).
    fn on_decision(&mut self, answer: DecisionAnswer, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        match answer {
            DecisionAnswer::Cut { stamp, cut, committed } => self.cut_answered(stamp, cut, committed),
            DecisionAnswer::Begin { cut, bound } => match bound {
                Ok(bound) => log::debug!("coordinator: world cut {}: its begin is recorded (hosts below {bound} take part)", cut.0),
                Err(e) => log::warn!(
                    "coordinator: world cut {}: its begin could not be recorded ({e}); a host joining during it aborts it",
                    cut.0
                ),
            },
            DecisionAnswer::Declared { stamp, outcome } => self.declare_answered(stamp, outcome, now_ms),
            DecisionAnswer::Ended { stamp, end, outcome } => self.end_answered(stamp, end, outcome, now_ms),
            DecisionAnswer::RolledBack { stamp, outcome } => self.rollback_answered(stamp, outcome, now_ms),
            DecisionAnswer::ReplicaEnded { replica } => log::info!("coordinator: replica {replica}'s row is ended"),
        }
    }

    /// For a link with no store behind it (the inline link): the state as it stands counts as
    /// committed — nothing is encoded, and every output is free at once ([`Self::take_out`]),
    /// from now on.
    pub fn commit_unstored(&mut self) {
        self.unstored = true;
        self.history.clear_content_dirty();
    }

    /// Test support (and tools): the projection as it stands, `(core, history)`, encoded as a
    /// transition would encode it — nothing is taken or marked.
    #[doc(hidden)]
    pub fn encode_projection(&self) -> (Vec<u8>, Vec<u8>) {
        (seal_core(&raw_core(&self.projection())), crate::history::persist::encode(&self.history))
    }

    /// Test support: the projection as it stands, in `Debug` form (a failing comparison of two
    /// projections names what differs).
    #[doc(hidden)]
    pub fn projection_debug(&self) -> String {
        format!("{:#?}", self.projection())
    }

    /// This core's replica ([`ReplicaId`]).
    pub fn replica(&self) -> ReplicaId {
        self.replica
    }

    /// The calendar day last broadcast (test support: it never goes back).
    #[doc(hidden)]
    pub fn calendar_day(&self) -> u64 {
        self.calendar_day
    }

    /// The claims as the projection holds them, by player.
    pub fn claims(&self) -> Vec<(PlayerId, Claim)> {
        self.player_claims.iter().map(|(p, c)| (*p, *c)).collect()
    }

    /// The memoised answers' ids, oldest first.
    pub fn memo_ids(&self) -> Vec<RequestId> {
        self.memo_order.iter().copied().collect()
    }

    /// The calendar's anchor `(store ms, tick)` and its last day turn (store ms), as held.
    pub fn calendar_anchor(&self) -> ((u64, CalendarTick), u64) {
        (self.calendar_anchor, self.calendar_seen_ms)
    }

    /// Queue an outgoing message for `to`, behind every message decided before it.
    fn push(&mut self, to: Target, msg: CoordMsg) {
        self.out.push((to, msg));
    }

    fn push_directives(&mut self) {
        for d in self.history.take_directives() {
            self.push_directive(d);
        }
    }

    /// Queue a directive for the host that runs its region, stamped with the region's
    /// latest started incarnation. A region no host has announced cannot take it: it is
    /// treated as bounced (held for the region's next start), never broadcast.
    fn push_directive(&mut self, directive: Directive) {
        let Some(&host) = self.region_host.get(&directive.region) else {
            log::debug!("coordinator: no host runs {:?}; directive deferred: {:?}", directive.region, directive.event);
            self.history.defer_directive(directive);
            return;
        };
        let incarnation = self.incarnations.get(&directive.region).copied().unwrap_or(0);
        self.push(Target::Host(host), CoordMsg::Directive { directive, incarnation });
    }

    /// Forget every directive for `host` not yet handed out (waiting or free to leave):
    /// its `Attach` re-decides every region it runs from its bodies, so they are stale.
    /// Those already handed to the link reach the host ahead of [`CoordMsg::Attached`],
    /// and it drops them. Other hosts' directives stand.
    fn drop_unsent_directives(&mut self, host: HostId) {
        self.drop_unsent(host, "directives", |m| matches!(m, CoordMsg::Directive { .. }));
    }

    /// Forget every message for `host` matching `drops` not yet handed out (waiting or
    /// free to leave), superseded by its attach (`what` names them in the log).
    fn drop_unsent(&mut self, host: HostId, what: &str, drops: impl Fn(&CoordMsg) -> bool) {
        let before = self.out.len();
        let routed = self.routed;
        let mut i = 0;
        let mut routed_dropped = 0;
        self.out.retain(|(to, m)| {
            let keep = !(*to == Target::Host(host) && drops(m));
            if !keep && i < routed {
                routed_dropped += 1;
            }
            i += 1;
            keep
        });
        self.routed -= routed_dropped;
        let dropped = before - self.out.len();
        if dropped > 0 {
            log::info!("coordinator: {dropped} unsent {what} for host {host} superseded by its attach");
        }
    }

    fn push_view(&mut self, to: Target) {
        // Never behind the history (a rebased cache's clock may lag the row's writer).
        let calendar = self.calendar_now(self.clock_ms).max(self.history.now());
        if !self.calendar_gap(self.clock_ms) {
            // A view is a sign of the calendar's life: what it says is durable (the projection
            // changed, so it leaves with the transition holding this) — a load never re-anchors
            // the calendar before it ([`Self::calendar_resume`]).
            self.calendar_seen_ms = self.calendar_seen_ms.max(self.clock_ms);
        }
        self.push(to, CoordMsg::View {
            civs: self.history.civ_markers(),
            sites: self.history.site_markers(),
            calendar,
        });
    }

    /// At a calendar day's turn (at store `now_ms`): the calendar, and the sites if they
    /// changed. The turn is the calendar's sign of life ([`Self::calendar_now`]'s gap). Only
    /// forward: a day at or before the one last broadcast turns nothing (two replicas a few
    /// milliseconds apart never flap the day, Elastic 2b decision 9).
    fn broadcast_daily(&mut self, now: CalendarTick, now_ms: u64) {
        let day = now.0 / TICKS_PER_DAY;
        if day <= self.calendar_day {
            return;
        }
        self.calendar_day = day;
        self.calendar_seen_ms = self.calendar_seen_ms.max(now_ms);
        self.push(Target::Every, CoordMsg::Calendar(now));
        let key = sites_key(&self.history);
        if key != self.sites_key {
            self.sites_key = key;
            self.push(Target::Every, CoordMsg::Sites(self.history.site_markers()));
        }
    }

    /// `host` is the first to attach at world generation `generation`, higher than any before:
    /// the world restarted from a cut (a rollback, a total crash), so every host of an older
    /// generation is gone — its processes are fenced and exit, and a restarted process is a
    /// new host. Each one the projection still names (running a region, holding a player) is
    /// taken out as a predecessor is: its regions park, its players go offline; and each one
    /// still attached (its link not dropped yet), to any replica, is detached — never the gate host, an heir, or
    /// a host a cut waits for (Elastic 2a final review, I2; as [`Self::rolled_back`] detaches
    /// every host). What a restart of this coordinator used to undo by forgetting every host
    /// (before Elastic 2a its view of the hosts was never saved) is undone here, where the
    /// hosts' generation says so.
    fn first_of_generation(&mut self, host: HostId, generation: u64) {
        self.attached_generation = generation;
        let gone: BTreeSet<HostId> =
            self.region_host.values().chain(self.online_on.values()).copied().filter(|h| *h != host).collect();
        for h in gone {
            log::warn!("coordinator: host {host} attaches at world generation {generation}: host {h} of an older one is gone");
            self.host_succeeded(h);
        }
        // Every replica's attachments (Elastic 2b Task 4), not only the live ones' counted ones.
        let attached: Vec<HostId> = self.attachments_any().into_iter().filter(|h| *h != host).collect();
        for h in attached {
            log::warn!("coordinator: host {host} attaches at world generation {generation}: host {h} of an older one is detached");
            self.detach_host(h);
        }
    }

    /// A host (re)attached: it is told so first ([`CoordMsg::Attached`], ahead of every
    /// directive for it decided from here on), its running regions are re-embodied as they
    /// stand (regions the history had live on this host — or on no known host — that it
    /// does not run park empty; another host's stand), its online players are the ones
    /// online on it, and it gets the view.
    fn attach(
        &mut self,
        host: HostId,
        epoch: u64,
        running: Vec<RunningRegion>,
        online: Vec<OnlinePlayer>,
        handed: Vec<(PlayerId, HostId, u64)>,
    ) {
        log::info!(
            "coordinator: host {host} attached (epoch {epoch}) with {} running regions, {} online, {} handed",
            running.len(),
            online.len(),
            handed.len()
        );
        self.drop_unsent_directives(host);
        // The attach reports everyone afresh: an answer to an earlier report (perhaps the
        // old process's, whose numbers a restarted host reuses) is void.
        self.drop_unsent(host, "claim answers", |m| matches!(m, CoordMsg::ClaimIs { .. } | CoordMsg::HopIs { .. }));
        self.push(Target::Host(host), CoordMsg::Attached { epoch });
        let listed: BTreeSet<RegionCoords> = running.iter().map(|r| r.rc).collect();
        for rc in self.history.live_regions() {
            let runs_here = self.region_host.get(&rc).is_none_or(|h| *h == host);
            if runs_here && !listed.contains(&rc) {
                log::warn!("coordinator: {rc:?} was live but host {host} does not run it; parking it empty");
                self.history.region_parked(rc, Vec::new());
            }
        }
        self.region_host.retain(|_, h| *h != host);
        for r in &running {
            if let Some(other) = self.region_host.insert(r.rc, host) {
                log::warn!("coordinator: host {host} attached running {:?}, which host {other} had announced", r.rc);
            }
            self.incarnations.insert(r.rc, r.incarnation);
        }
        let regions: Vec<(RegionCoords, Vec<(crate::history::PersonId, [f32; 3])>)> =
            running.into_iter().map(|r| (r.rc, r.people)).collect();
        for d in self.history.reattach_regions(&regions) {
            self.push_directive(d);
        }
        // The host's players: its lists say who is online on it and who it handed away
        // (each applied by its hop, as the message it stands for); any other player the
        // coordinator still has online on it is not there any more.
        let listed: BTreeSet<PlayerId> = online.iter().map(|p| p.player).collect();
        for p in online {
            self.player_online(host, p);
        }
        for (player, to, hop) in handed {
            self.player_handed(host, player, to, hop);
        }
        let gone: Vec<PlayerId> = self
            .online_on
            .iter()
            .filter(|(p, h)| **h == host && !listed.contains(p))
            .map(|(p, _)| *p)
            .collect();
        for p in gone {
            self.online_on.remove(&p);
            self.online_reported.remove(&p);
            self.displaced_told.retain(|(q, _, _)| *q != p);
        }
        self.push_view(Target::Host(host));
    }

    /// `player` is online on `host`, as that host reports it. With a claim-changing
    /// request of the player unsettled on the host (`claim_pending`) the coordinator's
    /// claim stands (that request's answer, or the host's `ReleaseClaim` after it, settles
    /// it); without one the host's claim is the truth: the coordinator's own may be a save
    /// behind (a crash lost a grant) or ahead (a crash lost a release). A claim the host
    /// reports and the coordinator lacks is taken. Whenever the claim the coordinator ends
    /// with differs from the report, the host is told it ([`CoordMsg::ClaimIs`]): the
    /// coordinator's answer is final.
    ///
    /// A report whose hop is lower than the player's latest is stale and ignored; the
    /// host is told the latest ([`CoordMsg::HopIs`]) in case the report is of its current
    /// placement (a login that could not know the hops elsewhere).
    fn player_online(&mut self, host: HostId, report: OnlinePlayer) {
        let OnlinePlayer { player, claim, claim_pending, report, hop } = report;
        let known = self.hop_of(player);
        let on = self.online_on.get(&player).copied();
        // Two hosts are never online at one hop: at the known hop only the host the
        // player is online on (a re-report, or the grant's host) counts.
        if hop < known || (hop == known && on != Some(host)) {
            log::debug!("coordinator: {player:?} online on host {host} at hop {hop} ignored (hop {known}, online on {on:?})");
            self.push(Target::Host(host), CoordMsg::HopIs { player, reported: hop, hop: known });
            return;
        }
        self.displace(player, host, hop);
        self.hop_of.insert(player, hop);
        self.online_on.insert(player, host);
        self.online_reported.insert(player, (host, hop));
        let mine = self.player_claims.get(&player).copied();
        match (mine, claim) {
            (Some(mine), Some(theirs)) if mine == theirs => {}
            (Some(mine), theirs) if claim_pending => {
                log::debug!("coordinator: {player:?} comes online with {theirs:?} and a request unsettled; it holds {mine:?}");
            }
            (Some(mine), theirs) => {
                log::warn!("coordinator: {player:?} comes online with {theirs:?}; its {mine:?} here is dropped");
                self.release_claim(player);
                if let Some(theirs) = theirs {
                    self.adopt_claim(player, theirs);
                }
            }
            (None, Some(theirs)) => self.adopt_claim(player, theirs),
            (None, None) => {}
        }
        let now = self.player_claims.get(&player).copied();
        if now != claim {
            log::info!("coordinator: host {host} reported {player:?} with {claim:?}; the claim is {now:?}");
            self.push(Target::Host(host), CoordMsg::ClaimIs { player, claim: now, report });
        }
    }

    /// `player`'s placement `hop` on `host` ended. Stale below the latest hop; at it, only
    /// the host the player is online on ends it (a transit to another host stands); above
    /// it, it is the end of a placement whose start has not arrived yet (it travels on
    /// another link, or was lost): offline at that hop, and every message of that hop or
    /// lower is stale from here on.
    fn player_offline(&mut self, host: HostId, player: PlayerId, hop: u64) {
        let known = self.hop_of(player);
        let on = self.online_on.get(&player).copied();
        if hop < known || (hop == known && on != Some(host)) {
            log::debug!("coordinator: {player:?} offline on host {host} at hop {hop} ignored (hop {known}, online on {on:?})");
            return;
        }
        if on == Some(host) {
            self.displaced_told.retain(|(q, _, _)| *q != player);
        } else {
            self.displace(player, host, hop);
        }
        self.hop_of.insert(player, hop);
        self.online_on.remove(&player);
        self.online_reported.remove(&player);
    }

    /// A message of `by` at `hop` (above the known one) moves `player` away from the host
    /// it was online on by that host's own report: that host is told
    /// ([`CoordMsg::HopIs`]), once per reported hop. In a real hand-over it no longer holds
    /// the player and ignores it; if it does hold it (its placement lost the race against
    /// an older placement's messages in flight, or a duplicate) it renumbers above `hop`
    /// and reports again — the newest reporter wins, as the newest session does.
    fn displace(&mut self, player: PlayerId, by: HostId, hop: u64) {
        let Some(&(host, reported)) = self.online_reported.get(&player) else { return };
        if host == by || self.online_on.get(&player) != Some(&host) {
            return;
        }
        if self.displaced_told.insert((player, host, reported)) {
            log::debug!("coordinator: {player:?} moved off host {host} (hop {reported}) by host {by} at hop {hop}: told");
            self.push(Target::Host(host), CoordMsg::HopIs { player, reported, hop });
        }
    }

    /// `host` handed `player`, placed there at `hop`, to host `to`: in transit — online on
    /// `to` (its claim in use) until `to` reports it (at `hop + 1`). Stale below the latest
    /// hop, and at it from a host the player is not online on; above it, the placement on
    /// `host` whose start has not arrived yet, then the hand-over.
    fn player_handed(&mut self, host: HostId, player: PlayerId, to: HostId, hop: u64) {
        let known = self.hop_of(player);
        let on = self.online_on.get(&player).copied();
        if hop < known || (hop == known && on != Some(host)) {
            log::debug!("coordinator: {player:?} handed by host {host} to {to} at hop {hop} ignored (hop {known}, online on {on:?})");
            return;
        }
        if on != Some(host) {
            self.displace(player, host, hop);
        }
        self.hop_of.insert(player, hop);
        self.online_reported.remove(&player);
        if self.host_is_succeeded(to) {
            // Handed to a host declared dead (this message was on its way when the
            // declaration came): nobody will report the player there — offline, not "online
            // on" a dead ordinal. If the hand-off
            // reached the dead host's journal the heir parks it and reports it offline too.
            log::info!("coordinator: {player:?} handed by host {host} to host {to}, which was succeeded: offline");
            self.online_on.remove(&player);
            return;
        }
        self.online_on.insert(player, to);
    }

    /// Take the claim the host reports for `player` (who holds none here).
    fn adopt_claim(&mut self, player: PlayerId, claim: Claim) {
        self.player_claims.insert(player, claim);
    }

    /// `player` gives up what it plays (durable with the transition of the message that
    /// released it).
    fn release_claim(&mut self, player: PlayerId) {
        self.player_claims.remove(&player);
    }

    /// Settle a timed-out state-changing request ([`HostMsg::Settle`]): its memoised
    /// answer if it was applied, else a `WorldUnavailable` refusal, memoised without
    /// executing it (a later copy of the request gets that refusal too).
    fn settle(&mut self, host: HostId, id: RequestId, req: WorldRequest) {
        let reply = match self.memo.get(&id) {
            Some(reply) => reply.clone(),
            None => {
                let refusal = match req {
                    WorldRequest::StartAdventure(_) => WorldReply::AdventureRefused(AdventureRefusal::WorldUnavailable),
                    other => {
                        log::warn!("coordinator: settle of {id:?} for a query {other:?}: nothing to settle");
                        return;
                    }
                };
                log::info!("coordinator: {id:?} settled unapplied: refused");
                self.remember(id, refusal.clone());
                refusal
            }
        };
        self.push(Target::Host(host), CoordMsg::Reply { id, reply });
    }

    /// Whether a request of `player` made at placement `hop` on `host` is of an older
    /// placement than the one the coordinator knows: below its hop, or at it while the
    /// player is online on another host (two hosts are never online at one hop).
    fn from_older_placement(&self, host: HostId, player: PlayerId, hop: u64) -> bool {
        let known = self.hop_of(player);
        hop < known || (hop == known && self.online_on.get(&player).is_some_and(|on| *on != host))
    }

    fn request(&mut self, host: HostId, id: RequestId, player: PlayerId, hop: u64, req: WorldRequest) {
        if let Some(reply) = self.memo.get(&id) {
            log::debug!("coordinator: {id:?} repeated; answering from the memo");
            let reply = reply.clone();
            return self.push(Target::Host(host), CoordMsg::Reply { id, reply });
        }
        // A state-changing request of an older placement of its player grants nothing:
        // refused through the settle path (memoised), as a zombie's is.
        if req.changes_state() && self.from_older_placement(host, player, hop) {
            log::warn!("coordinator: {id:?} of {player:?} at hop {hop} from host {host} is of an older placement: refused");
            return self.settle(host, id, req);
        }
        let memoise = req.changes_state();
        let reply = match req {
            WorldRequest::StartAdventure(site) => self.start_adventure(player, site),
            WorldRequest::Legends(q) => WorldReply::Legends(self.history.legends(&q)),
            WorldRequest::PersonInfo(p) => WorldReply::PersonInfo(self.history.person_info(p)),
        };
        if memoise {
            self.remember(id, reply.clone());
        }
        let granted = reply.grants_claim();
        if granted {
            // The player plays it on the host that asked. If another host had it online by
            // its own report, that host is told (`displace`): a grant to an older placement
            // still in flight must not silently take the player off the one holding it.
            let known = self.hop_of(player);
            self.displace(player, host, known);
            self.online_on.insert(player, host);
            if self.online_reported.get(&player).is_some_and(|(h, _)| *h != host) {
                self.online_reported.remove(&player);
            }
        }
        self.push(Target::Host(host), CoordMsg::Reply { id, reply });
        if granted {
            // Every host's view of the sites follows at once (the player may play on
            // another host); clients still get them at the day's turn.
            self.push(Target::Every, CoordMsg::SiteMarkers(self.history.site_markers()));
        }
    }

    /// Memoise `reply` for `id` until its host acknowledges it ([`Self::forget_settled`]);
    /// never evicted. Past [`ANSWERED_MEMO_MAX`] an error names the host holding most.
    fn remember(&mut self, id: RequestId, reply: WorldReply) {
        if self.memo.insert(id, reply).is_none() {
            self.memo_order.push_back(id);
        }
        let quiet = self.memo_warned_ms.is_some_and(|ms| self.clock_ms.saturating_sub(ms) < MEMO_WARN_SECS * 1000);
        if self.memo.len() > ANSWERED_MEMO_MAX && !quiet {
            self.memo_warned_ms = Some(self.clock_ms);
            let mut per_host: BTreeMap<HostId, usize> = BTreeMap::new();
            for id in self.memo.keys() {
                *per_host.entry(id.host).or_default() += 1;
            }
            let (host, n) = per_host.into_iter().max_by_key(|(_, n)| *n).expect("not empty");
            log::error!(
                "coordinator: {} unacknowledged answers remembered (over {ANSWERED_MEMO_MAX}); host {host} holds {n}",
                self.memo.len()
            );
        }
    }

    /// Host `host` acknowledged its answers of `boot` below `below` ([`HostMsg::Settled`],
    /// an `Attach`'s watermark): forgotten. At an `Attach` (`attach`) naming a boot not
    /// seen from the host before, the host's answers of every other boot go too (that
    /// process is gone).
    fn forget_settled(&mut self, host: HostId, boot: u64, below: u64, attach: bool) {
        let new_boot = attach && self.boots_seen.entry(host).or_default().insert(boot);
        let forget = |id: &RequestId| id.host == host && ((id.boot == boot && id.seq < below) || (new_boot && id.boot != boot));
        let before = self.memo.len();
        self.memo.retain(|id, _| !forget(id));
        if self.memo.len() != before {
            self.memo_order.retain(|id| !forget(id));
            log::debug!("coordinator: {} answers of host {host} acknowledged", before - self.memo.len());
        }
    }

    /// Start an adventure at a populated site (`WorldManager::start_adventure`'s checks).
    fn start_adventure(&mut self, player: PlayerId, site: SiteId) -> WorldReply {
        let refuse = WorldReply::AdventureRefused;
        let Some(s) = self.history.site(site) else {
            return refuse(AdventureRefusal::UnknownSite);
        };
        if s.abandoned.is_some() {
            return refuse(AdventureRefusal::SiteAbandoned);
        }
        if !s.members.iter().any(|&p| self.history.person(p).is_some_and(|p| p.died.is_none())) {
            return refuse(AdventureRefusal::Unpopulated);
        }
        if self.player_claims.contains_key(&player) {
            return refuse(AdventureRefusal::AlreadyPlaying);
        }
        let region = s.region;
        self.player_claims.insert(player, Claim::Adventure(site));
        WorldReply::AdventureStarted { site, region }
    }
}
