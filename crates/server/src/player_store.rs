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
    /// The login ticket's epoch (`None` for a local session).
    pub session_epoch: Option<u64>,
}

/// The refusal of a login whose session id is already live (a replayed ticket).
pub const REFUSE_ALREADY_CONNECTED: &str = game::DUPLICATE_SESSION_REFUSAL;

impl HelloSession {
    /// The session's id, allocated now that it is authenticated: a login session's is
    /// [`game::session_client_id`] (the same on every host), a local session's the next of
    /// the process counter.
    pub fn client_id(&self, local_ids: &std::sync::atomic::AtomicU64) -> game::ClientId {
        match self.session_epoch {
            Some(epoch) => game::session_client_id(self.player, epoch),
            None => local_ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        }
    }
}

/// Store `sink` as the live connection of `id`, in one step with the check that no session
/// holds `id` yet. `Err` hands the sink back: the id is live (a replayed ticket), the caller
/// refuses that connection with [`REFUSE_ALREADY_CONNECTED`] and sends no `ClientConnected`,
/// no `ClientDisconnected` and no `session_ended` for it (the live session owns all three).
pub fn claim_sink<V>(sinks: &dashmap::DashMap<game::ClientId, V>, id: game::ClientId, sink: V) -> Result<(), V> {
    match sinks.entry(id) {
        dashmap::mapref::entry::Entry::Occupied(_) => Err(sink),
        dashmap::mapref::entry::Entry::Vacant(slot) => {
            slot.insert(sink);
            Ok(())
        }
    }
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
        Ok((player, record)) => {
            Ok(HelloSession { player, record, ticket: admitted.ticket, session_epoch: admitted.session_epoch })
        }
        Err(why) => {
            if let Some(ticket) = &admitted.ticket {
                auth.session_ended(ticket);
            }
            Err(why)
        }
    }
}

/// The edge announces a resolved session to the manager: `ClientConnected` — for a
/// spawn-order session under the login link's lock, and only if no `Kick` revoked it and no
/// newer login of its player is known since its admission ([`crate::login_link::SpawnOrders::announce`]).
/// `Err` is the refusal: nothing was announced; the edge refuses the connection and ends
/// the ticket's session ([`crate::auth_edge::Authenticator::session_ended`]).
pub fn announce_session(
    send: &crossbeam::channel::Sender<game::ServerEvent>,
    client: game::ClientId,
    session: &HelloSession,
    auth: &crate::auth_edge::Authenticator,
) -> Result<(), String> {
    let player = session.player;
    let relay = auth.relays_logins();
    auth.announce(session.ticket.as_ref(), || {
        let record = session.record.clone();
        let event = match &session.ticket {
            // Several hosts: the manager keeps the session's login (it travels with the
            // player, and the manager reports its end).
            Some(ticket) if relay => game::ServerEvent::LoginConnected {
                client,
                player,
                record,
                player_token: ticket.player_token.clone(),
                session_epoch: ticket.session_epoch,
            },
            _ => game::ServerEvent::ClientConnected { client, player, record },
        };
        let _ = send.send(event);
    })
    .inspect_err(|why| log::info!("[server] client {client}: its login was revoked while it connected: {why}"))
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
