//! Stored-terrain notices (R3): each committed world cut is announced with
//! `pg_notify('world_cut', '<cut>,<tick>')` ([`crate::StoreHandle::notify_cut`], its own
//! transaction, never inside a cut's), and every host listens
//! ([`crate::StoreHandle::listen_cuts`]) on one direct, unpooled connection of its own, on
//! its own `terrain-listener` thread (its own current-thread runtime). A notice is only a
//! hint to re-read committed rows: a missed one is covered by the [`CutNotice::Resync`]
//! emitted on every (re)connect and every [`TERRAIN_POLL_SECS`]; a duplicate costs one read.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crossbeam::channel::{Receiver, Sender};
use game::Tick;

/// The `LISTEN` / `NOTIFY` channel of committed world cuts.
pub const WORLD_CUT_CHANNEL: &str = "world_cut";
/// How often a listener says [`CutNotice::Resync`] anyway: the backstop for a notice lost
/// (a dropped connection, a notifier that died between its commit and its notify).
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

/// What a host's listener says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutNotice {
    /// A world cut at `tick` committed: the regions saved at `tick` or later
    /// ([`crate::StoreHandle::stored_regions_since`]) may have new terrain.
    Cut { tick: Tick },
    /// Notices may have been missed (a (re)connect, or the poll period passed): read every
    /// stored region's version again ([`crate::StoreHandle::stored_regions`]).
    Resync,
}

/// How a host learns of committed cuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NotifyMode {
    /// `LISTEN` on [`WORLD_CUT_CHANNEL`], plus the poll's `Resync`.
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

/// How long to wait before the next connection, after one that lived `lived`.
fn reconnect_wait(lived: Duration) -> Duration {
    if lived < LISTEN_MIN_LIFE { LISTEN_RETRY } else { Duration::ZERO }
}

/// Where the listener connects, for logs: host and database, never the URL (it holds a
/// password).
fn describe(url: &str) -> String {
    match url.parse::<sqlx::postgres::PgConnectOptions>() {
        Ok(o) => format!("{}/{}", o.get_host(), o.get_database().unwrap_or("?")),
        Err(_) => "an unparsable URL".into(),
    }
}

/// The poll period shared by a handle's clones (test setter: [`crate::StoreHandle::set_terrain_poll_period`]).
pub(crate) fn default_poll_ms() -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(TERRAIN_POLL_SECS * 1000))
}

/// Starts the `terrain-listener` thread: `url` is `None` for a store that cannot listen
/// (SQLite: a receiver that never says anything). The thread ends once the receiver is
/// dropped (seen at its next send).
pub(crate) fn start(url: Option<String>, mode: NotifyMode, poll_ms: &AtomicU64) -> Receiver<CutNotice> {
    let Some(url) = url else { return crossbeam::channel::never() };
    let period = Duration::from_millis(poll_ms.load(Ordering::SeqCst).max(1));
    let (tx, rx) = crossbeam::channel::unbounded();
    let spawned = std::thread::Builder::new().name("terrain-listener".into()).spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => return log::error!("[world-store] the terrain listener's runtime: {e}; no cut notices"),
        };
        rt.block_on(async move {
            if mode == NotifyMode::Listen {
                tokio::spawn(listen(url, tx.clone()));
            } else if tx.send(CutNotice::Resync).is_err() {
                return;
            }
            // The poll: ends the thread (and the listen task with its runtime) once nobody
            // reads the notices.
            let mut every = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            loop {
                every.tick().await;
                if tx.send(CutNotice::Resync).is_err() {
                    return;
                }
            }
        });
    });
    if let Err(e) = spawned {
        log::error!("[world-store] spawn the terrain listener: {e}; only the store reads at start");
    }
    rx
}

/// One listener connection after another: `Resync` once each is listening, `Cut` per notice.
async fn listen(url: String, tx: Sender<CutNotice>) {
    let at = describe(&url);
    let mut failing = false;
    let mut malformed_warned: Option<std::time::Instant> = None;
    loop {
        let mut listener = match connect(&url).await {
            Ok(l) => l,
            Err(e) => {
                if !failing {
                    log::warn!("[world-store] the terrain listener cannot connect to {at} ({e}); retrying every {LISTEN_RETRY:?}");
                }
                failing = true;
                tokio::time::sleep(LISTEN_RETRY).await;
                continue;
            }
        };
        if failing {
            log::info!("[world-store] the terrain listener is connected to {at} again");
        }
        failing = false;
        if tx.send(CutNotice::Resync).is_err() {
            return;
        }
        let connected = std::time::Instant::now();
        loop {
            let notice = match listener.try_recv().await {
                Ok(Some(n)) => {
                    let notice = notice_of(n.payload());
                    if notice == CutNotice::Resync && malformed_warned.is_none_or(|w| w.elapsed() >= MALFORMED_WARN) {
                        malformed_warned = Some(std::time::Instant::now());
                        log::warn!("[world-store] a malformed world_cut notice ({:?}); resyncing", n.payload());
                    }
                    notice
                }
                // The connection was lost (not made again: `eager_reconnect(false)`); what
                // is sent until the next one listens is lost — its `Resync` covers it.
                Ok(None) => {
                    log::warn!("[world-store] the terrain listener lost {at}; reconnecting");
                    break;
                }
                Err(e) => {
                    log::warn!("[world-store] the terrain listener lost {at} ({e}); reconnecting");
                    break;
                }
            };
            if tx.send(notice).is_err() {
                return;
            }
        }
        drop(listener);
        tokio::time::sleep(reconnect_wait(connected.elapsed())).await;
    }
}

/// A listener on one connection of its own (a pool of one, never shared), listening.
async fn connect(url: &str) -> Result<sqlx::postgres::PgListener, sqlx::Error> {
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
    listener.listen(WORLD_CUT_CHANNEL).await?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_of_reads_the_tick_and_resyncs_on_anything_malformed() {
        assert_eq!(notice_of(&payload(game::CutId(7), 1234)), CutNotice::Cut { tick: 1234 });
        assert_eq!(notice_of(" 7 , 99 "), CutNotice::Cut { tick: 99 });
        for bad in ["", "7", "7,", ",5", "x,5", "7,x", "7,-1", "7,5,6", "hello"] {
            assert_eq!(notice_of(bad), CutNotice::Resync, "{bad:?}");
        }
    }

    #[test]
    fn notify_mode_parses_listen_poll_and_refuses_the_rest() {
        assert_eq!(NotifyMode::parse(None), Ok(NotifyMode::Listen));
        assert_eq!(NotifyMode::parse(Some("")), Ok(NotifyMode::Listen));
        assert_eq!(NotifyMode::parse(Some(" listen ")), Ok(NotifyMode::Listen));
        assert_eq!(NotifyMode::parse(Some("poll")), Ok(NotifyMode::Poll));
        for bad in ["Listen", "POLL", "notify", "1"] {
            let e = NotifyMode::parse(Some(bad)).unwrap_err();
            assert!(e.contains(TERRAIN_NOTIFY_ENV), "{e}");
        }
    }

    #[test]
    fn a_short_lived_connection_waits_before_the_next() {
        assert_eq!(reconnect_wait(Duration::ZERO), LISTEN_RETRY);
        assert_eq!(reconnect_wait(LISTEN_MIN_LIFE - Duration::from_millis(1)), LISTEN_RETRY);
        assert_eq!(reconnect_wait(LISTEN_MIN_LIFE), Duration::ZERO);
        assert_eq!(reconnect_wait(Duration::from_secs(60)), Duration::ZERO);
    }
}
