//! Who a connection is: the [`PlayerStore`] turns the [`PlayerToken`] a client
//! sends in its `Hello` into a stable [`PlayerId`], plus the [`PlayerRecord`]
//! kept for that player while it is offline.
//!
//! Resolution happens at the network edge (QUIC and WebTransport ingress), on
//! tokio's blocking pool, BEFORE the session is announced to the
//! `WorldManager` — which stays I/O-free and only sees the resolved id. That
//! edge is where an external account database slots in later (an async impl),
//! without touching the manager.
//!
//! Two stores:
//! - [`StorePlayers`] for forged worlds: the world store's `players` table. A
//!   new token's id is written at once (the one write outside save cuts);
//!   records reach the store only in save cuts (`WorldManager::take_cut`), so
//!   players and regions are restored from the same world tick.
//! - [`MemoryPlayerStore`] for procedural worlds and tests: nothing persisted.
//!
//! Which token a connection may claim is decided first, by the
//! [`Authenticator`](crate::auth_edge::Authenticator): its own token on a
//! `token` host (single-player / dev), the one in a login ticket on a `jwt` host.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use game::{ClientPacket, PlayerId, PlayerRecord, PlayerToken};

/// Resolves tokens to players and keeps their offline records.
pub trait PlayerStore: Send {
    /// The player of `token` and its record (if one was stored). An unknown
    /// token becomes a new player with a fresh id, never reused.
    fn resolve(&mut self, token: &PlayerToken) -> io::Result<(PlayerId, Option<PlayerRecord>)>;
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

/// The handshake: `first` must be a `Hello` whose credential `auth` accepts
/// (see [`crate::auth_edge`]); the store then resolves its player token on
/// tokio's blocking pool. `Err` is the refusal sent back.
pub async fn resolve_hello(
    store: &SharedPlayerStore,
    auth: &crate::auth_edge::Authenticator,
    first: ClientPacket,
) -> Result<(PlayerId, Option<PlayerRecord>), String> {
    let ClientPacket::Hello { credential } = first else {
        return Err("the first packet must be Hello".into());
    };
    let token = auth.authenticate(credential).await?;
    let store = store.clone();
    let resolved = tokio::task::spawn_blocking(move || {
        let mut store = store.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        store.resolve(&token)
    })
    .await
    .map_err(|e| format!("player store task failed: {e}"))?;
    resolved.map_err(|e| {
        log::error!("[server] player store: {e}");
        format!("the player store failed: {e}")
    })
}

/// Nothing persisted: ids from 0 per process, no records (a player parked
/// in this process comes back from the manager's parked records).
/// Procedural worlds and tests.
#[derive(Default)]
pub struct MemoryPlayerStore {
    ids: BTreeMap<PlayerToken, PlayerId>,
    next_id: u64,
}

impl PlayerStore for MemoryPlayerStore {
    fn resolve(&mut self, token: &PlayerToken) -> io::Result<(PlayerId, Option<PlayerRecord>)> {
        let id = *self.ids.entry(token.clone()).or_insert_with(|| {
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
    fn resolve(&mut self, token: &PlayerToken) -> io::Result<(PlayerId, Option<PlayerRecord>)> {
        self.0.resolve_player(token).map_err(io::Error::other)
    }
}
