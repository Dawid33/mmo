//! The messages between two region hosts. Protocol, not sim state: never hashed, framed
//! with [`crate::encode_packet`] on a real link.

use serde::{Deserialize, Serialize};

use crate::{
    Claim, ClientId, CutId, EntityBundle, GameEventKind, HaloEntityMsg, HaloUpdate, IntentMsg,
    PlayerId, PlayerRecord, RegionCoords, Tick,
};

/// Where a host can be reached: by its peers (`quic`) and by players (`webtransport`),
/// with the hash of its pinned self-signed certificate. `None` = not offered.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HostEndpoint {
    pub id: crate::HostId,
    pub quic: Option<String>,
    pub webtransport: Option<String>,
    pub cert_hash: Option<String>,
}

/// A host's first message on every (re)connected peer link.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PeerHello {
    pub id: crate::HostId,
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

/// Where a moved player's body is made ([`Reliable::PlayerMove`], Elastic 5).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovePlace {
    /// At the target region's landing (a move into another host's region).
    Landing,
    /// At the record's own pose (its land moved to that host: it stands where it stood).
    Pose,
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
    /// The player's placement count on hosts at its arrival: the sender's + 1
    /// ([`crate::HostMsg`]'s hop rule).
    pub hop: u64,
}

/// An observer ticket's secret ([`Reliable::ObserverOrder`]): serialized as the bare
/// number, never printed (`Debug` shows `<secret>`).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(transparent)]
pub struct Nonce(pub u64);

impl std::fmt::Debug for Nonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(SECRET)
    }
}

/// What must arrive exactly once, in order: numbered per directed link, kept by the
/// sender until acknowledged ([`super::PeerSession::queue`]).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Reliable {
    /// An entity crossing into a region of the other host, applied at `stamp`.
    Transfer {
        target: RegionCoords,
        stamp: Tick,
        bundle: Box<EntityBundle>,
        player: Option<PlayerHandoff>,
    },
    Intent(IntentMsg),
    /// A player without a body (parked, observing) moving to a region of the other host;
    /// `place` (Elastic 5): created at the target's landing, or restored at the record's pose
    /// (a player whose land moved under it).
    PlayerMove {
        handoff: PlayerHandoff,
        record: Box<PlayerRecord>,
        target: RegionCoords,
        place: MovePlace,
    },
    /// Admit `client` (of `player`) presenting `nonce` for `ttl_secs` from receipt (the
    /// receiving edge dates it by its own clock).
    ObserverOrder {
        client: ClientId,
        player: PlayerId,
        nonce: Nonce,
        ttl_secs: u64,
    },
    /// Spawn a login session's player; `origin` is the host the order entered at (the
    /// one the login service pushed it to), `hops` how often it was forwarded. `player` and
    /// `record_region` are what `origin` read from the store for the token.
    SpawnOrder {
        origin: crate::HostId,
        hops: u8,
        player_token: String,
        session_epoch: u64,
        player: PlayerId,
        record_region: Option<RegionCoords>,
        editor_allowed: bool,
    },
    /// The answer to a [`Reliable::SpawnOrder`], to its `origin`: `endpoint` is the host
    /// that took it (where the client connects), `None` when refused.
    SpawnAck {
        player_token: String,
        session_epoch: u64,
        ok: bool,
        endpoint: Option<HostEndpoint>,
    },
    /// The login service's kick, from the gate host it entered at: applied here, then answered
    /// with [`Reliable::KickAck`].
    Kick {
        player_token: String,
        session_epoch: u64,
        reason: String,
    },
    KickAck {
        player_token: String,
        session_epoch: u64,
    },
    /// A login session left the world on the sender. NO LONGER SENT (Elastic 1 Task 6: each
    /// host tells the login service up its own coordinator link, `HostMsg::Login`); kept
    /// because this enum is append-only on the wire.
    SessionEnded {
        player_token: String,
        session_epoch: u64,
    },
    /// The sender's in-band mark of a world save cut: what it sent before the mark
    /// belongs to the cut's receiver side, what follows to the next cut.
    CutMark {
        cut: CutId,
    },
    /// The sender parked `player` (of `client`), handed to it by the receiver, with no
    /// session of the client on the sender: a session the receiver still holds for the
    /// client (an observer) is ended.
    PlayerParked {
        client: ClientId,
        player: PlayerId,
    },
    /// A state message (`Transfer`, `PlayerMove`, `Intent`) the SENDER already accounts
    /// for in its part of world cut `cut` (in a region snapshot of the sender, or written
    /// as its own row / player record): the receiver never counts it in that cut. Never
    /// nested.
    InCut {
        cut: CutId,
        body: Box<Reliable>,
    },
    /// "These super-regions are mine at these epochs" (R4, `PgClaim`): sent to every peer
    /// when the sender claims (`WorldManager::claimed`) and, whole, on every hello. The
    /// receiver's directory takes each row unless it knows a newer epoch. Not a state
    /// message: a cut never accounts for it and it never travels as [`Reliable::InCut`] —
    /// the store's `super_regions` is the truth (every host reads it at boot and every
    /// `TERRAIN_POLL_SECS`); `Owns` only lets a peer learn a claim sooner.
    Owns {
        rows: Vec<(crate::SuperRegion, u64)>,
    },
    /// A survivor's report to the heir of succession `id` (R4, `world_manager/succession.rs`):
    /// its peer journal against the predecessor since `base_cut` (every transfer and player
    /// move exchanged with it, acknowledged or not — the journal records at queue time) and,
    /// in `owed`, the intents and replies it still owed the predecessor, in order and
    /// unwrapped (never a transfer or move: those are `Sent` entries). Not a state message:
    /// it never travels [`Reliable::InCut`] — no world cut runs while a succession is open
    /// and unanswered, and the heir's resolution (not a cut's mark) decides what it holds.
    SuccessionReport {
        id: crate::SuccessionId,
        base_cut: CutId,
        entries: Vec<super::JournalEntry>,
        owed: Vec<Reliable>,
    },
    /// "These super-regions were mine; they are free now, at these epochs" (Elastic 3, a
    /// committed release: `WorldManager::land_released`): sent to every peer when the
    /// sender's releasing cut commits and, with every row it released and no read showed
    /// owned since (`recently_released`), on every hello after [`Reliable::Owns`]. The
    /// receiver's directory marks each row free unless it knows a newer epoch. Not a state
    /// message, as [`Reliable::Owns`] is not: it names no entity, player or item, the store's
    /// `super_regions` is the truth (a directory read repairs a lost one), and applying it
    /// twice or late is harmless (`Directory::set` ignores an older epoch). Never
    /// [`Reliable::InCut`], never journalled.
    Released {
        rows: Vec<(crate::SuperRegion, u64)>,
    },
}

impl Reliable {
    /// The message itself, an [`Reliable::InCut`] unwrapped.
    pub fn inner(&self) -> &Reliable {
        match self {
            Reliable::InCut { body, .. } => body,
            other => other,
        }
    }

    /// A message whose content a save cut must account for exactly once: a transfer
    /// (with or without a player), a player move, an intent or a reply.
    pub fn is_state(&self) -> bool {
        matches!(
            self.inner(),
            Reliable::Transfer { .. } | Reliable::PlayerMove { .. } | Reliable::Intent(_)
        )
    }

    /// A copy with every player token shown as `<secret>` (as [`Nonce`] is): what a log
    /// prints of a message.
    pub fn redacted(&self) -> Reliable {
        let mut r = self.clone();
        r.redact();
        r
    }

    fn redact(&mut self) {
        match self {
            Reliable::SpawnOrder { player_token, .. }
            | Reliable::SpawnAck { player_token, .. }
            | Reliable::Kick { player_token, .. }
            | Reliable::KickAck { player_token, .. }
            | Reliable::SessionEnded { player_token, .. } => *player_token = SECRET.into(),
            Reliable::Transfer {
                player: Some(h), ..
            }
            | Reliable::PlayerMove { handoff: h, .. } => {
                if let Some((token, _)) = h.login.as_mut() {
                    *token = SECRET.into();
                }
            }
            Reliable::InCut { body, .. } => body.redact(),
            Reliable::SuccessionReport { entries, owed, .. } => {
                for e in entries {
                    if let super::JournalEvent::Sent(m) = &mut e.event {
                        m.redact();
                    }
                }
                owed.iter_mut().for_each(Reliable::redact);
            }
            _ => {}
        }
    }
}

/// What a redacted secret shows ([`Nonce`]'s `Debug`, [`Reliable::redacted`]).
const SECRET: &str = "<secret>";

/// What may be lost: sent only while the peer is reachable, never replayed.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Volatile {
    /// Every region the sender runs (after a reconnect).
    Running(Vec<RegionCoords>),
    RegionUp(RegionCoords),
    /// The sender's region was lost (its thread died, its load failed): every mirror of
    /// it ends (`HaloSourceGone`).
    RegionDown(RegionCoords),
    HaloRequest {
        from: RegionCoords,
        of: RegionCoords,
    },
    HaloUpdate {
        to: RegionCoords,
        update: HaloUpdate,
        stamp: Tick,
    },
    HaloEntity {
        to: RegionCoords,
        msg: HaloEntityMsg,
        stamp: Tick,
    },
    Input {
        client: ClientId,
        kind: GameEventKind,
        stamp: Tick,
    },
    /// The sender's region parked (its final halo already sent): no longer running, but
    /// not lost — unlike [`Volatile::RegionDown`] (a lost region: its mirrors end).
    RegionParked(RegionCoords),
    /// The sender needs a region of this free super-region and takes this host for its
    /// preferred claimer (R4, `PgClaim`). Never sent since Elastic 3 (the coordinator assigns
    /// land, `HostMsg::LandNeeded`); ignored on receipt. Kept: wire enums are append-only.
    ClaimWanted(crate::SuperRegion),
}

/// The largest frame a peer link carries: the service link's limit
/// (`svc_link::LINK_MAX_FRAME_BYTES`, which a server test pins equal). A bigger message is
/// dropped by the link, never delivered.
pub const PEER_FRAME_MAX_BYTES: usize = 64 * 1024 * 1024;

/// The bytes `body` takes on a peer link as one reliable frame (`encode_packet` of
/// `PeerMsg::Reliable`, compressed as the wire compresses it), or `None` if it does not
/// encode. Measured once per succession report, never per message.
pub fn reliable_frame_bytes(body: &Reliable) -> Option<usize> {
    crate::encode_packet(&PeerMsg::Reliable {
        seq: u64::MAX,
        body: body.clone(),
    })
    .ok()
    .map(|p| p.len())
}

/// One message on a peer link.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PeerMsg {
    Hello(PeerHello),
    Reliable {
        seq: u64,
        body: Reliable,
    },
    /// Cumulative: every reliable message up to `through` arrived.
    Ack {
        through: u64,
    },
    Volatile(Volatile),
}

impl PeerMsg {
    /// A copy with every player token shown as `<secret>` ([`Reliable::redacted`]).
    pub fn redacted(&self) -> PeerMsg {
        match self {
            PeerMsg::Reliable { seq, body } => PeerMsg::Reliable {
                seq: *seq,
                body: body.redacted(),
            },
            other => other.clone(),
        }
    }
}
