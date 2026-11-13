//! Host ↔ host over the service link (R3): one [`QuicPeerLink`] per other host, the
//! [`game::PeerLink`] the manager's [`game::PeerSession`]s run over.
//!
//! **One link per pair**: the HIGHER ordinal dials the LOWER. Every host binds one
//! `svc-link` listener ([`PEER_BIND_ENV`]) that admits `PeerRole::Host` peers of a higher
//! ordinal below `hosts` with the shared service token, writes its address and pin into
//! its `hosts` row (its one `register_host` of the boot), and finds each lower ordinal's
//! address and pin in [`StoreHandle::hosts`] — re-read every [`PEER_DISCOVERY_SECS`] while
//! that peer is down (a restarted peer may listen elsewhere).
//!
//! **One ordered stream** per connection carries every frame (hello, reliable, ack,
//! volatile) in send order. Every inbound frame is tagged with its connection (the
//! listener's `PeerId`, or a dialer's `Up` count); a link drops frames whose tag is not
//! its live connection's, so no frame of an older connection surfaces after
//! [`PeerLink::connection`] changed. Of two live connections from one ordinal the newer
//! replaces the older (the coordinator's admission: the higher claim epoch wins, ties to
//! the newer).
//!
//! [`PEER_LINK_DELAY_MS_ENV`] holds every outgoing frame that long before it is written
//! (order kept; a latency measurement knob, never on in play).

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, Sender};
use game::{HostEndpoint, PeerLink, PeerMsg};
use svc_link::{LinkDialer, LinkEvent, LinkHello, LinkListener, PeerId, PeerRole, PeerSenders};
use world_store::{Fence, HostBoot, HostRow, StoreError, StoreHandle};

/// Environment: where a host's peer listener binds (default `127.0.0.1:{6500 + ordinal}`).
pub const PEER_BIND_ENV: &str = "VOXEL_PEER_BIND";
/// The default peer listener port of host 0; host `n` listens on this plus `n`.
pub const PEER_PORT_BASE: u16 = 6500;
/// Environment: the file a host writes its peer listener's certificate pin to (the
/// certificate and key are kept beside it, so a restart keeps the pin). Default
/// `target/dev-r3/host-{ordinal}.peer.cert-sha256`.
pub const PEER_CERT_HASH_FILE_ENV: &str = "VOXEL_PEER_CERT_HASH_FILE";
/// Environment: the QUIC address players reach this host at (`ServerPacket::Host`, the
/// `hosts` row). Default `127.0.0.1:{VOXEL_SERVER_PORT}`.
pub const ADVERTISE_QUIC_ENV: &str = "VOXEL_ADVERTISE_QUIC";
/// Environment: the WebTransport address browsers reach this host at (default: none).
pub const ADVERTISE_WT_ENV: &str = "VOXEL_ADVERTISE_WT";
/// Environment: hold every outgoing peer frame this many ms (default 0: no queue at all).
pub const PEER_LINK_DELAY_MS_ENV: &str = "VOXEL_PEER_LINK_DELAY_MS";
/// What a host logs (WARN) at start when [`PEER_LINK_DELAY_MS_ENV`] is not 0.
pub const PEER_LINK_DELAY_WARNING: &str = "peer link delay injected — measurement only";
/// While a lower ordinal is down, its `hosts` row is re-read this often.
pub const PEER_DISCOVERY_SECS: u64 = 2;

/// The peer listener's address from [`PEER_BIND_ENV`]'s value; unset or empty is
/// `127.0.0.1:{PEER_PORT_BASE + ordinal}`.
pub fn peer_bind_from(ordinal: u32, value: Option<&str>) -> Result<SocketAddr, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => v.parse().map_err(|_| format!("{PEER_BIND_ENV}={v:?} is not an address (ip:port)")),
        None => u16::try_from(ordinal)
            .ok()
            .and_then(|o| PEER_PORT_BASE.checked_add(o))
            .map(|port| SocketAddr::from(([127, 0, 0, 1], port)))
            .ok_or_else(|| format!("host {ordinal} has no default peer port: set {PEER_BIND_ENV}")),
    }
}

/// The peer pin file from [`PEER_CERT_HASH_FILE_ENV`]'s value; unset or empty is
/// `target/dev-r3/host-{ordinal}.peer.cert-sha256`.
pub fn peer_cert_hash_file_from(ordinal: u32, value: Option<&str>) -> PathBuf {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => PathBuf::from(format!("target/dev-r3/host-{ordinal}.peer.cert-sha256")),
    }
}

/// The advertised QUIC address from [`ADVERTISE_QUIC_ENV`]'s value; unset or empty is
/// `127.0.0.1:{port}` (the server's own port).
pub fn advertise_quic_from(value: Option<&str>, port: u16) -> String {
    value.map(str::trim).filter(|v| !v.is_empty()).map_or_else(|| format!("127.0.0.1:{port}"), str::to_string)
}

/// The advertised WebTransport address from [`ADVERTISE_WT_ENV`]'s value (none by default).
pub fn advertise_wt_from(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|v| !v.is_empty()).map(str::to_string)
}

/// The injected delay from [`PEER_LINK_DELAY_MS_ENV`]'s value; unset or empty is 0.
pub fn peer_link_delay_from(value: Option<&str>) -> Result<Duration, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(Duration::ZERO),
        Some(v) => v
            .parse::<u64>()
            .map(Duration::from_millis)
            .map_err(|_| format!("{PEER_LINK_DELAY_MS_ENV}={v:?} is not a whole number of ms")),
    }
}

/// Where a host's peer listener binds, where it keeps its pin, and the injected delay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerLinkConfig {
    pub bind: SocketAddr,
    pub cert_hash_file: PathBuf,
    pub delay: Duration,
}

impl PeerLinkConfig {
    /// From [`PEER_BIND_ENV`], [`PEER_CERT_HASH_FILE_ENV`] and [`PEER_LINK_DELAY_MS_ENV`]
    /// (a non-zero delay logs [`PEER_LINK_DELAY_WARNING`]).
    pub fn from_env(ordinal: u32) -> Result<PeerLinkConfig, String> {
        let var = |name: &str| std::env::var(name).ok();
        let config = PeerLinkConfig {
            bind: peer_bind_from(ordinal, var(PEER_BIND_ENV).as_deref())?,
            cert_hash_file: peer_cert_hash_file_from(ordinal, var(PEER_CERT_HASH_FILE_ENV).as_deref()),
            delay: peer_link_delay_from(var(PEER_LINK_DELAY_MS_ENV).as_deref())?,
        };
        if !config.delay.is_zero() {
            log::warn!("[peers] {PEER_LINK_DELAY_WARNING}: {} ms on every outgoing frame", config.delay.as_millis());
        }
        Ok(config)
    }
}

/// Registers this boot as host `ordinal` — the boot's ONE `register_host` (a second in
/// the same boot would raise the world generation) — with where players reach it
/// (`endpoint`) and, with `peer`, its peer listener's address and pin.
pub fn register(
    store: &StoreHandle,
    fence: &Fence,
    ordinal: u32,
    boot: u64,
    endpoint: &HostEndpoint,
    peer: Option<(String, String)>,
) -> Result<HostBoot, StoreError> {
    let (peer_addr, peer_cert) = peer.unzip();
    let row = HostRow {
        ordinal,
        boot,
        generation: 0,
        clean: false,
        peer_addr,
        peer_cert,
        quic: endpoint.quic.clone(),
        webtransport: endpoint.webtransport.clone(),
        wt_cert_hash: endpoint.cert_hash.clone(),
    };
    store.register_host(row, fence)
}

/// The other hosts that left the world cleanly in a committed leave cut, each with the boot
/// that left: only a committed leave decision marks a `hosts` row clean, and every boot makes
/// its own row dirty when it registers (a registration racing the leave commit loses or wins
/// on the world generation, so a clean row always names the boot that left). A crashed host's
/// row is dirty: it is not one of them. Read at boot for
/// [`game::WorldManager::note_left_peers`]; a later boot is expected again from its hello.
pub fn left_cleanly(store: &world_store::StoreHandle, me: u32) -> Result<Vec<(u32, u64)>, world_store::StoreError> {
    Ok(store.hosts()?.into_iter().filter(|r| r.clean && r.ordinal != me).map(|r| (r.ordinal, r.boot)).collect())
}

/// Whether a [`PeerLinks::start`] error is the store's fence (this boot was replaced
/// between its claim and its registration): exit fenced, not a start-up error.
pub fn is_fenced(e: &io::Error) -> bool {
    e.get_ref().and_then(|e| e.downcast_ref::<StoreError>()).is_some_and(|e| matches!(e, StoreError::Fenced { .. }))
}

// --- the link --------------------------------------------------------------------------

/// What reaches a [`QuicPeerLink`] from its connections, each tagged with the connection
/// it belongs to (unique per link).
#[derive(Debug)]
enum Inbound {
    Up(u64),
    Msg(u64, PeerMsg),
    Down(u64),
}

/// Where a [`QuicPeerLink`]'s frames go: the connection tagged `tag`, if it is still the
/// live one (a frame for an ended connection is dropped).
trait PeerWire {
    fn write(&self, tag: u64, msg: PeerMsg);
    fn close(&self, tag: u64);
}

/// The listener's side: the tag is the connection's `PeerId`.
struct ListenerWire(PeerSenders<PeerMsg>);

impl PeerWire for ListenerWire {
    fn write(&self, tag: u64, msg: PeerMsg) {
        self.0.send(PeerId(tag), msg);
    }
    fn close(&self, tag: u64) {
        self.0.close(PeerId(tag));
    }
}

/// What a link asks of its dialing thread.
enum DialCmd {
    Write(u64, PeerMsg),
    Close(u64),
}

/// The dialer's side: its thread ([`dial_peer`]) writes a frame only to the connection
/// it is tagged for.
struct DialerWire(Sender<DialCmd>);

impl PeerWire for DialerWire {
    fn write(&self, tag: u64, msg: PeerMsg) {
        let _ = self.0.send(DialCmd::Write(tag, msg));
    }
    fn close(&self, tag: u64) {
        let _ = self.0.send(DialCmd::Close(tag));
    }
}

/// One host's end of the link to one other host, over the service link.
///
/// State moves only in [`PeerLink::pump`]: inbound events are taken in order, a frame of
/// any connection but the live one is dropped, and a connection change (`Up` of another
/// connection, `Down` of the live one) waits while frames before it are unread — so the
/// session reads them as the link stood. Every new connection counts
/// [`PeerLink::connection`] up. Sends while down are dropped (the session resends what
/// is reliable after the next hello).
pub struct QuicPeerLink {
    peer: u32,
    wire: Box<dyn PeerWire + Send>,
    events: Receiver<Inbound>,
    /// A change taken while frames before it were still unread.
    held: Option<Inbound>,
    inbox: VecDeque<PeerMsg>,
    /// The live connection's tag.
    live: Option<u64>,
    connection: u64,
    /// [`PEER_LINK_DELAY_MS_ENV`]: zero = frames are written at once, never queued.
    delay: Duration,
    /// Frames held by the delay: when each is due, its connection, the frame — in order.
    delayed: VecDeque<(Instant, u64, PeerMsg)>,
    clock: Box<dyn Fn() -> Instant + Send>,
}

impl QuicPeerLink {
    fn new(peer: u32, wire: Box<dyn PeerWire + Send>, events: Receiver<Inbound>, delay: Duration) -> QuicPeerLink {
        QuicPeerLink {
            peer,
            wire,
            events,
            held: None,
            inbox: VecDeque::new(),
            live: None,
            connection: 0,
            delay,
            delayed: VecDeque::new(),
            clock: Box::new(Instant::now),
        }
    }

    /// Write every delayed frame that is due, in order.
    fn flush_delayed(&mut self) {
        if self.delayed.is_empty() {
            return;
        }
        let now = (self.clock)();
        while self.delayed.front().is_some_and(|(due, _, _)| *due <= now) {
            let (_, tag, msg) = self.delayed.pop_front().expect("a front");
            self.wire.write(tag, msg);
        }
    }
}

impl PeerLink for QuicPeerLink {
    fn send(&mut self, msg: PeerMsg) {
        let Some(tag) = self.live else {
            log::debug!("[peers] link to host {} down: a frame lost", self.peer);
            return;
        };
        if self.delay.is_zero() {
            self.wire.write(tag, msg);
        } else {
            self.delayed.push_back(((self.clock)() + self.delay, tag, msg));
        }
    }

    fn recv(&mut self) -> Option<PeerMsg> {
        self.inbox.pop_front()
    }

    fn is_up(&self) -> bool {
        self.live.is_some()
    }

    fn connection(&self) -> u64 {
        self.connection
    }

    fn pump(&mut self, _now_ms: u64) {
        self.flush_delayed();
        loop {
            let event = match self.held.take() {
                Some(event) => event,
                None => match self.events.try_recv() {
                    Ok(event) => event,
                    Err(_) => return,
                },
            };
            match event {
                Inbound::Msg(tag, msg) => {
                    if self.live == Some(tag) {
                        self.inbox.push_back(msg);
                    }
                }
                // The end of a connection that is not the live one changes nothing.
                Inbound::Down(tag) if self.live != Some(tag) => {}
                change if !self.inbox.is_empty() => {
                    // Read what came before the change first, as the link stood.
                    self.held = Some(change);
                    return;
                }
                Inbound::Up(tag) => {
                    self.live = Some(tag);
                    self.connection += 1;
                    log::info!("[peers] link to host {} up (connection {})", self.peer, self.connection);
                }
                Inbound::Down(_) => {
                    self.live = None;
                    log::warn!("[peers] link to host {} down", self.peer);
                }
            }
        }
    }

    fn close(&mut self) {
        let Some(tag) = self.live.take() else { return };
        log::warn!("[peers] closing connection {} to host {}", self.connection, self.peer);
        self.wire.close(tag);
        // Nothing of the closed connection surfaces any more.
        self.inbox.clear();
        self.delayed.retain(|(_, t, _)| *t != tag);
    }
}

// --- the listener's admission ----------------------------------------------------------

/// Which host peers the listener of host `mine` (of `hosts`) serves: a higher ordinal
/// below `hosts` (the higher dials the lower), one live connection per ordinal — the
/// highest claim epoch's, ties to the newer connection.
struct Admission {
    mine: u32,
    hosts: u32,
    best_epoch: BTreeMap<u32, u64>,
    live: BTreeMap<u32, PeerId>,
}

impl Admission {
    fn new(mine: u32, hosts: u32) -> Self {
        Admission { mine, hosts, best_epoch: BTreeMap::new(), live: BTreeMap::new() }
    }

    /// `peer` said hello as `role`: its ordinal and the connection it replaces, or why
    /// it is refused.
    fn admit(&mut self, peer: PeerId, role: &PeerRole) -> Result<(u32, Option<PeerId>), String> {
        let PeerRole::Host { ordinal, epoch, .. } = *role else {
            return Err(format!("{role:?} is not a host"));
        };
        if ordinal >= self.hosts {
            return Err(format!("host {ordinal} is not below {} hosts", self.hosts));
        }
        if ordinal == self.mine {
            return Err(format!("host {ordinal} is this host"));
        }
        if ordinal < self.mine {
            return Err(format!("host {ordinal} is lower: this host dials it"));
        }
        let best = self.best_epoch.get(&ordinal).copied().unwrap_or(0);
        if epoch < best {
            return Err(format!("host {ordinal} with claim epoch {epoch}: epoch {best} has it (a stale process)"));
        }
        self.best_epoch.insert(ordinal, epoch);
        Ok((ordinal, self.live.insert(ordinal, peer)))
    }

    /// `peer`'s connection ended.
    fn gone(&mut self, ordinal: u32, peer: PeerId) {
        if self.live.get(&ordinal) == Some(&peer) {
            self.live.remove(&ordinal);
        }
    }
}

/// The listener's events, routed to each higher ordinal's link.
fn route_listener(
    mut admission: Admission,
    events: Receiver<(PeerId, LinkEvent<PeerMsg>)>,
    senders: PeerSenders<PeerMsg>,
    links: BTreeMap<u32, Sender<Inbound>>,
    wake: Sender<()>,
) {
    let mut of_peer: BTreeMap<PeerId, u32> = BTreeMap::new();
    for (peer, ev) in events {
        let routed = match ev {
            LinkEvent::Up(hello) => match admission.admit(peer, &hello.role) {
                Ok((ordinal, replaced)) => {
                    if let Some(old) = replaced {
                        log::info!("[peers] host {ordinal}: connection {} replaces {}", peer.0, old.0);
                        of_peer.remove(&old);
                        senders.close(old);
                    }
                    of_peer.insert(peer, ordinal);
                    Some((ordinal, Inbound::Up(peer.0)))
                }
                Err(why) => {
                    log::warn!("[peers] refusing peer {}: {why}", peer.0);
                    senders.close(peer);
                    None
                }
            },
            LinkEvent::Msg(msg) => of_peer.get(&peer).map(|&o| (o, Inbound::Msg(peer.0, msg))),
            LinkEvent::Down(why) => of_peer.remove(&peer).map(|o| {
                log::info!("[peers] host {o}: connection {} ended: {why}", peer.0);
                admission.gone(o, peer);
                (o, Inbound::Down(peer.0))
            }),
        };
        if let Some((ordinal, inbound)) = routed {
            if let Some(link) = links.get(&ordinal) {
                let _ = link.send(inbound);
                let _ = wake.try_send(());
            }
        }
    }
}

// --- the dialer --------------------------------------------------------------------------

/// Host `ordinal`'s peer address and pin from the `hosts` table (`None`: no row, or the
/// row names no listener yet).
fn lookup(store: &StoreHandle, ordinal: u32) -> Result<Option<(SocketAddr, String)>, String> {
    let rows = store.hosts().map_err(|e| e.to_string())?;
    let Some(row) = rows.into_iter().find(|r| r.ordinal == ordinal) else { return Ok(None) };
    match (row.peer_addr, row.peer_cert) {
        (Some(addr), Some(pin)) => {
            let addr = addr.parse().map_err(|_| format!("host {ordinal}'s peer address {addr:?} is not ip:port"))?;
            Ok(Some((addr, pin)))
        }
        _ => Ok(None),
    }
}

/// The thread behind a link to the lower host `peer`: finds it in the `hosts` table,
/// dials it (a [`LinkDialer`] that redials one address for ever), re-reads the row every
/// [`PEER_DISCOVERY_SECS`] while the peer is down and dials anew when it changed; on a
/// close, drops the dialer (ending its connection) and dials again.
fn dial_peer(
    peer: u32,
    hello: LinkHello,
    store: StoreHandle,
    inbound: Sender<Inbound>,
    cmds: Receiver<DialCmd>,
    wake: Sender<()>,
) {
    type Dialer = (Sender<PeerMsg>, Receiver<LinkEvent<PeerMsg>>);
    let dial = |(addr, pin): &(SocketAddr, String)| -> Dialer {
        log::info!("[peers] dialing host {peer} at {addr} (pin {pin})");
        LinkDialer::connect::<PeerMsg, PeerMsg>(*addr, pin.clone(), hello.clone())
    };
    let discovery = Duration::from_secs(PEER_DISCOVERY_SECS);
    let mut target: Option<(SocketAddr, String)> = None;
    let mut dialer: Option<Dialer> = None;
    // The live connection's tag, and the last tag handed out.
    let mut live: Option<u64> = None;
    let mut tags = 0u64;
    let mut next_lookup = Instant::now();
    let mut said_missing = false;
    loop {
        if live.is_none() && Instant::now() >= next_lookup {
            next_lookup = Instant::now() + discovery;
            match lookup(&store, peer) {
                Ok(Some(found)) => {
                    said_missing = false;
                    if target.as_ref() != Some(&found) || dialer.is_none() {
                        if target.is_some() {
                            log::info!("[peers] host {peer}'s hosts row changed: dialing anew");
                        }
                        // The old dialer (if any) stops as it is dropped.
                        drop(dialer.take());
                        dialer = Some(dial(&found));
                        target = Some(found);
                    }
                }
                Ok(None) => {
                    if !said_missing {
                        log::info!("[peers] host {peer} has no peer listener in the hosts table yet; looking again");
                        said_missing = true;
                    }
                }
                Err(e) => log::warn!("[peers] reading host {peer}'s hosts row: {e}"),
            }
        }
        let dial_events = dialer.as_ref().map_or_else(crossbeam::channel::never, |d| d.1.clone());
        let timeout = if live.is_some() { discovery } else { next_lookup.saturating_duration_since(Instant::now()) };
        let mut sent: Option<Inbound> = None;
        crossbeam::channel::select! {
            recv(cmds) -> cmd => match cmd {
                Ok(DialCmd::Write(tag, msg)) => {
                    if live == Some(tag) {
                        if let Some((out, _)) = &dialer {
                            let _ = out.send(msg);
                        }
                    }
                }
                Ok(DialCmd::Close(tag)) => {
                    if live == Some(tag) {
                        live = None;
                        log::warn!("[peers] closing the connection to host {peer}: dialing it again");
                        drop(dialer.take());
                        dialer = target.as_ref().map(dial);
                        sent = Some(Inbound::Down(tag));
                    }
                }
                // The link was dropped: dropping the dialer ends it.
                Err(_) => return,
            },
            recv(dial_events) -> ev => match ev {
                Ok(LinkEvent::Up(_)) => {
                    if let Some(old) = live.take() {
                        let _ = inbound.send(Inbound::Down(old));
                    }
                    tags += 1;
                    live = Some(tags);
                    sent = Some(Inbound::Up(tags));
                }
                Ok(LinkEvent::Msg(msg)) => sent = live.map(|tag| Inbound::Msg(tag, msg)),
                Ok(LinkEvent::Down(why)) => {
                    if let Some(tag) = live.take() {
                        log::warn!("[peers] connection to host {peer} down: {why}");
                        sent = Some(Inbound::Down(tag));
                    }
                }
                // The dialer stopped (its setup failed): look the peer up and dial anew.
                Err(_) => {
                    dialer = None;
                    if let Some(tag) = live.take() {
                        sent = Some(Inbound::Down(tag));
                    }
                }
            },
            default(timeout) => {}
        }
        if let Some(ev) = sent {
            if inbound.send(ev).is_err() {
                return;
            }
            let _ = wake.try_send(());
        }
    }
}

// --- the set of links --------------------------------------------------------------------

/// A host's peer listener and its links to every other host of the world.
pub struct PeerLinks {
    listener: LinkListener,
    links: Vec<(u32, Box<dyn PeerLink + Send>)>,
    registered: HostBoot,
    wake: Receiver<()>,
}

impl PeerLinks {
    /// Binds this host's peer listener (`config.bind`, its pin written to
    /// `config.cert_hash_file`), registers the boot — the boot's one `register_host`, with
    /// the listener's address and pin and `endpoint`'s addresses — and starts a link to
    /// every other of `hosts` hosts: a higher ordinal dials this listener, a lower one is
    /// dialed at the address its `hosts` row names. `token` is the shared service token;
    /// `fence` this boot's claim. A fenced registration is an error [`is_fenced`] knows.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        ordinal: u32,
        hosts: u32,
        boot: u64,
        endpoint: HostEndpoint,
        token: String,
        store: StoreHandle,
        fence: &Fence,
        config: PeerLinkConfig,
    ) -> io::Result<PeerLinks> {
        if let Some(dir) = config.cert_hash_file.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let (listener, events, senders) =
            LinkListener::bind::<PeerMsg, PeerMsg>(config.bind, token.clone(), Some(config.cert_hash_file.clone()))?;
        let peer = Some((listener.local_addr().to_string(), listener.cert_sha256().to_string()));
        let registered = register(&store, fence, ordinal, boot, &endpoint, peer).map_err(io::Error::other)?;
        log::info!(
            "[peers] host {ordinal} of {hosts}: peers dial {} (pin {})",
            listener.local_addr(),
            listener.cert_sha256()
        );
        let hello = LinkHello::new(token, PeerRole::Host { ordinal, boot, epoch: fence.claim_epoch() });
        let (wake_tx, wake) = crossbeam::channel::bounded(1);
        let mut links: Vec<(u32, Box<dyn PeerLink + Send>)> = Vec::new();
        let mut routes = BTreeMap::new();
        for other in (0..hosts).filter(|&o| o != ordinal) {
            let (inbound, events) = crossbeam::channel::unbounded();
            let wire: Box<dyn PeerWire + Send> = if other > ordinal {
                routes.insert(other, inbound);
                Box::new(ListenerWire(senders.clone()))
            } else {
                let (cmds_tx, cmds) = crossbeam::channel::unbounded();
                let (hello, store, wake) = (hello.clone(), store.clone(), wake_tx.clone());
                std::thread::Builder::new()
                    .name(format!("peer-dial-{other}"))
                    .spawn(move || dial_peer(other, hello, store, inbound, cmds, wake))?;
                Box::new(DialerWire(cmds_tx))
            };
            links.push((other, Box::new(QuicPeerLink::new(other, wire, events, config.delay))));
        }
        std::thread::Builder::new()
            .name("peer-listener".into())
            .spawn(move || route_listener(Admission::new(ordinal, hosts), events, senders, routes, wake_tx))?;
        Ok(PeerLinks { listener, links, registered, wake })
    }

    /// The links, one per other host (by ordinal), for [`game::WorldManager::set_peer`];
    /// empty after the first call.
    pub fn take(&mut self) -> Vec<(u32, Box<dyn PeerLink + Send>)> {
        std::mem::take(&mut self.links)
    }

    /// What this boot's registration decided (its world generation).
    pub fn registered(&self) -> HostBoot {
        self.registered
    }

    /// Ready when a link has something (a frame, up or down): the host loop selects on
    /// it; the manager pumps the links.
    pub fn wake(&self) -> Receiver<()> {
        self.wake.clone()
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr()
    }

    pub fn cert_sha256(&self) -> &str {
        self.listener.cert_sha256()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Frames written, by connection tag; and closes.
    #[derive(Clone, Default)]
    struct Tap(Arc<Mutex<(Vec<(u64, PeerMsg)>, Vec<u64>)>>);

    impl PeerWire for Tap {
        fn write(&self, tag: u64, msg: PeerMsg) {
            self.0.lock().unwrap().0.push((tag, msg));
        }
        fn close(&self, tag: u64) {
            self.0.lock().unwrap().1.push(tag);
        }
    }

    fn link(delay: Duration) -> (QuicPeerLink, Tap, Sender<Inbound>) {
        let tap = Tap::default();
        let (tx, rx) = crossbeam::channel::unbounded();
        (QuicPeerLink::new(1, Box::new(tap.clone()), rx, delay), tap, tx)
    }

    fn ack(n: u64) -> PeerMsg {
        PeerMsg::Ack { through: n }
    }

    /// With a 5 ms delay, three frames are held until they are due and then written in
    /// the order sent (a fake clock).
    #[test]
    fn delay_knob_holds_frames_in_order() {
        let (mut link, tap, tx) = link(Duration::from_millis(5));
        let t0 = Instant::now();
        let now = Arc::new(Mutex::new(t0));
        let clock = now.clone();
        link.clock = Box::new(move || *clock.lock().unwrap());
        tx.send(Inbound::Up(9)).unwrap();
        link.pump(0);
        for n in 1..=3 {
            link.send(ack(n));
        }
        *now.lock().unwrap() = t0 + Duration::from_millis(4);
        link.pump(0);
        assert!(tap.0.lock().unwrap().0.is_empty(), "nothing is written before it is due");
        *now.lock().unwrap() = t0 + Duration::from_millis(5);
        link.pump(0);
        assert_eq!(tap.0.lock().unwrap().0, vec![(9, ack(1)), (9, ack(2)), (9, ack(3))], "all three, in order");
    }

    /// Without a delay a frame is written at once; frames of a connection that is not the
    /// live one never surface; a change waits until what came before it is read; a close
    /// ends the live connection and forgets what it received.
    #[test]
    fn frames_of_an_older_connection_never_surface() {
        let (mut link, tap, tx) = link(Duration::ZERO);
        link.send(ack(0));
        assert!(tap.0.lock().unwrap().0.is_empty(), "nothing is sent while down");
        for ev in [Inbound::Up(1), Inbound::Msg(1, ack(1)), Inbound::Up(2), Inbound::Msg(1, ack(99)), Inbound::Msg(2, ack(2))] {
            tx.send(ev).unwrap();
        }
        link.pump(0);
        assert_eq!((link.is_up(), link.connection()), (true, 1));
        assert_eq!(link.recv(), Some(ack(1)));
        assert_eq!(link.recv(), None, "connection 2's frame waits for the change");
        link.pump(0);
        assert_eq!(link.connection(), 2, "the newer connection replaced the older");
        assert_eq!(link.recv(), Some(ack(2)), "the old connection's late frame was dropped");
        link.send(ack(3));
        assert_eq!(tap.0.lock().unwrap().0, vec![(2, ack(3))], "written at once, to the live connection");
        tx.send(Inbound::Down(1)).unwrap();
        link.pump(0);
        assert!(link.is_up(), "the end of a replaced connection changes nothing");

        tx.send(Inbound::Msg(2, ack(4))).unwrap();
        link.pump(0);
        link.close();
        assert_eq!(tap.0.lock().unwrap().1, vec![2]);
        assert!(!link.is_up());
        assert_eq!(link.recv(), None, "a closed connection's frames are forgotten");
        tx.send(Inbound::Msg(2, ack(5))).unwrap();
        tx.send(Inbound::Up(3)).unwrap();
        link.pump(0);
        assert_eq!((link.is_up(), link.connection(), link.recv()), (true, 3, None));
    }

    /// The listener admits higher ordinals below `hosts` only; of two connections of one
    /// ordinal the newer replaces the older unless its claim epoch is lower.
    #[test]
    fn admission_takes_higher_ordinals_and_the_newest_connection() {
        let host = |ordinal, epoch| PeerRole::Host { ordinal, boot: 1, epoch };
        let mut a = Admission::new(1, 3);
        assert!(a.admit(PeerId(1), &host(3, 1)).is_err(), "not below hosts");
        assert!(a.admit(PeerId(2), &host(1, 1)).is_err(), "itself");
        assert!(a.admit(PeerId(3), &host(0, 1)).is_err(), "a lower host is dialed, not admitted");
        assert!(a.admit(PeerId(4), &PeerRole::Login).is_err(), "hosts only");
        assert_eq!(a.admit(PeerId(5), &host(2, 4)), Ok((2, None)));
        assert_eq!(a.admit(PeerId(6), &host(2, 4)), Ok((2, Some(PeerId(5)))), "the newer replaces the older");
        assert!(a.admit(PeerId(7), &host(2, 3)).is_err(), "a stale process");
        a.gone(2, PeerId(5));
        assert_eq!(a.live.get(&2), Some(&PeerId(6)), "a replaced connection's end changes nothing");
        a.gone(2, PeerId(6));
        assert_eq!(a.admit(PeerId(8), &host(2, 4)), Ok((2, None)));
    }

    #[test]
    fn env_defaults() {
        assert_eq!(peer_bind_from(2, None), Ok("127.0.0.1:6502".parse().unwrap()));
        assert_eq!(peer_bind_from(0, Some("10.0.0.1:7000")), Ok("10.0.0.1:7000".parse().unwrap()));
        assert!(peer_bind_from(0, Some("x")).unwrap_err().contains(PEER_BIND_ENV));
        assert_eq!(peer_cert_hash_file_from(1, None), PathBuf::from("target/dev-r3/host-1.peer.cert-sha256"));
        assert_eq!(advertise_quic_from(None, 6466), "127.0.0.1:6466");
        assert_eq!(advertise_quic_from(Some("h:1"), 6466), "h:1");
        assert_eq!(advertise_wt_from(Some(" ")), None);
        assert_eq!(peer_link_delay_from(None), Ok(Duration::ZERO));
        assert_eq!(peer_link_delay_from(Some("40")), Ok(Duration::from_millis(40)));
        assert!(peer_link_delay_from(Some("-1")).is_err());
    }
}
