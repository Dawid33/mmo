//! The login service's HTTP API: one axum [`Router`] over [`AppState`].
//!
//! Security rules this file keeps (constraints decisions 5–12):
//! - **No passwords here.** Keycloak owns them: a player logs in there, and `/v1/login/oidc`
//!   trades the access token it issued for a refresh token of ours.
//! - **Refresh tokens** are [`TOKEN_BYTES`] random bytes, base64url without padding; the database
//!   holds SHA-256 over the token's base64url STRING bytes ([`token_hash`]) — so any presented
//!   string hashes, and a malformed one is simply unknown.
//!   Every refresh / join rotates the token within its family (take + successor in one
//!   transaction, `LoginStore::rotate_refresh`); presenting a spent token revokes
//!   the whole family.
//! - **Nothing secret is logged**: no refresh token or whole JWT, and no request
//!   or response body (the `login-api` DTOs' `Debug` prints them). Joins log account id + `kid`.

use std::future::Future;
use std::sync::Arc;

use axum::extract::{FromRequest, MatchedPath, Request, State};
use axum::middleware::{self, Next};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use login_api::*;
use rand_core::{OsRng, RngCore};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::config::LoginConfig;
use crate::host_link::HostLinks;
use crate::metrics::{JoinOutcome, Metrics, OidcOutcome};
use crate::oidc::{OidcError, OidcVerifier};
use crate::config::OidcConfig;
use crate::store::{AccountRow, LoginError, LoginStore, OidcUpsert, RefreshOutcome};
use crate::AccountId;

/// Random bytes in a refresh token.
pub const TOKEN_BYTES: usize = 32;
/// Random bytes in a refresh-token family id.
pub const FAMILY_BYTES: usize = 16;
/// Random bytes in a new player token (hex-encoded, like `game::PlayerToken::generate`).
pub const PLAYER_TOKEN_BYTES: usize = 16;
/// Seconds per day, for [`REFRESH_TTL_DAYS`].
const SECS_PER_DAY: u64 = 86_400;
const REVOKED_MSG: &str = "signed out: please log in again";
const IDENTITY_REFUSED_MSG: &str = "the identity provider's token was refused: sign in again";
const IDENTITY_CONFLICT_MSG: &str = "this email address belongs to another account";
const WORLD_UNAVAILABLE_MSG: &str = "the world server is not reachable right now; try again shortly";
/// The `Refused` text a kicked session gets after a logout everywhere.
pub const KICK_LOGOUT_ALL: &str = "signed out everywhere";
/// The `Refused` text a kicked session gets after an operator revoked the account (`login account revoke`).
pub const KICK_ACCOUNT_REVOKED: &str = "the account was signed out by an operator; log in again";

/// What every handler shares. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub store: LoginStore,
    pub signer: Arc<auth::Signer>,
    pub config: Arc<LoginConfig>,
    /// Seconds since the Unix epoch.
    pub clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// The world hosts' service links (`SpawnPlayer` / `Kick`).
    pub host_links: Arc<HostLinks>,
    /// Verifies the identity provider's tokens; `None` = identity-provider login is off.
    pub oidc: Option<Arc<OidcVerifier>>,
    /// The Prometheus metrics (`GET /metrics`).
    pub metrics: Metrics,
}

impl AppState {
    pub fn new(
        store: LoginStore,
        signer: Arc<auth::Signer>,
        config: Arc<LoginConfig>,
        clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> AppState {
        let metrics = Metrics::new();
        let host_links = Arc::new(HostLinks::new(None, store.clone(), clock.clone()).with_metrics(metrics.clone()));
        AppState { store, signer, config, clock, host_links, oidc: None, metrics }
    }

    /// Log players in with the identity provider's tokens, checked by `verifier` (the settings
    /// are `config.oidc`).
    pub fn with_oidc(mut self, verifier: Arc<OidcVerifier>) -> AppState {
        verifier.attach_metrics(self.metrics.clone());
        self.oidc = Some(verifier);
        self
    }

    /// Dial world hosts with the service `token` (without one, a world with a `host_link` is
    /// unavailable to join).
    pub fn with_service_token(self, token: Option<String>) -> AppState {
        let links = HostLinks::new(token, self.store.clone(), self.clock.clone());
        self.with_host_links(links)
    }

    /// Use these host links (tests: a short ack timeout).
    /// Starts their kick sweeper when called on a runtime.
    pub fn with_host_links(mut self, links: HostLinks) -> AppState {
        self.host_links = Arc::new(links.with_metrics(self.metrics.clone()));
        self.host_links.start();
        self
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    /// Now as the store's `i64`.
    fn now_db(&self) -> i64 {
        i64::try_from(self.now()).unwrap_or(i64::MAX)
    }

}

/// The real clock.
pub fn system_clock() -> Arc<dyn Fn() -> u64 + Send + Sync> {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    })
}

/// A non-2xx answer: the status and an [`ApiError`] JSON body.
#[derive(Debug)]
pub struct Fail {
    pub status: StatusCode,
    pub code: ApiErrorCode,
    pub message: String,
}

impl Fail {
    fn new(status: StatusCode, code: ApiErrorCode, message: impl Into<String>) -> Fail {
        Fail { status, code, message: message.into() }
    }
    pub(crate) fn bad_request(message: impl Into<String>) -> Fail {
        Fail::new(StatusCode::BAD_REQUEST, ApiErrorCode::BadRequest, message)
    }
    fn revoked() -> Fail {
        Fail::new(StatusCode::UNAUTHORIZED, ApiErrorCode::Revoked, REVOKED_MSG)
    }
    fn world_unavailable() -> Fail {
        Fail::new(StatusCode::SERVICE_UNAVAILABLE, ApiErrorCode::WorldUnavailable, WORLD_UNAVAILABLE_MSG)
    }
    /// No host of the world takes logins and it is starting one (Elastic 4): the client repeats
    /// the join.
    fn world_starting() -> Fail {
        Fail::new(StatusCode::SERVICE_UNAVAILABLE, ApiErrorCode::WorldStarting, login_api::WORLD_STARTING_MESSAGE)
    }
    fn identity_refused() -> Fail {
        Fail::new(StatusCode::UNAUTHORIZED, ApiErrorCode::IdentityRefused, IDENTITY_REFUSED_MSG)
    }
    /// The identity provider could not be reached to check a token: try again later.
    fn provider_unavailable() -> Fail {
        Fail::new(StatusCode::SERVICE_UNAVAILABLE, ApiErrorCode::Internal, "the identity provider is unavailable")
    }
    fn identity_conflict() -> Fail {
        Fail::new(StatusCode::CONFLICT, ApiErrorCode::IdentityConflict, IDENTITY_CONFLICT_MSG)
    }
    /// A route of a feature this deployment does not run.
    fn not_configured() -> Fail {
        Fail::new(StatusCode::NOT_FOUND, ApiErrorCode::BadRequest, "not available")
    }
    fn internal() -> Fail {
        Fail::new(StatusCode::INTERNAL_SERVER_ERROR, ApiErrorCode::Internal, "internal error")
    }
}

impl From<LoginError> for Fail {
    fn from(e: LoginError) -> Fail {
        log::error!("[login] database: {e}");
        Fail::internal()
    }
}

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        (self.status, Json(ApiError { code: self.code, message: self.message })).into_response()
    }
}

/// `axum::Json` whose rejection is an [`ApiError`] 400 too. The message is generic: serde's
/// would echo parts of the body (a token).
pub struct ApiJson<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for ApiJson<T> {
    type Rejection = Fail;

    fn from_request(req: Request, state: &S) -> impl Future<Output = Result<Self, Fail>> + Send {
        let fut = Json::<T>::from_request(req, state);
        async move {
            match fut.await {
                Ok(Json(v)) => Ok(ApiJson(v)),
                Err(r) => Err(Fail::bad_request(format!(
                    "malformed request body (expected JSON, status {})",
                    r.status().as_u16()
                ))),
            }
        }
    }
}

/// `n` random bytes from the OS.
fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    OsRng.fill_bytes(&mut b);
    b
}

/// A new refresh token (base64url, no padding).
fn new_token() -> String {
    URL_SAFE_NO_PAD.encode(random::<TOKEN_BYTES>())
}

/// What the database stores for a token: SHA-256 over the token string's bytes.
pub fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// A new player token: [`PLAYER_TOKEN_BYTES`] random bytes as lowercase hex.
fn new_player_token() -> String {
    random::<PLAYER_TOKEN_BYTES>().iter().map(|b| format!("{b:02x}")).collect()
}

/// A new refresh token of a new family for `account`, stored with the normal expiry
/// ([`REFRESH_TTL_DAYS`] from `now`). The handlers and `login account issue-refresh` both make
/// their tokens here.
pub async fn mint_refresh(store: &LoginStore, account: AccountId, now: i64) -> Result<String, LoginError> {
    let token = new_token();
    store.insert_refresh(token_hash(&token), account, random::<FAMILY_BYTES>(), refresh_expiry_from(now)).await?;
    Ok(token)
}

/// When a refresh token issued at `now` expires.
fn refresh_expiry_from(now: i64) -> i64 {
    let ttl = i64::try_from(REFRESH_TTL_DAYS * SECS_PER_DAY).unwrap_or(i64::MAX);
    now.saturating_add(ttl)
}

/// When a refresh token issued now expires.
fn refresh_expiry(state: &AppState) -> i64 {
    refresh_expiry_from(state.now_db())
}

fn login_response(account: &AccountRow, refresh_token: String) -> LoginResponse {
    LoginResponse {
        refresh_token,
        account_email: account.email.clone(),
        email_verified: account.email_verified_at.is_some(),
        account_name: account.display_name.clone(),
    }
}

/// Rotate `presented`: spend it and store its successor in the same family, atomically
/// ([`LoginStore::rotate_refresh`]); the live account and the successor token. A spent token
/// revokes its family; an unknown / expired one or a deleted account is refused (the successor is
/// then never handed out; it dies with the family at the next reuse or expiry).
async fn rotate_refresh(state: &AppState, presented: &str) -> Result<(AccountRow, String), Fail> {
    let now = state.now_db();
    let successor = new_token();
    let expires = refresh_expiry(state);
    match state.store.rotate_refresh(token_hash(presented), token_hash(&successor), expires, now).await? {
        RefreshOutcome::Valid { account, .. } => {
            let Some(row) = state.store.account(account).await? else {
                return Err(Fail::revoked());
            };
            Ok((row, successor))
        }
        RefreshOutcome::Reused { account, family } => {
            log::warn!("[login] refresh token reused: account {account}'s token family revoked");
            state.store.revoke_family(family, now).await?;
            Err(Fail::revoked())
        }
        RefreshOutcome::Unknown => Err(Fail::revoked()),
    }
}

/// The verifier and settings, or 404 when identity-provider login is off (both routes alike).
fn oidc_parts(s: &AppState) -> Result<(&Arc<OidcVerifier>, &OidcConfig), Fail> {
    match (&s.oidc, &s.config.oidc) {
        (Some(verifier), Some(config)) => Ok((verifier, config)),
        _ => Err(Fail::not_configured()),
    }
}

async fn login_oidc(State(s): State<AppState>, ApiJson(req): ApiJson<OidcLoginRequest>) -> Result<Json<LoginResponse>, Fail> {
    let (verifier, _) = oidc_parts(&s)?;
    let identity = verifier.verify_refreshing(&req.access_token, s.now()).await.map_err(|e| match e {
        // The provider's keys could not be fetched: our problem, not the player's.
        OidcError::Fetch(_) => {
            log::warn!("[login] identity-provider login unavailable: {e}");
            s.metrics.oidc_login(OidcOutcome::ProviderUnavailable);
            Fail::provider_unavailable()
        }
        // The variant only: never the token or the claims.
        _ => {
            log::warn!("[login] identity-provider token refused: {e}");
            s.metrics.oidc_login(OidcOutcome::Refused);
            Fail::identity_refused()
        }
    })?;
    let account = match s.store.upsert_oidc_account(&identity.subject, &identity.email, &identity.name, s.now_db()).await? {
        OidcUpsert::Created(id) => {
            s.metrics.oidc_login(OidcOutcome::Created);
            id
        }
        OidcUpsert::Existing(id) => {
            s.metrics.oidc_login(OidcOutcome::Existing);
            id
        }
        OidcUpsert::Adopted(id) => {
            s.metrics.oidc_login(OidcOutcome::Adopted);
            id
        }
        OidcUpsert::EmailTaken => {
            s.metrics.oidc_login(OidcOutcome::EmailTaken);
            log::warn!("[login] identity-provider login refused: its email belongs to another account");
            return Err(Fail::identity_conflict());
        }
    };
    let Some(row) = s.store.account(account).await? else {
        return Err(Fail::internal());
    };
    let token = mint_refresh(&s.store, account, s.now_db()).await?;
    log::info!("[login] account {account} logged in with the identity provider");
    Ok(Json(login_response(&row, token)))
}

async fn config(State(s): State<AppState>) -> Result<Json<LoginConfigResponse>, Fail> {
    let (_, oidc) = oidc_parts(&s)?;
    Ok(Json(LoginConfigResponse { oidc_issuer: oidc.issuer.clone(), oidc_client_id: oidc.client_id.clone() }))
}

async fn refresh(State(s): State<AppState>, ApiJson(req): ApiJson<RefreshRequest>) -> Result<Json<LoginResponse>, Fail> {
    let rotated = rotate_refresh(&s, &req.refresh_token).await;
    // An internal error (database) is neither: the request counters show it as a 500.
    match &rotated {
        Ok(_) => s.metrics.refresh(true),
        Err(f) if f.code != ApiErrorCode::Internal => s.metrics.refresh(false),
        Err(_) => {}
    }
    let (account, token) = rotated?;
    Ok(Json(login_response(&account, token)))
}

/// Always 204: revokes the token's family if the token is known. With `all` (and a known
/// token, live or already rotated), every refresh token of the account too, and its session
/// in every world is kicked.
async fn logout(State(s): State<AppState>, ApiJson(req): ApiJson<LogoutRequest>) -> Result<StatusCode, Fail> {
    let now = s.now_db();
    match s.store.take_refresh(token_hash(&req.refresh_token), now).await? {
        // A reused (already rotated) token with `all` signs out everywhere too: it may be the
        // stolen copy's owner who saw the theft — its sessions must not survive.
        RefreshOutcome::Valid { account, family } | RefreshOutcome::Reused { account, family } if req.all => {
            s.store.revoke_family(family, now).await?;
            let kicks = s.store.revoke_account_with_kicks(account, now, KICK_LOGOUT_ALL).await?;
            log::info!("[login] account {account} logged out everywhere ({kicks} kick(s) queued)");
            s.host_links.wake_kicks();
        }
        RefreshOutcome::Valid { account, family } | RefreshOutcome::Reused { account, family } => {
            s.store.revoke_family(family, now).await?;
            log::info!("[login] account {account} logged out");
        }
        RefreshOutcome::Unknown => {}
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn worlds(State(s): State<AppState>) -> Result<Json<Vec<WorldEntry>>, Fail> {
    let rows = s.store.worlds().await?;
    Ok(Json(rows.into_iter().map(|w| WorldEntry { id: w.world_id, name: w.name, status: w.status }).collect()))
}

async fn join(State(s): State<AppState>, ApiJson(req): ApiJson<JoinRequest>) -> Result<Json<JoinResponse>, Fail> {
    let result = join_inner(&s, req).await;
    // Counted once, where the outcome is known. An internal error (database) is neither.
    match &result {
        Ok(_) => s.metrics.join(JoinOutcome::Ok),
        Err(f) if f.code == ApiErrorCode::WorldUnavailable => s.metrics.join(JoinOutcome::WorldUnavailable),
        Err(f) if f.code == ApiErrorCode::WorldStarting => s.metrics.join(JoinOutcome::WorldStarting),
        Err(f) if f.code != ApiErrorCode::Internal => s.metrics.join(JoinOutcome::Refused),
        Err(_) => {}
    }
    result
}

async fn join_inner(s: &AppState, req: JoinRequest) -> Result<Json<JoinResponse>, Fail> {
    // The world first, so a bad world id does not spend the client's refresh token.
    let Some(world) = s.store.world(&req.world).await? else {
        return Err(Fail::new(StatusCode::NOT_FOUND, ApiErrorCode::UnknownWorld, "no such world"));
    };
    if world.status != WorldStatus::Open {
        return Err(Fail::new(StatusCode::FORBIDDEN, ApiErrorCode::WorldClosed, "this world is closed"));
    }
    // The account first, WITHOUT spending the token: a host that does not take the spawn order
    // answers 503 and the client keeps its refresh token. The rotation comes after the ack.
    let presented = token_hash(&req.refresh_token);
    let Some(account_id) = s.store.live_refresh_account(presented, s.now_db()).await? else {
        // Not live: the rotation answers it (a spent token revokes its family).
        rotate_refresh(s, &req.refresh_token).await?;
        return Err(Fail::revoked());
    };
    let Some(account) = s.store.account(account_id).await? else {
        return Err(Fail::revoked());
    };
    let now = s.now();
    let player_token =
        s.store.player_token_for(account.account_id, &world.world_id, s.now_db(), new_player_token).await?;
    let session_epoch = s.store.bump_session(account.account_id, &world.world_id, s.now_db()).await?;
    // The host parks any session of this player now (newest wins) and admits this epoch only.
    // A 503 does not mean nothing happened: an order that reached the host and was applied
    // (its ack lost, or later than the timeout) has already parked the player's online
    // session there — the player is then logged out of that world and must join again.
    // With several hosts the order may be taken by another host than the linked one: the
    // ticket names the addresses its ack gave (else the world row's).
    let place = match s.host_links.spawn(&world, &player_token, session_epoch).await {
        Ok(place) => place,
        // No host takes logins and the world is starting one: the client waits and repeats the
        // join (with the same, unspent refresh token).
        Err(crate::host_link::SpawnError::Starting) => {
            log::info!(
                "[login] join: account {} world {} epoch {session_epoch}: the world is starting a host",
                account.account_id,
                world.world_id
            );
            return Err(Fail::world_starting());
        }
        Err(e) => {
            log::warn!(
                "[login] join: account {} world {} epoch {session_epoch}: {e}",
                account.account_id,
                world.world_id
            );
            return Err(Fail::world_unavailable());
        }
    };
    let (quic, webtransport, cert_hash) = if place.named() {
        (place.quic, place.webtransport, place.cert_hash)
    } else {
        (world.quic, world.webtransport, world.cert_hash)
    };
    // A linked world whose ack and row name no address would hand out a ticket to nowhere.
    if world.host_link.is_some() && quic.is_none() && webtransport.is_none() {
        log::warn!(
            "[login] join: world {} has no endpoint (the ack named none and the world row has none)",
            world.world_id
        );
        return Err(Fail::world_unavailable());
    }
    // Two joins racing with one token: the loser's spawn order is superseded by the winner's or
    // expires unused at the host; its rotation answers Reused (the family is revoked).
    let (rotated, refresh_token) = rotate_refresh(s, &req.refresh_token).await?;
    if rotated.account_id != account.account_id {
        return Err(Fail::revoked());
    }
    let claims = JoinClaims {
        sub: account.account_id.to_string(),
        world: world.world_id.clone(),
        player_token,
        session_epoch,
        aud: HOST_AUDIENCE.to_string(),
        iat: now,
        exp: now + JOIN_JWT_TTL_SECS,
    };
    let jwt = s.signer.sign(&claims);
    log::info!(
        "[login] join: account {} world {} epoch {session_epoch} kid {}",
        account.account_id,
        world.world_id,
        s.signer.signing_key().kid()
    );
    Ok(Json(JoinResponse {
        ticket: JoinTicket {
            world: world.world_id,
            jwt,
            quic,
            webtransport,
            cert_hash,
            expires_in_secs: JOIN_JWT_TTL_SECS,
        },
        refresh_token,
    }))
}

async fn jwks(State(s): State<AppState>) -> Json<auth::Jwks> {
    Json(s.signer.jwks())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn metrics(State(s): State<AppState>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], s.metrics.render())
}

/// Counts every request but the scrape and the probe, by matched route template (`other` for an
/// unmatched path, so a scanner cannot make series) and status.
async fn count_requests(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let route = match req.extensions().get::<MatchedPath>() {
        Some(path) => path.as_str().to_string(),
        None => "other".to_string(),
    };
    let start = std::time::Instant::now();
    let response = next.run(req).await;
    if route != "/metrics" && route != "/healthz" {
        s.metrics.http_request(&route, response.status().as_u16(), start.elapsed().as_secs_f64());
    }
    response
}

/// Exact origins from the config; an unparsable one is skipped with a warning.
fn cors(config: &LoginConfig) -> CorsLayer {
    let origins: Vec<HeaderValue> = config
        .cors_origins
        .iter()
        .filter_map(|o| match HeaderValue::from_str(o) {
            Ok(v) => Some(v),
            Err(_) => {
                log::warn!("[login] ignoring CORS origin {o:?}: not a valid header value");
                None
            }
        })
        .collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE])
}

/// Every endpoint, behind one CORS layer (a rate limit would be one more layer here).
pub fn router(state: AppState) -> Router {
    let cors = cors(&state.config);
    Router::new()
        .route(PATH_LOGIN_OIDC, post(login_oidc))
        .route(PATH_CONFIG, get(config))
        .route(PATH_REFRESH, post(refresh))
        .route(PATH_LOGOUT, post(logout))
        .route(PATH_WORLDS, get(worlds))
        .route(PATH_JOIN, post(join))
        .route(PATH_JWKS, get(jwks))
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .layer(cors)
        .layer(middleware::from_fn_with_state(state.clone(), count_requests))
        .with_state(state)
}
