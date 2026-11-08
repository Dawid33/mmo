//! Who may connect: the credential check of a connection's `Hello`, at the
//! network edge (QUIC and WebTransport ingress), before the session reaches
//! the `WorldManager`.
//!
//! [`AuthMode`] (`VOXEL_AUTH`) picks the check:
//! - `local` (the default; `token` is an alias — single-player, LAN and dev
//!   servers, Factorio's model): the client says who it is with a free
//!   [`PlayerName`]. No secret and no login service: anyone may claim any
//!   name, so a public server should run `jwt`.
//! - `jwt` (server worlds): a join JWT the login service signed for THIS world
//!   ([`VOXEL_WORLD_ID`](WORLD_ID_ENV)), checked against the service's public
//!   keys ([`JwksCache`], fetched from [`LOGIN_URL_ENV`] + `PATH_JWKS`):
//!   signature, expiry, audience, world, and then its `session_epoch`:
//!   - with a login link ([`crate::login_link`]; a host with a service
//!     token): it must be the epoch of a pending spawn order the login
//!     service pushed, which the connect consumes;
//!   - without one (R2a; a registry world without `host_link`): not older
//!     than the newest this host has admitted for the same player.
//!
//! The JWT is checked only at connect. A refusal's text is sent to the client;
//! nothing here logs a whole JWT (only its `sub` / `kid`).

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use auth::{Jwks, Verifier, VerifyError};
use game::{Credential, PlayerName, PlayerToken};

use crate::login_link::SpawnOrders;
use crate::player_store::PlayerKey;

/// `local` (default; alias `token`) or `jwt`.
pub const AUTH_ENV: &str = "VOXEL_AUTH";
/// The registry id of the world this host serves (required by `jwt`).
pub const WORLD_ID_ENV: &str = "VOXEL_WORLD_ID";
/// The login service's base URL, e.g. `http://127.0.0.1:8790` (required by `jwt`).
pub const LOGIN_URL_ENV: &str = "VOXEL_LOGIN_URL";

/// An unknown `kid` refetches the JWKS at most this often.
pub const JWKS_REFETCH_MIN_SECS: u64 = 30;
/// How long one JWKS fetch may take.
pub const JWKS_FETCH_TIMEOUT_SECS: u64 = 5;

/// The refusal a `local` host gives a login ticket.
pub const REFUSE_JWT_ON_LOCAL_HOST: &str =
    "this server does not use login (single-player / dev)";
/// The refusal a `jwt` host gives a player name.
pub const REFUSE_LOCAL_ON_JWT_HOST: &str = "this server requires login";
/// The refusal of a ticket older than one this host already admitted.
pub const REFUSE_SUPERSEDED: &str = "superseded by a newer login";
/// The refusal of a ticket over [`game::MAX_JWT_BYTES`].
pub const REFUSE_TOO_LARGE: &str = "login ticket too large";

/// Seconds since the Unix epoch.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The wall clock.
pub fn system_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    })
}

/// How this host checks a connection's credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMode {
    Local,
    Jwt { world: String, login_url: String },
}

impl AuthMode {
    /// From [`AUTH_ENV`], [`WORLD_ID_ENV`] and [`LOGIN_URL_ENV`]. `Err` names
    /// the variable at fault; the server refuses to start with it.
    pub fn from_env() -> Result<AuthMode, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// The mode of this server process. A single-player server (the one a
    /// client spawns for its own world) is always [`AuthMode::Local`]: it
    /// inherits the client's environment, and a `VOXEL_AUTH=jwt` left
    /// exported from dev-login experiments must not make it need a login
    /// service. The second value is a warning to log when such a setting
    /// was overridden.
    pub fn resolve(
        single_player: bool,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<(AuthMode, Option<String>), String> {
        if !single_player {
            return Self::from_lookup(lookup).map(|mode| (mode, None));
        }
        let warning = match lookup(AUTH_ENV).as_deref().map(str::trim) {
            None | Some("") | Some("local") | Some("token") => None,
            Some(other) => Some(format!(
                "{AUTH_ENV}={other:?} ignored: a single-player server always uses local player names"
            )),
        };
        Ok((AuthMode::Local, warning))
    }

    /// [`from_env`](Self::from_env) over any variable lookup.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<AuthMode, String> {
        let required = |name: &str| match lookup(name) {
            Some(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
            _ => Err(format!("{AUTH_ENV}=jwt requires {name}")),
        };
        match lookup(AUTH_ENV).as_deref().map(str::trim) {
            None | Some("") | Some("local") | Some("token") => Ok(AuthMode::Local),
            Some("jwt") => Ok(AuthMode::Jwt { world: required(WORLD_ID_ENV)?, login_url: required(LOGIN_URL_ENV)? }),
            Some(other) => Err(format!("{AUTH_ENV}={other:?}: expected `local` or `jwt`")),
        }
    }
}

/// One JWKS fetch: the keys, or why not.
pub type JwksFuture = Pin<Box<dyn Future<Output = Result<Jwks, String>> + Send>>;
/// How a [`JwksCache`] fetches (HTTP in production, canned in tests).
pub type JwksFetch = Box<dyn Fn() -> JwksFuture + Send + Sync>;

/// The login service's public keys as this host last fetched them.
pub struct JwksCache {
    fetch: JwksFetch,
    /// The keys connects verify against. Read without waiting: a lock guard
    /// is never held across an `await`, so a refetch in flight never stalls
    /// a connect whose key is already known.
    published: RwLock<Arc<Verifier>>,
    /// When the last fetch was attempted (clock seconds; `None` = never).
    /// Held across a refetch, so concurrent connects with one unknown `kid`
    /// cause one fetch, not one each — and only they wait for it.
    refetch: tokio::sync::Mutex<Option<u64>>,
    fetches: AtomicU64,
}

impl JwksCache {
    /// Starts empty; `fetch` fills it.
    pub fn with_fetch(fetch: JwksFetch) -> JwksCache {
        Self::seeded(fetch, &Jwks { keys: Vec::new() })
    }

    fn seeded(fetch: JwksFetch, jwks: &Jwks) -> JwksCache {
        JwksCache {
            fetch,
            published: RwLock::new(Arc::new(Verifier::from_jwks(jwks))),
            refetch: tokio::sync::Mutex::new(None),
            fetches: AtomicU64::new(0),
        }
    }

    /// Keys from the login service at `login_url` (`GET {login_url}{PATH_JWKS}`).
    pub fn from_login(login_url: &str) -> JwksCache {
        let url = format!("{}{}", login_url.trim_end_matches('/'), login_api::PATH_JWKS);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(JWKS_FETCH_TIMEOUT_SECS))
            .build()
            .expect("build the JWKS HTTP client");
        Self::with_fetch(Box::new(move || {
            let client = client.clone();
            let url = url.clone();
            Box::pin(async move {
                let response = client.get(&url).send().await.map_err(|e| format!("GET {url}: {e}"))?;
                let response = response.error_for_status().map_err(|e| format!("GET {url}: {e}"))?;
                response.json::<Jwks>().await.map_err(|e| format!("GET {url}: bad JWKS: {e}"))
            })
        }))
    }

    /// Always these keys (tests): the cache starts with them (no fetch
    /// counted), and every refetch returns them again.
    pub fn fixed(jwks: Jwks) -> JwksCache {
        let seed = jwks.clone();
        Self::seeded(
            Box::new(move || {
                let jwks = jwks.clone();
                Box::pin(async move { Ok(jwks) })
            }),
            &seed,
        )
    }

    /// How many fetches were attempted (after construction).
    pub fn fetches(&self) -> u64 {
        self.fetches.load(Ordering::SeqCst)
    }

    /// The startup fetch: fills the cache if the login service answers. A
    /// failure is logged and does not count against the refetch throttle, so
    /// the first connect tries again.
    pub async fn prefetch(&self, now_secs: u64) {
        let mut last_attempt = self.refetch.lock().await;
        if self.refresh().await {
            *last_attempt = Some(now_secs);
        }
    }

    /// A verifier that knows `kid`: the published one, else (at most every
    /// [`JWKS_REFETCH_MIN_SECS`]) a refetched one. A failed fetch keeps the
    /// published keys.
    pub async fn verifier_for(&self, kid: &str, now_secs: u64) -> Result<Arc<Verifier>, VerifyError> {
        let current = self.current();
        if current.knows(kid) {
            return Ok(current);
        }
        let mut last_attempt = self.refetch.lock().await;
        // A refetch that finished while this one waited may have brought it.
        let current = self.current();
        if current.knows(kid) {
            return Ok(current);
        }
        let due = last_attempt.is_none_or(|at| now_secs >= at.saturating_add(JWKS_REFETCH_MIN_SECS));
        if due {
            *last_attempt = Some(now_secs);
            self.refresh().await;
        }
        drop(last_attempt);
        let current = self.current();
        if current.knows(kid) {
            Ok(current)
        } else {
            Err(VerifyError::UnknownKid(kid.to_string()))
        }
    }

    /// The published verifier, whatever keys it knows. Never waits on a fetch.
    pub fn current(&self) -> Arc<Verifier> {
        self.published.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    /// Fetch and, on success, publish the keys. Whether it succeeded.
    async fn refresh(&self) -> bool {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        match (self.fetch)().await {
            Ok(jwks) => {
                log::info!("[auth] JWKS fetched: {} key(s)", jwks.keys.len());
                let verifier = Arc::new(Verifier::from_jwks(&jwks));
                *self.published.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = verifier;
                true
            }
            Err(e) => {
                log::warn!("[auth] JWKS fetch failed (keeping the cached keys): {e}");
                false
            }
        }
    }
}

/// An attacker chooses the `kid` of a ticket: a refusal (sent back and
/// logged) shows at most this many of its characters.
pub const MAX_KID_SHOWN_CHARS: usize = 64;

/// `error` with an `UnknownKid`'s kid cut to [`MAX_KID_SHOWN_CHARS`].
fn shown(error: VerifyError) -> VerifyError {
    match error {
        VerifyError::UnknownKid(kid) if kid.chars().count() > MAX_KID_SHOWN_CHARS => {
            VerifyError::UnknownKid(format!("{}…", kid.chars().take(MAX_KID_SHOWN_CHARS).collect::<String>()))
        }
        other => other,
    }
}

/// Which ticket epoch a `jwt` host admits.
enum Admission {
    /// No login link (R2a: a world without `host_link`): the newest
    /// `session_epoch` admitted per `player_token`. In memory and never
    /// pruned: one entry per player that ever connected to this process.
    Newest(Mutex<BTreeMap<String, u64>>),
    /// The login service pushes spawn orders: exactly a pending order's epoch, once.
    SpawnOrders(Arc<SpawnOrders>),
}

/// A session the login service's spawn order admitted: reported back
/// ([`Authenticator::session_ended`]) when its connection ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTicket {
    pub player_token: String,
    pub session_epoch: u64,
}

/// A credential the edge accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admitted {
    pub key: PlayerKey,
    /// `Some` for a session admitted against a spawn order.
    pub ticket: Option<SessionTicket>,
}

/// The connect edge's credential check. Shared by every connection task.
pub struct Authenticator {
    mode: AuthMode,
    jwks: Option<JwksCache>,
    admission: Admission,
    now: Clock,
}

impl Authenticator {
    /// A `local` host: players by free name, no login.
    pub fn local() -> Authenticator {
        Authenticator {
            mode: AuthMode::Local,
            jwks: None,
            admission: Admission::Newest(Mutex::default()),
            now: system_clock(),
        }
    }

    /// A `jwt` host for `world`, checking tickets against `jwks` at `now`.
    pub fn jwt(world: impl Into<String>, jwks: JwksCache, now: Clock) -> Authenticator {
        Self::jwt_from(world.into(), String::new(), jwks, now)
    }

    fn jwt_from(world: String, login_url: String, jwks: JwksCache, now: Clock) -> Authenticator {
        Authenticator {
            mode: AuthMode::Jwt { world, login_url },
            jwks: Some(jwks),
            admission: Admission::Newest(Mutex::default()),
            now,
        }
    }

    /// Admit tickets only against `orders` (the login link's spawn orders)
    /// instead of by newest epoch.
    pub fn with_spawn_orders(mut self, orders: Arc<SpawnOrders>) -> Authenticator {
        self.admission = Admission::SpawnOrders(orders);
        self
    }

    /// Whether tickets are admitted against the login link's spawn orders.
    pub fn uses_spawn_orders(&self) -> bool {
        matches!(self.admission, Admission::SpawnOrders(_))
    }

    /// The production authenticator for `mode` (wall clock, HTTP JWKS).
    pub fn for_mode(mode: AuthMode) -> Authenticator {
        match mode {
            AuthMode::Local => Self::local(),
            AuthMode::Jwt { world, login_url } => {
                let jwks = JwksCache::from_login(&login_url);
                Self::jwt_from(world, login_url, jwks, system_clock())
            }
        }
    }

    pub fn mode(&self) -> &AuthMode {
        &self.mode
    }

    /// The JWKS cache of a `jwt` host.
    pub fn jwks(&self) -> Option<&JwksCache> {
        self.jwks.as_ref()
    }

    /// The startup JWKS fetch of a `jwt` host (a no-op for `local`). Never
    /// fails: the server starts without the login service and fetches again
    /// on the first connect.
    pub async fn prefetch(&self) {
        if let Some(jwks) = &self.jwks {
            jwks.prefetch((self.now)()).await;
        }
    }

    /// The player `credential` claims (a `local` host) or proves (a `jwt`
    /// host), or the refusal text. A local name is re-validated: the wire
    /// form is not trusted to have gone through [`PlayerName::parse`].
    pub async fn authenticate(&self, credential: Credential) -> Result<PlayerKey, String> {
        self.admit(credential).await.map(|admitted| admitted.key)
    }

    /// [`authenticate`](Self::authenticate), with the spawn-order session to
    /// report when the connection ends ([`session_ended`](Self::session_ended)).
    pub async fn admit(&self, credential: Credential) -> Result<Admitted, String> {
        match (&self.mode, credential) {
            (AuthMode::Local, Credential::Local(name)) => match PlayerName::parse(&name.0) {
                Ok(name) => Ok(Admitted { key: PlayerKey::Local(name), ticket: None }),
                Err(why) => Err(format!("invalid player name: {why}")),
            },
            (AuthMode::Local, Credential::Jwt(_)) => Err(REFUSE_JWT_ON_LOCAL_HOST.into()),
            (AuthMode::Jwt { .. }, Credential::Local(_)) => Err(REFUSE_LOCAL_ON_JWT_HOST.into()),
            (AuthMode::Jwt { world, .. }, Credential::Jwt(jwt)) => self.check_jwt(world, &jwt).await,
        }
    }

    /// The connection of a session admitted with `ticket` ended (or never
    /// started): the login link forgets it and tells the login service.
    pub fn session_ended(&self, ticket: &SessionTicket) {
        if let Admission::SpawnOrders(orders) = &self.admission {
            orders.ended(&ticket.player_token, ticket.session_epoch);
        }
    }

    async fn check_jwt(&self, world: &str, jwt: &str) -> Result<Admitted, String> {
        if jwt.len() > game::MAX_JWT_BYTES {
            return Err(REFUSE_TOO_LARGE.into());
        }
        let jwks = self.jwks.as_ref().expect("a jwt host has a JWKS cache");
        let now = (self.now)();
        let claims = match jwks.current().verify(jwt, now, world) {
            Err(VerifyError::UnknownKid(kid)) => match jwks.verifier_for(&kid, now).await {
                Ok(verifier) => verifier.verify(jwt, now, world),
                Err(e) => Err(e),
            },
            checked => checked,
        }
        .map_err(|e| shown(e).to_string())?;
        let token = PlayerToken(claims.player_token.clone());
        if !token.is_valid() {
            log::warn!("[auth] ticket of account {} carries a malformed player token", claims.sub);
            return Err("login ticket carries an invalid player token".into());
        }
        let epochs = match &self.admission {
            Admission::SpawnOrders(orders) => {
                return match orders.admit(&claims.player_token, claims.session_epoch) {
                    Ok(()) => {
                        log::info!("[auth] account {} admitted by its spawn order (epoch {})", claims.sub, claims.session_epoch);
                        let ticket = SessionTicket { player_token: claims.player_token, session_epoch: claims.session_epoch };
                        Ok(Admitted { key: PlayerKey::Online(token), ticket: Some(ticket) })
                    }
                    Err(why) => {
                        log::info!("[auth] account {} ticket epoch {} refused: {why}", claims.sub, claims.session_epoch);
                        Err(why.into())
                    }
                };
            }
            Admission::Newest(epochs) => epochs,
        };
        // Checked and recorded under one lock: of two racing tickets the
        // older can never be admitted after the newer.
        let mut epochs = epochs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let newest = epochs.entry(claims.player_token).or_insert(claims.session_epoch);
        if claims.session_epoch < *newest {
            log::info!(
                "[auth] account {} ticket epoch {} refused: epoch {} already admitted",
                claims.sub,
                claims.session_epoch,
                *newest
            );
            return Err(REFUSE_SUPERSEDED.into());
        }
        *newest = claims.session_epoch;
        log::info!("[auth] account {} admitted (epoch {})", claims.sub, claims.session_epoch);
        Ok(Admitted { key: PlayerKey::Online(token), ticket: None })
    }
}
