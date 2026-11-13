//! The login service's HTTP API as the client calls it. [`LoginApi`] is a
//! plain `reqwest::Client` wrapper: it spawns nothing and needs no particular
//! runtime flavour (the login screen drives it from a current-thread tokio
//! runtime on native, `spawn_local` in the browser, where reqwest uses
//! `fetch`).
//!
//! It also calls the identity provider's device flow endpoints
//! ([`LoginApi::device_start`], [`LoginApi::device_poll`]; parsing in
//! [`super::oidc`]).
//!
//! The DTOs print refresh tokens, access tokens and JWTs with `{:?}`: never
//! log them; [`LoginFailure`] holds none of them.

use std::fmt;

use login_api::{
    ApiError, ApiErrorCode, JoinRequest, JoinResponse, LoginConfigResponse, LoginResponse, OidcLoginRequest,
    RefreshRequest, WorldEntry, PATH_CONFIG, PATH_JOIN, PATH_LOGIN_OIDC, PATH_LOGOUT, PATH_REFRESH, PATH_WORLDS,
};
use serde::de::DeserializeOwned;

use super::oidc::{self, DevicePoll, DeviceStart};

/// Overrides the login service's address on native (empty = unset); the
/// build-time `VOXEL_LOGIN_URL` is the fallback.
pub const LOGIN_URL_ENV: &str = "VOXEL_LOGIN_URL";
/// The browser's query parameter (`?login=<url>`) that overrides it.
pub const LOGIN_URL_QUERY: &str = "login";
/// Longest a login request may take before it fails as a network error
/// (native: reqwest's own timeout).
#[cfg(not(target_arch = "wasm32"))]
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// The browser's equivalent of [`REQUEST_TIMEOUT`]: reqwest's fetch backend
/// has no timeout, so each call races a `setTimeout` of this many seconds.
#[cfg(target_arch = "wasm32")]
pub const WASM_REQUEST_TIMEOUT_SECS: i32 = 15;

/// Why a login call failed. `Display` is the player-facing line and never
/// contains a credential.
#[derive(Clone, PartialEq, Eq)]
pub enum LoginFailure {
    /// The service could not be reached (or the answer could not be read).
    Network(String),
    /// The service answered with an error.
    Api(ApiError),
    /// The answer was neither a success nor an `ApiError` (the HTTP status).
    Protocol(String),
    /// The login service has no identity provider (`GET /v1/config` is 404).
    NoSignIn,
    /// The identity provider would not start a sign-in (its OAuth error code,
    /// or the HTTP status).
    Provider(String),
}

/// The line when the login service has no identity provider.
pub const NO_SIGN_IN: &str = "This server has no sign-in configured.";

impl fmt::Display for LoginFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoginFailure::Network(_) => f.write_str("Can't reach the login server"),
            LoginFailure::Api(e) => match e.code {
                ApiErrorCode::Revoked => f.write_str("Your login expired \u{2014} please log in again"),
                ApiErrorCode::UnknownWorld => f.write_str("That world does not exist"),
                ApiErrorCode::WorldClosed => f.write_str("That world is closed"),
                ApiErrorCode::WorldUnavailable => {
                    f.write_str("That world's server is not reachable right now \u{2014} try again shortly")
                }
                ApiErrorCode::IdentityRefused => f.write_str("Sign-in was not accepted. Try again."),
                ApiErrorCode::IdentityConflict => f.write_str("That email already belongs to another game account."),
                ApiErrorCode::BadRequest => f.write_str(&e.message),
                ApiErrorCode::Internal => f.write_str("The login server had a problem \u{2014} try again later"),
            },
            LoginFailure::Protocol(_) => f.write_str("The login server sent an answer we do not understand"),
            LoginFailure::NoSignIn => f.write_str(NO_SIGN_IN),
            LoginFailure::Provider(_) => f.write_str("Can't start the sign-in \u{2014} try again later"),
        }
    }
}

/// Debug shows the failure's kind and the server's code, no free text from a
/// request.
impl fmt::Debug for LoginFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoginFailure::Network(why) => write!(f, "Network({why})"),
            LoginFailure::Api(e) => write!(f, "Api({:?})", e.code),
            LoginFailure::Protocol(status) => write!(f, "Protocol({status})"),
            LoginFailure::NoSignIn => f.write_str("NoSignIn"),
            LoginFailure::Provider(why) => write!(f, "Provider({why})"),
        }
    }
}

impl std::error::Error for LoginFailure {}

/// One answer, as a pure function of the status and body: a success decodes
/// as `T` (use `()` for the bodiless 202/204), anything else is an
/// [`ApiError`] body, else [`LoginFailure::Protocol`].
pub fn parse_response<T: DeserializeOwned>(status: u16, body: &[u8]) -> Result<T, LoginFailure> {
    if (200..300).contains(&status) {
        // `()` has no body to read.
        if let Ok(unit) = serde_json::from_value::<T>(serde_json::Value::Null) {
            return Ok(unit);
        }
        return serde_json::from_slice(body).map_err(|e| LoginFailure::Protocol(format!("{status} (bad body: {e})")));
    }
    match serde_json::from_slice::<ApiError>(body) {
        Ok(error) => Err(LoginFailure::Api(error)),
        Err(_) => Err(LoginFailure::Protocol(status.to_string())),
    }
}

/// The login service at `base` (no trailing `/`).
#[derive(Clone)]
pub struct LoginApi {
    base: String,
    http: reqwest::Client,
}

impl LoginApi {
    pub fn new(base: impl Into<String>) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        #[cfg(not(target_arch = "wasm32"))]
        let http = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap_or_default();
        #[cfg(target_arch = "wasm32")]
        let http = reqwest::Client::new();
        Self { base, http }
    }

    /// Where this service's players sign in; [`LoginFailure::NoSignIn`] when
    /// it has no identity provider.
    pub async fn config(&self) -> Result<LoginConfigResponse, LoginFailure> {
        let (status, body) = self.exchange(self.http.get(format!("{}{PATH_CONFIG}", self.base))).await?;
        if status == 404 {
            return Err(LoginFailure::NoSignIn);
        }
        parse_response(status, &body)
    }

    /// Starts a device sign-in at the identity provider `issuer` for
    /// `client_id`, with the PKCE S256 `challenge` of the verifier the polls
    /// will present.
    pub async fn device_start(&self, issuer: &str, client_id: &str, challenge: &str) -> Result<DeviceStart, LoginFailure> {
        let form = [
            ("client_id", client_id),
            ("scope", oidc::SCOPE),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ];
        let (status, body) = self.exchange(self.http.post(oidc::device_endpoint(issuer)).form(&form)).await?;
        let body = String::from_utf8_lossy(&body);
        if status != 200 {
            return Err(LoginFailure::Provider(match oidc::parse_device_poll(status, &body) {
                DevicePoll::Failed(why) => why,
                _ => format!("HTTP {status}"),
            }));
        }
        oidc::parse_device_start(&body).map_err(LoginFailure::Provider)
    }

    /// One poll of a device sign-in (`verifier`: the PKCE verifier its start
    /// sent the challenge of). A transport error or a provider-side failure
    /// (5xx) is an `Err`: the sign-in goes on and polls again.
    pub async fn device_poll(
        &self,
        issuer: &str,
        client_id: &str,
        device_code: &str,
        verifier: &str,
    ) -> Result<DevicePoll, LoginFailure> {
        let form = [
            ("grant_type", oidc::DEVICE_GRANT_TYPE),
            ("client_id", client_id),
            ("device_code", device_code),
            ("code_verifier", verifier),
        ];
        let (status, body) = self.exchange(self.http.post(oidc::token_endpoint(issuer)).form(&form)).await?;
        if status >= 500 {
            return Err(LoginFailure::Protocol(status.to_string()));
        }
        Ok(oidc::parse_device_poll(status, &String::from_utf8_lossy(&body)))
    }

    /// Logs in with the identity provider's access token.
    pub async fn login_oidc(&self, access_token: &str) -> Result<LoginResponse, LoginFailure> {
        self.post(PATH_LOGIN_OIDC, &OidcLoginRequest { access_token: access_token.into() }).await
    }

    pub async fn refresh(&self, token: &str) -> Result<LoginResponse, LoginFailure> {
        self.post(PATH_REFRESH, &RefreshRequest { refresh_token: token.into() }).await
    }

    pub async fn logout(&self, token: &str) -> Result<(), LoginFailure> {
        self.post(PATH_LOGOUT, &RefreshRequest { refresh_token: token.into() }).await
    }

    pub async fn worlds(&self) -> Result<Vec<WorldEntry>, LoginFailure> {
        let request = self.http.get(format!("{}{PATH_WORLDS}", self.base));
        self.answer(request).await
    }

    /// A join ticket for `world`; the answer also rotates the refresh token.
    pub async fn join(&self, token: &str, world: &str) -> Result<JoinResponse, LoginFailure> {
        self.post(PATH_JOIN, &JoinRequest { refresh_token: token.into(), world: world.into() }).await
    }

    async fn post<B: serde::Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, LoginFailure> {
        self.answer(self.http.post(format!("{}{path}", self.base)).json(body)).await
    }

    async fn answer<T: DeserializeOwned>(&self, request: reqwest::RequestBuilder) -> Result<T, LoginFailure> {
        let (status, body) = self.exchange(request).await?;
        parse_response(status, &body)
    }

    /// Sends `request`: the answer's status and body.
    async fn exchange(&self, request: reqwest::RequestBuilder) -> Result<(u16, Vec<u8>), LoginFailure> {
        let call = async {
            let response = request.send().await.map_err(network)?;
            let status = response.status().as_u16();
            let body = response.bytes().await.map_err(network)?;
            Ok((status, body.to_vec()))
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            call.await
        }
        #[cfg(target_arch = "wasm32")]
        {
            with_timeout(call, WASM_REQUEST_TIMEOUT_SECS).await
        }
    }
}

/// `call`, or [`LoginFailure::Network`] once `secs` have passed (the
/// browser's fetch never times out by itself; dropping the call abandons it).
#[cfg(target_arch = "wasm32")]
async fn with_timeout<T>(
    call: impl std::future::Future<Output = Result<T, LoginFailure>>,
    secs: i32,
) -> Result<T, LoginFailure> {
    use std::future::Future;
    use std::task::Poll;
    let mut call = std::pin::pin!(call);
    let mut timer = std::pin::pin!(crate::web_worker::sleep_ms(secs.saturating_mul(1000)));
    std::future::poll_fn(|cx| {
        if let Poll::Ready(answer) = call.as_mut().poll(cx) {
            return Poll::Ready(answer);
        }
        if timer.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(LoginFailure::Network(format!("no answer within {secs} s"))));
        }
        Poll::Pending
    })
    .await
}

/// A transport error as a failure, without its URL (which could carry a
/// query) in the text.
fn network(e: reqwest::Error) -> LoginFailure {
    LoginFailure::Network(e.without_url().to_string())
}

/// Where the client finds the login service, as a pure function: `explicit`
/// is the runtime override (native `VOXEL_LOGIN_URL` env, browser `?login=`),
/// else `built`, the build-time `VOXEL_LOGIN_URL`, else none (no Play online:
/// the menu stays as it was). Empty strings count as unset; a trailing `/` is
/// dropped.
///
/// There is deliberately no `{page origin}/login` fallback: nothing serves
/// `/login` on the public site yet, so a deployed page would open on a dead
/// login screen. R5 restores it once the service sits behind `/login`.
pub fn resolve_login_url(explicit: Option<&str>, built: Option<&str>) -> Option<String> {
    let set = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(|v| v.trim_end_matches('/').to_string());
    set(explicit).or_else(|| set(built))
}

/// The login service this client talks to, if there is one (no "Play online"
/// otherwise).
pub fn login_url() -> Option<String> {
    let built = option_env!("VOXEL_LOGIN_URL");
    #[cfg(not(target_arch = "wasm32"))]
    {
        resolve_login_url(std::env::var(LOGIN_URL_ENV).ok().as_deref(), built)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let location = web_sys::window().map(|w| w.location());
        let search = location.as_ref().and_then(|l| l.search().ok()).unwrap_or_default();
        let explicit = crate::world_scale::query_param(&search, LOGIN_URL_QUERY)
            .map(|v| js_sys::decode_uri_component(v).ok().and_then(|s| s.as_string()).unwrap_or_else(|| v.to_string()));
        resolve_login_url(explicit.as_deref(), built)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_error(code: &str, message: &str) -> Vec<u8> {
        format!(r#"{{"code":"{code}","message":"{message}"}}"#).into_bytes()
    }

    #[test]
    fn error_body_maps_to_failure() {
        let failure = parse_response::<LoginResponse>(401, &api_error("identity_refused", "no")).unwrap_err();
        assert_eq!(failure, LoginFailure::Api(ApiError { code: ApiErrorCode::IdentityRefused, message: "no".into() }));
        assert_eq!(failure.to_string(), "Sign-in was not accepted. Try again.");
        let conflict = parse_response::<LoginResponse>(409, &api_error("identity_conflict", "x")).unwrap_err();
        assert_eq!(conflict.to_string(), "That email already belongs to another game account.");
        assert_eq!(LoginFailure::NoSignIn.to_string(), NO_SIGN_IN);
        assert!(!format!("{:?}", LoginFailure::Provider("invalid_client".into())).is_empty());
        let revoked = parse_response::<LoginResponse>(401, &api_error("revoked", "x")).unwrap_err();
        assert_eq!(revoked.to_string(), "Your login expired \u{2014} please log in again");
        let bad = parse_response::<()>(400, &api_error("bad_request", "World name is too long")).unwrap_err();
        assert_eq!(bad.to_string(), "World name is too long", "bad_request shows the server's message");
        let internal = parse_response::<()>(500, &api_error("internal", "db exploded at 10.0.0.3")).unwrap_err();
        assert!(!internal.to_string().contains("10.0.0.3"), "internal errors stay generic");
        // Not an ApiError: the status only.
        assert_eq!(parse_response::<()>(502, b"<html>").unwrap_err(), LoginFailure::Protocol("502".into()));
        assert_eq!(
            LoginFailure::Network("connection refused".into()).to_string(),
            "Can't reach the login server"
        );
    }

    #[test]
    fn success_bodies_decode() {
        assert_eq!(parse_response::<()>(202, b""), Ok(()));
        assert_eq!(parse_response::<()>(204, b""), Ok(()));
        let ok = parse_response::<LoginResponse>(
            200,
            br#"{"refresh_token":"r","account_email":"a@b.c","email_verified":true}"#,
        )
        .unwrap();
        assert_eq!(ok.account_email, "a@b.c");
        assert!(matches!(parse_response::<LoginResponse>(200, b"nope"), Err(LoginFailure::Protocol(_))));
        let worlds = parse_response::<Vec<WorldEntry>>(200, br#"[{"id":"w","name":"W","status":"open"}]"#).unwrap();
        assert_eq!(worlds.len(), 1);
    }

    #[test]
    fn debug_never_shows_free_text_secrets() {
        let failure = LoginFailure::Api(ApiError { code: ApiErrorCode::BadRequest, message: "hunter2".into() });
        assert!(!format!("{failure:?}").contains("hunter2"));
    }

    #[test]
    fn login_url_resolution() {
        // The runtime override wins, then the build-time value.
        assert_eq!(resolve_login_url(Some("http://l:1/"), Some("http://b")).as_deref(), Some("http://l:1"));
        assert_eq!(resolve_login_url(None, Some("http://b/")).as_deref(), Some("http://b"));
        // Empty = unset.
        assert_eq!(resolve_login_url(Some(""), Some("  ")), None);
        assert_eq!(resolve_login_url(Some(""), Some("http://b")).as_deref(), Some("http://b"));
        // Nothing configured: no login (no page-origin fallback until R5,
        // so a deployed page keeps its join-only menu).
        assert_eq!(resolve_login_url(None, None), None);
    }
}
