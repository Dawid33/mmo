//! The identity provider's device flow (OAuth 2.0 Device Authorization Grant,
//! RFC 8628, with PKCE, RFC 7636) as pure data and parsing: no IO, so it tests
//! without a network. [`super::api::LoginApi`] sends the requests,
//! [`super::flow`] drives the polling.
//!
//! The device code, the access token and the PKCE verifier are secrets: none
//! of the types here print them (no derived `Debug` over them).

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
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
    format!("{}/protocol/openid-connect/auth/device", issuer.trim_end_matches('/'))
}

/// The provider's token endpoint under its issuer URL.
pub fn token_endpoint(issuer: &str) -> String {
    format!("{}/protocol/openid-connect/token", issuer.trim_end_matches('/'))
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
    let starts = |scheme: &str| url.get(..scheme.len()).is_some_and(|head| head.eq_ignore_ascii_case(scheme));
    starts("https://") || starts("http://")
}

/// The device authorization endpoint's success body.
pub fn parse_device_start(body: &str) -> Result<DeviceStart, String> {
    let mut start: DeviceStart =
        serde_json::from_str(body).map_err(|e| format!("bad device authorization answer: {e}"))?;
    if start.device_code.is_empty() || start.user_code.is_empty() || start.verification_uri.is_empty() {
        return Err("device authorization answer without its codes".into());
    }
    if start.verification_uri_complete.is_empty() {
        start.verification_uri_complete = start.verification_uri.clone();
    }
    // The pages are opened in the player's browser: only web pages.
    for (field, url) in [
        ("verification_uri", &start.verification_uri),
        ("verification_uri_complete", &start.verification_uri_complete),
    ] {
        if !is_web_url(url) {
            return Err(format!("device authorization answer with a non-http(s) {field}"));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Keycloak 26's device authorization answer, field for field.
    const KEYCLOAK_START: &str = r#"{
        "device_code": "2c3b6a0e-device",
        "user_code": "WDJB-MJHT",
        "verification_uri": "https://auth.example.com/realms/mmo/device",
        "verification_uri_complete": "https://auth.example.com/realms/mmo/device?user_code=WDJB-MJHT",
        "expires_in": 600,
        "interval": 5
    }"#;

    #[test]
    fn parses_device_start() {
        let start = parse_device_start(KEYCLOAK_START).unwrap();
        assert_eq!(start.device_code, "2c3b6a0e-device");
        assert_eq!(start.user_code, "WDJB-MJHT");
        assert_eq!(start.verification_uri, "https://auth.example.com/realms/mmo/device");
        assert_eq!(start.verification_uri_complete, "https://auth.example.com/realms/mmo/device?user_code=WDJB-MJHT");
        assert_eq!(start.expires_in, 600);
        assert_eq!(start.interval, 5);
        assert!(parse_device_start("<html>").is_err());
        assert!(parse_device_start(r#"{"device_code":"","user_code":"","verification_uri":"","expires_in":1}"#).is_err());
    }

    #[test]
    fn device_start_refuses_a_non_http_verification_url() {
        let body = |plain: &str, complete: &str| {
            format!(
                r#"{{"device_code":"d","user_code":"U","verification_uri":"{plain}","verification_uri_complete":"{complete}","expires_in":600}}"#
            )
        };
        for bad in ["javascript:alert(1)", "file:///etc/passwd", "/relative", "ftp://x"] {
            let err = parse_device_start(&body(bad, "")).err().unwrap();
            assert!(err.contains("verification_uri") && !err.contains(bad), "{err}");
            assert!(parse_device_start(&body("https://a/device", bad)).is_err(), "{bad}");
        }
        assert!(parse_device_start(&body("https://a/device", "javascript:alert(1)")).err().unwrap().contains("_complete"));
        assert!(parse_device_start(&body("HTTP://127.0.0.1:8180/device", "http://127.0.0.1:8180/device?user_code=U")).is_ok());
    }

    #[test]
    fn interval_defaults_to_five() {
        let body = r#"{"device_code":"d","user_code":"U","verification_uri":"https://a/device","expires_in":600}"#;
        let start = parse_device_start(body).unwrap();
        assert_eq!(start.interval, 5);
        assert_eq!(start.verification_uri_complete, "https://a/device", "no complete link: the plain page");
    }

    #[test]
    fn poll_pending() {
        assert_eq!(parse_device_poll(400, r#"{"error":"authorization_pending"}"#), DevicePoll::Pending);
        assert_eq!(next_interval(5, &DevicePoll::Pending), 5);
    }

    #[test]
    fn poll_slow_down_adds_five_seconds() {
        let poll = parse_device_poll(400, r#"{"error":"slow_down","error_description":"Slow down"}"#);
        assert_eq!(poll, DevicePoll::SlowDown);
        assert_eq!(next_interval(5, &DevicePoll::SlowDown), 10);
    }

    #[test]
    fn poll_token() {
        let poll = parse_device_poll(200, r#"{"access_token":"eyJ.a.b","expires_in":300,"token_type":"Bearer"}"#);
        assert_eq!(poll, DevicePoll::Token("eyJ.a.b".into()));
        assert!(!format!("{poll:?}").contains("eyJ"), "Debug hides the token");
        assert!(matches!(parse_device_poll(200, r#"{"token_type":"Bearer"}"#), DevicePoll::Failed(_)));
    }

    #[test]
    fn poll_expired() {
        assert_eq!(parse_device_poll(400, r#"{"error":"expired_token"}"#), DevicePoll::Expired);
    }

    #[test]
    fn poll_denied() {
        assert_eq!(parse_device_poll(400, r#"{"error":"access_denied"}"#), DevicePoll::Denied);
    }

    #[test]
    fn poll_garbage_is_failed() {
        assert_eq!(parse_device_poll(502, "<html>bad gateway</html>"), DevicePoll::Failed("HTTP 502".into()));
        assert_eq!(parse_device_poll(400, r#"{"error":"invalid_grant"}"#), DevicePoll::Failed("invalid_grant".into()));
        assert_eq!(next_interval(5, &DevicePoll::Failed("x".into())), 5);
    }

    /// RFC 7636 Appendix B's verifier and S256 challenge (confirmed with
    /// `printf '%s' <verifier> | openssl dgst -sha256 -binary | basenc --base64url | tr -d '='`).
    #[test]
    fn pkce_challenge_matches_rfc7636() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn new_verifier_is_well_formed() {
        let verifier = new_pkce_verifier();
        assert!((43..=128).contains(&verifier.len()), "{} characters", verifier.len());
        assert!(verifier.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b)));
        assert_ne!(verifier, new_pkce_verifier(), "every sign-in gets its own");
    }

    #[test]
    fn endpoints_sit_under_the_issuer() {
        assert_eq!(device_endpoint("https://a/realms/mmo/"), "https://a/realms/mmo/protocol/openid-connect/auth/device");
        assert_eq!(token_endpoint("https://a/realms/mmo"), "https://a/realms/mmo/protocol/openid-connect/token");
    }
}
