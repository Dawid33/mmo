//! The messages between a host (regions, sessions) and the world coordinator (history,
//! claims, world requests). Protocol, not sim state: never hashed, framed with
//! [`crate::encode_packet`] on a real link.

use serde::{Deserialize, Serialize};

use crate::HostId;
use crate::history::{
    AdventureRefusal, CalendarTick, Directive, LegendsPage, LegendsQuery, PersonId, SiteId,
};
use crate::{
    CivMarker, CutId, GameEventKind, PersonInfo, PlayerId, RegionCoords, SiteMarker, SuccessionId, SuccessionKind, SuperRegion, Tick,
};

/// One host's world request, unique across the host's restarts: `boot` is random per
/// host process start, `seq` counts up within it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId {
    pub host: HostId,
    pub boot: u64,
    pub seq: u64,
}

/// A player's request of the world services, sent by its host.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum WorldRequest {
    StartAdventure(SiteId),
    Legends(LegendsQuery),
    PersonInfo(PersonId),
}

impl WorldRequest {
    /// Whether the request may change world state (take a claim): its
    /// answer is memoised and replayed for a repeated [`RequestId`], never re-executed.
    pub fn changes_state(&self) -> bool {
        matches!(self, Self::StartAdventure(_))
    }
}

/// The coordinator's answer to a [`WorldRequest`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum WorldReply {
    AdventureStarted { site: SiteId, region: RegionCoords },
    AdventureRefused(AdventureRefusal),
    Legends(LegendsPage),
    PersonInfo(Option<PersonInfo>),
}

impl WorldReply {
    /// A reply that hands out a claim (like every output, sent only once the state deciding it
    /// is written: `Coordinator::take_transition`).
    pub fn grants_claim(&self) -> bool {
        matches!(self, Self::AdventureStarted { .. })
    }
}

/// What a player plays: an adventure at a site.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    Adventure(SiteId),
}

impl Claim {
    pub fn site(self) -> SiteId {
        match self {
            Self::Adventure(s) => s,
        }
    }
}

/// A region running on the host, with the history people whose bodies stand in it
/// (region-local translations).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct RunningRegion {
    pub rc: RegionCoords,
    pub incarnation: u64,
    pub people: Vec<(PersonId, [f32; 3])>,
}

/// An online player as its host reports it: what it plays, and whether a claim-changing
/// request of the player is still unsettled on the host (pending toward the client, or
/// timed out and waiting for its answer). Without one the host's `claim` is the truth
/// for the coordinator (its own may be a save behind, or ahead of a lost release).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct OnlinePlayer {
    pub player: PlayerId,
    pub claim: Option<Claim>,
    pub claim_pending: bool,
    /// The host's number of this report (increasing per host process): echoed in the
    /// coordinator's [`CoordMsg::ClaimIs`] answer, so the host applies only an answer to
    /// its latest report of the player.
    pub report: u64,
    /// The player's placement count on hosts ([`HostMsg`]'s hop rule): this placement's.
    pub hop: u64,
}

/// Host → coordinator.
///
/// **Hops:** every message about a player's presence carries the player's `hop`, its
/// placement count on hosts within the world generation: a hand-off or move to another
/// host places it at the sender's hop + 1 ([`crate::PlayerHandoff::hop`]), a login at
/// one more than the host knows of it (raised by [`CoordMsg::HopIs`] when the
/// coordinator knows a higher one, or another host's message displaced it). Messages of different hosts may arrive in any order;
/// the coordinator orders them by hop and ignores one lower than the player's latest.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum HostMsg {
    /// The host's first message on a (re)connected link: everything it runs and who is
    /// online, superseding whatever lifecycle messages were lost while the link was down.
    /// `epoch` counts the host's attaches; the coordinator echoes it first
    /// ([`CoordMsg::Attached`]), so the host knows which of its messages were decided
    /// before the attach (stale) and which after. `generation`: the world generation the
    /// host process runs (a coordinator runtime refuses an `Attach` of a lower one than the
    /// store's: a stale host process about to exit). `boot`: the host process's
    /// [`RequestId::boot`] — a boot the coordinator has not seen from this host means its
    /// old process is gone, so the answers remembered for that host's other boots are
    /// forgotten. `settled_below`: as [`HostMsg::Settled`]'s `below`, for `boot`.
    Attach {
        host: HostId,
        epoch: u64,
        running: Vec<RunningRegion>,
        online: Vec<OnlinePlayer>,
        generation: u64,
        boot: u64,
        settled_below: u64,
        /// Players this host handed to another host (`to`, the hop they had here) since
        /// the last [`CoordMsg::Attached`] it received: their `PlayerHandedTo` may have been
        /// lost with the old connection.
        handed: Vec<(PlayerId, HostId, u64)>,
        /// The world cut this host takes part in and has not heard the outcome of
        /// ([`CoordMsg::CutDone`]): the coordinator answers it (from what it decided, or
        /// the store's decision, an undecided cut aborted).
        pending_cut: Option<CutId>,
        /// The successions this host has answered (R4) and not yet heard settled
        /// ([`CoordMsg::SuccessionDone`]): `(id, adopted)` — applied as a survivor, and, for
        /// the heir, whether it adopted too. A restarted coordinator counts these as answered
        /// again; an order not listed is (re)sent.
        successions: Vec<(SuccessionId, bool)>,
        /// What a crash of this host costs the world ([`crate::Ownership::policy`], from its
        /// `VOXEL_OWNERSHIP`). A coordinator refuses an `Attach` whose policy is not its own,
        /// whatever the number of hosts: under `Rollback` it would run no world cuts and never
        /// declare a `claim` host's death.
        policy: crate::CrashPolicy,
    },
    /// Region `rc` started (its `incarnation`-th start on the host): answered with
    /// [`CoordMsg::SpawnPeople`] for that incarnation.
    RegionStarted { rc: RegionCoords, incarnation: u64 },
    RegionStopping { rc: RegionCoords },
    /// Region `rc` parked with these people's bodies in it (empty: the region was lost).
    RegionParked { rc: RegionCoords, people: Vec<(PersonId, [f32; 3])> },
    /// A person's body moved over a seam into `to`; `delivered`: the host put the body
    /// there (`to` was running), else the host dropped it for the history to absorb.
    PersonCrossed { person: PersonId, to: RegionCoords, delivered: bool },
    /// Where embodied people stand now (the autosave's note).
    Positions(Vec<(PersonId, [f32; 3])>),
    /// A directive the host could not apply ([`CoordMsg::Directive`]'s rule), with the
    /// incarnation it was stamped for.
    DirectiveBounced { directive: Directive, incarnation: u64 },
    /// A player's world request. A replay of an id whose first reply is still on its way
    /// (in the coordinator's queue or on the link) is answered again from the memo, so one
    /// id may get two `Reply`s: the host answers the first and drops any later one by id.
    ///
    /// `hop` is the player's placement number on the asking host when the request was
    /// made. A state-changing request of an older placement (below the coordinator's hop
    /// for the player, or at it while the player is online on another host) is refused.
    Request { id: RequestId, player: PlayerId, hop: u64, req: WorldRequest },
    /// A state-changing request that timed out toward its client (who was told the world
    /// is unavailable), sent again after an `Attach` to learn its fate — never to run it:
    /// the coordinator answers from its memo if it applied the request, else it memoises
    /// and answers a `WorldUnavailable` refusal without executing (a request its player
    /// was told failed must never complete later).
    Settle { id: RequestId, req: WorldRequest },
    PlayerOnline(OnlinePlayer),
    /// The player's placement here (`hop`) ended: it disconnected or parked.
    PlayerOffline { player: PlayerId, hop: u64 },
    /// The player gave up its claim (ended the adventure, or left before the claim's
    /// reply reached it). The site stays.
    ReleaseClaim { player: PlayerId },
    /// The player left this host for host `to` (a walking hand-off or a move), its claim
    /// going along: it stays online — in transit — until host `to` reports it
    /// (`PlayerOnline`, or `PlayerOffline` if it parks there on arrival). Sent instead of
    /// `PlayerOffline`, so a player crossing hosts never reads as offline. `hop`: the
    /// one it had here (host `to` places it at `hop + 1`).
    PlayerHandedTo { player: PlayerId, to: HostId, hop: u64 },
    /// Every request of this host's `boot` with `seq < below` has been answered and
    /// applied here, or was never sent: the coordinator forgets their memoised answers
    /// (no copy of them can come any more). Sent whenever the host's lowest outstanding
    /// sequence rises.
    Settled { boot: u64, below: u64 },
    /// This host's part of world cut `cut` ([`CoordMsg::BeginCut`]) is staged in the store
    /// (prepared), waiting for the coordinator's decision.
    CutPrepared { cut: CutId },
    /// This host's part of world cut `cut` failed: the cut is aborted. The host still
    /// waits for [`CoordMsg::CutDone`] before it takes part in another cut.
    CutFailed { cut: CutId, why: CutFailure },
    /// The host is about to exit (several hosts): it asks for a cut in which it leaves
    /// (`BeginCut { leaving }` naming it), parks everything for it and exits once it
    /// commits — its `hosts` row then clean, so its return keeps the world generation.
    LeaveRequest,
    /// A survivor of succession `id` ([`CoordMsg::Succession`]) reported its journal to the
    /// heir and re-routed what it owed the predecessor (R4).
    SuccessionApplied { id: SuccessionId },
    /// The heir of succession `id` resolved the survivors' journals, adopted the
    /// predecessor's rows and loaded its regions: ready to cut (R4, Task 9).
    SuccessionAdopted { id: SuccessionId },
    /// Something on this host needs a region of super-region `sr`, whose directory owner
    /// left in a committed leave cut (its `hosts` row clean) and has not come back (R4
    /// drain). The coordinator looks the owner up and gives ALL its land to one heir by a
    /// `Leave` succession at once (a host that left never returns as itself: a restarted
    /// process is a new host). Sent at most
    /// once per [`crate::LAND_WANTED_EVERY_MS`] per super-region; ignored under `Static`.
    LandWanted { sr: SuperRegion },
    /// World cut `cut` runs here in count mode with participant `peer` (R4 Task 12,
    /// `world_manager/world_cut.rs`): their link is down (or was not up at the `BeginCut`),
    /// this host froze its session with it, and has received `received_through` of `peer`'s
    /// numbering toward it, counted against `peer`'s process `boot` (0: no hello of `peer`
    /// yet). The coordinator relays it to `peer` ([`CoordMsg::CutPeerCount`]), only between
    /// participants of the running cut.
    CutPeerCount { cut: CutId, peer: HostId, received_through: u64, boot: u64 },
    /// What this host tells the login service (Elastic 1 Task 6): the gate host's answer to a
    /// [`CoordMsg::Login`] order (a `SpawnAck`, or a `KickAck` once every peer applied the
    /// kick), or the end of a login session that left the world here. The coordinator forwards
    /// it to the login service unchanged and keeps nothing of it.
    Login(crate::LoginOut),
}

/// Why a host's part of a world cut failed ([`HostMsg::CutFailed`]).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum CutFailure {
    /// The cut tick was too close or past when the host (or one of its regions) got it:
    /// the next cut doubles its lead.
    Missed,
    /// The cut could not be taken whole: a peer link went down, a region was lost, a
    /// deadline passed, another cut was still pending, the generation differed.
    Aborted,
    /// The store refused or failed the part.
    Store(String),
}

/// Where a coordinator message goes ([`crate::coordinator::Coordinator::take_out`]).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// One host (by ordinal): a reply or claim answer to the host that asked, a region's
    /// messages to the host that runs it, an attach's echo and view to the host attaching.
    Host(HostId),
    /// Every host (the world's view, the calendar, the sites).
    Every,
}

/// Coordinator → host.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum CoordMsg {
    /// The world as clients see it: sent when the coordinator starts and on every
    /// [`HostMsg::Attach`].
    View { civs: Vec<CivMarker>, sites: Vec<SiteMarker>, calendar: CalendarTick },
    /// The answer to [`HostMsg::RegionStarted`]: the history's people for that start.
    SpawnPeople { rc: RegionCoords, incarnation: u64, events: Vec<GameEventKind> },
    /// A history directive, stamped with the latest incarnation of its region the
    /// coordinator saw start. The host applies it only if the region runs that very
    /// incarnation and that incarnation's `SpawnPeople` has been delivered; otherwise it
    /// answers [`HostMsg::DirectiveBounced`] with the same incarnation (the coordinator
    /// drops a bounce for an incarnation older than the region's latest start — the park
    /// in between restored the person and the newer catch-up decided afresh).
    Directive { directive: Directive, incarnation: u64 },
    Reply { id: RequestId, reply: WorldReply },
    /// A calendar day turned.
    Calendar(CalendarTick),
    /// The sites changed (founded, abandoned, a population crossing a multiple of 10).
    Sites(Vec<SiteMarker>),
    /// The sites as they stand right after a request granted a claim, for the host's view
    /// of them. Not fanned out at once: connected clients get [`CoordMsg::Sites`] at the
    /// day's turn, but the host's view reaches a player who ends an adventure and every
    /// new connection straight away.
    SiteMarkers(Vec<SiteMarker>),
    /// The claim the host reported for `player` (`PlayerOnline`, or an `Attach`'s online
    /// entry) differs from the coordinator's, which is final: the host sets the player's
    /// claim to exactly `claim` and tells its client. Sent to the reporting host only.
    /// `report` is the [`OnlinePlayer::report`] it answers.
    ClaimIs { player: PlayerId, claim: Option<Claim>, report: u64 },
    /// A history log line for the clients whose home is `region`.
    Notice { region: RegionCoords, line: String },
    /// The coordinator has applied the host's [`HostMsg::Attach`] of this `epoch`: every
    /// `SpawnPeople` / `Directive` after this was decided from it; the host drops those
    /// that came before (the attach re-decided their regions from the bodies listed).
    Attached { epoch: u64 },
    /// The host's report of `player` at hop `reported` (`PlayerOnline`, an `Attach`'s
    /// online entry) was ignored as stale — below `hop`, the highest the coordinator
    /// knows, or at it from another host — or another host's message at `hop` moved the
    /// player off this host (sent once per reported hop). If the player's current
    /// placement on the host is that very one (a login that could not know the player's
    /// hops elsewhere, or one that lost the race against an older placement's messages in
    /// flight), the host numbers it `hop + 1` and reports it again: the newest reporter
    /// wins, as the newest session does, and a real holder re-asserts itself once. A host
    /// renumbers one player at most `HOP_RENUMBER_MAX_PER_PLAYER` times.
    HopIs { player: PlayerId, reported: u64, hop: u64 },
    /// Take part in world cut `cut` at world tick `tick` (several hosts): every host of
    /// `hosts` snapshots its regions at `tick`, marks its peer links in band
    /// ([`crate::Reliable::CutMark`]) and stages its part; `leaving` are the hosts that
    /// leave in this cut (among `hosts`; [`HostMsg::LeaveRequest`]). `generation`: the world generation the cut is of — a
    /// host of another generation refuses it.
    BeginCut { cut: CutId, tick: Tick, hosts: Vec<HostId>, leaving: Vec<HostId>, generation: u64 },
    /// World cut `cut` is decided: `committed` (every part visible) or aborted (none).
    CutDone { cut: CutId, committed: bool },
    /// Host `predecessor` (its process `predecessor_boot`) was declared dead (or left) and
    /// all its super-regions (`moved`, at their new epochs) are `heir`'s now (R4). Sent to
    /// every survivor (`survivors`, the heir among them): each reports its peer journal
    /// against the predecessor relative to `base_cut` to the heir, re-routes what it owed
    /// the predecessor, and answers [`HostMsg::SuccessionApplied`].
    Succession {
        id: SuccessionId,
        predecessor: HostId,
        predecessor_boot: u64,
        heir: HostId,
        kind: SuccessionKind,
        base_cut: CutId,
        moved: Vec<(SuperRegion, u64)>,
        survivors: Vec<HostId>,
    },
    /// Succession `id` is over: `settled` by a committed world cut, or abandoned by a world
    /// rollback. The coordinator's OWN rollback is followed by [`CoordMsg::Generation`]; one
    /// it only found in the store (a re-read that no longer lists the run: `worldctl
    /// rollback`, a generation-raising registration) is NOT — a host of the older generation
    /// then learns it from its next cut part (`StaleGeneration`), a peer of the newer
    /// generation, or its next `Attach` (refused, answered with the generation).
    SuccessionDone { id: SuccessionId, settled: bool },
    /// The coordinator rolled the world back to its last committed cut (R4: a second host
    /// lost before a succession settled, or a succession's deadline): the world generation
    /// is this now — every host notes it and exits (`EXIT_WORLD_ROLLBACK`).
    Generation(u64),
    /// Participant `from` of world cut `cut` has received `received_through` of this host's
    /// numbering toward it, counted against this host's process `boot` (its
    /// [`HostMsg::CutPeerCount`], relayed): everything numbered above it is this host's in
    /// the cut (a boot that is not this host's own counts 0).
    CutPeerCount { cut: CutId, from: HostId, received_through: u64, boot: u64 },
    /// An order of the login service for the gate host (Elastic 1 Task 6,
    /// [`crate::Coordinator::login_order`]): its server resolves it and the manager places a
    /// spawn (here or on the host holding the player) or applies and relays a kick; the
    /// answers come back as [`HostMsg::Login`].
    Login(crate::LoginOrder),
}
