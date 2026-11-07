//! The login service's HTTP API as the client calls it. [`LoginApi`] is a
//! plain `reqwest::Client` wrapper: it spawns nothing and needs no particular
//! runtime flavour (the login screen drives it from a current-thread tokio
//! runtime on native, `spawn_local` in the browser, where reqwest uses
//! `fetch`).
//!
//! The DTOs print passwords, refresh tokens and JWTs with `{:?}`: never log
//! them; [`LoginFailure`] holds none of them.

use std::fmt;

use login_api::{
    ApiError, ApiErrorCode, JoinRequest, JoinResponse, LoginRequest, LoginResponse, RefreshRequest, RegisterRequest,
    ResetRequest, WorldEntry, PATH_JOIN, PATH_LOGIN, PATH_LOGOUT, PATH_REFRESH, PATH_REGISTER, PATH_RESET_REQUEST,
    PATH_WORLDS,
};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

/// Overrides the login service's address on native (empty = unset); the
/// build-time `VOXEL_LOGIN_URL` is the fallback.
pub const LOGIN_URL_ENV: &str = "VOXEL_LOGIN_URL";
/// The browser's query parameter (`?login=<url>`) that overrides it.
pub const LOGIN_URL_QUERY: &str = "login";
/// Where the login service sits behind the page's own origin.
pub const LOGIN_PATH_ON_PAGE_ORIGIN: &str = "/login";
/// Longest a login request may take before it fails as a network error.
/// Native only: reqwest's fetch backend has no timeout (the browser's own
/// applies).
#[cfg(not(target_arch = "wasm32"))]
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

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
}

impl fmt::Display for LoginFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoginFailure::Network(_) => f.write_str("Can't reach the login server"),
            LoginFailure::Api(e) => match e.code {
                ApiErrorCode::BadCredentials => f.write_str("Wrong email or password"),
                ApiErrorCode::NotVerified => f.write_str("Please verify your email address first"),
                ApiErrorCode::Revoked => f.write_str("Your login expired \u{2014} please log in again"),
                ApiErrorCode::UnknownWorld => f.write_str("That world does not exist"),
                ApiErrorCode::WorldClosed => f.write_str("That world is closed"),
                ApiErrorCode::BadRequest => f.write_str(&e.message),
                ApiErrorCode::Internal => f.write_str("The login server had a problem \u{2014} try again later"),
            },
            LoginFailure::Protocol(_) => f.write_str("The login server sent an answer we do not understand"),
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

    pub async fn register(&self, email: &str, password: &str) -> Result<(), LoginFailure> {
        self.post(PATH_REGISTER, &RegisterRequest { email: email.into(), password: password.into() }).await
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<LoginResponse, LoginFailure> {
        self.post(PATH_LOGIN, &LoginRequest { email: email.into(), password: password.into() }).await
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

    pub async fn request_reset(&self, email: &str) -> Result<(), LoginFailure> {
        self.post(PATH_RESET_REQUEST, &ResetRequest { email: email.into() }).await
    }

    async fn post<B: serde::Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, LoginFailure> {
        self.answer(self.http.post(format!("{}{path}", self.base)).json(body)).await
    }

    async fn answer<T: DeserializeOwned>(&self, request: reqwest::RequestBuilder) -> Result<T, LoginFailure> {
        let response = request.send().await.map_err(network)?;
        let status: StatusCode = response.status();
        let body = response.bytes().await.map_err(network)?;
        parse_response(status.as_u16(), &body)
    }
}

/// A transport error as a failure, without its URL (which could carry a
/// query) in the text.
fn network(e: reqwest::Error) -> LoginFailure {
    LoginFailure::Network(e.without_url().to_string())
}

/// What the browser page says about itself (decision 15's last fallback).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageInfo {
    /// `location.origin`, e.g. `https://game.example.com`.
    pub origin: String,
    /// `location.protocol`, e.g. `https:`.
    pub protocol: String,
    pub hostname: String,
}

/// Decision 15 as a pure function. `explicit` is the runtime override (native
/// env, browser `?login=`), `built` the build-time `VOXEL_LOGIN_URL`, `page`
/// the browser page (`None` on native): else an `https:` page on a real host
/// (`web_endpoint`'s rule) has the service at `{origin}/login`. Empty
/// strings count as unset; a trailing `/` is dropped.
pub fn resolve_login_url(explicit: Option<&str>, built: Option<&str>, page: Option<PageInfo>) -> Option<String> {
    let set = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(|v| v.trim_end_matches('/').to_string());
    if let Some(url) = set(explicit).or_else(|| set(built)) {
        return Some(url);
    }
    let page = page?;
    if page.protocol != "https:" || crate::web_endpoint::is_local_host(&page.hostname) {
        return None;
    }
    let origin = set(Some(&page.origin))?;
    Some(format!("{origin}{LOGIN_PATH_ON_PAGE_ORIGIN}"))
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
        let location = web_sys::window().map(|w| w.location());
        let search = location.as_ref().and_then(|l| l.search().ok()).unwrap_or_default();
        let page = location.as_ref().map(|l| PageInfo {
            origin: l.origin().unwrap_or_default(),
            protocol: l.protocol().unwrap_or_default(),
            hostname: l.hostname().unwrap_or_default(),
        });
        let explicit = crate::world_scale::query_param(&search, LOGIN_URL_QUERY)
            .map(|v| js_sys::decode_uri_component(v).ok().and_then(|s| s.as_string()).unwrap_or_else(|| v.to_string()));
        resolve_login_url(explicit.as_deref(), built, page)
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
        let failure = parse_response::<LoginResponse>(401, &api_error("bad_credentials", "no")).unwrap_err();
        assert_eq!(failure, LoginFailure::Api(ApiError { code: ApiErrorCode::BadCredentials, message: "no".into() }));
        assert_eq!(failure.to_string(), "Wrong email or password");
        let revoked = parse_response::<LoginResponse>(401, &api_error("revoked", "x")).unwrap_err();
        assert_eq!(revoked.to_string(), "Your login expired \u{2014} please log in again");
        let bad = parse_response::<()>(400, &api_error("bad_request", "Password is too short")).unwrap_err();
        assert_eq!(bad.to_string(), "Password is too short", "bad_request shows the server's message");
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
        let page = |origin: &str, protocol: &str, hostname: &str| {
            Some(PageInfo { origin: origin.into(), protocol: protocol.into(), hostname: hostname.into() })
        };
        // The runtime override wins, then the build-time value.
        assert_eq!(resolve_login_url(Some("http://l:1/"), Some("http://b"), None).as_deref(), Some("http://l:1"));
        assert_eq!(resolve_login_url(None, Some("http://b/"), None).as_deref(), Some("http://b"));
        // Empty = unset.
        assert_eq!(resolve_login_url(Some(""), Some("  "), None), None);
        assert_eq!(resolve_login_url(Some(""), Some("http://b"), None).as_deref(), Some("http://b"));
        // Native has no page: nothing configured, no login.
        assert_eq!(resolve_login_url(None, None, None), None);
        // A browser page: https on a real host -> origin + /login.
        assert_eq!(
            resolve_login_url(None, None, page("https://game.example.com", "https:", "game.example.com")).as_deref(),
            Some("https://game.example.com/login")
        );
        // Explicit still wins over the page.
        assert_eq!(
            resolve_login_url(Some("http://x"), None, page("https://game.example.com", "https:", "game.example.com"))
                .as_deref(),
            Some("http://x")
        );
        // Not https, or a local host: none (the join-only dev menu stays).
        assert_eq!(resolve_login_url(None, None, page("http://game.example.com", "http:", "game.example.com")), None);
        assert_eq!(resolve_login_url(None, None, page("https://localhost", "https:", "localhost")), None);
        assert_eq!(resolve_login_url(None, None, page("http://127.0.0.1:1334", "http:", "127.0.0.1")), None);
    }
}
