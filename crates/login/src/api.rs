//! The login service's HTTP API: one axum [`Router`] over [`AppState`].
//!
//! Security rules this file keeps (constraints decisions 5–12):
//! - **No account enumeration.** Login answers one `BadCredentials` body for an unknown email and
//!   a wrong password, and runs one Argon2 verification either way (against [`AppState`]'s dummy
//!   hash when the email is unknown). Register and reset-request always answer 202; an existing
//!   email gets an "already registered" mail instead of a verify mail. Mail goes out on a spawned
//!   task, so the answer never waits on (or times) the mail transport.
//! - **Refresh tokens** are [`TOKEN_BYTES`] random bytes, base64url without padding; the database
//!   holds SHA-256 over the token's base64url STRING bytes ([`token_hash`]) — so any presented
//!   string hashes, and a malformed one is simply unknown. Email tokens use the same scheme.
//!   Every refresh / join rotates the token within its family (take + successor in one
//!   transaction, `LoginStore::rotate_refresh`); presenting a spent token revokes
//!   the whole family.
//! - **Nothing secret is logged**: no password, refresh / email token or whole JWT, and no request
//!   or response body (the `login-api` DTOs' `Debug` prints them). Joins log account id + `kid`.

use std::future::Future;
use std::sync::Arc;

use axum::extract::{FromRequest, Request, State};
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
use crate::mail::{
    Mail, Mailer, RESET_TOKEN_TTL_SECS, VERIFY_TOKEN_TTL_SECS, already_registered_mail, reset_mail,
    verify_mail,
};
use crate::password::{Hasher, check_password, normalize_email};
use crate::store::{AccountRow, LoginError, LoginStore, RefreshOutcome};
use crate::{EMAIL_TOKEN_RESET, EMAIL_TOKEN_VERIFY};

/// Random bytes in a refresh or email token.
pub const TOKEN_BYTES: usize = 32;
/// Random bytes in a refresh-token family id.
pub const FAMILY_BYTES: usize = 16;
/// Random bytes in a new player token (hex-encoded, like `game::PlayerToken::generate`).
pub const PLAYER_TOKEN_BYTES: usize = 16;
/// Seconds per day, for [`REFRESH_TTL_DAYS`].
const SECS_PER_DAY: u64 = 86_400;
/// The dummy password [`AppState`] hashes once, verified against when a login's email is unknown.
const DUMMY_PASSWORD: &str = "dummy password for unknown accounts";
/// The single answer to an unknown email or a wrong password.
const BAD_CREDENTIALS_MSG: &str = "wrong email or password";
const REVOKED_MSG: &str = "signed out: please log in again";
const NOT_VERIFIED_MSG: &str = "confirm your email address first (see the mail we sent)";
const BAD_LINK_MSG: &str = "this link is invalid, used or expired";
const WORLD_UNAVAILABLE_MSG: &str = "the world server is not reachable right now; try again shortly";
/// The `Refused` text a kicked session gets after a logout everywhere.
pub const KICK_LOGOUT_ALL: &str = "signed out everywhere";
/// The `Refused` text a kicked session gets after a password reset.
pub const KICK_PASSWORD_RESET: &str = "the account's password was reset; log in again";
/// The `Refused` text a kicked session gets after an admin set the password (`login account`).
pub const KICK_PASSWORD_SET: &str = "the account's password was changed; log in again";

/// What every handler shares. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub store: LoginStore,
    pub signer: Arc<auth::Signer>,
    pub hasher: Arc<Hasher>,
    pub mailer: Arc<dyn Mailer>,
    pub config: Arc<LoginConfig>,
    /// Seconds since the Unix epoch.
    pub clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// The world hosts' service links (`SpawnPlayer` / `Kick`).
    pub host_links: Arc<HostLinks>,
    /// A PHC string of [`DUMMY_PASSWORD`] under `hasher`, so an unknown email costs one verify.
    dummy_phc: Arc<str>,
}

impl AppState {
    pub fn new(
        store: LoginStore,
        signer: Arc<auth::Signer>,
        hasher: Hasher,
        mailer: Arc<dyn Mailer>,
        config: Arc<LoginConfig>,
        clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> AppState {
        let dummy_phc = hasher.hash(DUMMY_PASSWORD).into();
        let host_links = Arc::new(HostLinks::new(None, store.clone(), clock.clone()));
        AppState { store, signer, hasher: Arc::new(hasher), mailer, config, clock, host_links, dummy_phc }
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
        self.host_links = Arc::new(links);
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

    /// Send on a spawned task; a failure is logged (recipient's absence of a token: the error
    /// carries the transport message only).
    fn send_mail(&self, mail: Mail) {
        let subject = mail.subject.clone();
        let fut = self.mailer.send(mail);
        tokio::spawn(async move {
            if let Err(e) = fut.await {
                log::error!("[login] mail {subject:?} failed: {e}");
            }
        });
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
    fn bad_credentials() -> Fail {
        Fail::new(StatusCode::UNAUTHORIZED, ApiErrorCode::BadCredentials, BAD_CREDENTIALS_MSG)
    }
    fn revoked() -> Fail {
        Fail::new(StatusCode::UNAUTHORIZED, ApiErrorCode::Revoked, REVOKED_MSG)
    }
    fn not_verified() -> Fail {
        Fail::new(StatusCode::FORBIDDEN, ApiErrorCode::NotVerified, NOT_VERIFIED_MSG)
    }
    fn world_unavailable() -> Fail {
        Fail::new(StatusCode::SERVICE_UNAVAILABLE, ApiErrorCode::WorldUnavailable, WORLD_UNAVAILABLE_MSG)
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
/// would echo parts of the body (a password, a token).
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

/// A new refresh or email token (base64url, no padding).
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

async fn hash_password(state: &AppState, password: String) -> Result<String, Fail> {
    let hasher = state.hasher.clone();
    tokio::task::spawn_blocking(move || hasher.hash(&password)).await.map_err(|e| {
        log::error!("[login] hashing task: {e}");
        Fail::internal()
    })
}

async fn verify_password(state: &AppState, password: String, phc: String) -> Result<bool, Fail> {
    let hasher = state.hasher.clone();
    tokio::task::spawn_blocking(move || hasher.verify(&password, &phc)).await.map_err(|e| {
        log::error!("[login] verify task: {e}");
        Fail::internal()
    })
}

/// Store a new refresh token in `family` for `account`; the token.
async fn issue_refresh(state: &AppState, account: i64, family: [u8; FAMILY_BYTES]) -> Result<String, Fail> {
    let token = new_token();
    state.store.insert_refresh(token_hash(&token), account, family, refresh_expiry(state)).await?;
    Ok(token)
}

/// When a refresh token issued now expires.
fn refresh_expiry(state: &AppState) -> i64 {
    let ttl = REFRESH_TTL_DAYS * SECS_PER_DAY;
    i64::try_from(state.now().saturating_add(ttl)).unwrap_or(i64::MAX)
}

fn login_response(account: &AccountRow, refresh_token: String) -> LoginResponse {
    LoginResponse {
        refresh_token,
        account_email: account.email.clone(),
        email_verified: account.email_verified_at.is_some(),
        account_name: String::new(),
    }
}

/// Rotate `presented`: spend it and store its successor in the same family, atomically
/// ([`LoginStore::rotate_refresh`]); the live account and the successor token. A spent token
/// revokes its family; an unknown / expired one, a deleted account or (with `require_verified`)
/// an unverified one is refused (the successor is then never handed out; it dies with the family
/// at the next reuse or expiry).
async fn rotate_refresh(state: &AppState, presented: &str) -> Result<(AccountRow, String), Fail> {
    let now = state.now_db();
    let successor = new_token();
    let expires = refresh_expiry(state);
    match state.store.rotate_refresh(token_hash(presented), token_hash(&successor), expires, now).await? {
        RefreshOutcome::Valid { account, .. } => {
            let Some(row) = state.store.account(account).await? else {
                return Err(Fail::revoked());
            };
            if state.config.require_verified && row.email_verified_at.is_none() {
                return Err(Fail::not_verified());
            }
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

async fn register(State(s): State<AppState>, ApiJson(req): ApiJson<RegisterRequest>) -> Result<StatusCode, Fail> {
    let email = normalize_email(&req.email).map_err(Fail::bad_request)?;
    check_password(&req.password).map_err(Fail::bad_request)?;
    // Hash before knowing whether the email exists: both answers cost the same.
    let phc = hash_password(&s, req.password).await?;
    let now = s.now_db();
    match s.store.create_account(&email, &phc, now).await? {
        Some(account) => {
            let token = new_token();
            s.store
                .insert_email_token(token_hash(&token), account, EMAIL_TOKEN_VERIFY, now + VERIFY_TOKEN_TTL_SECS)
                .await?;
            log::info!("[login] account {account} registered");
            s.send_mail(verify_mail(&email, &s.config.public_url, &token));
        }
        None => s.send_mail(already_registered_mail(&email, &s.config.public_url)),
    }
    Ok(StatusCode::ACCEPTED)
}

async fn login(State(s): State<AppState>, ApiJson(req): ApiJson<LoginRequest>) -> Result<Json<LoginResponse>, Fail> {
    let email = normalize_email(&req.email).map_err(Fail::bad_request)?;
    // An over-long password can never match (the policy refuses it at register / reset).
    let password = if req.password.chars().count() > PASSWORD_MAX_CHARS { String::new() } else { req.password };
    let account = s.store.account_by_email(&email).await?;
    let phc = account.as_ref().map_or_else(|| s.dummy_phc.to_string(), |a| a.password_hash.clone());
    let matches = verify_password(&s, password.clone(), phc).await?;
    let account = match account {
        Some(a) if matches && !password.is_empty() => a,
        _ => return Err(Fail::bad_credentials()),
    };
    if s.config.require_verified && account.email_verified_at.is_none() {
        return Err(Fail::not_verified());
    }
    let token = issue_refresh(&s, account.account_id, random::<FAMILY_BYTES>()).await?;
    log::info!("[login] account {} logged in", account.account_id);
    Ok(Json(login_response(&account, token)))
}

async fn refresh(State(s): State<AppState>, ApiJson(req): ApiJson<RefreshRequest>) -> Result<Json<LoginResponse>, Fail> {
    let (account, token) = rotate_refresh(&s, &req.refresh_token).await?;
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
        rotate_refresh(&s, &req.refresh_token).await?;
        return Err(Fail::revoked());
    };
    let Some(account) = s.store.account(account_id).await? else {
        return Err(Fail::revoked());
    };
    if s.config.require_verified && account.email_verified_at.is_none() {
        return Err(Fail::not_verified());
    }
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
    // Two joins racing with one token: the loser's spawn order is superseded by the winner's or
    // expires unused at the host; its rotation answers Reused (the family is revoked).
    let (rotated, refresh_token) = rotate_refresh(&s, &req.refresh_token).await?;
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

/// Confirm an email address with a mailed token (API and page).
pub(crate) async fn confirm_email(s: &AppState, token: &str) -> Result<(), Fail> {
    let now = s.now_db();
    let Some(account) = s.store.use_email_token(token_hash(token), EMAIL_TOKEN_VERIFY, now).await? else {
        return Err(Fail::bad_request(BAD_LINK_MSG));
    };
    s.store.mark_verified(account, now).await?;
    log::info!("[login] account {account} verified its email");
    Ok(())
}

/// Mail a reset link if `email` has an account; the same outcome either way (API and page).
pub(crate) async fn request_reset(s: &AppState, email: &str) -> Result<(), Fail> {
    let Ok(email) = normalize_email(email) else { return Ok(()) };
    if let Some(account) = s.store.account_by_email(&email).await? {
        let token = new_token();
        let expires = s.now_db() + RESET_TOKEN_TTL_SECS;
        s.store.insert_email_token(token_hash(&token), account.account_id, EMAIL_TOKEN_RESET, expires).await?;
        s.send_mail(reset_mail(&email, &s.config.public_url, &token));
    }
    Ok(())
}

/// Set a new password with a mailed reset token and sign every session out (API and page). The
/// policy is checked first, so a refused password does not spend the token. Following the link
/// also proves the address, so the account counts as verified.
pub(crate) async fn confirm_reset(s: &AppState, token: &str, password: String) -> Result<(), Fail> {
    check_password(&password).map_err(Fail::bad_request)?;
    let now = s.now_db();
    // The link is checked before the slow hash, and spent only with the reset itself: one
    // transaction sets the password, revokes every session and queues their kicks.
    if s.store.peek_email_token(token_hash(token), EMAIL_TOKEN_RESET, now).await?.is_none() {
        return Err(Fail::bad_request(BAD_LINK_MSG));
    }
    let phc = hash_password(s, password).await?;
    let reset = s.store.reset_password(token_hash(token), EMAIL_TOKEN_RESET, &phc, now, KICK_PASSWORD_RESET).await?;
    let Some((account, kicks)) = reset else {
        return Err(Fail::bad_request(BAD_LINK_MSG)); // spent meanwhile (a second click)
    };
    log::info!("[login] account {account} reset its password ({kicks} kick(s) queued)");
    s.host_links.wake_kicks();
    Ok(())
}

async fn verify(State(s): State<AppState>, ApiJson(req): ApiJson<VerifyRequest>) -> Result<StatusCode, Fail> {
    confirm_email(&s, &req.token).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn reset_request(State(s): State<AppState>, ApiJson(req): ApiJson<ResetRequest>) -> Result<StatusCode, Fail> {
    request_reset(&s, &req.email).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn reset_confirm(State(s): State<AppState>, ApiJson(req): ApiJson<ResetConfirm>) -> Result<StatusCode, Fail> {
    confirm_reset(&s, &req.token, req.password).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn jwks(State(s): State<AppState>) -> Json<auth::Jwks> {
    Json(s.signer.jwks())
}

async fn healthz() -> &'static str {
    "ok"
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
        .route(PATH_REGISTER, post(register))
        .route(PATH_LOGIN, post(login))
        .route(PATH_REFRESH, post(refresh))
        .route(PATH_LOGOUT, post(logout))
        .route(PATH_WORLDS, get(worlds))
        .route(PATH_JOIN, post(join))
        .route(PATH_VERIFY, post(verify))
        .route(PATH_RESET_REQUEST, post(reset_request))
        .route(PATH_RESET_CONFIRM, post(reset_confirm))
        .route(PATH_JWKS, get(jwks))
        .route("/healthz", get(healthz))
        .merge(crate::pages::routes())
        .layer(cors)
        .with_state(state)
}
