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
}

/// How long `/v1/join` waits for the world host's [`HostToLogin::SpawnAck`].
pub const SPAWN_ACK_TIMEOUT_MS: u64 = 3000;
/// How long a host keeps a spawn order it has not seen a matching `Hello` for.
pub const SPAWN_PENDING_TTL_SECS: u64 = 120;

/// The login service to a world host, over the service link (`svc-link`, role `Login`). Its
/// `Debug` hides `player_token` ([`REDACTED`]): no token reaches a log.
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

/// A world host to the login service, over the service link. Its `Debug` hides
/// `player_token` ([`REDACTED`]).
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
    /// The session admitted with this epoch's ticket disconnected (best effort: lost while the
    /// link is down).
    SessionEnded { player_token: String, session_epoch: u64 },
    /// The host applied the [`LoginToHost::Kick`] up to this epoch (sent for every copy).
    KickAck { player_token: String, session_epoch: u64 },
    /// A coordinator replica's answer to an order (spawn or kick) when no host attached to IT
    /// takes logins (Elastic 2b): it relayed nothing — the login service tries its next link
    /// (another replica). Matched by token and epoch, as the acks are.
    NoGate { player_token: String, session_epoch: u64 },
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The link messages' `Debug` (what any log of them shows) never carries the token.
    #[test]
    fn link_messages_debug_hides_the_token() {
        let t = "tok-secret-0123456789".to_string();
        let shown = [
            format!("{:?}", LoginToHost::SpawnPlayer { player_token: t.clone(), session_epoch: 1 }),
            format!("{:?}", LoginToHost::Kick { player_token: t.clone(), session_epoch: 2, reason: "r".into() }),
            format!("{:?}", HostToLogin::SpawnAck { player_token: t.clone(), session_epoch: 3, ok: true, quic: None, webtransport: None, cert_hash: None }),
            format!("{:?}", HostToLogin::SessionEnded { player_token: t.clone(), session_epoch: 4 }),
            format!("{:?}", HostToLogin::KickAck { player_token: t.clone(), session_epoch: 5 }),
            format!("{:?}", HostToLogin::NoGate { player_token: t.clone(), session_epoch: 6 }),
        ];
        for s in shown {
            assert!(!s.contains(&t) && s.contains(REDACTED) && s.contains("session_epoch"), "{s}");
        }
    }

    #[test]
    fn enums_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&WorldStatus::Open).unwrap(),
            "\"open\""
        );
        assert_eq!(
            serde_json::to_string(&ApiErrorCode::UnknownWorld).unwrap(),
            "\"unknown_world\""
        );
        assert_eq!(
            serde_json::to_string(&ApiErrorCode::WorldClosed).unwrap(),
            "\"world_closed\""
        );
        let e: ApiErrorCode = serde_json::from_str("\"world_unavailable\"").unwrap();
        assert_eq!(e, ApiErrorCode::WorldUnavailable);
    }

    #[test]
    fn oidc_types_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&ApiErrorCode::IdentityConflict).unwrap(),
            "\"identity_conflict\""
        );
        let r: LoginResponse = serde_json::from_str(
            r#"{"refresh_token":"r","account_email":"a@b.c","email_verified":true}"#,
        )
        .unwrap();
        assert_eq!(r.account_name, "");
    }
}
