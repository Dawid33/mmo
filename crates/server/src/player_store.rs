//! Who a connection is: the [`PlayerStore`] turns the [`PlayerKey`] a
//! connection's `Hello` proved into a stable [`PlayerId`], plus the
//! [`PlayerRecord`] kept for that player while it is offline.
//!
//! Resolution happens at the network edge (QUIC and WebTransport ingress), on
//! tokio's blocking pool, BEFORE the session is announced to the
//! `WorldManager` — which stays I/O-free and only sees the resolved id. That
//! edge is where an external account database slots in later (an async impl),
//! without touching the manager.
//!
//! Two stores:
//! - [`StorePlayers`] for forged worlds: the world store's `players` table. A
//!   new key's id is written at once (the one write outside save cuts);
//!   records reach the store only in save cuts (`WorldManager::take_cut`), so
//!   players and regions are restored from the same world tick.
//! - [`MemoryPlayerStore`] for procedural worlds and tests: nothing persisted.
//!
//! Which key a connection may claim is decided first, by the
//! [`Authenticator`](crate::auth_edge::Authenticator): any player name it
//! chooses on a `local` host (single-player / LAN / dev), the account token in
//! a login ticket on a `jwt` host.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use game::{ClientPacket, PlayerId, PlayerName, PlayerRecord, PlayerToken};

/// Who a connection is, as the [`Authenticator`](crate::auth_edge::Authenticator)
/// admitted it.
///
/// A `local` host trusts the name a client sends (Factorio's LAN model): a
/// second connection with the same name IS that player and takes the first
/// one over, as a reconnect does. Only an online (`jwt`) host proves identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlayerKey {
    /// A freely chosen name (single-player / LAN / dev).
    Local(PlayerName),
    /// A login-service account's player token (online).
    Online(PlayerToken),
}

/// The prefix of a local key in the store. A [`PlayerToken`] is lowercase
/// hex and never contains `:`, so a name can never collide with an account.
pub const LOCAL_KEY_PREFIX: &str = "local:";

impl PlayerKey {
    /// The key as the world store's `players.token` column holds it: an
    /// online token bare (as before local names existed), a name as
    /// `local:<name>`.
    pub fn store_key(&self) -> String {
        match self {
            PlayerKey::Local(name) => format!("{LOCAL_KEY_PREFIX}{}", name.0),
            PlayerKey::Online(token) => token.0.clone(),
        }
    }
}

/// Resolves player keys to players and keeps their offline records.
pub trait PlayerStore: Send {
    /// The player of `key` and its record (if one was stored). An unknown
    /// key becomes a new player with a fresh id, never reused.
    fn resolve(&mut self, key: &PlayerKey) -> io::Result<(PlayerId, Option<PlayerRecord>)>;
}

/// The store as the network edge shares it between connection tasks.
pub type SharedPlayerStore = Arc<Mutex<dyn PlayerStore>>;

/// Wrap a store for the network edge.
pub fn shared(store: impl PlayerStore + 'static) -> SharedPlayerStore {
    Arc::new(Mutex::new(store))
}

/// How long a new connection may take to send its `Hello` before it is closed.
pub const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Largest first packet the edge reads (a `Hello` is a few dozen bytes).
pub const HELLO_MAX_BYTES: usize = 4096;

/// A connection's resolved handshake.
#[derive(Debug)]
pub struct HelloSession {
    pub player: PlayerId,
    pub record: Option<PlayerRecord>,
    /// A session a login spawn order admitted: the edge reports its end
    /// ([`Authenticator::session_ended`](crate::auth_edge::Authenticator::session_ended)).
    pub ticket: Option<crate::auth_edge::SessionTicket>,
}

/// The handshake: `first` must be a `Hello` whose credential `auth` accepts
/// (see [`crate::auth_edge`]); the store then resolves its player key on
/// tokio's blocking pool. `Err` is the refusal sent back.
pub async fn resolve_hello(
    store: &SharedPlayerStore,
    auth: &crate::auth_edge::Authenticator,
    first: ClientPacket,
) -> Result<(PlayerId, Option<PlayerRecord>), String> {
    resolve_session(store, auth, first).await.map(|s| (s.player, s.record))
}

/// [`resolve_hello`] with the session's spawn-order ticket. A ticket whose
/// player the store cannot resolve is ended at once (no session started).
pub async fn resolve_session(
    store: &SharedPlayerStore,
    auth: &crate::auth_edge::Authenticator,
    first: ClientPacket,
) -> Result<HelloSession, String> {
    let ClientPacket::Hello { credential } = first else {
        return Err("the first packet must be Hello".into());
    };
    let admitted = auth.admit(credential).await?;
    let store = store.clone();
    let key = admitted.key;
    let resolved = tokio::task::spawn_blocking(move || {
        let mut store = store.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        store.resolve(&key)
    })
    .await
    .map_err(|e| format!("player store task failed: {e}"))
    .and_then(|r| {
        r.map_err(|e| {
            log::error!("[server] player store: {e}");
            format!("the player store failed: {e}")
        })
    });
    match resolved {
        Ok((player, record)) => Ok(HelloSession { player, record, ticket: admitted.ticket }),
        Err(why) => {
            if let Some(ticket) = &admitted.ticket {
                auth.session_ended(ticket);
            }
            Err(why)
        }
    }
}

/// The edge announces a resolved session to the manager: `ClientConnected`,
/// then — for a spawn-order session — a `Supersede` if a `Kick` revoked it
/// between its admission and this announcement (that kick found no session).
/// Both go on the manager's one event channel, in this order.
pub fn announce_session(
    send: &crossbeam::channel::Sender<game::ServerEvent>,
    client: game::ClientId,
    session: &HelloSession,
    auth: &crate::auth_edge::Authenticator,
) {
    let player = session.player;
    let _ = send.send(game::ServerEvent::ClientConnected { client, player, record: session.record.clone() });
    if let Some(reason) = session.ticket.as_ref().and_then(|t| auth.revoked(t)) {
        log::info!("[server] client {client}: its login was revoked while it connected");
        let _ = send.send(game::ServerEvent::Supersede { player, reason });
    }
}

/// Nothing persisted: ids from 0 per process, no records (a player parked
/// in this process comes back from the manager's parked records).
/// Procedural worlds and tests.
#[derive(Default)]
pub struct MemoryPlayerStore {
    ids: BTreeMap<String, PlayerId>,
    next_id: u64,
}

impl PlayerStore for MemoryPlayerStore {
    fn resolve(&mut self, key: &PlayerKey) -> io::Result<(PlayerId, Option<PlayerRecord>)> {
        let id = *self.ids.entry(key.store_key()).or_insert_with(|| {
            let id = PlayerId(self.next_id);
            self.next_id += 1;
            id
        });
        Ok((id, None))
    }
}

/// The players of a world in its world store.
pub struct StorePlayers(pub world_store::StoreHandle);

impl PlayerStore for StorePlayers {
    fn resolve(&mut self, key: &PlayerKey) -> io::Result<(PlayerId, Option<PlayerRecord>)> {
        self.0.resolve_player(&key.store_key()).map_err(io::Error::other)
    }
}
