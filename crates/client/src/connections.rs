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
//! Pure game-thread state (no sockets, no clock): the transport behind each
//! connection is a [`ConnHandle`] from a [`ConnOpener`] (`netcode::NetThread` on
//! native; the harness's fake connections in tests). Lives and dies with the game
//! thread's session — it is not a Bevy resource.
use std::collections::BTreeMap;

use crossbeam::channel::Sender;
use game::{ClientPacket, Credential, HostEndpoint, ObserverTicket, RegionId, ServerPacket};

/// How long after an observer connection closed its regions are asked for again,
/// through the home (which answers with a fresh `RegionElsewhere`), the first time in
/// a row a host's connection ends; each further one doubles it, up to
/// [`OBSERVER_RETRY_MAX_MS`] ([`observer_retry_delay_ms`]).
pub const OBSERVER_RETRY_MS: u64 = 1_000;
/// The longest wait before an observer connection's regions are asked for again (a
/// host that keeps refusing, an endpoint the client cannot dial).
pub const OBSERVER_RETRY_MAX_MS: u64 = 30_000;
/// A connection that is not the home and has served no region for this long is closed
/// (the window left that host's regions; a host would idle-close it after
/// `game::OBSERVER_IDLE_CLOSE_MS` anyway, and LOD subscriptions follow the home).
pub const CONN_IDLE_CLOSE_MS: u64 = 10_000;

/// The key of the session's first (login) connection. Every connection keeps the key
/// it was opened with for its whole life, and no key is ever used twice in a session:
/// what a dead connection still delivers can never count as its successor's.
pub const LOGIN_CONN: u32 = 0;

/// The wait before the regions of a host's observer connection are asked for again
/// after `failures` ends in a row (1 = the first): [`OBSERVER_RETRY_MS`], doubling, at
/// most [`OBSERVER_RETRY_MAX_MS`].
pub fn observer_retry_delay_ms(failures: u32) -> u64 {
    let doublings = failures.saturating_sub(1).min(16);
    (OBSERVER_RETRY_MS << doublings).min(OBSERVER_RETRY_MAX_MS)
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
    /// Connect to the host at `endpoint` (`endpoint.ordinal`), presenting `credential`
    /// as the Hello. The connection's events arrive keyed by `key` (new for every
    /// connection: a reopened one to the same host gets another).
    fn open(&mut self, key: u32, endpoint: &HostEndpoint, credential: Credential) -> ConnHandle;
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
    ordinal: Option<u32>,
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
        }
    }

    /// The home connection's key.
    pub fn home(&self) -> u32 {
        self.home
    }

    /// The home's host, once its `Host` packet came.
    pub fn home_ordinal(&self) -> Option<u32> {
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
    pub fn ordinal_of(&self, conn: u32) -> Option<u32> {
        self.conns.get(&conn)?.ordinal
    }

    /// The open connection to host `ordinal`, if any.
    pub fn conn_to(&self, ordinal: u32) -> Option<u32> {
        self.conns.iter().find(|(_, c)| c.ordinal == Some(ordinal)).map(|(key, _)| *key)
    }

    /// Every open connection's key.
    pub fn keys(&self) -> Vec<u32> {
        self.conns.keys().copied().collect()
    }

    /// The connection that serves `region` (the home unless another does).
    pub fn connection_of(&self, region: RegionId) -> u32 {
        self.region_conn.get(&region).copied().unwrap_or(self.home)
    }

    /// Connection `conn`'s `Host` packet named host `ordinal`: the login connection learns
    /// its host. `false` = the packet does not fit the connection (another host than the
    /// one dialled, or a host another open connection is to): the caller closes it.
    pub fn on_host(&mut self, conn: u32, ordinal: u32) -> bool {
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
                    self.region_conn.insert(region, conn);
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

    /// The home sent `region`'s owner: open a connection to `endpoint.ordinal` unless one
    /// is live (one connection per host, however many regions; after the home moved, the
    /// old home's connection serves as this one), serve `region` from it and subscribe it
    /// there. Never opens an observer connection to the home host itself (an observer
    /// ticket must not replace the home session there).
    pub fn on_region_elsewhere(
        &mut self,
        region: RegionId,
        endpoint: HostEndpoint,
        ticket: ObserverTicket,
        opener: &mut dyn ConnOpener,
    ) {
        let ordinal = endpoint.ordinal;
        let Some(home) = self.home_ordinal() else {
            log::warn!("[client] {region:?} is said to be host {ordinal}'s before the home named itself: ignored");
            return;
        };
        if ordinal == home {
            log::warn!("[client] {region:?} is said to be host {ordinal}'s, which is the home: no observer connection");
            return;
        }
        let key = match self.conn_to(ordinal) {
            Some(key) => key,
            None => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam::channel::{unbounded, Receiver};
    use game::RegionCoords;

    /// Records every `open`; each opened connection's packets stay readable.
    #[derive(Default)]
    struct FakeOpener {
        opened: Vec<(u32, HostEndpoint, Credential, Receiver<ClientPacket>)>,
    }

    impl ConnOpener for FakeOpener {
        fn open(&mut self, key: u32, endpoint: &HostEndpoint, credential: Credential) -> ConnHandle {
            let (send, recv) = unbounded();
            self.opened.push((key, endpoint.clone(), credential, recv));
            ConnHandle { send }
        }
    }

    impl FakeOpener {
        /// What was sent on host `ordinal`'s connections since the last call ([`dbg`]).
        fn packets(&self, ordinal: u32) -> Vec<String> {
            self.opened.iter().filter(|o| o.1.ordinal == ordinal).flat_map(|o| o.3.try_iter()).map(|p| dbg(&p)).collect()
        }
    }

    /// `ClientPacket` has no `PartialEq`: compared by its `Debug`.
    fn dbg(p: &ClientPacket) -> String {
        format!("{p:?}")
    }

    fn sent(recv: &Receiver<ClientPacket>) -> Vec<String> {
        recv.try_iter().map(|p| dbg(&p)).collect()
    }

    fn endpoint(ordinal: u32) -> HostEndpoint {
        HostEndpoint { ordinal, quic: Some(format!("127.0.0.1:{}", 7000 + ordinal)), webtransport: None, cert_hash: None }
    }

    fn ticket() -> ObserverTicket {
        ObserverTicket { client: 7, nonce: 0xfeed }
    }

    /// A set whose home is host 0 (its `Host` came), and the home's packets.
    fn home_on_0() -> (ConnectionSet, Receiver<ClientPacket>) {
        let (send, recv) = unbounded();
        let mut set = ConnectionSet::new(ConnHandle { send });
        assert_eq!(set.home(), LOGIN_CONN);
        assert_eq!(set.home_ordinal(), None);
        assert!(set.on_host(LOGIN_CONN, 0));
        assert_eq!((set.home(), set.home_ordinal()), (LOGIN_CONN, Some(0)));
        (set, recv)
    }

    fn rc(x: i32, z: i32) -> RegionCoords {
        RegionCoords::new(x, z)
    }

    /// The host of the connection that serves `region`.
    fn host_of(set: &ConnectionSet, region: RegionCoords) -> Option<u32> {
        set.ordinal_of(set.connection_of(region))
    }

    #[test]
    fn region_elsewhere_opens_one_connection_per_host() {
        let (mut set, home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        set.on_region_elsewhere(rc(1, 1), endpoint(1), ticket(), &mut opener);
        assert_eq!(opener.opened.len(), 1, "two regions of one host: one connection");
        let (key, ep, credential, _) = &opener.opened[0];
        assert_ne!(*key, LOGIN_CONN);
        assert_eq!(ep, &endpoint(1));
        assert_eq!(credential, &Credential::Observer { client: 7, nonce: 0xfeed });
        assert_eq!(
            opener.packets(1),
            vec![dbg(&ClientPacket::RequestRegionConnection(rc(1, 0))), dbg(&ClientPacket::RequestRegionConnection(rc(1, 1)))]
        );
        assert!(home.try_recv().is_err(), "nothing about them goes home");
        set.on_region_elsewhere(rc(2, 0), endpoint(2), ticket(), &mut opener);
        assert_eq!(opener.opened.len(), 2, "another host: its own connection");
        assert_eq!((host_of(&set, rc(1, 1)), host_of(&set, rc(2, 0))), (Some(1), Some(2)));
    }

    #[test]
    fn subscribe_and_release_go_to_the_owning_connection() {
        let (mut set, home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        let _ = opener.packets(1);
        set.route(Outgoing::Region(rc(1, 0), ClientPacket::ReleaseRegionConnection(rc(1, 0))));
        set.release(rc(1, 0));
        set.route(Outgoing::Region(rc(0, 0), ClientPacket::RequestRegionConnection(rc(0, 0))));
        assert_eq!(opener.packets(1), vec![dbg(&ClientPacket::ReleaseRegionConnection(rc(1, 0)))]);
        assert_eq!(sent(&home), vec![dbg(&ClientPacket::RequestRegionConnection(rc(0, 0)))]);
        // Released: the next subscribe goes home, which says again who owns it.
        set.route(Outgoing::Region(rc(1, 0), ClientPacket::RequestRegionConnection(rc(1, 0))));
        assert_eq!(sent(&home), vec![dbg(&ClientPacket::RequestRegionConnection(rc(1, 0)))]);
        assert!(opener.packets(1).is_empty());
    }

    #[test]
    fn predictions_and_ui_requests_go_home_only() {
        let (mut set, home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        let _ = opener.packets(1);
        let input = game::GameEvent::new(
            game::GameEventKind::PlayerInput(7, game::InputEvent::Key { key: game::Key::KeyW, pressed: true }),
            0,
            rc(1, 0),
        );
        set.route(Outgoing::Home(ClientPacket::GameEventAfterTick(input.clone(), 3)));
        set.route(Outgoing::Home(ClientPacket::RequestEmbarkOptions));
        set.route(Outgoing::Home(ClientPacket::RequestLod(game::lod::LodKey::for_region(2, rc(1, 0)))));
        assert_eq!(home.try_iter().count(), 3, "input, UI and LOD go home, whatever region they are about");
        assert!(opener.packets(1).is_empty());
        set.route(Outgoing::Conn(5, ClientPacket::Leave));
        assert!(home.try_recv().is_err(), "a packet for a connection that is gone is dropped");
    }

    #[test]
    fn traffic_from_the_wrong_connection_is_not_accepted() {
        let (mut set, _home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        let one = set.conn_to(1).expect("a connection to host 1");
        assert!(set.accepts(one, rc(1, 0)));
        assert!(!set.accepts(LOGIN_CONN, rc(1, 0)), "the home no longer serves a region another host does");
        assert!(set.accepts(LOGIN_CONN, rc(0, 0)));
        assert!(!set.accepts(one, rc(0, 0)), "an observer connection serves only its own regions");
        assert!(!set.accepts(99, rc(0, 0)));
    }

    #[test]
    fn observer_close_returns_its_regions_home_close_ends_the_session() {
        let (mut set, _home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        set.on_region_elsewhere(rc(1, 1), endpoint(1), ticket(), &mut opener);
        set.on_region_elsewhere(rc(2, 0), endpoint(2), ticket(), &mut opener);
        let one = set.conn_to(1).unwrap();
        assert_eq!(set.on_closed(one), Closed::Observer { regions: vec![rc(1, 0), rc(1, 1)] });
        assert!(!set.is_live(one));
        assert!(set.accepts(LOGIN_CONN, rc(1, 0)), "unmapped again: the home's");
        assert_eq!(host_of(&set, rc(2, 0)), Some(2), "another host's regions stay");
        assert_eq!(set.on_closed(one), Closed::Observer { regions: Vec::new() }, "closed twice: nothing more");
        assert_eq!(set.on_closed(LOGIN_CONN), Closed::Home);
    }

    /// An observer ticket presented to the home host would be refused (or, before the
    /// edge checked, replace the home session): never dialled.
    #[test]
    fn never_opens_an_observer_connection_to_the_home_host() {
        let (mut set, _home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.on_region_elsewhere(rc(1, 0), endpoint(0), ticket(), &mut opener);
        assert!(opener.opened.is_empty());
        assert_eq!(set.connection_of(rc(1, 0)), LOGIN_CONN);
        // Nor before the home's own host is known.
        let (send, _recv) = unbounded();
        let mut fresh = ConnectionSet::new(ConnHandle { send });
        fresh.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        assert!(opener.opened.is_empty());
    }

    #[test]
    fn login_connection_learns_its_host() {
        let (send, _recv) = unbounded();
        let mut set = ConnectionSet::new(ConnHandle { send });
        assert!(!set.on_host(5, 3), "an unknown connection");
        assert!(set.on_host(LOGIN_CONN, 3));
        assert_eq!((set.home(), set.home_ordinal()), (LOGIN_CONN, Some(3)), "the key never changes");
        assert!(set.on_host(LOGIN_CONN, 3), "the same host again");
        assert!(!set.on_host(LOGIN_CONN, 4), "another host than the one it named");
    }

    /// Carry (Task 17 review): a reopened connection to a host is a new connection with
    /// its own key — what the old one still delivers (its `Closed`, late packets) can
    /// never count as the new one's.
    #[test]
    fn a_reopened_connection_gets_a_new_key() {
        let (mut set, _home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        let first = set.conn_to(1).unwrap();
        assert_eq!(set.on_closed(first), Closed::Observer { regions: vec![rc(1, 0)] });
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        let second = set.conn_to(1).unwrap();
        assert_ne!(first, second, "a reopened connection has a key of its own");
        assert_eq!(opener.opened.iter().map(|o| o.0).collect::<Vec<_>>(), vec![first, second]);
        assert!(!set.is_live(first), "the old key stays dead");
        assert!(!set.accepts(first, rc(1, 0)), "the old connection's late traffic does not count");
        assert!(set.accepts(second, rc(1, 0)));
        assert_eq!(set.on_closed(first), Closed::Observer { regions: Vec::new() }, "a stale close takes nothing");
        assert!(set.is_live(second) && set.accepts(second, rc(1, 0)), "nor closes the new connection");
    }

    /// The home moves to another host's connection when that host's `PlayerRegion` comes
    /// with a hop above every hop seen; the old home's connection stays, serving the
    /// regions it served.
    #[test]
    fn player_region_from_another_host_moves_the_home() {
        let (mut set, home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.route(Outgoing::Region(rc(0, 0), ClientPacket::RequestRegionConnection(rc(0, 0))));
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        let one = set.conn_to(1).unwrap();
        let _ = (sent(&home), opener.packets(1));
        assert!(!set.on_player_region(one), "no hop: not a hand-off's");
        assert_eq!(set.home(), LOGIN_CONN);
        set.on_home_hop(one, 1);
        assert!(set.on_player_region(one));
        assert_eq!((set.home(), set.home_ordinal()), (one, Some(1)));
        assert_eq!(host_of(&set, rc(0, 0)), Some(0), "the old home still serves its region");
        assert!(set.accepts(LOGIN_CONN, rc(0, 0)));
        set.route(Outgoing::Home(ClientPacket::RequestEmbarkOptions));
        assert_eq!(opener.packets(1), vec![dbg(&ClientPacket::RequestEmbarkOptions)], "home packets follow the home");
        assert!(sent(&home).is_empty());
        // The old home's host is an observer's now: a region of it found through the new
        // home reuses the connection there.
        set.on_region_elsewhere(rc(0, 1), endpoint(0), ticket(), &mut opener);
        assert_eq!(opener.opened.len(), 1, "no second connection to host 0");
        assert_eq!(set.connection_of(rc(0, 1)), LOGIN_CONN);
        assert_eq!(sent(&home), vec![dbg(&ClientPacket::RequestRegionConnection(rc(0, 1)))]);
    }

    /// Carry (Task 7): A -> B -> A, and B's `PlayerRegion` arrives after A's newer one:
    /// the home stays on A.
    #[test]
    fn a_late_player_region_from_the_host_left_again_is_ignored() {
        let (mut set, _home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        let one = set.conn_to(1).unwrap();
        // A (the home) took the body back at hop 2: its packets came first.
        set.on_home_hop(LOGIN_CONN, 2);
        assert!(set.on_player_region(LOGIN_CONN), "the home's PlayerRegion always counts");
        // B's, at hop 1, comes late.
        set.on_home_hop(one, 1);
        assert!(!set.on_player_region(one), "an older home's PlayerRegion");
        assert_eq!(set.home(), LOGIN_CONN);
        // In order this time: B at 1, then A at 2.
        let (mut set, _home) = home_on_0();
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        let one = set.conn_to(1).unwrap();
        set.on_home_hop(one, 1);
        assert!(set.on_player_region(one));
        assert_eq!(set.home(), one);
        set.on_home_hop(LOGIN_CONN, 2);
        assert!(set.on_player_region(LOGIN_CONN));
        assert_eq!(set.home(), LOGIN_CONN, "back on A");
    }

    #[test]
    fn idle_connection_closes_after_conn_idle_close_ms() {
        let (mut set, _home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        let one = set.conn_to(1).unwrap();
        assert!(set.close_idle(0).is_empty(), "it serves a region");
        set.release(rc(1, 0));
        assert!(set.close_idle(100).is_empty(), "idle from now");
        assert!(set.close_idle(100 + CONN_IDLE_CLOSE_MS - 1).is_empty());
        assert_eq!(set.close_idle(100 + CONN_IDLE_CLOSE_MS), vec![one]);
        assert!(!set.is_live(one));
        assert!(set.close_idle(u64::MAX).is_empty(), "never the home");
        assert!(set.is_live(LOGIN_CONN));
    }

    #[test]
    fn observer_retry_backs_off_to_a_cap() {
        assert_eq!(observer_retry_delay_ms(1), OBSERVER_RETRY_MS);
        assert_eq!(observer_retry_delay_ms(2), 2 * OBSERVER_RETRY_MS);
        assert_eq!(observer_retry_delay_ms(3), 4 * OBSERVER_RETRY_MS);
        assert_eq!(observer_retry_delay_ms(40), OBSERVER_RETRY_MAX_MS);
        assert!((1..64).map(observer_retry_delay_ms).collect::<Vec<_>>().windows(2).all(|w| w[0] <= w[1]));
    }
}
