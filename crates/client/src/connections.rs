//! One connection per server host (R3: a world runs on several hosts). The session's
//! first connection — the login one — is the HOME: input, UI requests, LOD and the
//! world clock go there. A region another host owns is announced by the home as
//! `ServerPacket::RegionElsewhere`: the client opens (or reuses) a connection to that
//! host with the observer ticket and subscribes the region there. A
//! region → connection table routes the region's (un)subscribes and decides which
//! connection's region traffic counts.
//!
//! Pure game-thread state (no sockets, no clock): the transport behind each
//! connection is a [`ConnHandle`] from a [`ConnOpener`] (`netcode::NetThread` on
//! native; the harness's fake connections in tests). Lives and dies with the game
//! thread's session — it is not a Bevy resource.
use std::collections::BTreeMap;

use crossbeam::channel::Sender;
use game::{ClientPacket, Credential, HostEndpoint, ObserverTicket, RegionId, ServerPacket};

/// How long after an observer connection closed its regions are asked for again,
/// through the home (which answers with a fresh `RegionElsewhere`).
pub const OBSERVER_RETRY_MS: u64 = 1_000;

/// The key of the session's first connection until its `ServerPacket::Host` names the
/// host's ordinal (host ordinals are small; this one never is one).
pub const PROVISIONAL_CONN: u32 = u32::MAX;

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
    /// Connect to host `ordinal` at `endpoint`, presenting `credential` as the Hello. The
    /// connection's events arrive keyed by `ordinal`.
    fn open(&mut self, ordinal: u32, endpoint: &HostEndpoint, credential: Credential) -> ConnHandle;
}

/// What a closed connection was ([`ConnectionSet::on_closed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Closed {
    /// The home: the session ends.
    Home,
    /// Another host's: the regions it served (to be dropped and asked for again).
    Observer { regions: Vec<RegionId> },
}

/// The key a transport tags `packet` with: its connection's key so far. A provisional
/// connection is re-keyed to the host's ordinal once its `Host` packet passed — that
/// packet itself still carries the provisional key, so the game thread re-keys its side
/// ([`ConnectionSet::on_host`]) at the same point of the stream.
pub fn tag_key(key: &mut u32, packet: &ServerPacket) -> u32 {
    let tag = *key;
    if let (PROVISIONAL_CONN, ServerPacket::Host(endpoint)) = (tag, packet) {
        *key = endpoint.ordinal;
    }
    tag
}

/// Every connection of one session, and which serves which region.
pub struct ConnectionSet {
    conns: BTreeMap<u32, ConnHandle>,
    home: u32,
    /// Regions served by a connection other than the home's.
    region_conn: BTreeMap<RegionId, u32>,
}

impl ConnectionSet {
    /// The login connection is the home until told otherwise; its ordinal is learned
    /// from `ServerPacket::Host` ([`Self::on_host`]).
    pub fn new(first: ConnHandle) -> Self {
        Self { conns: BTreeMap::from([(PROVISIONAL_CONN, first)]), home: PROVISIONAL_CONN, region_conn: BTreeMap::new() }
    }

    /// The home connection's key.
    pub fn home(&self) -> u32 {
        self.home
    }

    /// Whether the home's ordinal is known (its `Host` packet came).
    pub fn home_known(&self) -> bool {
        self.home != PROVISIONAL_CONN
    }

    /// Whether connection `conn` is open.
    pub fn is_live(&self, conn: u32) -> bool {
        self.conns.contains_key(&conn)
    }

    /// The connection that serves `region` (the home unless another does).
    pub fn connection_of(&self, region: RegionId) -> u32 {
        self.region_conn.get(&region).copied().unwrap_or(self.home)
    }

    /// Connection `conn`'s `Host` packet named host `ordinal`: a provisional connection
    /// is re-keyed to it. `false` = the packet does not fit the connection (another host
    /// than the one dialled, or an ordinal already connected): the caller closes it.
    pub fn on_host(&mut self, conn: u32, ordinal: u32) -> bool {
        if conn == ordinal && self.conns.contains_key(&conn) {
            return true;
        }
        if conn != PROVISIONAL_CONN || !self.conns.contains_key(&conn) {
            log::warn!("[client] connection {conn} says it is host {ordinal}: not the host it was opened to");
            return false;
        }
        if self.conns.contains_key(&ordinal) {
            log::warn!("[client] a second connection to host {ordinal}");
            return false;
        }
        let handle = self.conns.remove(&conn).expect("checked");
        self.conns.insert(ordinal, handle);
        if self.home == conn {
            self.home = ordinal;
        }
        for served_by in self.region_conn.values_mut().filter(|c| **c == conn) {
            *served_by = ordinal;
        }
        true
    }

    /// Send `out` where it goes: `Home` to the home, `Region(rc, _)` to the connection
    /// that serves `rc` (else the home), `Conn(o, _)` to that connection (dropped if it
    /// is gone).
    pub fn route(&mut self, out: Outgoing) {
        let (conn, packet) = match out {
            Outgoing::Home(packet) => (self.home, packet),
            Outgoing::Region(region, packet) => (self.connection_of(region), packet),
            Outgoing::Conn(conn, packet) => (conn, packet),
        };
        match self.conns.get(&conn) {
            // A transport that already ended drops it; its `Closed` event follows.
            Some(handle) => {
                let _ = handle.send.send(packet);
            }
            None => log::debug!("[client] a packet for connection {conn}, which is closed: dropped"),
        }
    }

    /// The home sent `region`'s owner: open a connection to `endpoint.ordinal` unless one
    /// is live (one connection per host, however many regions), serve `region` from it
    /// and subscribe it there. Never opens an observer connection to the home host
    /// itself (an observer ticket must not replace the home session there).
    pub fn on_region_elsewhere(
        &mut self,
        region: RegionId,
        endpoint: HostEndpoint,
        ticket: ObserverTicket,
        opener: &mut dyn ConnOpener,
    ) {
        let ordinal = endpoint.ordinal;
        if !self.home_known() {
            log::warn!("[client] {region:?} is said to be host {ordinal}'s before the home named itself: ignored");
            return;
        }
        if ordinal == self.home {
            log::warn!("[client] {region:?} is said to be host {ordinal}'s, which is the home: no observer connection");
            return;
        }
        if !self.conns.contains_key(&ordinal) {
            log::info!("[client] connecting to host {ordinal} for {region:?}");
            let credential = Credential::Observer { client: ticket.client, nonce: ticket.nonce };
            let handle = opener.open(ordinal, &endpoint, credential);
            self.conns.insert(ordinal, handle);
        }
        if let Some(old) = self.region_conn.insert(region, ordinal).filter(|old| *old != ordinal) {
            // It moved hosts: the one that served it lets it go.
            self.route(Outgoing::Conn(old, ClientPacket::ReleaseRegionConnection(region)));
        }
        self.route(Outgoing::Conn(ordinal, ClientPacket::RequestRegionConnection(region)));
    }

    /// Whether region traffic for `region` from `conn` counts: only from the connection
    /// the table names (the home when unmapped).
    pub fn accepts(&self, conn: u32, region: RegionId) -> bool {
        self.connection_of(region) == conn && self.conns.contains_key(&conn)
    }

    /// Connection `conn` is the home from now on.
    pub fn set_home(&mut self, conn: u32) {
        self.home = conn;
    }

    /// Connection `conn` ended (or was closed here): forget it. The home's end ends the
    /// session; another's hands back the regions it served (unmapped: the home's again).
    pub fn on_closed(&mut self, conn: u32) -> Closed {
        self.conns.remove(&conn);
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
        fn open(&mut self, ordinal: u32, endpoint: &HostEndpoint, credential: Credential) -> ConnHandle {
            let (send, recv) = unbounded();
            self.opened.push((ordinal, endpoint.clone(), credential, recv));
            ConnHandle { send }
        }
    }

    impl FakeOpener {
        /// What was sent on host `ordinal`'s connection since the last call ([`dbg`]).
        fn packets(&self, ordinal: u32) -> Vec<String> {
            self.opened.iter().filter(|o| o.0 == ordinal).flat_map(|o| o.3.try_iter()).map(|p| dbg(&p)).collect()
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
        assert_eq!(set.home(), PROVISIONAL_CONN);
        assert!(set.on_host(PROVISIONAL_CONN, 0));
        assert_eq!(set.home(), 0);
        (set, recv)
    }

    fn rc(x: i32, z: i32) -> RegionCoords {
        RegionCoords::new(x, z)
    }

    #[test]
    fn region_elsewhere_opens_one_connection_per_host() {
        let (mut set, home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        set.on_region_elsewhere(rc(1, 1), endpoint(1), ticket(), &mut opener);
        assert_eq!(opener.opened.len(), 1, "two regions of one host: one connection");
        let (ordinal, ep, credential, _) = &opener.opened[0];
        assert_eq!((*ordinal, ep), (1, &endpoint(1)));
        assert_eq!(credential, &Credential::Observer { client: 7, nonce: 0xfeed });
        assert_eq!(
            opener.packets(1),
            vec![dbg(&ClientPacket::RequestRegionConnection(rc(1, 0))), dbg(&ClientPacket::RequestRegionConnection(rc(1, 1)))]
        );
        assert!(home.try_recv().is_err(), "nothing about them goes home");
        set.on_region_elsewhere(rc(2, 0), endpoint(2), ticket(), &mut opener);
        assert_eq!(opener.opened.len(), 2, "another host: its own connection");
        assert_eq!((set.connection_of(rc(1, 1)), set.connection_of(rc(2, 0))), (1, 2));
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
        assert!(set.accepts(1, rc(1, 0)));
        assert!(!set.accepts(0, rc(1, 0)), "the home no longer serves a region another host does");
        assert!(set.accepts(0, rc(0, 0)));
        assert!(!set.accepts(1, rc(0, 0)), "an observer connection serves only its own regions");
        assert!(!set.accepts(9, rc(0, 0)));
    }

    #[test]
    fn observer_close_returns_its_regions_home_close_ends_the_session() {
        let (mut set, _home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        set.on_region_elsewhere(rc(1, 1), endpoint(1), ticket(), &mut opener);
        set.on_region_elsewhere(rc(2, 0), endpoint(2), ticket(), &mut opener);
        assert_eq!(set.on_closed(1), Closed::Observer { regions: vec![rc(1, 0), rc(1, 1)] });
        assert!(!set.is_live(1));
        assert!(set.accepts(0, rc(1, 0)), "unmapped again: the home's");
        assert_eq!(set.connection_of(rc(2, 0)), 2, "another host's regions stay");
        assert_eq!(set.on_closed(1), Closed::Observer { regions: Vec::new() }, "closed twice: nothing more");
        assert_eq!(set.on_closed(0), Closed::Home);
    }

    /// An observer ticket presented to the home host would be refused (or, before the
    /// edge checked, replace the home session): never dialled.
    #[test]
    fn never_opens_an_observer_connection_to_the_home_host() {
        let (mut set, _home) = home_on_0();
        let mut opener = FakeOpener::default();
        set.on_region_elsewhere(rc(1, 0), endpoint(0), ticket(), &mut opener);
        assert!(opener.opened.is_empty());
        assert_eq!(set.connection_of(rc(1, 0)), 0);
        // Nor before the home's own ordinal is known.
        let (send, _recv) = unbounded();
        let mut fresh = ConnectionSet::new(ConnHandle { send });
        fresh.on_region_elsewhere(rc(1, 0), endpoint(1), ticket(), &mut opener);
        assert!(opener.opened.is_empty());
    }

    #[test]
    fn provisional_connection_is_rekeyed_on_its_host_packet() {
        let mut key = PROVISIONAL_CONN;
        let host = ServerPacket::Host(endpoint(3));
        assert_eq!(tag_key(&mut key, &ServerPacket::Refused("x".into())), PROVISIONAL_CONN);
        assert_eq!(tag_key(&mut key, &host), PROVISIONAL_CONN, "the Host packet itself: the old key");
        assert_eq!(tag_key(&mut key, &ServerPacket::Refused("x".into())), 3);
        assert_eq!(tag_key(&mut key, &ServerPacket::Host(endpoint(4))), 3, "a known key never changes");

        let (send, _recv) = unbounded();
        let mut set = ConnectionSet::new(ConnHandle { send });
        assert!(!set.on_host(5, 3), "an unknown connection");
        assert!(set.on_host(PROVISIONAL_CONN, 3));
        assert_eq!(set.home(), 3);
        assert!(set.on_host(3, 3), "a connection keyed by its ordinal already");
        assert!(!set.on_host(3, 4), "another host than the one dialled");
    }
}
