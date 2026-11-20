//! The identity provider's device flow (OAuth 2.0 Device Authorization Grant,
//! RFC 8628, with PKCE, RFC 7636) as pure data and parsing: no IO, so it tests
//! without a network. [`super::api::LoginApi`] sends the requests,
//! [`super::flow`] drives the polling.
//!
//! The device code, the access token and the PKCE verifier are secrets: none
//! of the types here print them (no derived `Debug` over them).

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The OAuth scope the game asks for.
pub const SCOPE: &str = "openid profile email";
/// The device flow's token grant type (RFC 8628 §3.4).
pub const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// The poll interval when the provider names none (RFC 8628 §3.2).
pub const DEFAULT_INTERVAL_SECS: u64 = 5;
/// What a `slow_down` answer adds to the poll interval (RFC 8628 §3.5).
pub const SLOW_DOWN_SECS: u64 = 5;
/// Random bytes in a PKCE verifier: 32 bytes are 43 base64url characters.
const VERIFIER_BYTES: usize = 32;

/// The provider's device authorization endpoint under its issuer URL.
pub fn device_endpoint(issuer: &str) -> String {
    format!(
        "{}/protocol/openid-connect/auth/device",
        issuer.trim_end_matches('/')
    )
}

/// The provider's token endpoint under its issuer URL.
pub fn token_endpoint(issuer: &str) -> String {
    format!(
        "{}/protocol/openid-connect/token",
        issuer.trim_end_matches('/')
    )
}

/// The device authorization answer: the code the player types (or the link
/// that carries it) and the code the game polls with. Holds the device code:
/// no `Debug`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// The page with the user code filled in; the plain
    /// [`Self::verification_uri`] when the provider sends none.
    #[serde(default)]
    pub verification_uri_complete: String,
    /// Seconds the device code stays valid.
    pub expires_in: u64,
    /// Least seconds between two polls.
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    DEFAULT_INTERVAL_SECS
}

/// One answer of the token endpoint while the player signs in.
#[derive(Clone, PartialEq, Eq)]
pub enum DevicePoll {
    /// Not confirmed yet: poll again after the interval.
    Pending,
    /// Polling too fast: poll again after a longer interval ([`next_interval`]).
    SlowDown,
    /// Signed in: the provider's access token (a secret).
    Token(String),
    /// The device code ran out before the player confirmed.
    Expired,
    /// The player refused the sign-in.
    Denied,
    /// Any other answer (its OAuth error code, or the HTTP status).
    Failed(String),
}

/// Debug without the access token.
impl fmt::Debug for DevicePoll {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DevicePoll::Pending => f.write_str("Pending"),
            DevicePoll::SlowDown => f.write_str("SlowDown"),
            DevicePoll::Token(_) => f.write_str("Token(..)"),
            DevicePoll::Expired => f.write_str("Expired"),
            DevicePoll::Denied => f.write_str("Denied"),
            DevicePoll::Failed(why) => write!(f, "Failed({why})"),
        }
    }
}

/// `http://` or `https://` (the scheme in any case).
fn is_web_url(url: &str) -> bool {
    let starts = |scheme: &str| {
        url.get(..scheme.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
    };
    starts("https://") || starts("http://")
}

/// The device authorization endpoint's success body.
pub fn parse_device_start(body: &str) -> Result<DeviceStart, String> {
    let mut start: DeviceStart =
        serde_json::from_str(body).map_err(|e| format!("bad device authorization answer: {e}"))?;
    if start.device_code.is_empty()
        || start.user_code.is_empty()
        || start.verification_uri.is_empty()
    {
        return Err("device authorization answer without its codes".into());
    }
    if start.verification_uri_complete.is_empty() {
        start.verification_uri_complete = start.verification_uri.clone();
    }
    // The pages are opened in the player's browser: only web pages.
    for (field, url) in [
        ("verification_uri", &start.verification_uri),
        (
            "verification_uri_complete",
            &start.verification_uri_complete,
        ),
    ] {
        if !is_web_url(url) {
            return Err(format!(
                "device authorization answer with a non-http(s) {field}"
            ));
        }
    }
    Ok(start)
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

/// The token endpoint's answer to one device poll (`status`: the HTTP status).
pub fn parse_device_poll(status: u16, body: &str) -> DevicePoll {
    if status == 200 {
        return match serde_json::from_str::<TokenBody>(body) {
            Ok(token) if !token.access_token.is_empty() => DevicePoll::Token(token.access_token),
            _ => DevicePoll::Failed("token answer without an access token".into()),
        };
    }
    match serde_json::from_str::<ErrorBody>(body) {
        Ok(e) => match e.error.as_str() {
            "authorization_pending" => DevicePoll::Pending,
            "slow_down" => DevicePoll::SlowDown,
            "expired_token" => DevicePoll::Expired,
            "access_denied" => DevicePoll::Denied,
            other => DevicePoll::Failed(other.to_string()),
        },
        Err(_) => DevicePoll::Failed(format!("HTTP {status}")),
    }
}

/// Whether a device poll's HTTP `status` says nothing about the sign-in itself
/// and the poll should simply be repeated (whatever the body): a server error
/// (5xx), or a request timeout / rate limit (408 / 429) that a proxy in front
/// of the provider may answer.
pub fn poll_status_is_transient(status: u16) -> bool {
    status >= 500 || status == 408 || status == 429
}

/// The poll interval after `poll`: [`SLOW_DOWN_SECS`] longer on `slow_down`.
pub fn next_interval(current: u64, poll: &DevicePoll) -> u64 {
    match poll {
        DevicePoll::SlowDown => current.saturating_add(SLOW_DOWN_SECS),
        _ => current,
    }
}

/// The PKCE S256 challenge of `verifier`: base64url (no padding) of the
/// SHA-256 of its ASCII bytes (RFC 7636 §4.2).
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A fresh PKCE verifier: 32 bytes from the operating system's (the browser's
/// Web Crypto) secure source, base64url without padding — 43 characters of
/// `[A-Za-z0-9-_]` (RFC 7636 §4.1).
pub fn new_pkce_verifier() -> String {
    let mut bytes = [0u8; VERIFIER_BYTES];
    getrandom::getrandom(&mut bytes).expect("no secure random source for the sign-in");
    URL_SAFE_NO_PAD.encode(bytes)
}
