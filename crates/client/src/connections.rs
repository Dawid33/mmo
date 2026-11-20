//! One connection per server host (R3: a world runs on several hosts). The session's
//! first connection — the login one — is the HOME until the body moves: input, UI
//! requests, LOD and the world clock go to the home. A region another host owns is
//! announced by the home as `ServerPacket::RegionElsewhere`: the client opens (or
//! reuses) a connection to that host with the observer ticket and subscribes the region
//! there. A region → connection table routes the region's (un)subscribes and decides
//! which connection's region traffic counts. When the body crosses into another host's
//! region (or is moved there), that host's `PlayerRegion` — behind the player's hop
//! there, `ServerPacket::HomeHop` — makes its connection the home; the old home's stays
//! as an observer connection for the regions it serves.
//!
//! Regions are keyed by their canonical form (what the server names): on a world that
//! wraps east–west every lookup folds its argument first ([`ConnectionSet::set_extent`]),
//! so a copy of a region in the client's unwrapped frame finds the same connection.
//!
//! Pure game-thread state (no sockets, no clock): the transport behind each
//! connection is a [`ConnHandle`] from a [`ConnOpener`] (`netcode::NetThread` on
//! native; the harness's fake connections in tests). Lives and dies with the game
//! thread's session — it is not a Bevy resource.
use std::collections::BTreeMap;

use crossbeam::channel::Sender;
use game::{ClientPacket, Credential, HostEndpoint, HostId, MapExtent, ObserverTicket, RegionId, ServerPacket};

/// How long after an observer connection closed its regions are asked for again,
/// through the home (which answers with a fresh `RegionElsewhere`), the first time in
/// a row a host's connection ends; each further one doubles it, up to
/// [`OBSERVER_RETRY_MAX_MS`] ([`observer_retry_delay_ms`]).
pub const OBSERVER_RETRY_MS: u64 = 1_000;
/// The longest wait before an observer connection's regions are asked for again (a
/// host that keeps refusing, an endpoint the client cannot dial).
pub const OBSERVER_RETRY_MAX_MS: u64 = 30_000;
/// The longest wait when a lost region is the body's, a move's target, or next to the
/// body's region: the body may be handed to that host any moment, and a session must
/// reach it within the hosts' hand-off grace or the body is parked there.
pub const OBSERVER_RETRY_NEAR_MAX_MS: u64 = 2_000;
const _: () = assert!(OBSERVER_RETRY_NEAR_MAX_MS < game::HANDOFF_SESSION_GRACE_MS);
/// A connection that is not the home and has served no region for this long is closed
/// (the window left that host's regions; a host would idle-close it after
/// `game::OBSERVER_IDLE_CLOSE_MS` anyway, and LOD subscriptions follow the home).
pub const CONN_IDLE_CLOSE_MS: u64 = 10_000;

/// The key of the session's first (login) connection. Every connection keeps the key
/// it was opened with for its whole life, and no key is ever used twice in a session:
/// what a dead connection still delivers can never count as its successor's.
pub const LOGIN_CONN: u32 = 0;

/// The wait before the regions of a host's observer connection are asked for again
/// after `failures` failed ends in a row (1 = the first; a routine close counts 0):
/// [`OBSERVER_RETRY_MS`], doubling, at most [`OBSERVER_RETRY_MAX_MS`] — at most
/// [`OBSERVER_RETRY_NEAR_MAX_MS`] when `near` (the body may cross into that host's
/// region soon).
pub fn observer_retry_delay_ms(failures: u32, near: bool) -> u64 {
    let doublings = failures.saturating_sub(1).min(16);
    let cap = if near { OBSERVER_RETRY_NEAR_MAX_MS } else { OBSERVER_RETRY_MAX_MS };
    (OBSERVER_RETRY_MS << doublings).min(cap)
}

/// What the manager and the region runners send. The runners send `Home`.
#[derive(Debug)]
pub enum Outgoing {
    /// To the home connection: input, predictions, UI requests, LOD.
    Home(ClientPacket),
    /// About one region (its subscribe / release): to the connection that serves it,
    /// the home when no other does.
    Region(RegionId, ClientPacket),
    /// To this connection only; dropped if it is gone.
    Conn(u32, ClientPacket),
}

/// The game thread's end of one connection: what is sent here goes out on it. Dropping
/// it closes the connection.
pub struct ConnHandle {
    pub send: Sender<ClientPacket>,
}

/// What a connection delivers to the game thread, tagged with its key (one channel for
/// every connection of the session).
#[derive(Debug)]
pub enum ConnEvent {
    Packet(ServerPacket),
    /// The connection ended; the reason, for the session's end when it was the home.
    Closed(String),
}

/// Opens a connection to another host of the world (the transport's side).
pub trait ConnOpener {
    /// Connect to the host at `endpoint` (`endpoint.id`), presenting `credential`
    /// as the Hello. The connection's events arrive keyed by `key` (new for every
    /// connection: a reopened one to the same host gets another).
    fn open(&mut self, key: u32, endpoint: &HostEndpoint, credential: Credential) -> ConnHandle;
}

/// What [`ConnectionSet::on_region_named`] did with a `RegionElsewhere`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    /// Served by the home connection and subscribed there.
    Home,
    /// Served by this (observer) connection and subscribed there.
    Observer(u32),
    /// Nothing sent (the home's own host not known yet, or no way to connect).
    Unrouted,
}

/// What a closed connection was ([`ConnectionSet::on_closed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Closed {
    /// The home: the session ends.
    Home,
    /// Another host's: the regions it served (to be dropped and asked for again).
    Observer { regions: Vec<RegionId> },
}

/// One open connection.
struct Conn {
    handle: ConnHandle,
    /// The host it is to (`None` for the login connection until its `Host` packet).
    ordinal: Option<HostId>,
    /// Since when (session ms) it is not the home and serves no region
    /// ([`ConnectionSet::close_idle`]).
    idle_since: Option<u64>,
}

/// Every connection of one session, and which serves which region.
pub struct ConnectionSet {
    conns: BTreeMap<u32, Conn>,
    /// The key the next opened connection gets.
    next_key: u32,
    home: u32,
    /// The connection each subscribed region is served by (asked for through the home
    /// unless another host was named for it): a region keeps its connection when the
    /// home moves to another host.
    region_conn: BTreeMap<RegionId, u32>,
    /// The highest player hop a home was taken at (`ServerPacket::HomeHop`).
    home_hop: u64,
    /// A `HomeHop` each connection sent, for the `PlayerRegion` that follows it there.
    hop_of: BTreeMap<u32, u64>,
    /// The world's extent: region keys are canonical under it (`None`: as given).
    extent: Option<MapExtent>,
}

impl ConnectionSet {
    /// The login connection ([`LOGIN_CONN`]) is the home until told otherwise; its host
    /// is learned from `ServerPacket::Host` ([`Self::on_host`]).
    pub fn new(first: ConnHandle) -> Self {
        Self {
            conns: BTreeMap::from([(LOGIN_CONN, Conn { handle: first, ordinal: None, idle_since: None })]),
            next_key: LOGIN_CONN + 1,
            home: LOGIN_CONN,
            region_conn: BTreeMap::new(),
            home_hop: 0,
            hop_of: BTreeMap::new(),
            extent: None,
        }
    }

    /// The world's extent is `extent` now (its `WorldMap`): region keys are folded to
    /// their canonical form under it, the ones held so far too.
    pub fn set_extent(&mut self, extent: Option<MapExtent>) {
        self.extent = extent;
        self.region_conn = std::mem::take(&mut self.region_conn)
            .into_iter()
            .map(|(region, conn)| (game::canonical(extent, region), conn))
            .collect();
    }

    /// `region`'s key here: its canonical form.
    fn key(&self, region: RegionId) -> RegionId {
        game::canonical(self.extent, region)
    }

    /// The home connection's key.
    pub fn home(&self) -> u32 {
        self.home
    }

    /// The home's host, once its `Host` packet came.
    pub fn home_ordinal(&self) -> Option<HostId> {
        self.ordinal_of(self.home)
    }

    /// Whether the home's host is known (its `Host` packet came).
    pub fn home_known(&self) -> bool {
        self.home_ordinal().is_some()
    }

    /// Whether connection `conn` is open.
    pub fn is_live(&self, conn: u32) -> bool {
        self.conns.contains_key(&conn)
    }

    /// The host connection `conn` is to, if it is open and its host known.
    pub fn ordinal_of(&self, conn: u32) -> Option<HostId> {
        self.conns.get(&conn)?.ordinal
    }

    /// The open connection to host `ordinal`, if any.
    pub fn conn_to(&self, ordinal: HostId) -> Option<u32> {
        self.conns.iter().find(|(_, c)| c.ordinal == Some(ordinal)).map(|(key, _)| *key)
    }

    /// Every open connection's key.
    pub fn keys(&self) -> Vec<u32> {
        self.conns.keys().copied().collect()
    }

    /// The connection that serves `region` (the home unless another does).
    pub fn connection_of(&self, region: RegionId) -> u32 {
        self.region_conn.get(&self.key(region)).copied().unwrap_or(self.home)
    }

    /// Connection `conn`'s `Host` packet named host `ordinal`: the login connection learns
    /// its host. `false` = the packet does not fit the connection (another host than the
    /// one dialled, or a host another open connection is to): the caller closes it.
    pub fn on_host(&mut self, conn: u32, ordinal: HostId) -> bool {
        let taken = self.conn_to(ordinal).is_some_and(|other| other != conn);
        let Some(c) = self.conns.get_mut(&conn) else { return false };
        match c.ordinal {
            Some(known) if known == ordinal => true,
            Some(known) => {
                log::warn!("[client] connection {conn} to host {known} says it is host {ordinal}");
                false
            }
            None if taken => {
                log::warn!("[client] connection {conn} is a second connection to host {ordinal}");
                false
            }
            None => {
                c.ordinal = Some(ordinal);
                true
            }
        }
    }

    /// Send `out` where it goes: `Home` to the home, `Region(rc, _)` to the connection
    /// that serves `rc` (else the home), `Conn(o, _)` to that connection (dropped if it
    /// is gone). A subscribe through the home makes the home `rc`'s connection.
    pub fn route(&mut self, out: Outgoing) {
        let (conn, packet) = match out {
            Outgoing::Home(packet) => (self.home, packet),
            Outgoing::Region(region, packet) => {
                let conn = self.connection_of(region);
                if matches!(packet, ClientPacket::RequestRegionConnection(_)) {
                    self.region_conn.insert(self.key(region), conn);
                }
                (conn, packet)
            }
            Outgoing::Conn(conn, packet) => (conn, packet),
        };
        match self.conns.get(&conn) {
            // A transport that already ended drops it; its `Closed` event follows.
            Some(c) => {
                let _ = c.handle.send.send(packet);
            }
            None => log::debug!("[client] a packet for connection {conn}, which is closed: dropped"),
        }
    }

    /// The home sent `region`'s owner ([`Self::on_region_named`] with an opener; what it
    /// did is left to the caller).
    pub fn on_region_elsewhere(
        &mut self,
        region: RegionId,
        endpoint: HostEndpoint,
        ticket: ObserverTicket,
        opener: &mut dyn ConnOpener,
    ) {
        let _ = self.on_region_named(region, endpoint, ticket, Some(opener));
    }

    /// The home sent `region`'s owner (`RegionElsewhere`) — the one place it is acted on:
    /// - the home itself (R4: it inherited the region from the host that served it): the
    ///   region is served by the home connection ([`Self::remap_to_home`]) and subscribed
    ///   there — never dialled (an observer ticket must not replace the home session);
    /// - another host: a connection to `endpoint.id` is opened unless one is live (one
    ///   connection per host, however many regions; after the home moved, the old home's
    ///   connection serves as this one), the region is served from it and subscribed there.
    ///   A region another connection served is released there first (re-mapped: its owner
    ///   died and an heir took it) — one host serves it at a time.
    ///
    /// What it did ([`Named`]): `Unrouted` (nothing sent) before the home named its own host,
    /// or for another host with no `opener` (a transport with one connection only).
    pub fn on_region_named(
        &mut self,
        region: RegionId,
        endpoint: HostEndpoint,
        ticket: ObserverTicket,
        opener: Option<&mut dyn ConnOpener>,
    ) -> Named {
        let region = self.key(region);
        let ordinal = endpoint.id;
        let Some(home) = self.home_ordinal() else {
            log::warn!("[client] {region:?} is said to be host {ordinal}'s before the home named itself: ignored");
            return Named::Unrouted;
        };
        if ordinal == home {
            self.remap_to_home(region);
            self.route(Outgoing::Conn(self.home, ClientPacket::RequestRegionConnection(region)));
            return Named::Home;
        }
        let key = match self.conn_to(ordinal) {
            Some(key) => key,
            None => {
                let Some(opener) = opener else {
                    log::warn!("[client] {region:?} is served by host {ordinal}, which this transport cannot connect to");
                    return Named::Unrouted;
                };
                let key = self.next_key;
                self.next_key += 1;
                log::info!("[client] connecting to host {ordinal} for {region:?} (connection {key})");
                let credential = Credential::Observer { client: ticket.client, nonce: ticket.nonce };
                let handle = opener.open(key, &endpoint, credential);
                self.conns.insert(key, Conn { handle, ordinal: Some(ordinal), idle_since: None });
                key
            }
        };
        let old = self.region_conn.insert(region, key);
        if let Some(old) = old.filter(|old| *old != key && *old != self.home) {
            // It moved hosts: the one that served it lets it go.
            self.route(Outgoing::Conn(old, ClientPacket::ReleaseRegionConnection(region)));
        }
        self.route(Outgoing::Conn(key, ClientPacket::RequestRegionConnection(region)));
        Named::Observer(key)
    }

    /// `region` is served by the home connection from now on (the home named itself for
    /// it, R4: it inherited the region from the host that served it): another connection
    /// that served it lets it go. [`Self::on_region_named`] subscribes it on the home.
    fn remap_to_home(&mut self, region: RegionId) {
        let region = self.key(region);
        let home = self.home;
        if let Some(old) = self.region_conn.insert(region, home).filter(|old| *old != home) {
            self.route(Outgoing::Conn(old, ClientPacket::ReleaseRegionConnection(region)));
        }
    }

    /// Whether region traffic for `region` from `conn` counts: only from the connection
    /// the table names (the home when unmapped).
    pub fn accepts(&self, conn: u32, region: RegionId) -> bool {
        self.connection_of(region) == conn && self.conns.contains_key(&conn)
    }

    /// Connection `conn` sent the player's hop there (`ServerPacket::HomeHop`): it goes
    /// with the `PlayerRegion` that follows it on that connection.
    pub fn on_home_hop(&mut self, conn: u32, hop: u64) {
        if self.conns.contains_key(&conn) {
            self.hop_of.insert(conn, hop);
        }
    }

    /// Whether connection `conn` sent a `HomeHop` its next `PlayerRegion` goes with.
    pub fn has_hop(&self, conn: u32) -> bool {
        self.hop_of.contains_key(&conn)
    }

    /// Connection `conn` sent `PlayerRegion`: whether it counts. The home's always does.
    /// Another host's makes that connection the home — whatever the client predicted —
    /// when the hop sent right before it is above every hop a home was taken at:
    /// connections have no order between them, so after a ping-pong (A → B → A) B's
    /// `PlayerRegion` may arrive after A's newer one, and must not move the home back.
    /// One with no hop is not a hand-off's: it does not count.
    pub fn on_player_region(&mut self, conn: u32) -> bool {
        let hop = self.hop_of.remove(&conn);
        if conn == self.home {
            if let Some(hop) = hop {
                self.home_hop = self.home_hop.max(hop);
            }
            return true;
        }
        match hop {
            Some(hop) if hop > self.home_hop && self.conns.contains_key(&conn) => {
                self.home_hop = hop;
                self.set_home(conn);
                true
            }
            _ => false,
        }
    }

    /// Connection `conn` is the home from now on.
    pub fn set_home(&mut self, conn: u32) {
        self.home = conn;
        if let Some(c) = self.conns.get_mut(&conn) {
            c.idle_since = None;
        }
    }

    /// Connection `conn` ended (or was closed here): forget it. The home's end ends the
    /// session; another's hands back the regions it served (unmapped: the home's again).
    pub fn on_closed(&mut self, conn: u32) -> Closed {
        self.conns.remove(&conn);
        self.hop_of.remove(&conn);
        if conn == self.home {
            return Closed::Home;
        }
        let regions: Vec<RegionId> =
            self.region_conn.iter().filter(|(_, c)| **c == conn).map(|(region, _)| *region).collect();
        for region in &regions {
            self.region_conn.remove(region);
        }
        Closed::Observer { regions }
    }

    /// `region` is no longer subscribed: forget which connection served it.
    pub fn release(&mut self, region: RegionId) {
        let region = self.key(region);
        self.region_conn.remove(&region);
    }

    /// Close every connection that is not the home and has served no region for
    /// [`CONN_IDLE_CLOSE_MS`] by `now_ms` (session time); their keys (dropping a handle
    /// closes its connection).
    pub fn close_idle(&mut self, now_ms: u64) -> Vec<u32> {
        let mut closed = Vec::new();
        for (key, conn) in self.conns.iter_mut() {
            if *key == self.home || self.region_conn.values().any(|c| c == key) {
                conn.idle_since = None;
                continue;
            }
            let since = *conn.idle_since.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= CONN_IDLE_CLOSE_MS {
                closed.push(*key);
            }
        }
        for key in &closed {
            self.conns.remove(key);
            self.hop_of.remove(key);
        }
        closed
    }
}

