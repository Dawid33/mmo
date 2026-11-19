//! `LISTEN` / `NOTIFY` channels of the store, each heard on one direct, unpooled connection of
//! its own, on a listener thread of its own (its own current-thread runtime)
//! ([`crate::StoreHandle::listen_channel`]). A notice is only a hint: a missed one is covered by
//! the [`Notice::Resync`] emitted on every (re)connect and every poll period; a duplicate costs
//! one read.
//!
//! Two channels:
//! - **Stored-terrain notices** (R3): each committed world cut is announced with
//!   `pg_notify('world_cut', '<cut>,<tick>')` ([`crate::StoreHandle::notify_cut`], its own
//!   transaction, never inside a cut's), and every host listens
//!   ([`crate::StoreHandle::listen_cuts`]: [`CutNotice`]s, a `Resync` every
//!   [`TERRAIN_POLL_SECS`]).
//! - **The coordinator's row** (Elastic 2b Task 5): every committed transition says
//!   `pg_notify('coordinator_row', '<version>')` in its own transaction
//!   ([`COORDINATOR_ROW_CHANNEL`]); each coordinator replica refreshes its cache on a version
//!   newer than its own and relays the outbox rows other replicas left for its hosts (a
//!   `Resync` every [`COORDINATOR_POLL_MS`]).

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crossbeam::channel::{Receiver, Sender};
use game::Tick;

/// The `LISTEN` / `NOTIFY` channel of committed world cuts.
pub const WORLD_CUT_CHANNEL: &str = "world_cut";
/// The `LISTEN` / `NOTIFY` channel of the coordinator's row: the payload is the version a
/// transition committed as.
pub const COORDINATOR_ROW_CHANNEL: &str = "coordinator_row";
/// How often a coordinator replica's row listener says [`Notice::Resync`] anyway, and the
/// backstop of a replica's relay (only while more than one replica is live): a lost notice
/// delays a relayed message at most this long.
pub const COORDINATOR_POLL_MS: u64 = 1_000;
/// How often a terrain listener says [`CutNotice::Resync`] anyway: the backstop for a notice
/// lost (a dropped connection, a notifier that died between its commit and its notify).
pub const TERRAIN_POLL_SECS: u64 = 30;
/// The listener connection's `application_name` (what an operator, or a test, finds it by).
pub const TERRAIN_LISTENER_APP: &str = "mmo-terrain-listener";
/// `listen` (default) or `poll`: whether hosts `LISTEN` for cut notices or only poll.
pub const TERRAIN_NOTIFY_ENV: &str = "VOXEL_TERRAIN_NOTIFY";
/// A direct Postgres URL for the listener (a pooler in front of the store's URL — e.g.
/// PgBouncer in transaction mode — does not carry `LISTEN`); unset: the store's URL.
pub const WORLD_DB_DIRECT_ENV: &str = "VOXEL_WORLD_DB_DIRECT";
/// The wait before a listener that could not connect, or whose connection lived less than
/// [`LISTEN_MIN_LIFE`], tries again.
const LISTEN_RETRY: Duration = Duration::from_secs(1);
/// A listener connection lost sooner than this after it was made (a proxy or an
/// `idle_session_timeout` ending sessions at once) waits [`LISTEN_RETRY`] before the next:
/// no tight reconnect loop, no back-to-back `Resync`s on every host.
const LISTEN_MIN_LIFE: Duration = Duration::from_secs(1);
/// At most one warning per this long about malformed `world_cut` payloads (any database
/// user can `NOTIFY` the channel).
const MALFORMED_WARN: Duration = Duration::from_secs(30);

/// What a channel's listener says ([`crate::StoreHandle::listen_channel`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// One notice, its payload as sent (any database user can `NOTIFY`: parse it with care).
    Payload(String),
    /// Notices may have been missed (a (re)connect, or the poll period passed).
    Resync,
}

/// What a host's terrain listener says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutNotice {
    /// A world cut at `tick` committed: the regions saved at `tick` or later
    /// ([`crate::StoreHandle::stored_regions_since`]) may have new terrain.
    Cut { tick: Tick },
    /// Notices may have been missed (a (re)connect, or the poll period passed): read every
    /// stored region's version again ([`crate::StoreHandle::stored_regions`]).
    Resync,
}

/// How a listener learns of notices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NotifyMode {
    /// `LISTEN` on the channel, plus the poll's `Resync`.
    #[default]
    Listen,
    /// Only the poll's `Resync` (no listener connection).
    Poll,
}

impl NotifyMode {
    /// [`TERRAIN_NOTIFY_ENV`]'s value: `listen` (also unset or empty) or `poll`.
    pub fn parse(value: Option<&str>) -> Result<NotifyMode, String> {
        match value.map(str::trim).unwrap_or("") {
            "" | "listen" => Ok(NotifyMode::Listen),
            "poll" => Ok(NotifyMode::Poll),
            other => Err(format!("{TERRAIN_NOTIFY_ENV}={other:?}: expected `listen` or `poll`")),
        }
    }

    /// [`Self::parse`] of [`TERRAIN_NOTIFY_ENV`].
    pub fn from_env() -> Result<NotifyMode, String> {
        Self::parse(std::env::var(TERRAIN_NOTIFY_ENV).ok().as_deref())
    }
}

/// The payload of a cut's notice.
pub(crate) fn payload(cut: game::CutId, tick: Tick) -> String {
    format!("{},{tick}", cut.0)
}

/// What a notice's payload (`<cut>,<tick>`) says; a malformed one: resync.
fn notice_of(payload: &str) -> CutNotice {
    let parsed = payload
        .split_once(',')
        .and_then(|(c, t)| c.trim().parse::<u64>().ok().and(t.trim().parse::<Tick>().ok()));
    match parsed {
        Some(tick) => CutNotice::Cut { tick },
        None => CutNotice::Resync,
    }
}

/// The terrain listener's reading of a [`Notice`] on [`WORLD_CUT_CHANNEL`]: a malformed payload
/// is a `Resync`, warned about at most once per [`MALFORMED_WARN`].
pub(crate) fn cut_notices() -> impl Fn(Notice) -> CutNotice + Send + Sync + 'static {
    let warned: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    move |n| match n {
        Notice::Resync => CutNotice::Resync,
        Notice::Payload(p) => {
            let notice = notice_of(&p);
            if notice == CutNotice::Resync {
                let mut warned = warned.lock().unwrap_or_else(|e| e.into_inner());
                if warned.is_none_or(|w| w.elapsed() >= MALFORMED_WARN) {
                    *warned = Some(std::time::Instant::now());
                    log::warn!("[world-store] a malformed world_cut notice ({p:?}); resyncing");
                }
            }
            notice
        }
    }
}

/// How long to wait before the next connection, after one that lived `lived`.
fn reconnect_wait(lived: Duration) -> Duration {
    if lived < LISTEN_MIN_LIFE { LISTEN_RETRY } else { Duration::ZERO }
}

/// Where the listener connects, for logs: host and database, never the URL (it holds a
/// password).
fn describe(url: &str) -> String {
    crate::url::redact_url(url)
}

/// The poll period shared by a handle's clones (test setter: [`crate::StoreHandle::set_terrain_poll_period`]).
pub(crate) fn default_poll_ms() -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(TERRAIN_POLL_SECS * 1000))
}

/// The terrain poll period of [`default_poll_ms`]'s cell.
pub(crate) fn poll_period(poll_ms: &AtomicU64) -> Duration {
    Duration::from_millis(poll_ms.load(Ordering::SeqCst).max(1))
}

/// Starts a listener thread named `name` on `channel`, each [`Notice`] read through `map`:
/// `url` is `None` for a store that cannot listen (SQLite: a receiver that never says anything).
/// A `Resync` every `period` in both modes. The thread ends once the receiver is dropped (seen at
/// its next send).
pub(crate) fn start<T: Send + 'static>(
    url: Option<String>,
    name: &str,
    channel: &'static str,
    mode: NotifyMode,
    period: Duration,
    map: impl Fn(Notice) -> T + Send + Sync + 'static,
) -> Receiver<T> {
    let Some(url) = url else { return crossbeam::channel::never() };
    let (tx, rx) = crossbeam::channel::unbounded();
    let map = Arc::new(map);
    let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => return log::error!("[world-store] the {channel} listener's runtime: {e}; no notices"),
        };
        rt.block_on(async move {
            if mode == NotifyMode::Listen {
                tokio::spawn(listen(url, channel, tx.clone(), map.clone()));
            } else if tx.send(map(Notice::Resync)).is_err() {
                return;
            }
            // The poll: ends the thread (and the listen task with its runtime) once nobody
            // reads the notices.
            let mut every = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            loop {
                every.tick().await;
                if tx.send(map(Notice::Resync)).is_err() {
                    return;
                }
            }
        });
    });
    if let Err(e) = spawned {
        log::error!("[world-store] spawn the {channel} listener: {e}; only the poll's reads");
    }
    rx
}

/// One listener connection after another: `Resync` once each is listening, a `Payload` per
/// notice.
async fn listen<T, F>(url: String, channel: &'static str, tx: Sender<T>, map: Arc<F>)
where
    T: Send + 'static,
    F: Fn(Notice) -> T + Send + Sync + 'static,
{
    let at = describe(&url);
    let mut failing = false;
    loop {
        let mut listener = match connect(&url, channel).await {
            Ok(l) => l,
            Err(e) => {
                if !failing {
                    log::warn!("[world-store] the {channel} listener cannot connect to {at} ({e}); retrying every {LISTEN_RETRY:?}");
                }
                failing = true;
                tokio::time::sleep(LISTEN_RETRY).await;
                continue;
            }
        };
        if failing {
            log::info!("[world-store] the {channel} listener is connected to {at} again");
        }
        failing = false;
        if tx.send(map(Notice::Resync)).is_err() {
            return;
        }
        let connected = std::time::Instant::now();
        loop {
            let notice = match listener.try_recv().await {
                Ok(Some(n)) => Notice::Payload(n.payload().to_string()),
                // The connection was lost (not made again: `eager_reconnect(false)`); what
                // is sent until the next one listens is lost — its `Resync` covers it.
                Ok(None) => {
                    log::warn!("[world-store] the {channel} listener lost {at}; reconnecting");
                    break;
                }
                Err(e) => {
                    log::warn!("[world-store] the {channel} listener lost {at} ({e}); reconnecting");
                    break;
                }
            };
            if tx.send(map(notice)).is_err() {
                return;
            }
        }
        drop(listener);
        tokio::time::sleep(reconnect_wait(connected.elapsed())).await;
    }
}

/// A listener on one connection of its own (a pool of one, never shared), listening on
/// `channel`.
async fn connect(url: &str, channel: &str) -> Result<sqlx::postgres::PgListener, sqlx::Error> {
    let options = url.parse::<sqlx::postgres::PgConnectOptions>()?.application_name(TERRAIN_LISTENER_APP);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .min_connections(0)
        .max_lifetime(None)
        .idle_timeout(None)
        .connect_with(options)
        .await?;
    let mut listener = sqlx::postgres::PgListener::connect_with(&pool).await?;
    listener.ignore_pool_close_event(true);
    // A lost connection is made again by `listen` (with its backoff), not inside `try_recv`.
    listener.eager_reconnect(false);
    listener.listen(channel).await?;
    Ok(listener)
}

