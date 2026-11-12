//! The messages between a host (regions, sessions) and the world coordinator (history,
//! claims, world requests). Protocol, not sim state: never hashed, framed with
//! [`crate::encode_packet`] on a real link.

use serde::{Deserialize, Serialize};

use crate::history::{
    AdventureRefusal, CalendarTick, Directive, EmbarkRefusal, LegendsPage, LegendsQuery, PersonId, SiteId,
};
use crate::{CivMarker, EmbarkOptions, EmbarkRequest, GameEventKind, PersonInfo, PlayerId, RegionCoords, SiteMarker};

/// One host's world request, unique across the host's restarts: `boot` is random per
/// host process start, `seq` counts up within it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId {
    pub host: u32,
    pub boot: u64,
    pub seq: u64,
}

/// A player's request of the world services, sent by its host.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum WorldRequest {
    EmbarkOptions,
    Embark(EmbarkRequest),
    ReturnTo(SiteId),
    StartAdventure(SiteId),
    Legends(LegendsQuery),
    PersonInfo(PersonId),
}

impl WorldRequest {
    /// Whether the request may change world state (found a site, take a claim): its
    /// answer is memoised and replayed for a repeated [`RequestId`], never re-executed.
    pub fn changes_state(&self) -> bool {
        matches!(self, Self::Embark(_) | Self::ReturnTo(_) | Self::StartAdventure(_))
    }
}

/// The coordinator's answer to a [`WorldRequest`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum WorldReply {
    EmbarkOptions(EmbarkOptions),
    Embarked { site: SiteId, region: RegionCoords },
    EmbarkRefused(EmbarkRefusal),
    Returned { site: SiteId, region: RegionCoords },
    AdventureStarted { site: SiteId, region: RegionCoords },
    AdventureRefused(AdventureRefusal),
    Legends(LegendsPage),
    PersonInfo(Option<PersonInfo>),
}

impl WorldReply {
    /// A reply that hands out a claim: sent only once the state granting it is saved.
    pub fn grants_claim(&self) -> bool {
        matches!(self, Self::Embarked { .. } | Self::Returned { .. } | Self::AdventureStarted { .. })
    }
}

/// What a player plays: a fortress (exclusive per site while its player is online) or an
/// adventure at a site.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    Fortress(SiteId),
    Adventure(SiteId),
}

impl Claim {
    pub fn site(self) -> SiteId {
        match self {
            Self::Fortress(s) | Self::Adventure(s) => s,
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
}

/// Host → coordinator.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum HostMsg {
    /// The host's first message on a (re)connected link: everything it runs and who is
    /// online, superseding whatever lifecycle messages were lost while the link was down.
    /// `epoch` counts the host's attaches; the coordinator echoes it first
    /// ([`CoordMsg::Attached`]), so the host knows which of its messages were decided
    /// before the attach (stale) and which after. `generation`: the world generation the
    /// host process runs (a coordinator runtime refuses an `Attach` of a lower one than the
    /// store's: a stale host process about to exit).
    Attach { host: u32, epoch: u64, running: Vec<RunningRegion>, online: Vec<OnlinePlayer>, generation: u64 },
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
    Request { id: RequestId, player: PlayerId, req: WorldRequest },
    /// A state-changing request that timed out toward its client (who was told the world
    /// is unavailable), sent again after an `Attach` to learn its fate — never to run it:
    /// the coordinator answers from its memo if it applied the request, else it memoises
    /// and answers a `WorldUnavailable` refusal without executing (a request its player
    /// was told failed must never complete later).
    Settle { id: RequestId, req: WorldRequest },
    PlayerOnline(OnlinePlayer),
    PlayerOffline { player: PlayerId },
    /// The player gave up its claim (retired, ended the adventure, or left before the
    /// claim's reply reached it). The site stays.
    ReleaseClaim { player: PlayerId },
}

/// Where a coordinator message goes ([`crate::coordinator::Coordinator::take_out`]).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// One host (by ordinal): a reply or claim answer to the host that asked, a region's
    /// messages to the host that runs it, an attach's echo and view to the host attaching.
    Host(u32),
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
    /// of them (the region of a founded site). Not fanned out at once: connected clients
    /// get [`CoordMsg::Sites`] at the day's turn, but the host's view reaches a player who
    /// retires or ends an adventure and every new connection straight away.
    SiteMarkers(Vec<SiteMarker>),
    /// The claim the host reported for `player` (`PlayerOnline`, or an `Attach`'s online
    /// entry) differs from the coordinator's, which is final: the host sets the player's
    /// claim to exactly `claim` and tells its client. Sent to the reporting host only.
    ClaimIs { player: PlayerId, claim: Option<Claim> },
    /// A history log line for the clients whose home is `region`.
    Notice { region: RegionCoords, line: String },
    /// The coordinator has applied the host's [`HostMsg::Attach`] of this `epoch`: every
    /// `SpawnPeople` / `Directive` after this was decided from it; the host drops those
    /// that came before (the attach re-decided their regions from the bodies listed).
    Attached { epoch: u64 },
}

impl CoordMsg {
    /// Durable before visible: a directive or a claim-granting reply leaves only after
    /// the state that produced it is saved.
    pub fn is_gated(&self) -> bool {
        match self {
            Self::Directive { .. } => true,
            Self::Reply { reply, .. } => reply.grants_claim(),
            _ => false,
        }
    }
}
