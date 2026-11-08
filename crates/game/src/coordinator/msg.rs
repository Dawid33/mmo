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

/// Host → coordinator.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum HostMsg {
    /// The host's first message on a (re)connected link: everything it runs and who is
    /// online, superseding whatever lifecycle messages were lost while the link was down.
    Attach { host: u32, running: Vec<RunningRegion>, online: Vec<(PlayerId, Option<Claim>)> },
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
    PlayerOnline { player: PlayerId, claim: Option<Claim> },
    PlayerOffline { player: PlayerId },
    /// The player gave up its claim (retired, ended the adventure, or left before the
    /// claim's reply reached it). The site stays.
    ReleaseClaim { player: PlayerId },
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
    /// A history log line for the clients whose home is `region`.
    Notice { region: RegionCoords, line: String },
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
