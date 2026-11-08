//! The host's side of the login service's push (`svc-link`, role `Login`): `SpawnPlayer` and
//! `Kick` in, `SpawnAck`, `KickAck` and `SessionEnded` out.
//!
//! A `jwt` host that has a service token listens on [`HOST_LINK_BIND_ENV`] and admits a login
//! ticket only against a pending spawn order ([`SpawnOrders`]): the login service pushes
//! `SpawnPlayer { player_token, session_epoch }` on every join and waits for the ack before it
//! hands the ticket out. So:
//! - a ticket the login service never ordered (or whose order expired, after
//!   [`SPAWN_PENDING_TTL_SECS`]) is refused ([`REFUSE_NO_SPAWN_ORDER`]);
//! - an order is consumed by its first connect (a second connect with the same ticket is refused);
//! - an order for a newer epoch parks the player's online session at once
//!   ([`ServerEvent::Supersede`], `Refused(LOGGED_IN_ELSEWHERE)`) — before the newer ticket
//!   exists, so newest-wins holds across login replicas and processes — and an older ticket is
//!   refused ([`REFUSE_SUPERSEDED`](crate::auth_edge::REFUSE_SUPERSEDED));
//! - `Kick { up to epoch }` parks the session with its reason and drops (and fences off) any
//!   order up to that epoch; a newer login is untouched. It is acked (`KickAck`) after it is
//!   applied: the login service resends it until then (kicks are durable there), and a copy
//!   applies nothing new. A kick that lands while a revoked ticket's connect is between its
//!   admission and its `ClientConnected` is caught by the edge, which checks
//!   [`SpawnOrders::revoked`] after announcing the session.
//!
//! The link is at-least-once around reconnects: an order repeated with an epoch already known
//! (ordered, live, or used within [`SPAWN_PENDING_TTL_SECS`]) is acked again and changes
//! nothing (no second park of the session it admitted, no re-armed ticket).
//!
//! `SessionEnded` is best effort: it goes to the login peers connected at that moment and is
//! lost while the link is down (the login service then keeps the session open in its
//! registry; a kick for it later is a no-op here).
//!
//! The order is consumed and the supersede queued on this link's thread BEFORE the ack goes
//! out, and the supersede shares the manager's event channel with the connects: the new
//! session's `ClientConnected` always comes after the old one's park was asked for.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crossbeam::channel::{Receiver, Sender};
use game::{PlayerToken, ServerEvent, LOGGED_IN_ELSEWHERE};
use login_api::{HostToLogin, LoginToHost, SPAWN_PENDING_TTL_SECS};
use svc_link::{LinkEvent, LinkListener, PeerId, PeerRole, PeerSenders};

use crate::auth_edge::{Clock, REFUSE_SUPERSEDED};
use crate::player_store::{PlayerKey, SharedPlayerStore};

/// Where the host listens for the login service (`ip:port`).
pub const HOST_LINK_BIND_ENV: &str = "VOXEL_HOST_LINK_BIND";
pub const DEFAULT_HOST_LINK_BIND: &str = "127.0.0.1:6490";
/// Where the host writes its link certificate's SHA-256 pin (the login registry's
/// `host_link_cert`); its cert and key persist beside it (keep it under a git-ignored dir).
pub const HOST_LINK_CERT_HASH_FILE_ENV: &str = "VOXEL_HOST_LINK_CERT_HASH_FILE";
pub const DEFAULT_HOST_LINK_CERT_HASH_FILE: &str = "target/dev-r2/host-0.cert-sha256";

/// The refusal of a ticket no pending spawn order matches.
pub const REFUSE_NO_SPAWN_ORDER: &str = "no spawn order for this login — log in again";

/// The service token, if one is configured (`VOXEL_SERVICE_TOKEN` or
/// `VOXEL_SERVICE_TOKEN_FILE` set): a `jwt` host with one listens for the login service's
/// push; without one it keeps R2a's admission (no link). A configured token that cannot be
/// read is an error.
pub fn service_token_if_configured(lookup: impl Fn(&str) -> Option<String>) -> Result<Option<String>, String> {
    let set = |name: &str| lookup(name).is_some_and(|v| !v.trim().is_empty());
    if !set(svc_link::SERVICE_TOKEN_ENV) && !set(svc_link::SERVICE_TOKEN_FILE_ENV) {
        return Ok(None);
    }
    svc_link::service_token_from_env().map(Some).map_err(|e| format!("the login link's service token: {e}"))
}

/// The listen address from an env value; unset or empty is [`DEFAULT_HOST_LINK_BIND`].
pub fn bind_from(value: Option<&str>) -> Result<SocketAddr, String> {
    let value = value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(DEFAULT_HOST_LINK_BIND);
    value.parse().map_err(|_| format!("{HOST_LINK_BIND_ENV}={value:?} is not an address (ip:port)"))
}

/// The pin file from an env value; unset or empty is [`DEFAULT_HOST_LINK_CERT_HASH_FILE`].
pub fn cert_hash_file_from(value: Option<&str>) -> PathBuf {
    PathBuf::from(value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(DEFAULT_HOST_LINK_CERT_HASH_FILE))
}

/// The spawn orders a host holds and has not seen a matching ticket for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingSpawns {
    /// `player_token` → (`session_epoch`, expires at, seconds since the Unix epoch).
    pub by_token: BTreeMap<String, (u64, u64)>,
}

/// How a `SpawnPlayer` relates to what the host already knows of the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnOrder {
    /// A newer epoch: recorded (replacing an older order); the player's session is superseded.
    New,
    /// The epoch already ordered or admitted (a resend): acked again, nothing changes.
    Repeat,
    /// An older epoch than one already ordered or admitted, or one a `Kick` revoked: refused.
    Stale,
}

/// A `Kick` the host applied: every epoch up to `up_to` is revoked, until `expires`.
struct Kicked {
    up_to: u64,
    expires: u64,
    reason: String,
}

#[derive(Default)]
struct OrderState {
    pending: PendingSpawns,
    /// The epoch of each player's live session (admitted, not ended).
    admitted: BTreeMap<String, u64>,
    /// The newest epoch whose ticket was used, after its session ended, until it expires
    /// (a very late resend of that order must not re-arm the used ticket).
    used: BTreeMap<String, (u64, u64)>,
    /// After a `Kick`: what it revoked, until it expires (a late resend of a revoked order
    /// must not re-open it).
    kicked: BTreeMap<String, Kicked>,
}

impl OrderState {
    /// The newest epoch known for `token` (ordered, admitted, used or kicked).
    fn known(&self, token: &str, now: u64) -> Option<u64> {
        let pending = self.pending.by_token.get(token).map(|(epoch, _)| *epoch);
        let admitted = self.admitted.get(token).copied();
        let used = self.used.get(token).filter(|(_, expires)| now < *expires).map(|(epoch, _)| *epoch);
        let kicked = self.kicked.get(token).filter(|k| now < k.expires).map(|k| k.up_to);
        pending.max(admitted).max(used).max(kicked)
    }

    fn prune(&mut self, now: u64) {
        self.pending.by_token.retain(|_, (_, expires)| now < *expires);
        self.used.retain(|_, (_, expires)| now < *expires);
        self.kicked.retain(|_, k| now < k.expires);
    }
}

/// The login peers `SessionEnded` goes to.
struct LoginPeers {
    senders: PeerSenders<HostToLogin>,
    peers: Mutex<BTreeSet<PeerId>>,
}

/// The admission state of a host the login service pushes to. Shared by the link's thread
/// (orders, kicks) and the connect edge ([`admit`](Self::admit), [`ended`](Self::ended)).
pub struct SpawnOrders {
    state: Mutex<OrderState>,
    now: Clock,
    /// Set once the link listens.
    login: OnceLock<LoginPeers>,
}

impl SpawnOrders {
    pub fn new(now: Clock) -> SpawnOrders {
        SpawnOrders { state: Mutex::default(), now, login: OnceLock::new() }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, OrderState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A copy of the pending orders (tests, diagnostics).
    pub fn pending(&self) -> PendingSpawns {
        self.state().pending.clone()
    }

    /// Record `SpawnPlayer { token, epoch }`.
    pub fn order(&self, token: &str, epoch: u64) -> SpawnOrder {
        let now = (self.now)();
        let mut state = self.state();
        state.prune(now);
        if state.kicked.get(token).is_some_and(|k| epoch <= k.up_to) {
            return SpawnOrder::Stale;
        }
        match state.known(token, now) {
            Some(known) if epoch < known => SpawnOrder::Stale,
            Some(known) if epoch == known => SpawnOrder::Repeat,
            _ => {
                let expires = now.saturating_add(SPAWN_PENDING_TTL_SECS);
                state.pending.by_token.insert(token.to_string(), (epoch, expires));
                SpawnOrder::New
            }
        }
    }

    /// `Kick { token, up_to, reason }`: a pending order up to `up_to` is dropped, and no order
    /// or ticket up to it is taken for [`SPAWN_PENDING_TTL_SECS`]. Whether the player's live
    /// session (if any) is revoked by it — a session of a newer login is not. Applying the
    /// same kick again changes nothing.
    pub fn kick(&self, token: &str, up_to: u64, reason: &str) -> bool {
        let now = (self.now)();
        let mut state = self.state();
        state.prune(now);
        let ceiling = state.kicked.get(token).map_or(up_to, |k| k.up_to.max(up_to));
        let expires = now.saturating_add(SPAWN_PENDING_TTL_SECS);
        state.kicked.insert(token.to_string(), Kicked { up_to: ceiling, expires, reason: reason.to_string() });
        if state.pending.by_token.get(token).is_some_and(|(epoch, _)| *epoch <= up_to) {
            state.pending.by_token.remove(token);
        }
        state.admitted.get(token).is_none_or(|epoch| *epoch <= up_to)
    }

    /// The kick's reason if the session admitted with (`token`, `epoch`) was revoked since.
    pub fn revoked(&self, token: &str, epoch: u64) -> Option<String> {
        let now = (self.now)();
        let state = self.state();
        state.kicked.get(token).filter(|k| now < k.expires && epoch <= k.up_to).map(|k| k.reason.clone())
    }

    /// A verified ticket of `token` with `epoch` connects: admitted (its order consumed) only
    /// if it is the pending order's epoch and the order has not expired; else the refusal.
    /// Checked and consumed under one lock: of two connects with one ticket only one wins.
    pub fn admit(&self, token: &str, epoch: u64) -> Result<(), &'static str> {
        let now = (self.now)();
        let mut state = self.state();
        state.prune(now);
        match state.pending.by_token.get(token).copied() {
            Some((ordered, _)) if ordered == epoch => {
                state.pending.by_token.remove(token);
                state.admitted.insert(token.to_string(), epoch);
                Ok(())
            }
            Some((ordered, _)) if epoch < ordered => Err(REFUSE_SUPERSEDED),
            _ if state.known(token, now).is_some_and(|known| epoch < known) => Err(REFUSE_SUPERSEDED),
            _ => Err(REFUSE_NO_SPAWN_ORDER),
        }
    }

    /// The session admitted with (`token`, `epoch`) ended: forgotten (unless a newer one of
    /// the player is live) and reported to the login service.
    pub fn ended(&self, token: &str, epoch: u64) {
        {
            let now = (self.now)();
            let mut state = self.state();
            if state.admitted.get(token) == Some(&epoch) {
                state.admitted.remove(token);
            }
            let newest = state.used.get(token).map_or(epoch, |(used, _)| (*used).max(epoch));
            state.used.insert(token.to_string(), (newest, now.saturating_add(SPAWN_PENDING_TTL_SECS)));
        }
        if let Some(login) = self.login.get() {
            let peers = login.peers.lock().unwrap_or_else(|p| p.into_inner()).clone();
            for peer in peers {
                login.senders.send(peer, HostToLogin::SessionEnded { player_token: token.to_string(), session_epoch: epoch });
            }
        }
    }
}

/// The running link: the listener (drop it from synchronous code) and its address and pin.
pub struct LoginLink {
    listener: LinkListener,
}

impl LoginLink {
    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr()
    }

    /// The pin the login registry needs (`world add --link-cert`).
    pub fn cert_sha256(&self) -> &str {
        self.listener.cert_sha256()
    }
}

/// Listen for the login service on `bind` (port 0 picks one) with the service `token`; the
/// pin goes to `cert_hash_file` (its directory is created). Orders land in `orders`; players
/// are resolved through `players`; supersedes go to the manager on `events`.
pub fn start(
    bind: SocketAddr,
    token: String,
    cert_hash_file: Option<PathBuf>,
    orders: Arc<SpawnOrders>,
    players: SharedPlayerStore,
    events: Sender<ServerEvent>,
) -> io::Result<LoginLink> {
    if let Some(dir) = cert_hash_file.as_deref().and_then(std::path::Path::parent).filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let (listener, link_events, senders) = LinkListener::bind::<LoginToHost, HostToLogin>(bind, token, cert_hash_file)?;
    if orders.login.set(LoginPeers { senders: senders.clone(), peers: Mutex::default() }).is_err() {
        return Err(io::Error::other("these spawn orders already have a login link"));
    }
    log::info!("[login-link] listening for the login service on {}", listener.local_addr());
    std::thread::Builder::new()
        .name("login-link".into())
        .spawn(move || serve(link_events, senders, orders, players, events))?;
    Ok(LoginLink { listener })
}

fn serve(
    link_events: Receiver<(PeerId, LinkEvent<LoginToHost>)>,
    senders: PeerSenders<HostToLogin>,
    orders: Arc<SpawnOrders>,
    players: SharedPlayerStore,
    events: Sender<ServerEvent>,
) {
    let login_peers = |f: &mut dyn FnMut(&mut BTreeSet<PeerId>)| {
        if let Some(login) = orders.login.get() {
            f(&mut login.peers.lock().unwrap_or_else(|p| p.into_inner()));
        }
    };
    for (peer, event) in link_events.iter() {
        match event {
            LinkEvent::Up(hello) if hello.role == PeerRole::Login => {
                log::info!("[login-link] login service connected (peer {})", peer.0);
                login_peers(&mut |p| {
                    p.insert(peer);
                });
            }
            LinkEvent::Up(hello) => {
                log::error!("[login-link] peer {} is {:?}, not the login service: ignored", peer.0, hello.role);
            }
            LinkEvent::Down(why) => {
                login_peers(&mut |p| {
                    p.remove(&peer);
                });
                log::info!("[login-link] peer {} gone: {why}", peer.0);
            }
            LinkEvent::Msg(msg) => {
                let mut is_login = false;
                login_peers(&mut |p| is_login = p.contains(&peer));
                if !is_login {
                    log::warn!("[login-link] message from peer {} that is not the login service: ignored", peer.0);
                    continue;
                }
                match msg {
                    LoginToHost::SpawnPlayer { player_token, session_epoch } => {
                        let ok = spawn(&orders, &players, &events, &player_token, session_epoch);
                        senders.send(peer, HostToLogin::SpawnAck { player_token, session_epoch, ok });
                    }
                    LoginToHost::Kick { player_token, session_epoch, reason } => {
                        kick(&orders, &players, &events, &player_token, session_epoch, reason);
                        senders.send(peer, HostToLogin::KickAck { player_token, session_epoch });
                    }
                }
            }
        }
    }
    log::info!("[login-link] stopped");
}

/// The player of a login token, through the player store (created if new). `None` (logged)
/// for a malformed token or a store failure.
fn player_of(players: &SharedPlayerStore, token: &str) -> Option<game::PlayerId> {
    let token = PlayerToken(token.to_string());
    if !token.is_valid() {
        log::warn!("[login-link] a malformed player token");
        return None;
    }
    let resolved = players.lock().unwrap_or_else(|p| p.into_inner()).resolve(&PlayerKey::Online(token));
    match resolved {
        Ok((player, _)) => Some(player),
        Err(e) => {
            log::error!("[login-link] player store: {e}");
            None
        }
    }
}

/// `SpawnPlayer`: whether it is acked ok.
fn spawn(
    orders: &SpawnOrders,
    players: &SharedPlayerStore,
    events: &Sender<ServerEvent>,
    token: &str,
    epoch: u64,
) -> bool {
    let Some(player) = player_of(players, token) else { return false };
    match orders.order(token, epoch) {
        SpawnOrder::New => {
            log::info!("[login-link] spawn order for player {} (epoch {epoch})", player.0);
            // Any session of the player parks now, before the new ticket is handed out.
            let _ = events.send(ServerEvent::Supersede { player, reason: LOGGED_IN_ELSEWHERE.to_string() });
            true
        }
        SpawnOrder::Repeat => true,
        SpawnOrder::Stale => {
            log::info!("[login-link] stale spawn order for player {} (epoch {epoch}): refused", player.0);
            false
        }
    }
}

/// `Kick`: orders up to `up_to` are dropped and a revoked session parked with `reason`. The
/// revocation is recorded before the supersede is queued (the edge's re-check relies on it).
fn kick(
    orders: &SpawnOrders,
    players: &SharedPlayerStore,
    events: &Sender<ServerEvent>,
    token: &str,
    up_to: u64,
    reason: String,
) {
    if !orders.kick(token, up_to, &reason) {
        log::info!("[login-link] kick up to epoch {up_to}: the live session is a newer login; kept");
        return;
    }
    if let Some(player) = player_of(players, token) {
        log::info!("[login-link] kick of player {} (up to epoch {up_to}): {reason}", player.0);
        let _ = events.send(ServerEvent::Supersede { player, reason });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::auth_edge::REFUSE_SUPERSEDED;

    fn orders_at(now: u64) -> (Arc<AtomicU64>, SpawnOrders) {
        let clock = Arc::new(AtomicU64::new(now));
        let c = clock.clone();
        (clock, SpawnOrders::new(Arc::new(move || c.load(Ordering::SeqCst))))
    }

    #[test]
    fn orders_admit_once_and_newest_wins() {
        let (clock, o) = orders_at(1000);
        assert_eq!(o.admit("t", 1), Err(REFUSE_NO_SPAWN_ORDER));
        assert_eq!(o.order("t", 1), SpawnOrder::New);
        assert_eq!(o.order("t", 1), SpawnOrder::Repeat, "a resend");
        assert_eq!(o.admit("t", 2), Err(REFUSE_NO_SPAWN_ORDER), "never ordered");
        assert_eq!(o.admit("t", 1), Ok(()));
        assert_eq!(o.admit("t", 1), Err(REFUSE_NO_SPAWN_ORDER), "consumed");
        assert_eq!(o.order("t", 1), SpawnOrder::Repeat, "a resend after the connect");
        assert_eq!(o.admit("t", 1), Err(REFUSE_NO_SPAWN_ORDER), "the resend did not re-open it");
        assert_eq!(o.order("t", 2), SpawnOrder::New);
        assert_eq!(o.order("t", 1), SpawnOrder::Stale);
        assert_eq!(o.admit("t", 1), Err(REFUSE_SUPERSEDED));
        o.ended("t", 1);
        assert_eq!(o.admit("t", 1), Err(REFUSE_SUPERSEDED), "older than the pending order");
        // Orders expire.
        clock.store(1000 + SPAWN_PENDING_TTL_SECS, Ordering::SeqCst);
        assert_eq!(o.admit("t", 2), Err(REFUSE_NO_SPAWN_ORDER));
        assert_eq!(o.pending(), PendingSpawns::default());
    }

    #[test]
    fn kick_drops_the_order_and_fences_its_resend() {
        let (clock, o) = orders_at(1000);
        assert_eq!(o.order("t", 3), SpawnOrder::New);
        assert!(o.kick("t", 3, "bye"), "no live session: nothing newer to keep");
        assert!(o.kick("t", 3, "bye"), "a copy");
        assert_eq!(o.revoked("t", 3).as_deref(), Some("bye"));
        assert_eq!(o.admit("t", 3), Err(REFUSE_NO_SPAWN_ORDER));
        assert_eq!(o.order("t", 3), SpawnOrder::Stale, "a late resend of the revoked order");
        assert_eq!(o.admit("t", 3), Err(REFUSE_NO_SPAWN_ORDER));
        assert_eq!(o.order("t", 4), SpawnOrder::New, "a new login after the kick");
        assert_eq!(o.admit("t", 4), Ok(()));
        assert_eq!(o.revoked("t", 4), None);
        assert!(!o.kick("t", 3, "bye"), "a late copy of the old kick keeps the newer session");
        assert_eq!(o.revoked("t", 4), None);
        clock.store(1000 + SPAWN_PENDING_TTL_SECS, Ordering::SeqCst);
        assert_eq!(o.order("t", 4), SpawnOrder::Repeat, "admitted and live");
    }

    #[test]
    fn a_used_ticket_stays_used_after_its_session_ends() {
        let (clock, o) = orders_at(1000);
        assert_eq!(o.order("t", 5), SpawnOrder::New);
        assert_eq!(o.admit("t", 5), Ok(()));
        o.ended("t", 5);
        assert_eq!(o.order("t", 5), SpawnOrder::Repeat, "a very late resend after the session ended");
        assert_eq!(o.admit("t", 5), Err(REFUSE_NO_SPAWN_ORDER), "does not re-arm the used ticket");
        clock.store(1000 + SPAWN_PENDING_TTL_SECS, Ordering::SeqCst);
        assert_eq!(o.order("t", 6), SpawnOrder::New);
    }

    #[test]
    fn host_link_env_defaults() {
        assert_eq!(bind_from(None).unwrap().to_string(), DEFAULT_HOST_LINK_BIND);
        assert_eq!(bind_from(Some(" 0.0.0.0:7000 ")).unwrap().to_string(), "0.0.0.0:7000");
        assert!(bind_from(Some("nope")).is_err());
        assert_eq!(cert_hash_file_from(Some("")), PathBuf::from(DEFAULT_HOST_LINK_CERT_HASH_FILE));
    }
}
