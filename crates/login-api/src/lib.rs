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
/// Shortest accepted password, in characters.
pub const PASSWORD_MIN_CHARS: usize = 10;
/// Longest accepted password, in characters.
pub const PASSWORD_MAX_CHARS: usize = 128;
/// Longest accepted email address, in characters (after trimming).
pub const EMAIL_MAX_CHARS: usize = 254;

pub const PATH_REGISTER: &str = "/v1/register";
pub const PATH_LOGIN: &str = "/v1/login";
pub const PATH_REFRESH: &str = "/v1/refresh";
pub const PATH_LOGOUT: &str = "/v1/logout";
pub const PATH_WORLDS: &str = "/v1/worlds";
pub const PATH_JOIN: &str = "/v1/join";
pub const PATH_VERIFY: &str = "/v1/verify";
pub const PATH_RESET_REQUEST: &str = "/v1/reset/request";
pub const PATH_RESET_CONFIRM: &str = "/v1/reset/confirm";
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
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginResponse {
    pub refresh_token: String,
    pub account_email: String,
    pub email_verified: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetRequest {
    pub email: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetConfirm {
    pub token: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyRequest {
    pub token: String,
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
    BadCredentials,
    NotVerified,
    Revoked,
    UnknownWorld,
    WorldClosed,
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&WorldStatus::Open).unwrap(),
            "\"open\""
        );
        assert_eq!(
            serde_json::to_string(&ApiErrorCode::BadCredentials).unwrap(),
            "\"bad_credentials\""
        );
        assert_eq!(
            serde_json::to_string(&ApiErrorCode::WorldClosed).unwrap(),
            "\"world_closed\""
        );
        let e: ApiErrorCode = serde_json::from_str("\"not_verified\"").unwrap();
        assert_eq!(e, ApiErrorCode::NotVerified);
    }
}
