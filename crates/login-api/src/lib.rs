//! The login service's HTTP API: JSON request / response bodies, the join JWT's claims and the
//! constants both ends share. Serde only, so the browser build can use it (`wasm32-unknown-unknown`).
//!
//! The service signs a [`JoinClaims`] JWT (crate `auth`); a game host checks it at connect.

use serde::{Deserialize, Serialize};

/// How long a join JWT is valid, from `iat` to `exp`.
pub const JOIN_JWT_TTL_SECS: u64 = 300;
/// The `aud` claim every game host expects.
pub const HOST_AUDIENCE: &str = "mmo-host";
/// How long a refresh token lives.
pub const REFRESH_TTL_DAYS: u64 = 30;
/// Longest accepted email address, in characters (after trimming).
pub const EMAIL_MAX_CHARS: usize = 254;

pub const PATH_REFRESH: &str = "/v1/refresh";
pub const PATH_LOGOUT: &str = "/v1/logout";
pub const PATH_WORLDS: &str = "/v1/worlds";
pub const PATH_JOIN: &str = "/v1/join";
pub const PATH_LOGIN_OIDC: &str = "/v1/login/oidc";
pub const PATH_CONFIG: &str = "/v1/config";
pub const PATH_JWKS: &str = "/.well-known/jwks.json";
/// The coordinator's HTTP API (not the login service's own): `POST` a [`LoginToHost`] order.
pub const PATH_COORD_LOGIN_ORDERS: &str = "/v1/login/orders";
/// The coordinator's HTTP API: `GET` the [`LoginPage`] of answers after a cursor
/// (`?after=<seq>&wait_ms=<ms>`, long-polled).
pub const PATH_COORD_LOGIN_ANSWERS: &str = "/v1/login/answers";
/// The coordinator's HTTP API: the request header naming the caller's world id (412 `world` on a
/// mismatch). Every request also carries `Authorization: Bearer <service token>`.
pub const HEADER_COORD_WORLD: &str = "x-world";
/// The coordinator's HTTP API: the request header carrying the link protocol version (412
/// `version` on a mismatch).
pub const HEADER_COORD_LINK_VERSION: &str = "x-link-version";
/// The 503 body of an order no host takes (`POST` [`PATH_COORD_LOGIN_ORDERS`]).
pub const COORD_REFUSED_NO_GATE: &str = "no_gate";
/// The 503 body of a spawn order while the world starts a host.
pub const COORD_REFUSED_STARTING: &str = "world_starting";

/// The claims of a join JWT (`kid` rides in the header).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinClaims {
    /// The account id, as a decimal string.
    pub sub: String,
    /// The registry id of the world this ticket admits to.
    pub world: String,
    /// The account's player token in that world (hex; not a secret).
    pub player_token: String,
    /// Bumped on every join; a host refuses one older than the newest it has seen.
    pub session_epoch: u64,
    /// Always [`HOST_AUDIENCE`].
    pub aud: String,
    /// Issued at, seconds since the Unix epoch.
    pub iat: u64,
    /// Expires at, seconds since the Unix epoch.
    pub exp: u64,
}

/// What a client needs to connect to a world: the JWT and the host's addresses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinTicket {
    pub world: String,
    pub jwt: String,
    /// The native QUIC address, `host:port`.
    pub quic: Option<String>,
    /// The browser's WebTransport URL.
    pub webtransport: Option<String>,
    /// A dev host's self-signed WebTransport certificate hash.
    pub cert_hash: Option<String>,
    pub expires_in_secs: u64,
    /// The world map's tile route base (as [`WorldEntry::map_url`]).
    #[serde(default)]
    pub map_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginResponse {
    pub refresh_token: String,
    pub account_email: String,
    pub email_verified: bool,
    /// The account's display name (from the identity provider); empty when unknown.
    #[serde(default)]
    pub account_name: String,
}

/// `POST /v1/login/oidc`: logs in with an access token the identity provider issued.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OidcLoginRequest {
    pub access_token: String,
}

/// `GET /v1/config`: where a client signs in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginConfigResponse {
    pub oidc_issuer: String,
    pub oidc_client_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// `POST /v1/logout`: revokes `refresh_token`'s family; with `all`, every session of its account
/// (all its refresh tokens, and a `Kick` to every world host it has a session in).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogoutRequest {
    pub refresh_token: String,
    #[serde(default)]
    pub all: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub refresh_token: String,
    pub world: String,
}

/// A join also rotates the refresh token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinResponse {
    pub ticket: JoinTicket,
    pub refresh_token: String,
}

/// One world of the registry, as the world list shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldEntry {
    pub id: String,
    pub name: String,
    pub status: WorldStatus,
    /// The world map's tile route base (`<url>/v1/map/tiles/{level}/{x}/{z}`), if it serves one.
    #[serde(default)]
    pub map_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldStatus {
    Open,
    Closed,
}

/// The body of every non-2xx answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ApiErrorCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    BadRequest,
    Revoked,
    UnknownWorld,
    WorldClosed,
    /// The world's host did not take the spawn order (its link is down or it did not answer in
    /// time): try again later. HTTP 503.
    WorldUnavailable,
    /// The identity provider's token was bad, foreign or its email unverified.
    IdentityRefused,
    /// The identity cannot be tied to one account (it clashes with an existing one).
    IdentityConflict,
    Internal,
    /// No host of the world takes logins and the world is starting one (Elastic 4: the world
    /// went to zero hosts, or every host drains): the client repeats the join every second for
    /// a bounded time. HTTP 503. On the wire as the string `world_starting` (snake_case JSON).
    WorldStarting,
}

/// The message of a 503 [`ApiErrorCode::WorldStarting`].
pub const WORLD_STARTING_MESSAGE: &str = "the world is starting; this can take a minute";

/// How long `/v1/join` waits for the world host's [`HostToLogin::SpawnAck`].
pub const SPAWN_ACK_TIMEOUT_MS: u64 = 3000;
/// How long a host keeps a spawn order it has not seen a matching `Hello` for.
pub const SPAWN_PENDING_TTL_SECS: u64 = 120;

/// The login service to a world host: the body of `POST` [`PATH_COORD_LOGIN_ORDERS`] (the
/// coordinator relays it to the world's gate host). Its `Debug` hides `player_token`
/// ([`REDACTED`]): no token reaches a log.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoginToHost {
    /// A join was granted: the host admits exactly this epoch's ticket for the player (and
    /// parks any session of the player it has, newest-wins), then acks.
    SpawnPlayer { player_token: String, session_epoch: u64 },
    /// Revocation (logout everywhere, password reset): every session and spawn order of the
    /// player up to `session_epoch` (the newest join when it was revoked) is revoked — the host
    /// parks such a session, refusing it with `reason`, and drops such an order; a newer login
    /// is untouched. Durable: resent until [`HostToLogin::KickAck`]; applying it twice is the
    /// same as once.
    Kick { player_token: String, session_epoch: u64, reason: String },
}

/// A world host to the login service: a row of the coordinator's `login_outbox`, read with `GET`
/// [`PATH_COORD_LOGIN_ANSWERS`]. Its `Debug` hides `player_token` ([`REDACTED`]).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostToLogin {
    /// The answer to one [`LoginToHost::SpawnPlayer`] (matched by token and epoch). With
    /// several hosts the order may be taken by another host than the one the login service
    /// is linked to: `quic` / `webtransport` / `cert_hash` are then where the client
    /// connects (that host's addresses); all `None` = the world row's.
    SpawnAck {
        player_token: String,
        session_epoch: u64,
        ok: bool,
        quic: Option<String>,
        webtransport: Option<String>,
        cert_hash: Option<String>,
    },
    /// The session admitted with this epoch's ticket disconnected (durable in `login_outbox`
    /// until pruned).
    SessionEnded { player_token: String, session_epoch: u64 },
    /// The host applied the [`LoginToHost::Kick`] up to this epoch (sent for every copy).
    KickAck { player_token: String, session_epoch: u64 },
    /// Unsent since link protocol 12 (append-only enum): an order no host takes is answered 503
    /// [`COORD_REFUSED_NO_GATE`] by the coordinator's API.
    NoGate { player_token: String, session_epoch: u64 },
    /// Unsent since link protocol 12 (append-only enum): a spawn order while the world starts a
    /// host is answered 503 [`COORD_REFUSED_STARTING`] by the coordinator's API.
    Starting { player_token: String, session_epoch: u64 },
}

/// One page of the coordinator's login answers (`GET` [`PATH_COORD_LOGIN_ANSWERS`]): `(seq,
/// answer)` in commit order. Bodies are `game::encode_packet` of this type; the `login_outbox`
/// rows behind it hold `game::encode_packet` of [`HostToLogin`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoginPage {
    pub rows: Vec<(u64, HostToLogin)>,
}

/// What the link messages' `Debug` shows instead of a player token.
pub const REDACTED: &str = "<redacted>";

impl std::fmt::Debug for LoginToHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hidden = format_args!("{REDACTED}");
        match self {
            LoginToHost::SpawnPlayer { player_token: _, session_epoch } => f
                .debug_struct("SpawnPlayer")
                .field("player_token", &hidden)
                .field("session_epoch", session_epoch)
                .finish(),
            LoginToHost::Kick { player_token: _, session_epoch, reason } => f
                .debug_struct("Kick")
                .field("player_token", &hidden)
                .field("session_epoch", session_epoch)
                .field("reason", reason)
                .finish(),
        }
    }
}

impl std::fmt::Debug for HostToLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hidden = format_args!("{REDACTED}");
        match self {
            HostToLogin::SpawnAck { player_token: _, session_epoch, ok, quic, webtransport, cert_hash } => f
                .debug_struct("SpawnAck")
                .field("player_token", &hidden)
                .field("session_epoch", session_epoch)
                .field("ok", ok)
                .field("quic", quic)
                .field("webtransport", webtransport)
                .field("cert_hash", cert_hash)
                .finish(),
            HostToLogin::SessionEnded { player_token: _, session_epoch } => f
                .debug_struct("SessionEnded")
                .field("player_token", &hidden)
                .field("session_epoch", session_epoch)
                .finish(),
            HostToLogin::KickAck { player_token: _, session_epoch } => f
                .debug_struct("KickAck")
                .field("player_token", &hidden)
                .field("session_epoch", session_epoch)
                .finish(),
            HostToLogin::NoGate { player_token: _, session_epoch } => f
                .debug_struct("NoGate")
                .field("player_token", &hidden)
                .field("session_epoch", session_epoch)
                .finish(),
            HostToLogin::Starting { player_token: _, session_epoch } => f
                .debug_struct("Starting")
                .field("player_token", &hidden)
                .field("session_epoch", session_epoch)
                .finish(),
        }
    }
}

