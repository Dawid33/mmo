//! Host ↔ host over the service link (R3): one [`QuicPeerLink`] per other host, the
//! [`game::PeerLink`] the manager's [`game::PeerSession`]s run over.
//!
//! **One link per pair**: the HIGHER id dials the LOWER (ids are issued at registration, so
//! a newer host dials every older one). Every host binds one `svc-link` listener
//! ([`PEER_BIND_ENV`], [`PeerListener`]) BEFORE it registers — its `hosts` row names the
//! listener's address and pin — and starts its links once the store issued its id
//! ([`PeerLinks::start`]): a dialing link to every lower live row it reads then, each one
//! re-reading that row every [`PEER_DISCOVERY_SECS`] while the peer is down. A higher host
//! is admitted when it dials in, by its live `hosts` row (id and boot), and its link is made
//! then ([`PeerLinks::take_arrived`]): a host that registers later reaches this one without
//! a link object made in advance. Elastic 1 Task 4 replaces the one read at start with a
//! periodic refresh (a lower host is never new: ids only grow).
//!
//! **One ordered stream** per connection carries every frame (hello, reliable, ack,
//! volatile) in send order. Every inbound frame is tagged with its connection (the
//! listener's `PeerId`, or a dialer's `Up` count); a link drops frames whose tag is not
//! its live connection's, so no frame of an older connection surfaces after
//! [`PeerLink::connection`] changed. Of two live connections from one ordinal the newer
//! replaces the older (as the coordinator's admission: the higher registration number
//! wins, ties to the newer).
//!
//! [`PEER_LINK_DELAY_MS_ENV`] holds every outgoing frame that long before it is written
//! (order kept; a latency measurement knob, never on in play) and writes it when it is due:
//! a timer thread per link wakes the host loop then, so a d ms delay costs d ms, not the
//! loop's next wake.

use game::HostId;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, Sender};
use game::{PeerLink, PeerMsg};
use svc_link::{LinkDialer, LinkEvent, LinkHello, LinkListener, PeerId, PeerRole, PeerSenders};
use world_store::StoreHandle;

/// Environment: where a host's peer listener binds (default [`DEFAULT_PEER_BIND`]; several
/// hosts on one machine each set their own).
pub const PEER_BIND_ENV: &str = "VOXEL_PEER_BIND";
/// The peer listener's address when [`PEER_BIND_ENV`] is unset.
pub const DEFAULT_PEER_BIND: &str = "127.0.0.1:6500";
/// Environment: the file a host writes its peer listener's certificate pin to (the
/// certificate and key are kept beside it, so a restart keeps the pin). Default
/// `target/dev-r3/<name>.peer.cert-sha256` (`<name>`: `VOXEL_HOST_NAME`, `host` when empty).
pub const PEER_CERT_HASH_FILE_ENV: &str = "VOXEL_PEER_CERT_HASH_FILE";
/// Environment: the QUIC address players reach this host at (`ServerPacket::Host`, the
/// `hosts` row). Default `127.0.0.1:{VOXEL_SERVER_PORT}`.
pub const ADVERTISE_QUIC_ENV: &str = "VOXEL_ADVERTISE_QUIC";
/// Environment: the WebTransport address browsers reach this host at (default: none).
pub const ADVERTISE_WT_ENV: &str = "VOXEL_ADVERTISE_WT";
/// Environment: the `ip:port` other hosts dial this host's peer listener at (the `hosts`
/// row's `peer_addr`). Default: the bound address; required when the bind is a wildcard.
pub const ADVERTISE_PEER_ENV: &str = "VOXEL_ADVERTISE_PEER";
/// Environment: hold every outgoing peer frame this many ms (default 0: no queue at all).
pub const PEER_LINK_DELAY_MS_ENV: &str = "VOXEL_PEER_LINK_DELAY_MS";
/// What a host logs (WARN) at start when [`PEER_LINK_DELAY_MS_ENV`] is not 0.
pub const PEER_LINK_DELAY_WARNING: &str = "peer link delay injected — measurement only";
/// While a lower ordinal is down, its `hosts` row is re-read this often.
pub const PEER_DISCOVERY_SECS: u64 = 2;

/// The peer listener's address from [`PEER_BIND_ENV`]'s value; unset or empty is
/// [`DEFAULT_PEER_BIND`] (nothing is derived from a host number any more).
pub fn peer_bind_from(value: Option<&str>) -> Result<SocketAddr, String> {
    let v = value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(DEFAULT_PEER_BIND);
    v.parse().map_err(|_| format!("{PEER_BIND_ENV}={v:?} is not an address (ip:port)"))
}

/// The peer pin file from [`PEER_CERT_HASH_FILE_ENV`]'s value; unset or empty is
/// `target/dev-r3/<name>.peer.cert-sha256` for host name `name` (`host` when it is empty).
pub fn peer_cert_hash_file_from(name: &str, value: Option<&str>) -> PathBuf {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => PathBuf::from(format!("target/dev-r3/{}.peer.cert-sha256", crate::file_word(name))),
    }
}

/// The advertised QUIC address from [`ADVERTISE_QUIC_ENV`]'s value; unset or empty is
/// `127.0.0.1:{port}` (the server's own port).
pub fn advertise_quic_from(value: Option<&str>, port: u16) -> String {
    value.map(str::trim).filter(|v| !v.is_empty()).map_or_else(|| format!("127.0.0.1:{port}"), str::to_string)
}

/// The address peers dial from [`ADVERTISE_PEER_ENV`]'s value; unset or empty is `bound`.
/// A set value must be a literal `ip:port` (peers parse it as one) and not a wildcard; a
/// wildcard `bound` with nothing advertised is an error, as nobody can dial it.
pub fn advertise_peer_from(value: Option<&str>, bound: SocketAddr) -> Result<SocketAddr, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => {
            let addr: SocketAddr =
                v.parse().map_err(|_| format!("{ADVERTISE_PEER_ENV}={v:?} is not an address (ip:port, no names)"))?;
            if addr.ip().is_unspecified() {
                return Err(format!("{ADVERTISE_PEER_ENV}={v:?} is a wildcard address nobody can dial"));
            }
            Ok(addr)
        }
        None if bound.ip().is_unspecified() => {
            Err(format!("the peer listener is bound to the wildcard {bound}: set {ADVERTISE_PEER_ENV} to the ip:port peers dial"))
        }
        None => Ok(bound),
    }
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
    pub fn from_env(host_name: &str) -> Result<PeerLinkConfig, String> {
        let var = |name: &str| std::env::var(name).ok();
        let config = PeerLinkConfig {
            bind: peer_bind_from(var(PEER_BIND_ENV).as_deref())?,
            cert_hash_file: peer_cert_hash_file_from(host_name, var(PEER_CERT_HASH_FILE_ENV).as_deref()),
            delay: peer_link_delay_from(var(PEER_LINK_DELAY_MS_ENV).as_deref())?,
        };
        if !config.delay.is_zero() {
            log::warn!("[peers] {PEER_LINK_DELAY_WARNING}: {} ms on every outgoing frame", config.delay.as_millis());
        }
        Ok(config)
    }
}

/// The other hosts that left the world cleanly in a committed leave cut, each with the boot
/// that left: only a committed leave decision marks a `hosts` row clean, and every boot makes
/// its own row dirty when it registers (a registration racing the leave commit loses or wins
/// on the world generation, so a clean row always names the boot that left). A crashed host's
/// row is dirty: it is not one of them. Read at boot for
/// [`game::WorldManager::note_left_peers`]; a later boot is expected again from its hello.
pub fn left_cleanly(store: &world_store::StoreHandle, me: HostId) -> Result<Vec<(HostId, u64)>, world_store::StoreError> {
    Ok(store.hosts()?.into_iter().filter(|r| r.clean && r.id != me).map(|r| (r.id, r.boot)).collect())
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
    peer: HostId,
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
    /// A non-zero delay: the due time of every held frame goes to a timer thread that
    /// signals the host loop's peer wake when it comes (the loop would otherwise only
    /// flush at its next wake for something else — up to its 200 ms default). Zero delay:
    /// none (no thread, no extra wakeup).
    due_timer: Option<Sender<Instant>>,
}

impl QuicPeerLink {
    /// `wake` is the host loop's peer wake ([`PeerLinks::wake`]).
    fn new(
        peer: HostId,
        wire: Box<dyn PeerWire + Send>,
        events: Receiver<Inbound>,
        delay: Duration,
        wake: Sender<()>,
    ) -> QuicPeerLink {
        let due_timer = (!delay.is_zero()).then(|| {
            let (due_tx, dues) = crossbeam::channel::unbounded();
            std::thread::Builder::new()
                .name(format!("peer-delay-{peer}"))
                .spawn(move || wake_when_due(dues, wake))
                .expect("spawn the peer delay timer thread");
            due_tx
        });
        QuicPeerLink {
            due_timer,
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

/// The delay's timer thread: for each due time (sent in order), sleeps until it comes and
/// signals `wake` (bounded(1): a wake already pending covers this one). Ends when its link
/// is dropped.
fn wake_when_due(dues: Receiver<Instant>, wake: Sender<()>) {
    while let Ok(due) = dues.recv() {
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        let _ = wake.try_send(());
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
            let due = (self.clock)() + self.delay;
            self.delayed.push_back((due, tag, msg));
            if let Some(timer) = &self.due_timer {
                let _ = timer.send(due);
            }
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

/// Whether host `id`'s boot `boot` has a live `hosts` row now (`Err`: the row could not be
/// read). The listener's admission asks it of every host that dials in.
pub type LiveRow = Box<dyn Fn(HostId, u64) -> Result<bool, String> + Send>;

/// Which host peers the listener of host `mine` serves: a HIGHER id (the higher dials the
/// lower) whose `hosts` row is live with the boot its hello names ([`LiveRow`]: no host count,
/// no id known in advance), one live connection per id — the newest, unless its registration
/// number is lower than one seen (a stale process).
struct Admission {
    mine: HostId,
    live_row: LiveRow,
    best_reg: BTreeMap<HostId, u64>,
    live: BTreeMap<HostId, PeerId>,
}

impl Admission {
    fn new(mine: HostId, live_row: LiveRow) -> Self {
        Admission { mine, live_row, best_reg: BTreeMap::new(), live: BTreeMap::new() }
    }

    /// `peer` said hello as `role`: its id and the connection it replaces, or why it is
    /// refused.
    fn admit(&mut self, peer: PeerId, role: &PeerRole) -> Result<(HostId, Option<PeerId>), String> {
        let PeerRole::Host { id, boot, reg } = *role else {
            return Err(format!("{role:?} is not a host"));
        };
        if id == self.mine {
            return Err(format!("host {id} is this host"));
        }
        if id < self.mine {
            return Err(format!("host {id} is lower: this host dials it"));
        }
        let best = self.best_reg.get(&id).copied().unwrap_or(0);
        if reg < best {
            return Err(format!("host {id} with registration {reg}: registration {best} has it (a stale process)"));
        }
        match (self.live_row)(id, boot) {
            Ok(true) => {}
            Ok(false) => return Err(format!("host {id} (boot {boot:#x}) has no live hosts row")),
            Err(e) => return Err(format!("host {id}'s hosts row could not be read: {e}")),
        }
        self.best_reg.insert(id, reg);
        Ok((id, self.live.insert(id, peer)))
    }

    /// `peer`'s connection ended.
    fn gone(&mut self, ordinal: HostId, peer: PeerId) {
        if self.live.get(&ordinal) == Some(&peer) {
            self.live.remove(&ordinal);
        }
    }
}

/// The listener's events, routed to each higher host's link; the first admitted connection of
/// a host makes its link ([`QuicPeerLink`] over the listener), handed to the main loop on
/// `arrivals` ([`PeerLinks::take_arrived`]).
fn route_listener(
    mut admission: Admission,
    events: Receiver<(PeerId, LinkEvent<PeerMsg>)>,
    senders: PeerSenders<PeerMsg>,
    arrivals: Sender<(HostId, Box<dyn PeerLink + Send>)>,
    delay: Duration,
    wake: Sender<()>,
) {
    let mut links: BTreeMap<HostId, Sender<Inbound>> = BTreeMap::new();
    let mut of_peer: BTreeMap<PeerId, HostId> = BTreeMap::new();
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
                    if !links.contains_key(&ordinal) {
                        let (inbound, link_events) = crossbeam::channel::unbounded();
                        let wire = Box::new(ListenerWire(senders.clone()));
                        let link = QuicPeerLink::new(ordinal, wire, link_events, delay, wake.clone());
                        log::info!("[peers] host {ordinal} dialed in: a link to it");
                        if arrivals.send((ordinal, Box::new(link))).is_err() {
                            return;
                        }
                        links.insert(ordinal, inbound);
                    }
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

/// What the `hosts` table says of a lower host to dial.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// Its live boot listens there, with that pin.
    Listening(SocketAddr, String),
    /// No row, or the row names no listener yet.
    Missing,
    /// Its row was declared dead (R4, a succession): that boot is never dialed again — a
    /// zombie of it may still listen — until another boot of the ordinal registers.
    Dead,
}

/// Host `ordinal`'s peer address and pin from the `hosts` table.
fn lookup(store: &StoreHandle, ordinal: HostId) -> Result<Found, String> {
    let rows = store.hosts().map_err(|e| e.to_string())?;
    let Some(row) = rows.into_iter().find(|r| r.id == ordinal) else { return Ok(Found::Missing) };
    if row.state != game::HOST_LIVE {
        return Ok(Found::Dead);
    }
    match (row.peer_addr, row.peer_cert) {
        (Some(addr), Some(pin)) => {
            let addr = addr.parse().map_err(|_| format!("host {ordinal}'s peer address {addr:?} is not ip:port"))?;
            Ok(Found::Listening(addr, pin))
        }
        _ => Ok(Found::Missing),
    }
}

/// The thread behind a link to the lower host `peer`: finds it in the `hosts` table,
/// dials it (a [`LinkDialer`] that redials one address for ever), re-reads the row every
/// [`PEER_DISCOVERY_SECS`] while the peer is down and dials anew when it changed; on a
/// close, drops the dialer (ending its connection) and reads the row again at once: the same
/// live boot is dialed again, a boot declared dead (R4: the manager closes the link of a
/// succeeded peer) is not — its dialer state is gone, and the row is re-read every
/// [`PEER_DISCOVERY_SECS`] until the ordinal's next boot registers (live, another address).
fn dial_peer(
    peer: HostId,
    hello: LinkHello,
    store: StoreHandle,
    inbound: Sender<Inbound>,
    cmds: Receiver<DialCmd>,
    wake: Sender<()>,
    dialing: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    let mut said_dead = false;
    loop {
        dialing.store(dialer.is_some(), std::sync::atomic::Ordering::SeqCst);
        if live.is_none() && Instant::now() >= next_lookup {
            next_lookup = Instant::now() + discovery;
            match lookup(&store, peer) {
                Ok(Found::Listening(addr, pin)) => {
                    let found = (addr, pin);
                    said_missing = false;
                    said_dead = false;
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
                Ok(Found::Missing) => {
                    if !said_missing {
                        log::info!("[peers] host {peer} has no peer listener in the hosts table yet; looking again");
                        said_missing = true;
                    }
                }
                Ok(Found::Dead) => {
                    // A succeeded boot: nothing of it is dialed (a zombie may still listen
                    // there); its next boot registers live, at an address read then.
                    drop(dialer.take());
                    target = None;
                    dialing.store(false, std::sync::atomic::Ordering::SeqCst);
                    if !said_dead {
                        log::info!("[peers] host {peer}'s hosts row is dead (succeeded): not dialed until its next boot registers");
                        said_dead = true;
                    }
                }
                Err(e) => {
                    log::warn!("[peers] reading host {peer}'s hosts row: {e}");
                    // The store is unreachable: the last live target is dialed again rather
                    // than leaving the link down through an outage. A boot found dead has no
                    // target any more (forgotten above), so a zombie is never redialed here.
                    if dialer.is_none() {
                        if let Some(t) = target.as_ref() {
                            dialer = Some(dial(t));
                        }
                    }
                }
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
                        log::warn!("[peers] closing the connection to host {peer}: its hosts row is read again before a redial");
                        // Dropping the dialer ends the connection; the row decides the next dial
                        // (the same live boot again, nobody if that boot was declared dead).
                        drop(dialer.take());
                        next_lookup = Instant::now();
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

/// A host's peer listener, bound before the host registers (its `hosts` row names the
/// listener's address and pin); its connections wait until [`PeerLinks::start`] routes them.
pub struct PeerListener {
    listener: LinkListener,
    events: Receiver<(PeerId, LinkEvent<PeerMsg>)>,
    senders: PeerSenders<PeerMsg>,
    advertised: SocketAddr,
    token: String,
    config: PeerLinkConfig,
}

impl PeerListener {
    /// Binds `config.bind` (its pin written to `config.cert_hash_file`) with the shared service
    /// `token`; what peers dial is [`ADVERTISE_PEER_ENV`], else the bound address.
    pub fn bind(config: PeerLinkConfig, token: String) -> io::Result<PeerListener> {
        if let Some(dir) = config.cert_hash_file.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let (listener, events, senders) =
            LinkListener::bind::<PeerMsg, PeerMsg>(config.bind, token.clone(), Some(config.cert_hash_file.clone()))?;
        let advertised = advertise_peer_from(std::env::var(ADVERTISE_PEER_ENV).ok().as_deref(), listener.local_addr())
            .map_err(io::Error::other)?;
        Ok(PeerListener { listener, events, senders, advertised, token, config })
    }

    /// What the `hosts` row names: the address peers dial and the listener's pin.
    pub fn row_fields(&self) -> (String, String) {
        (self.advertised.to_string(), self.listener.cert_sha256().to_string())
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr()
    }
}

/// A host's peer listener and its links to the other hosts of the world.
pub struct PeerLinks {
    listener: LinkListener,
    links: Vec<(HostId, Box<dyn PeerLink + Send>)>,
    arrivals: Receiver<(HostId, Box<dyn PeerLink + Send>)>,
    wake: Receiver<()>,
    /// Per lower host: whether its dialing thread holds a dialer now.
    dialing: BTreeMap<HostId, std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl PeerLinks {
    /// Starts the links of host `me` (the id the store issued it), boot `boot`, over its bound
    /// `listener`: a dialing link to every LOWER id whose `hosts` row `store` shows live now
    /// (read once; each link re-reads its row while its peer is down), and an admission of
    /// higher hosts by their live rows — each one's link is made when it first dials in
    /// ([`Self::take_arrived`]).
    pub fn start(listener: PeerListener, me: HostId, boot: u64, store: StoreHandle) -> io::Result<PeerLinks> {
        let PeerListener { listener, events, senders, advertised, token, config } = listener;
        let rows = store.hosts().map_err(io::Error::other)?;
        let lower: Vec<HostId> =
            rows.iter().filter(|r| r.id < me && r.state == game::HOST_LIVE && !r.clean).map(|r| r.id).collect();
        log::info!(
            "[peers] host {me}: peers dial {advertised} (bound {}, pin {}); dialing the older live hosts {lower:?}",
            listener.local_addr(),
            listener.cert_sha256()
        );
        let hello = LinkHello::new(token, PeerRole::Host { id: me, boot, reg: me.0 });
        let (wake_tx, wake) = crossbeam::channel::bounded(1);
        let mut links: Vec<(HostId, Box<dyn PeerLink + Send>)> = Vec::new();
        let mut dialing = BTreeMap::new();
        for other in lower {
            let (inbound, link_events) = crossbeam::channel::unbounded();
            let (cmds_tx, cmds) = crossbeam::channel::unbounded();
            let (hello, store, wake) = (hello.clone(), store.clone(), wake_tx.clone());
            let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            dialing.insert(other, flag.clone());
            std::thread::Builder::new()
                .name(format!("peer-dial-{other}"))
                .spawn(move || dial_peer(other, hello, store, inbound, cmds, wake, flag))?;
            let wire: Box<dyn PeerWire + Send> = Box::new(DialerWire(cmds_tx));
            links.push((other, Box::new(QuicPeerLink::new(other, wire, link_events, config.delay, wake_tx.clone()))));
        }
        let live_row: LiveRow = Box::new(move |id, boot| {
            let rows = store.hosts().map_err(|e| e.to_string())?;
            Ok(rows.iter().any(|r| r.id == id && r.boot == boot && r.state == game::HOST_LIVE))
        });
        let (arrivals_tx, arrivals) = crossbeam::channel::unbounded();
        let delay = config.delay;
        std::thread::Builder::new()
            .name("peer-listener".into())
            .spawn(move || route_listener(Admission::new(me, live_row), events, senders, arrivals_tx, delay, wake_tx))?;
        Ok(PeerLinks { listener, links, arrivals, wake, dialing })
    }

    /// The links made at start (one per lower live host), for [`game::WorldManager::set_peer`];
    /// empty after the first call.
    pub fn take(&mut self) -> Vec<(HostId, Box<dyn PeerLink + Send>)> {
        std::mem::take(&mut self.links)
    }

    /// The links of higher hosts that dialed in since the last call (each once), for
    /// [`game::WorldManager::set_peer`]. Never blocks.
    pub fn take_arrived(&self) -> Vec<(HostId, Box<dyn PeerLink + Send>)> {
        self.arrivals.try_iter().collect()
    }

    /// [`Self::take_arrived`]'s source, for a holder that does not keep the links.
    pub fn arrivals(&self) -> Receiver<(HostId, Box<dyn PeerLink + Send>)> {
        self.arrivals.clone()
    }

    /// Ready when a link has something (a frame, up or down) or a host dialed in: the host
    /// loop selects on it; the manager pumps the links.
    pub fn wake(&self) -> Receiver<()> {
        self.wake.clone()
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr()
    }

    /// Test support: whether this host's dialing thread for the lower host `peer` holds a
    /// dialer now (`None`: `peer` is not dialed by this host). A dead boot's is dropped.
    #[doc(hidden)]
    pub fn dialing(&self, peer: HostId) -> Option<bool> {
        self.dialing.get(&peer).map(|f| f.load(std::sync::atomic::Ordering::SeqCst))
    }

    pub fn cert_sha256(&self) -> &str {
        self.listener.cert_sha256()
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use game::HostId;

    /// A host id from its number.
    #[allow(dead_code)]
    fn h(n: u64) -> HostId {
        HostId(n)
    }

    use super::*;
    use std::sync::{Arc, Mutex};

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn unset_advertises_the_bound_address() {
        let bound = addr("127.0.0.1:6500");
        assert_eq!(advertise_peer_from(None, bound), Ok(bound));
        assert_eq!(advertise_peer_from(Some("  "), bound), Ok(bound));
    }

    #[test]
    fn a_set_value_wins() {
        assert_eq!(advertise_peer_from(Some("10.42.0.7:6500"), addr("0.0.0.0:6500")), Ok(addr("10.42.0.7:6500")));
    }

    #[test]
    fn a_wildcard_bind_needs_an_advertised_address() {
        for bound in ["0.0.0.0:6500", "[::]:6500"] {
            let err = advertise_peer_from(None, addr(bound)).unwrap_err();
            assert!(err.contains("VOXEL_ADVERTISE_PEER"), "{err}");
        }
    }

    #[test]
    fn a_wildcard_cannot_be_advertised() {
        assert!(advertise_peer_from(Some("0.0.0.0:6500"), addr("127.0.0.1:6500")).is_err());
        assert!(advertise_peer_from(Some("[::]:6500"), addr("127.0.0.1:6500")).is_err());
    }

    #[test]
    fn a_name_is_refused() {
        assert!(advertise_peer_from(Some("host-0.hosts:6500"), addr("127.0.0.1:6500")).is_err());
    }

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
        let (link, tap, tx, _wake) = link_with_wake(delay);
        (link, tap, tx)
    }

    /// [`link`] and the host loop's peer wake it signals.
    fn link_with_wake(delay: Duration) -> (QuicPeerLink, Tap, Sender<Inbound>, Receiver<()>) {
        let tap = Tap::default();
        let (tx, rx) = crossbeam::channel::unbounded();
        let (wake_tx, wake) = crossbeam::channel::bounded(1);
        (QuicPeerLink::new(h(1), Box::new(tap.clone()), rx, delay, wake_tx), tap, tx, wake)
    }

    /// The host loop as `server::run` drives it, idle (no client, no region, no coordinator):
    /// wait for the peer wake or the 200 ms default, then pump. With a 5 ms delay a frame is
    /// written when it is due (± a few ms), not at the next default wake — a real 1 ms link
    /// would not lose a tick per crossing.
    #[test]
    fn a_delayed_frame_is_written_when_due_on_an_idle_loop() {
        let (mut link, tap, tx, wake) = link_with_wake(Duration::from_millis(5));
        tx.send(Inbound::Up(9)).unwrap();
        link.pump(0);
        while wake.try_recv().is_ok() {}
        let t0 = Instant::now();
        link.send(ack(1));
        while tap.0.lock().unwrap().0.is_empty() {
            assert!(t0.elapsed() < Duration::from_secs(2), "the frame was never written");
            crossbeam::channel::select! {
                recv(wake) -> _ => {},
                default(Duration::from_millis(200)) => {}
            }
            link.pump(0);
        }
        let took = t0.elapsed();
        assert!(took >= Duration::from_millis(5), "written before it was due ({took:?})");
        // Robust on a loaded machine; the old behaviour waits for the 200 ms default.
        assert!(took < Duration::from_millis(100), "written {took:?} after the send, not when due (5 ms)");
    }

    /// Without a delay no timer runs: a send never signals the wake by itself.
    #[test]
    fn no_delay_no_timer() {
        let (mut link, tap, tx, wake) = link_with_wake(Duration::ZERO);
        tx.send(Inbound::Up(9)).unwrap();
        link.pump(0);
        while wake.try_recv().is_ok() {}
        link.send(ack(1));
        assert_eq!(tap.0.lock().unwrap().0, vec![(9, ack(1))], "written at once");
        assert!(wake.recv_timeout(Duration::from_millis(20)).is_err(), "no wake of its own");
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

    /// The listener admits higher ids with a live row of the hello's boot only (no host
    /// count: any id); of two connections of one id the newer replaces the older unless its
    /// registration number is lower.
    #[test]
    fn admission_takes_higher_live_ids_and_the_newest_connection() {
        let host = |id, reg| PeerRole::Host { id, boot: 1, reg };
        let live: LiveRow = Box::new(|id: HostId, boot| Ok(boot == 1 && (id == h(2) || id == h(41))));
        let mut a = Admission::new(h(1), live);
        assert!(a.admit(PeerId(1), &host(h(3), 1)).is_err(), "no live row");
        assert!(a.admit(PeerId(2), &host(h(1), 1)).is_err(), "itself");
        assert!(a.admit(PeerId(3), &host(h(0), 1)).is_err(), "a lower host is dialed, not admitted");
        assert!(a.admit(PeerId(4), &PeerRole::Login).is_err(), "hosts only");
        assert!(a.admit(PeerId(9), &PeerRole::Host { id: h(2), boot: 2, reg: 4 }).is_err(), "another boot of its id");
        assert_eq!(a.admit(PeerId(10), &host(h(41), 41)), Ok((h(41), None)), "any id, no count");
        assert_eq!(a.admit(PeerId(5), &host(h(2), 4)), Ok((h(2), None)));
        assert_eq!(a.admit(PeerId(6), &host(h(2), 4)), Ok((h(2), Some(PeerId(5)))), "the newer replaces the older");
        assert!(a.admit(PeerId(7), &host(h(2), 3)).is_err(), "a stale process");
        a.gone(h(2), PeerId(5));
        assert_eq!(a.live.get(&h(2)), Some(&PeerId(6)), "a replaced connection's end changes nothing");
        a.gone(h(2), PeerId(6));
        assert_eq!(a.admit(PeerId(8), &host(h(2), 4)), Ok((h(2), None)));
        let unreadable: LiveRow = Box::new(|_, _| Err("down".into()));
        assert!(Admission::new(h(1), unreadable).admit(PeerId(1), &host(h(2), 1)).is_err(), "an unreadable row admits nobody");
    }

    /// The peer link's frame limit the manager checks a succession report against (game) is
    /// the service link's own.
    #[test]
    fn the_peer_frame_limit_is_the_service_links() {
        assert_eq!(game::PEER_FRAME_MAX_BYTES, svc_link::LINK_MAX_FRAME_BYTES);
    }

    #[test]
    fn env_defaults() {
        assert_eq!(peer_bind_from(None), Ok("127.0.0.1:6500".parse().unwrap()), "no number in the default");
        assert_eq!(peer_bind_from(Some("10.0.0.1:7000")), Ok("10.0.0.1:7000".parse().unwrap()));
        assert!(peer_bind_from(Some("x")).unwrap_err().contains(PEER_BIND_ENV));
        assert_eq!(peer_cert_hash_file_from("", None), PathBuf::from("target/dev-r3/host.peer.cert-sha256"));
        assert_eq!(peer_cert_hash_file_from("host-b", None), PathBuf::from("target/dev-r3/host-b.peer.cert-sha256"));
        assert_eq!(peer_cert_hash_file_from("x", Some("/p/pin")), PathBuf::from("/p/pin"));
        assert_eq!(advertise_quic_from(None, 6466), "127.0.0.1:6466");
        assert_eq!(advertise_quic_from(Some("h:1"), 6466), "h:1");
        assert_eq!(advertise_wt_from(Some(" ")), None);
        assert_eq!(peer_link_delay_from(None), Ok(Duration::ZERO));
        assert_eq!(peer_link_delay_from(Some("40")), Ok(Duration::from_millis(40)));
        assert!(peer_link_delay_from(Some("-1")).is_err());
    }
}
