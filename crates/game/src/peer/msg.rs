//! The messages between two region hosts. Protocol, not sim state: never hashed, framed
//! with [`crate::encode_packet`] on a real link.

use serde::{Deserialize, Serialize};

use crate::{
    Claim, ClientId, CutId, EntityBundle, GameEventKind, HaloEntityMsg, HaloUpdate, IntentMsg, PlayerId, PlayerRecord,
    RegionCoords, Tick,
};

/// Where a host can be reached: by its peers (`quic`) and by players (`webtransport`),
/// with the hash of its pinned self-signed certificate. `None` = not offered.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HostEndpoint {
    pub ordinal: u32,
    pub quic: Option<String>,
    pub webtransport: Option<String>,
    pub cert_hash: Option<String>,
}

/// A host's first message on every (re)connected peer link.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PeerHello {
    pub ordinal: u32,
    /// Random per host process start: a new boot voids the old numbering of reliable
    /// messages both ways.
    pub boot: u64,
    /// The world generation the host runs (a world rollback raises it); hosts of
    /// different generations exchange nothing.
    pub generation: u64,
    pub endpoint: HostEndpoint,
    /// The last reliable seq this side received from the other, counted against the
    /// other's boot [`PeerHello::received_from_boot`] (0 = none).
    pub received_through: u64,
    /// The other side's boot that `received_through` was counted against (`None` =
    /// nothing ever counted). The receiver of the hello resumes after `received_through`
    /// ONLY when this is its own boot and its numbering is addressed to the hello's boot;
    /// otherwise the count is not about its numbers (it is treated as 0) and it restarts
    /// its numbering from 1 for everything it still owes (`PeerSession`).
    pub received_from_boot: Option<u64>,
}

/// A player handed from one host to another with an entity or a record.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PlayerHandoff {
    pub client: ClientId,
    pub player: PlayerId,
    pub claim: Option<Claim>,
    /// The player's token and session epoch (login sessions).
    pub login: Option<(String, u64)>,
    pub park: bool,
}

/// What must arrive exactly once, in order: numbered per directed link, kept by the
/// sender until acknowledged ([`super::PeerSession::queue`]).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Reliable {
    /// An entity crossing into a region of the other host, applied at `stamp`.
    Transfer { target: RegionCoords, stamp: Tick, bundle: Box<EntityBundle>, player: Option<PlayerHandoff> },
    Intent(IntentMsg),
    /// A player without a body (parked, observing) moving to a region of the other host.
    PlayerMove { handoff: PlayerHandoff, record: Box<PlayerRecord>, target: RegionCoords },
    ObserverOrder { client: ClientId, player: PlayerId, nonce: u64, expires_unix_secs: u64 },
    /// Spawn a login session's player; `origin` is the host the order entered at,
    /// `hops` how often it was forwarded.
    SpawnOrder { origin: u32, hops: u8, player_token: String, session_epoch: u64 },
    SpawnAck { player_token: String, session_epoch: u64, ok: bool, endpoint: Option<HostEndpoint> },
    Kick { player_token: String, session_epoch: u64, reason: String },
    KickAck { player_token: String, session_epoch: u64 },
    SessionEnded { player_token: String, session_epoch: u64 },
    /// The sender's in-band mark of a world save cut: what it sent before the mark
    /// belongs to the cut's receiver side, what follows to the next cut.
    CutMark { cut: CutId },
}

/// What may be lost: sent only while the peer is reachable, never replayed.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Volatile {
    /// Every region the sender runs (after a reconnect).
    Running(Vec<RegionCoords>),
    RegionUp(RegionCoords),
    RegionDown(RegionCoords),
    HaloRequest { from: RegionCoords, of: RegionCoords },
    HaloUpdate { to: RegionCoords, update: HaloUpdate, stamp: Tick },
    HaloEntity { to: RegionCoords, msg: HaloEntityMsg, stamp: Tick },
    Input { client: ClientId, kind: GameEventKind, stamp: Tick },
}

/// One message on a peer link.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PeerMsg {
    Hello(PeerHello),
    Reliable { seq: u64, body: Reliable },
    /// Cumulative: every reliable message up to `through` arrived.
    Ack { through: u64 },
    Volatile(Volatile),
}
