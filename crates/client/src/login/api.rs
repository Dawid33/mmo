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
/// The browser's query parameter (`?login=<url>`) that overrides it — honoured
/// only on a page served from the local machine ([`login_override_allowed`]).
pub const LOGIN_URL_QUERY: &str = "login";
/// Where the login service sits behind the page's own origin.
pub const LOGIN_PATH_ON_PAGE_ORIGIN: &str = "/login";
/// Page hostnames on which the browser honours `?login=`: the local machine
/// only (as `Location.hostname` spells them; IPv6 comes bracketed).
pub const LOGIN_OVERRIDE_HOSTS: [&str; 4] = ["localhost", "127.0.0.1", "[::1]", "::1"];
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
    /// sent the challenge of). A transport error or a transient answer
    /// ([`oidc::poll_status_is_transient`]: 5xx, a proxy's 408 / 429) is an `Err`:
    /// the sign-in goes on and polls again.
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
        if oidc::poll_status_is_transient(status) {
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
/// is the runtime override (native `VOXEL_LOGIN_URL` env, browser `?login=` on
/// a local page only — see [`login_override_allowed`]), else `built`, the
/// build-time `VOXEL_LOGIN_URL`, else `{page_origin}/login` when `page_origin`
/// (the browser's `location.origin`; `None` on native) is an `https://` origin
/// whose host is not loopback, else none (no Play online: the menu stays as it
/// was). Empty strings count as unset; a trailing `/` is dropped.
///
/// The fallback was dropped once (nothing served `/login`, so a deployed page
/// opened on a dead login screen). It is back for pages whose own origin
/// proxies `/login/` to the login service (the web image); `https` plus a
/// non-loopback host keeps a dev page (`http://127.0.0.1:…`, `localhost`) on
/// its join-only menu.
pub fn resolve_login_url(override_: Option<&str>, build_time: Option<&str>, page_origin: Option<&str>) -> Option<String> {
    let set = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(|v| v.trim_end_matches('/').to_string());
    set(override_).or_else(|| set(build_time)).or_else(|| {
        let origin = set(page_origin)?;
        let authority = origin.strip_prefix("https://")?;
        (!authority.is_empty() && !authority.contains('/') && !crate::web_endpoint::is_local_host(origin_host(authority)))
            .then(|| format!("{origin}{LOGIN_PATH_ON_PAGE_ORIGIN}"))
    })
}

/// The host of an origin's `host[:port]` (an IPv6 literal keeps its brackets).
fn origin_host(authority: &str) -> &str {
    if authority.starts_with('[') {
        return authority.find(']').map_or(authority, |end| &authority[..=end]);
    }
    authority.split(':').next().unwrap_or(authority)
}

/// Whether the browser may take the login service's address from `?login=`
/// on a page whose hostname is `page_host`: only on the local machine. On any
/// other host a link to the genuine page with `?login=https://evil` would
/// otherwise run the genuine sign-in and post the player's access token to
/// evil; there only the build-time address counts.
pub fn login_override_allowed(page_host: Option<&str>) -> bool {
    page_host.is_some_and(|h| LOGIN_OVERRIDE_HOSTS.iter().any(|ok| h.eq_ignore_ascii_case(ok)))
}

/// The login service this client talks to, if there is one (no "Play online"
/// otherwise).
pub fn login_url() -> Option<String> {
    let built = option_env!("VOXEL_LOGIN_URL");
    #[cfg(not(target_arch = "wasm32"))]
    {
        resolve_login_url(std::env::var(LOGIN_URL_ENV).ok().as_deref(), built, None)
    }
    #[cfg(target_arch = "wasm32")]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static WARNED: AtomicBool = AtomicBool::new(false);
        let location = web_sys::window().map(|w| w.location());
        let search = location.as_ref().and_then(|l| l.search().ok()).unwrap_or_default();
        let host = location.as_ref().and_then(|l| l.hostname().ok());
        let mut explicit = crate::world_scale::query_param(&search, LOGIN_URL_QUERY)
            .map(|v| js_sys::decode_uri_component(v).ok().and_then(|s| s.as_string()).unwrap_or_else(|| v.to_string()));
        if explicit.is_some() && !login_override_allowed(host.as_deref()) {
            // Never the value: it is whatever a link put there.
            if !WARNED.swap(true, Ordering::Relaxed) {
                bevy::log::warn!("[login] ignoring ?{LOGIN_URL_QUERY}= (honoured on a localhost page only)");
            }
            explicit = None;
        }
        let origin = location.as_ref().and_then(|l| l.origin().ok());
        resolve_login_url(explicit.as_deref(), built, origin.as_deref())
    }
}

