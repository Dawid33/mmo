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
use game::{ClientId, Credential, PlayerId, PlayerName, PlayerToken};

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
pub const REFUSE_JWT_ON_LOCAL_HOST: &str = "this server does not use login (single-player / dev)";
/// The refusal a `jwt` host gives a player name.
pub const REFUSE_LOCAL_ON_JWT_HOST: &str = "this server requires login";
/// The refusal of an observer ticket ([`Credential::Observer`]) this host has no valid
/// order for: unknown, expired or a wrong nonce — one text for all three.
pub const REFUSE_OBSERVER: &str = "observer ticket not valid on this host";
/// Most observer orders an edge keeps ([`ObserverOrders`]): past it the order that expires
/// first goes.
pub const OBSERVER_ORDERS_MAX: usize = 4096;
/// Most observer orders an edge keeps for one client (the vouching host mints a new ticket
/// once half of one's life is gone): past it that client's order that expires first goes.
pub const OBSERVER_ORDERS_PER_CLIENT: usize = 4;
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
    /// The owner-launched child server for a native single-player world.
    SinglePlayer,
    Local,
    Jwt {
        world: String,
        login_url: String,
    },
}

impl AuthMode {
    /// From [`AUTH_ENV`], [`WORLD_ID_ENV`] and [`LOGIN_URL_ENV`]. `Err` names
    /// the variable at fault; the server refuses to start with it.
    pub fn from_env() -> Result<AuthMode, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// The mode of this server process. A single-player server (the one a
    /// client spawns for its own world) uses its trusted owner mode: it
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
        Ok((AuthMode::SinglePlayer, warning))
    }

    /// [`from_env`](Self::from_env) over any variable lookup.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<AuthMode, String> {
        let required = |name: &str| match lookup(name) {
            Some(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
            _ => Err(format!("{AUTH_ENV}=jwt requires {name}")),
        };
        match lookup(AUTH_ENV).as_deref().map(str::trim) {
            None | Some("") | Some("local") | Some("token") => Ok(AuthMode::Local),
            Some("jwt") => Ok(AuthMode::Jwt {
                world: required(WORLD_ID_ENV)?,
                login_url: required(LOGIN_URL_ENV)?,
            }),
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
        let url = format!(
            "{}{}",
            login_url.trim_end_matches('/'),
            login_api::PATH_JWKS
        );
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(JWKS_FETCH_TIMEOUT_SECS))
            .build()
            .expect("build the JWKS HTTP client");
        Self::with_fetch(Box::new(move || {
            let client = client.clone();
            let url = url.clone();
            Box::pin(async move {
                let response = client
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| format!("GET {url}: {e}"))?;
                let response = response
                    .error_for_status()
                    .map_err(|e| format!("GET {url}: {e}"))?;
                response
                    .json::<Jwks>()
                    .await
                    .map_err(|e| format!("GET {url}: bad JWKS: {e}"))
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
    pub async fn verifier_for(
        &self,
        kid: &str,
        now_secs: u64,
    ) -> Result<Arc<Verifier>, VerifyError> {
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
        let due =
            last_attempt.is_none_or(|at| now_secs >= at.saturating_add(JWKS_REFETCH_MIN_SECS));
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
        self.published
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Fetch and, on success, publish the keys. Whether it succeeded.
    async fn refresh(&self) -> bool {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        match (self.fetch)().await {
            Ok(jwks) => {
                log::info!("[auth] JWKS fetched: {} key(s)", jwks.keys.len());
                let verifier = Arc::new(Verifier::from_jwks(&jwks));
                *self
                    .published
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = verifier;
                true
            }
            Err(e) => {
                log::warn!("[auth] JWKS fetch failed (keeping the cached keys): {e}");
                false
            }
        }
    }
}

/// One observer order: a ticket another host handed `client` for this host.
struct ObserverOrder {
    player: PlayerId,
    nonce: u64,
    expires_unix_secs: u64,
}

/// The observer tickets this host admits ([`Credential::Observer`]): the orders other hosts
/// sent it (`Reliable::ObserverOrder`, read by the main loop from
/// [`game::WorldManager::take_observer_orders`] each pass). An order admits its ticket any
/// number of times until it expires. Bounded: expired orders are pruned on every record,
/// and at most [`OBSERVER_ORDERS_PER_CLIENT`] per client and [`OBSERVER_ORDERS_MAX`] in
/// all are kept. Nothing here logs a nonce.
pub struct ObserverOrders {
    now: Clock,
    orders: Mutex<BTreeMap<ClientId, Vec<ObserverOrder>>>,
}

impl ObserverOrders {
    /// No orders yet; `now` is this edge's clock (expiry is checked against it).
    pub fn new(now: Clock) -> ObserverOrders {
        ObserverOrders {
            now,
            orders: Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<ClientId, Vec<ObserverOrder>>> {
        self.orders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Admit `nonce` for `client`'s `player` until `expires_unix_secs`.
    pub fn record(&self, client: ClientId, player: PlayerId, nonce: u64, expires_unix_secs: u64) {
        let now = (self.now)();
        let mut orders = self.lock();
        orders.retain(|_, list| {
            list.retain(|o| o.expires_unix_secs > now);
            !list.is_empty()
        });
        if expires_unix_secs <= now {
            return;
        }
        let list = orders.entry(client).or_default();
        list.push(ObserverOrder {
            player,
            nonce,
            expires_unix_secs,
        });
        if list.len() > OBSERVER_ORDERS_PER_CLIENT {
            let first = list
                .iter()
                .enumerate()
                .min_by_key(|(_, o)| o.expires_unix_secs)
                .map(|(i, _)| i);
            list.remove(first.expect("not empty"));
        }
        while orders.values().map(Vec::len).sum::<usize>() > OBSERVER_ORDERS_MAX {
            let first = orders
                .iter()
                .flat_map(|(c, list)| {
                    list.iter()
                        .enumerate()
                        .map(move |(i, o)| (o.expires_unix_secs, *c, i))
                })
                .min()
                .expect("not empty");
            let list = orders.get_mut(&first.1).expect("listed");
            list.remove(first.2);
            if list.is_empty() {
                orders.remove(&first.1);
            }
        }
    }

    /// The orders the manager took since its last call (`(client, player, nonce, ttl_secs)`,
    /// [`game::WorldManager::take_observer_orders`]), each valid for its `ttl_secs` from now
    /// by this edge's clock.
    pub fn record_all(&self, orders: Vec<(ClientId, PlayerId, u64, u64)>) {
        let now = (self.now)();
        for (client, player, nonce, ttl_secs) in orders {
            // A peer's ttl is never trusted past the protocol's.
            let ttl_secs = ttl_secs.min(game::OBSERVER_ORDER_TTL_SECS);
            self.record(client, player, nonce, now.saturating_add(ttl_secs));
        }
    }

    /// `client`'s session here is its home: its orders go (its tickets admit nothing more).
    pub fn revoke(&self, client: ClientId) {
        self.lock().remove(&client);
    }

    /// The player of `client`'s order for `nonce`, if one is valid at `now_secs`. The nonce
    /// is compared in constant time with every order of the client; an unknown client, a
    /// wrong nonce and an expired order give the same refusal.
    pub fn admit(
        &self,
        client: ClientId,
        nonce: u64,
        now_secs: u64,
    ) -> Result<PlayerId, &'static str> {
        let orders = self.lock();
        let mut found = None;
        for order in orders.get(&client).map(Vec::as_slice).unwrap_or_default() {
            let valid = nonce_eq(order.nonce, nonce) & (order.expires_unix_secs > now_secs);
            if valid {
                found = Some(order.player);
            }
        }
        found.ok_or(REFUSE_OBSERVER)
    }

    /// How many orders are kept (test support).
    pub fn len(&self) -> usize {
        self.lock().values().map(Vec::len).sum()
    }

    /// Whether no order is kept.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `a == b` without a data-dependent branch.
fn nonce_eq(a: u64, b: u64) -> bool {
    let d = std::hint::black_box(a ^ b);
    // (d | -d) has its top bit set exactly when d != 0.
    ((d | d.wrapping_neg()) >> 63) == 0
}

/// An attacker chooses the `kid` of a ticket: a refusal (sent back and
/// logged) shows at most this many of its characters.
pub const MAX_KID_SHOWN_CHARS: usize = 64;

/// `error` with an `UnknownKid`'s kid cut to [`MAX_KID_SHOWN_CHARS`].
fn shown(error: VerifyError) -> VerifyError {
    match error {
        VerifyError::UnknownKid(kid) if kid.chars().count() > MAX_KID_SHOWN_CHARS => {
            VerifyError::UnknownKid(format!(
                "{}…",
                kid.chars().take(MAX_KID_SHOWN_CHARS).collect::<String>()
            ))
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

/// Who an accepted credential is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmittedAs {
    /// A player by its key (a name, or a login ticket's token): the store resolves it.
    Player(PlayerKey),
    /// An observer ticket another host vouched for ([`ObserverOrders`]): the connection's
    /// id IS `client` (the id that host knows the session by), its player `player`. No
    /// store, no login.
    Observer { client: ClientId, player: PlayerId },
}

/// A credential the edge accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admitted {
    pub who: AdmittedAs,
    /// `Some` for a session admitted against a spawn order.
    pub ticket: Option<SessionTicket>,
    /// The ticket's `session_epoch` (`Some` for every `jwt` session): with the player, the
    /// session's world-unique [`game::ClientId`].
    pub session_epoch: Option<u64>,
    /// The join-time capability authorized by the coordinator, or trusted
    /// directly by an owner-launched single-player server.
    pub editor_allowed: bool,
}

/// The connect edge's credential check. Shared by every connection task.
pub struct Authenticator {
    mode: AuthMode,
    jwks: Option<JwksCache>,
    admission: Admission,
    /// Several hosts: the observer tickets other hosts vouched for (`None`: every observer
    /// ticket is refused).
    observers: Option<Arc<ObserverOrders>>,
    now: Clock,
}

impl Authenticator {
    /// A `local` host: players by free name, no login.
    pub fn local() -> Authenticator {
        Authenticator {
            mode: AuthMode::Local,
            jwks: None,
            admission: Admission::Newest(Mutex::default()),
            observers: None,
            now: system_clock(),
        }
    }

    /// The owner-launched local child: local names are trusted with editor access.
    pub fn single_player() -> Authenticator {
        Authenticator {
            mode: AuthMode::SinglePlayer,
            jwks: None,
            admission: Admission::Newest(Mutex::default()),
            observers: None,
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
            observers: None,
            now,
        }
    }

    /// Admit tickets only against `orders` (the login link's spawn orders)
    /// instead of by newest epoch.
    pub fn with_spawn_orders(mut self, orders: Arc<SpawnOrders>) -> Authenticator {
        self.admission = Admission::SpawnOrders(orders);
        self
    }

    /// Admit observer tickets against `orders` (several hosts).
    pub fn with_observer_orders(mut self, orders: Arc<ObserverOrders>) -> Authenticator {
        self.observers = Some(orders);
        self
    }

    /// Whether tickets are admitted against the login link's spawn orders.
    pub fn uses_spawn_orders(&self) -> bool {
        matches!(self.admission, Admission::SpawnOrders(_))
    }

    /// The production authenticator for `mode` (wall clock, HTTP JWKS).
    pub fn for_mode(mode: AuthMode) -> Authenticator {
        match mode {
            AuthMode::SinglePlayer => Self::single_player(),
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
        match self.admit(credential).await?.who {
            AdmittedAs::Player(key) => Ok(key),
            AdmittedAs::Observer { .. } => Err("an observer ticket names no player key".into()),
        }
    }

    /// [`authenticate`](Self::authenticate), with the spawn-order session to
    /// report when the connection ends ([`session_ended`](Self::session_ended)).
    pub async fn admit(&self, credential: Credential) -> Result<Admitted, String> {
        match (&self.mode, credential) {
            (mode @ (AuthMode::SinglePlayer | AuthMode::Local), Credential::Local(name)) => {
                match PlayerName::parse(&name.0) {
                    Ok(name) => Ok(Admitted {
                        who: AdmittedAs::Player(PlayerKey::Local(name)),
                        ticket: None,
                        session_epoch: None,
                        editor_allowed: matches!(mode, AuthMode::SinglePlayer),
                    }),
                    Err(why) => Err(format!("invalid player name: {why}")),
                }
            }
            (AuthMode::SinglePlayer | AuthMode::Local, Credential::Jwt(_)) => {
                Err(REFUSE_JWT_ON_LOCAL_HOST.into())
            }
            (AuthMode::Jwt { .. }, Credential::Local(_)) => Err(REFUSE_LOCAL_ON_JWT_HOST.into()),
            (AuthMode::Jwt { world, .. }, Credential::Jwt(jwt)) => {
                self.check_jwt(world, &jwt).await
            }
            // Another host's redirect: whatever this host's mode, only that host's order admits it.
            (_, Credential::Observer { client, nonce }) => {
                let admitted = match &self.observers {
                    // Checked by the clock the order's expiry was set by.
                    Some(orders) => orders.admit(client, nonce, (orders.now)()),
                    None => Err(REFUSE_OBSERVER),
                };
                match admitted {
                    Ok(player) => {
                        log::info!(
                            "[auth] client {client} admitted as an observer (player {})",
                            player.0
                        );
                        Ok(Admitted {
                            who: AdmittedAs::Observer { client, player },
                            ticket: None,
                            session_epoch: None,
                            editor_allowed: false,
                        })
                    }
                    Err(why) => {
                        log::info!("[auth] observer ticket of client {client} refused");
                        Err(why.into())
                    }
                }
            }
        }
    }

    /// A session admitted with `ticket` never started (refused before or at its
    /// announcement): the login link forgets it and tells the login service.
    pub fn session_ended(&self, ticket: &SessionTicket) {
        if let Admission::SpawnOrders(orders) = &self.admission {
            orders.ended(&ticket.player_token, ticket.session_epoch);
        }
    }

    /// The connection of an announced session admitted with `ticket` closed: the login link
    /// forgets it. Role `all` tells the login service here; role `host` leaves that to the
    /// manager, which knows whether the session left the world or only this host
    /// ([`SpawnOrders::closed`]).
    pub fn session_closed(&self, ticket: &SessionTicket) {
        if let Admission::SpawnOrders(orders) = &self.admission {
            orders.closed(&ticket.player_token, ticket.session_epoch);
        }
    }

    /// Whether login sessions are announced to the manager with their login
    /// ([`game::ServerEvent::LoginConnected`]): role `host`, whose relay takes the login service's
    /// orders from the coordinator ([`SpawnOrders::relay_through`]).
    pub fn relays_logins(&self) -> bool {
        matches!(&self.admission, Admission::SpawnOrders(orders) if orders.relays())
    }

    /// Why the session admitted with `ticket` may no longer play (a kick's
    /// reason, or a newer login of the player), if it may not.
    pub fn revoked(&self, ticket: &SessionTicket) -> Option<String> {
        match &self.admission {
            Admission::SpawnOrders(orders) => {
                orders.revoked(&ticket.player_token, ticket.session_epoch)
            }
            Admission::Newest(_) => None,
        }
    }

    /// Announce a session: `connected` (its `ClientConnected`) is called — for
    /// a spawn-order ticket under the orders' lock
    /// ([`SpawnOrders::announce`]) — unless the ticket was revoked meanwhile
    /// (the refusal is returned; nothing is announced).
    pub fn announce(
        &self,
        ticket: Option<&SessionTicket>,
        connected: impl FnOnce(),
    ) -> Result<(), String> {
        match (&self.admission, ticket) {
            (Admission::SpawnOrders(orders), Some(t)) => {
                orders.announce(&t.player_token, t.session_epoch, connected)
            }
            _ => {
                connected();
                Ok(())
            }
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
            log::warn!("[auth] ticket carries a malformed player token");
            return Err("login ticket carries an invalid player token".into());
        }
        let epochs = match &self.admission {
            Admission::SpawnOrders(orders) => {
                return match orders.admit(&claims.player_token, claims.session_epoch) {
                    Ok(editor_allowed) => {
                        log::info!(
                            "[auth] ticket admitted by its spawn order (epoch {})",
                            claims.session_epoch
                        );
                        let ticket = SessionTicket {
                            player_token: claims.player_token,
                            session_epoch: claims.session_epoch,
                        };
                        Ok(Admitted {
                            who: AdmittedAs::Player(PlayerKey::Online(token)),
                            ticket: Some(ticket),
                            session_epoch: Some(claims.session_epoch),
                            editor_allowed,
                        })
                    }
                    Err(why) => {
                        log::info!(
                            "[auth] ticket epoch {} refused: {why}",
                            claims.session_epoch
                        );
                        Err(why.into())
                    }
                };
            }
            Admission::Newest(epochs) => epochs,
        };
        // Checked and recorded under one lock: of two racing tickets the
        // older can never be admitted after the newer.
        let mut epochs = epochs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let newest = epochs
            .entry(claims.player_token)
            .or_insert(claims.session_epoch);
        if claims.session_epoch < *newest {
            log::info!(
                "[auth] ticket epoch {} refused: epoch {} already admitted",
                claims.session_epoch,
                *newest
            );
            return Err(REFUSE_SUPERSEDED.into());
        }
        *newest = claims.session_epoch;
        log::info!("[auth] ticket admitted (epoch {})", claims.session_epoch);
        Ok(Admitted {
            who: AdmittedAs::Player(PlayerKey::Online(token)),
            ticket: None,
            session_epoch: Some(claims.session_epoch),
            editor_allowed: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{Credential, PlayerName};

    /// Regression: `/creative` after `just run --reset` was denied because the
    /// owner-launched native child used ordinary local-name admission (seen in
    /// play; single-player is trusted explicitly while LAN remains deny-by-default).
    #[tokio::test]
    async fn single_player_owner_gets_editor_permission_but_lan_does_not() {
        let name = PlayerName::parse("owner").unwrap();
        let (single_player, warning) = AuthMode::resolve(true, |_| None).unwrap();
        assert!(warning.is_none());
        let owner = Authenticator::for_mode(single_player)
            .admit(Credential::Local(name.clone()))
            .await
            .unwrap();
        assert!(owner.editor_allowed);

        let (local, warning) = AuthMode::resolve(false, |_| None).unwrap();
        assert!(warning.is_none());
        let lan = Authenticator::for_mode(local)
            .admit(Credential::Local(name))
            .await
            .unwrap();
        assert!(!lan.editor_allowed);
    }
}
