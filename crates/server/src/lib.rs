//! Server
// #![deny(missing_docs)]
use game::HostId;
use std::sync::{atomic::AtomicU64, Arc};

use crossbeam::channel::{Receiver, Sender};
use dashmap::DashMap;
use game::{ClientId, ClientPacket, ServerEvent, ServerPacket};
use log::{error, info};
use quinn::{
    crypto::rustls::QuicServerConfig,
    rustls,
    ConnectionError, Endpoint, TransportConfig,
};

/// Set to `1` by the client that launched this server for one world: once a
/// client has connected and the last one has left, the server exits. It also
/// exits when its stdin closes: the launching client holds the pipe's write
/// end, which the OS closes however the client dies (even before it ever
/// connected). So a single-player server needs its stdin to stay open —
/// don't start one with stdin at `/dev/null`.
pub const SINGLE_PLAYER_ENV: &str = "VOXEL_SINGLE_PLAYER";

/// The QUIC port to bind on 127.0.0.1 (unset or empty = [`DEFAULT_SERVER_PORT`]).
/// A client starting a menu world picks a free port and passes it here, so a
/// menu world never collides with a dev server on 6466.
pub const SERVER_PORT_ENV: &str = "VOXEL_SERVER_PORT";

/// The address both listeners (QUIC and WebTransport) bind (unset or empty =
/// `127.0.0.1`, local only). The deployed server (`just deploy`) sets `0.0.0.0`.
pub const BIND_ADDR_ENV: &str = "VOXEL_BIND_ADDR";

/// The listeners' address from the value of [`BIND_ADDR_ENV`].
fn bind_ip_from(value: Option<&str>) -> Result<std::net::IpAddr, String> {
    let value = value.map(str::trim).unwrap_or("");
    if value.is_empty() {
        return Ok(std::net::IpAddr::from([127, 0, 0, 1]));
    }
    value
        .parse()
        .map_err(|_| format!("invalid {BIND_ADDR_ENV} value `{value}` (expected an IP address, e.g. 0.0.0.0)"))
}

/// The dev server's QUIC port (`just run` / `just server`).
pub const DEFAULT_SERVER_PORT: u16 = 6466;

/// The dev server's WebTransport ingress port (browser clients).
pub const WEBTRANSPORT_PORT: u16 = 6467;

/// The QUIC port from the value of [`SERVER_PORT_ENV`].
fn server_port_from(value: Option<&str>) -> Result<u16, String> {
    let value = value.map(str::trim).unwrap_or("");
    if value.is_empty() {
        return Ok(DEFAULT_SERVER_PORT);
    }
    match value.parse::<u16>() {
        Ok(port) if port != 0 => Ok(port),
        _ => Err(format!("invalid {SERVER_PORT_ENV} value `{value}` (expected a port 1-65535)")),
    }
}

/// How long owned land nothing holds stays this host's before a world cut releases it, in ms
/// (Elastic 3; unset or empty = [`game::LAND_IDLE_MS`]): the twin's and the tests' dial.
pub const LAND_IDLE_MS_ENV: &str = "VOXEL_LAND_IDLE_MS";

/// The idle-land dial from the value of [`LAND_IDLE_MS_ENV`]; anything but a number refuses the
/// start.
fn land_idle_ms_from(value: Option<&str>) -> Result<u64, String> {
    let value = value.map(str::trim).unwrap_or("");
    if value.is_empty() {
        return Ok(game::LAND_IDLE_MS);
    }
    match value.parse::<u64>() {
        Ok(ms) if ms > 0 => Ok(ms),
        _ => Err(format!("invalid {LAND_IDLE_MS_ENV} value `{value}` (expected a number of ms above 0)")),
    }
}

/// The WebTransport ingress's port (unset or empty = [`WEBTRANSPORT_PORT`]): several hosts
/// on one machine (`just dev-r3`) each need their own.
pub const WT_PORT_ENV: &str = "VOXEL_WT_PORT";

/// Environment: this host's name (free text, default empty): log lines, default file names
/// (`target/dev-r3/<name>.peer.cert-sha256`, `webtransport-cert-hash-<name>.json`) and its
/// `hosts.name`. Never a key: the host's id is issued at registration. A registration lapses
/// the other dirty live rows of the same NON-EMPTY name (a restarted process's old id is then
/// declared at once instead of a lease later); two unnamed hosts never touch each other.
pub const HOST_NAME_ENV: &str = "VOXEL_HOST_NAME";

/// `name` as one word for a file name: `host` when empty; every character but ASCII letters,
/// digits, `-`, `_` and `.` becomes `_`.
pub fn file_word(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return "host".to_string();
    }
    name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect()
}

/// How a host is named in log lines: `h7`, and its name when it has one (`h7 "host-b"`).
pub fn host_label(id: HostId, name: &str) -> String {
    if name.trim().is_empty() { id.to_string() } else { format!("{id} {:?}", name.trim()) }
}

/// The WebTransport port from the value of [`WT_PORT_ENV`].
fn wt_port_from(value: Option<&str>) -> Result<u16, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(WEBTRANSPORT_PORT),
        Some(v) => match v.parse::<u16>() {
            Ok(port) if port != 0 => Ok(port),
            _ => Err(format!("invalid {WT_PORT_ENV} value `{v}` (expected a port 1-65535)")),
        },
    }
}

/// Whether to serve browsers over WebTransport (and write the cert-hash file
/// the page reads). A single-player world is never reached from a browser, and
/// its ingress would take the dev server's 6467 and overwrite its cert hash.
fn webtransport_enabled(single_player: bool) -> bool {
    !single_player
}

/// Why a single-player server should exit now, if it should.
fn single_player_exit(
    enabled: bool,
    ever_connected: bool,
    sessions: usize,
    parent_gone: bool,
) -> Option<&'static str> {
    if !enabled {
        None
    } else if parent_gone {
        Some("parent gone")
    } else if ever_connected && sessions == 0 {
        Some("last client left")
    } else {
        None
    }
}

/// Read `input` to its end (EOF or an error), then raise `gone`.
fn watch_parent(mut input: impl std::io::Read, gone: &std::sync::atomic::AtomicBool) {
    let _ = std::io::copy(&mut input, &mut std::io::sink());
    gone.store(true, std::sync::atomic::Ordering::SeqCst);
}

const DEFAULT_SERVER_WORKER_THREADS: usize = 4;

/// How long the shared writer task waits on one client send before dropping
/// the packet. Counted per window as `timeouts=` in the `[server]` line.
const SEND_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

fn server_worker_threads(value: Option<&str>) -> Result<usize, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_SERVER_WORKER_THREADS);
    };
    let worker_threads = value.parse::<usize>().map_err(|_| {
        format!("invalid TOKIO_WORKER_THREADS value `{value}` (expected positive integer)")
    })?;
    if worker_threads == 0 {
        return Err(format!(
            "invalid TOKIO_WORKER_THREADS value `{value}` (expected positive integer)"
        ));
    }
    Ok(worker_threads)
}

fn build_server_runtime(worker_threads: usize) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .unwrap()
}

pub mod auth_edge;
pub mod client_cert;
pub mod claims;
pub mod coordinator_api;
pub mod coordinator_rt;
pub mod cuts;
pub mod dev_structures;
pub mod drain;
pub mod fleet;
pub mod health;
pub mod lease;
pub mod load;
pub mod lod_workers;
pub mod login_link;
pub mod peer_link;
pub mod perf;
pub mod player_store;
pub mod region_threads;
pub mod roles;
pub mod webtransport;
pub mod world_file;
pub mod world_http;

/// One connected client's outgoing half, transport-agnostic. The router
/// writes bincode ServerPackets; each transport frames them as one
/// uni-stream per packet. Clones share the connection and its seat state.
#[derive(Clone)]
pub struct ClientSink {
    conn: SinkConnection,
    seat: Arc<SeatState>,
}

#[derive(Clone)]
enum SinkConnection {
    Quinn(quinn::Connection),
    Web(wtransport::Connection),
}

/// How a connection holds its id ([`seat_session`]).
#[derive(Default)]
struct SeatState {
    /// The id's session is its home here: admitted by a login or a player name, or an
    /// observer session the manager promoted ([`game::WorldManager::take_observer_revocations`]).
    /// An observer connection never replaces it.
    home: std::sync::atomic::AtomicBool,
    /// An observer connection that has not had its `Host` yet: packets still addressed to
    /// its id before that were the replaced session's, and are not sent to it.
    awaiting_host: std::sync::atomic::AtomicBool,
}

impl ClientSink {
    pub fn quinn(conn: quinn::Connection) -> ClientSink {
        ClientSink { conn: SinkConnection::Quinn(conn), seat: Arc::default() }
    }

    pub fn web(session: wtransport::Connection) -> ClientSink {
        ClientSink { conn: SinkConnection::Web(session), seat: Arc::default() }
    }

    /// Whether the id's session on this connection is its home ([`SeatState::home`]).
    pub fn is_home(&self) -> bool {
        self.seat.home.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The manager made this connection's session the home (or it was admitted as one).
    pub fn set_home(&self) {
        self.seat.home.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether `packet` goes to this connection: an observer connection gets nothing
    /// before its `Host` but a refusal ([`SeatState::awaiting_host`]).
    pub fn admits(&self, packet: &ServerPacket) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        if !self.seat.awaiting_host.load(SeqCst) {
            return true;
        }
        match packet {
            ServerPacket::Host(_) => {
                self.seat.awaiting_host.store(false, SeqCst);
                true
            }
            ServerPacket::Refused(_) => true,
            _ => false,
        }
    }

    /// Send one packet; a failed/vanished client is logged and skipped so a
    /// single dead connection can't kill the shared writer task.
    pub async fn send_packet(&self, packet: Vec<u8>) {
        match &self.conn {
            SinkConnection::Quinn(conn) => {
                let mut stream = match conn.open_uni().await {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!("dropping packet to gone client: {e:?}");
                        return;
                    }
                };
                if let Err(e) = stream.write_all(&packet).await {
                    log::warn!("write to client failed: {e:?}");
                    return;
                }
                let _ = stream.finish();
                tokio::spawn(async move {
                    let _ = stream.stopped().await;
                });
            }
            SinkConnection::Web(session) => {
                let mut stream = match session.open_uni().await {
                    Ok(opening) => match opening.await {
                        Ok(s) => s,
                        Err(e) => return log::warn!("webtransport open failed: {e:?}"),
                    },
                    Err(e) => return log::warn!("dropping packet to gone client: {e:?}"),
                };
                if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut stream, &packet).await {
                    return log::warn!("webtransport write failed: {e:?}");
                }
                let _ = stream.finish().await;
            }
        }
    }

    /// Close the connection with application code 1 (`refused`): the session
    /// was ended by the server (`ServerPacket::Refused` already sent).
    pub fn close_refused(&self) {
        match &self.conn {
            SinkConnection::Quinn(conn) => conn.close(1u32.into(), b"refused"),
            SinkConnection::Web(session) => session.close(wtransport::VarInt::from_u32(1), b"refused"),
        }
    }

    /// Whether `other` is this very connection.
    pub fn same_connection(&self, other: &ClientSink) -> bool {
        match (&self.conn, &other.conn) {
            (SinkConnection::Quinn(a), SinkConnection::Quinn(b)) => a.stable_id() == b.stable_id(),
            (SinkConnection::Web(a), SinkConnection::Web(b)) => a.stable_id() == b.stable_id(),
            _ => false,
        }
    }

    /// Tell the connection `why` (best effort, within [`REFUSE_FLUSH`]), then close it: an
    /// observer connection a newer one of its id replaced.
    pub fn refuse_later(self, why: &str) {
        let packet = game::encode_packet(&ServerPacket::Refused(why.to_string()));
        tokio::spawn(async move {
            if let Ok(packet) = packet {
                let _ = tokio::time::timeout(REFUSE_FLUSH, self.send_packet(packet)).await;
            }
            tokio::time::sleep(REFUSE_FLUSH).await;
            self.close_refused();
        });
    }
}

/// Seat an admitted session's connection `sink` as `id` and announce it to the manager.
/// An observer replaces an older connection of its id ([`player_store::seat_observer`]);
/// any other session claims the id ([`player_store::claim_sink`]) and is announced
/// ([`player_store::announce_session`]). `Err` is the refusal: nothing is announced or kept.
pub(crate) fn seat_session(
    sinks: &DashMap<ClientId, ClientSink>,
    send: &Sender<ServerEvent>,
    id: ClientId,
    hello: &player_store::HelloSession,
    auth: &auth_edge::Authenticator,
    sink: ClientSink,
) -> Result<(), String> {
    if hello.observer.is_some() {
        // Nothing reaches it before its `Host` (what is still addressed to the id is an
        // older session's).
        sink.seat.awaiting_host.store(true, std::sync::atomic::Ordering::SeqCst);
        return match player_store::seat_observer(sinks, send, id, hello.player, sink, ClientSink::is_home) {
            Ok(None) => Ok(()),
            Ok(Some(older)) => {
                log::info!("[server] client {id}: a newer observer connection replaces the older");
                older.refuse_later(player_store::REFUSE_REPLACED);
                Ok(())
            }
            // Its session here is the home: an observer ticket never takes its place.
            Err(_) => Err(auth_edge::REFUSE_OBSERVER.into()),
        };
    }
    sink.set_home();
    if player_store::claim_sink(sinks, id, sink).is_err() {
        return Err(player_store::REFUSE_ALREADY_CONNECTED.into());
    }
    // Announce before reading any other packets: guarantees the player exists before its
    // first request is served. A login revoked meanwhile (kicked, or a newer one) is
    // refused instead.
    if let Err(why) = player_store::announce_session(send, id, hello, auth) {
        sinks.remove(&id);
        if let Some(ticket) = &hello.ticket {
            auth.session_ended(ticket);
        }
        return Err(why);
    }
    Ok(())
}

/// Wrapper around RegionGroup with additional bookeeping / networking
/// to make it work as a server. Acts only as a dumb router of game event packets.
///
/// ## Initial Setup
///
/// - Load world from file or generate from scene.
/// - Enter main loop
///
/// ## Game Loop
/// - Order incoming client packets by game tick.
/// - Check if client packets are for the current game tick and execute them
///   if so.
/// - Execute game tick.
/// - If the loop didn't take a full TICK_TIME, wait until full TICK_TIME has passed.
pub struct WorldIngress {
    /// The address both listeners bind ([`BIND_ADDR_ENV`]).
    ip: std::net::IpAddr,
    /// The QUIC port.
    port: u16,
    /// Whether the WebTransport ingress (`ip`:[`Self::wt_port`]) runs.
    webtransport: bool,
    /// The WebTransport ingress's port ([`WEBTRANSPORT_PORT`] unless
    /// [`Self::with_webtransport_port`]).
    wt_port: u16,
    /// Resolves each connection's `Hello` token before its session starts.
    players: player_store::SharedPlayerStore,
    /// Checks each `Hello`'s credential first (`VOXEL_AUTH`).
    auth: Arc<auth_edge::Authenticator>,
    /// The certificate both listeners serve, made before they start (a self-signed one's
    /// hash is in this host's endpoint); `None`: [`Self::listen`] reads its source itself
    /// ([`client_cert::ClientCert::from_env`]).
    client_cert: Option<client_cert::ClientCert>,
    /// This host's name (`VOXEL_HOST_NAME`): which dev cert-hash files the ingress writes
    /// (`webtransport::cert_hash_file_names`).
    host_name: String,
    /// Each live session's connection, by id (shared with the main loop: [`Self::sinks`]).
    sinks: Arc<DashMap<ClientId, ClientSink>>,
}

impl WorldIngress {
    pub fn new(
        ip: std::net::IpAddr,
        port: u16,
        webtransport: bool,
        players: player_store::SharedPlayerStore,
        auth: Arc<auth_edge::Authenticator>,
    ) -> Self {
        Self {
            ip,
            port,
            webtransport,
            wt_port: WEBTRANSPORT_PORT,
            players,
            auth,
            client_cert: None,
            host_name: String::new(),
            sinks: Arc::new(DashMap::new()),
        }
    }

    /// The live connections by session id: the main loop marks the ones whose observer
    /// session the manager promoted to the home ([`ClientSink::set_home`]).
    pub fn sinks(&self) -> Arc<DashMap<ClientId, ClientSink>> {
        self.sinks.clone()
    }

    /// Serve WebTransport on `port` ([`WT_PORT_ENV`]).
    pub fn with_webtransport_port(mut self, port: u16) -> Self {
        self.wt_port = port;
        self
    }

    /// Serve QUIC and WebTransport with `cert` (made ahead, so its hash could be advertised).
    pub fn with_client_cert(mut self, cert: client_cert::ClientCert) -> Self {
        self.client_cert = Some(cert);
        self
    }

    /// Serve as the host named `name` (`VOXEL_HOST_NAME`; empty: unnamed).
    pub fn with_host_name(mut self, name: &str) -> Self {
        self.host_name = name.to_string();
        self
    }

    /// ## Network Loop
    /// - Process incoming client packets and send them to the game loop
    /// - Receive server packets from game loop and send them out to connected
    ///   clients. Maintain a buffer of events for each client and drop the client
    ///   if the buffer grows too large.
    /// - Recieve region packets from game loop and send them to their assorted
    ///   regions.
    pub async fn listen(
        &mut self,
        send: Sender<ServerEvent>,
        server_recv: Receiver<(Option<ClientId>, ServerPacket)>,
    ) {
        let client_cert = match self.client_cert.take() {
            Some(cert) => cert,
            None => client_cert::ClientCert::from_env(|name| std::env::var(name).ok()).unwrap_or_else(|e| panic!("{e}")),
        };
        if client_cert.is_from_files() {
            tokio::spawn(reload_client_cert(client_cert.clone()));
        }
        // The provider is named, not left to rustls's process default: the
        // workspace's unified features enable both `ring` and `aws-lc-rs`
        // (the login service's AWS SDK), and then `builder()` panics.
        let crypto = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .with_no_client_auth()
            .with_cert_resolver(client_cert.rustls_resolver());

        let mut config =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(crypto).unwrap()));
        let t_config = TransportConfig::default();
        config.transport_config(Arc::new(t_config));
        let quic_addr = std::net::SocketAddr::new(self.ip, self.port);
        let endpoint = Endpoint::server(config, quic_addr)
            .unwrap_or_else(|e| panic!("could not bind QUIC on {quic_addr}: {e}"));
        info!("listening on {quic_addr} (QUIC)");

        let sinks = self.sinks.clone();
        let writer_sinks = sinks.clone();
        tokio::spawn(async move {
            // A stalled or vanished peer must never wedge this shared task:
            // undetected dead connections stop yielding stream credit, so an
            // unbounded send here would freeze outgoing traffic for EVERY
            // client. Cleanup itself is the read task's job (idle timeout).
            // Returns whether the send timed out (the packet is then lost).
            async fn send_bounded(sink: ClientSink, packet: Vec<u8>) -> bool {
                let bytes = packet.len();
                let started = std::time::Instant::now();
                let timed_out = tokio::time::timeout(SEND_DEADLINE, sink.send_packet(packet))
                    .await
                    .is_err();
                if timed_out {
                    log::warn!("send to client timed out; skipping (peer stalled or gone)");
                }
                crate::perf::EDGE.record_send(bytes, started.elapsed(), timed_out);
                timed_out
            }

            // The channel is a blocking crossbeam one: `block_in_place` hands
            // this worker thread's queued tasks to another worker before it
            // blocks. A bare `recv()` here parked the thread with whatever sat
            // in its LIFO slot — which tokio never lets another worker steal —
            // and that was the connection task this writer had just woken, so
            // a WebTransport connection froze (no ACKs, no keep-alives) until
            // the server next had something to send: the browser saw its
            // streams never complete and closed the session as a "Network
            // blackhole" (2026-11-02, a join whose first request arrived while
            // the server was quiet).
            while let Ok((target, event)) = tokio::task::block_in_place(|| server_recv.recv()) {
                // Serialize + frame (large packets are lz4-compressed; see
                // `game::encode_packet`). `serialize_ms` covers both.
                let serialize_started = std::time::Instant::now();
                let packet = game::encode_packet(&event).unwrap();
                let serialize = serialize_started.elapsed();
                match target {
                    // Directed packet: if the client is gone, drop it.
                    Some(id) => {
                        let sink = writer_sinks.get(&id).map(|e| e.value().clone()).filter(|s| s.admits(&event));
                        if let Some(sink) = sink {
                            if matches!(event, ServerPacket::Refused(_)) {
                                // The manager ended this session (its player
                                // logged in elsewhere): nothing more goes to it,
                                // and the connection closes once the reason has
                                // had a moment to arrive. Its connection task
                                // then reports the disconnect (a no-op by now).
                                writer_sinks.remove(&id);
                                send_bounded(sink.clone(), packet.clone()).await;
                                tokio::spawn(async move {
                                    tokio::time::sleep(REFUSE_FLUSH).await;
                                    sink.close_refused();
                                });
                                continue;
                            }
                            let send_started = std::time::Instant::now();
                            let timed_out = send_bounded(sink, packet.clone()).await;
                            // Region snapshots are the join's big packets (MBs
                            // each); everything else is summed into `[server]`.
                            if let ServerPacket::Region(rc, _) = &event {
                                game::perf_event!(
                                    "snapshot_sent",
                                    "region={},{} client={id} bytes={} serialize_ms={} send_ms={} timed_out={timed_out}",
                                    rc.x,
                                    rc.z,
                                    packet.len(),
                                    game::perf::ms(serialize),
                                    game::perf::ms(send_started.elapsed())
                                );
                            }
                        }
                    }
                    None => {
                        let targets: Vec<ClientSink> =
                            writer_sinks.iter().map(|e| e.value().clone()).filter(|s| s.admits(&event)).collect();
                        for sink in targets {
                            send_bounded(sink, packet.clone()).await;
                        }
                    }
                }
            }
        });

        let next_client_id = Arc::new(AtomicU64::new(0));

        if self.webtransport {
            let wt_send = send.clone();
            let wt_sinks = sinks.clone();
            let wt_next = next_client_id.clone();
            let wt_players = self.players.clone();
            let wt_auth = self.auth.clone();
            let wt_addr = std::net::SocketAddr::new(self.ip, self.wt_port);
            let host_name = self.host_name.clone();
            let wt_cert = client_cert.clone();
            tokio::spawn(async move {
                let served =
                    crate::webtransport::serve(wt_addr, wt_cert, host_name, wt_send, wt_sinks, wt_next, wt_players, wt_auth).await;
                if let Err(e) = served {
                    log::error!("webtransport ingress died: {e:?}");
                }
            });
        } else {
            info!("single-player: no WebTransport ingress");
        }

        while let Some(conn) = endpoint.accept().await {
            let send = send.clone();
            let sinks = sinks.clone();
            let players = self.players.clone();
            let auth = self.auth.clone();
            let next_client_id = next_client_id.clone();
            tokio::spawn(async move {
                info!("accepting connection");
                let connection = match conn.await {
                    Ok(connection) => connection,
                    Err(e) => return log::warn!("quic connection failed: {e}"),
                };
                // The handshake comes first: no session (and no sink) exists
                // until the connection's Hello has resolved to a player.
                let hello =
                    match quic_hello(&connection, &players, &auth).await {
                        Ok(resolved) => resolved,
                        Err(why) => return refuse_quic(&connection, &why).await,
                    };
                let (player, ticket) = (hello.player, hello.ticket.clone());
                // The id exists only now that the session is authenticated; the sink is
                // claimed in the same step, so a replayed ticket never replaces a live one
                // (an observer's newer connection does replace its older one).
                let id = hello.client_id(&next_client_id);
                let mine = ClientSink::quinn(connection.clone());
                if let Err(why) = seat_session(&sinks, &send, id, &hello, &auth, mine.clone()) {
                    return refuse_quic(&connection, &why).await;
                }
                game::perf_event!("client_connected", "client={id} player={} transport=quic", player.0);

                let fut = handle_connection(connection, send.clone(), id, sinks.clone(), mine.clone());
                tokio::spawn(async move {
                    if let Err(e) = fut.await {
                        error!("connection failed: {reason}", reason = e.to_string())
                    }
                    // The world must know: unsubscribe everywhere, let the home region's
                    // grace timer start — unless a newer connection holds the id now.
                    player_store::release_sink(&sinks, &send, id, |live| live.same_connection(&mine));
                    // And the login service, for a session its spawn order admitted (role
                    // `host`: the manager tells it, once the session left the world).
                    if let Some(ticket) = &ticket {
                        auth.session_closed(ticket);
                    }
                });
            });
        }
    }
}

/// How long a refused connection waits for its `Refused` packet to be
/// acknowledged before it is closed.
const REFUSE_FLUSH: std::time::Duration = std::time::Duration::from_secs(1);

/// Read a QUIC connection's first packet (within
/// [`player_store::HELLO_TIMEOUT`]) and resolve it as the handshake.
async fn quic_hello(
    connection: &quinn::Connection,
    players: &player_store::SharedPlayerStore,
    auth: &auth_edge::Authenticator,
) -> Result<player_store::HelloSession, String> {
    let first = tokio::time::timeout(player_store::HELLO_TIMEOUT, async {
        let mut stream = connection.accept_uni().await.map_err(|e| format!("no first packet: {e}"))?;
        let bytes = stream
            .read_to_end(player_store::HELLO_MAX_BYTES)
            .await
            .map_err(|e| format!("first packet unreadable: {e}"))?;
        game::decode_packet::<ClientPacket>(&bytes).map_err(|e| format!("first packet does not decode: {e}"))
    })
    .await
    .map_err(|_| format!("no Hello within {:?}", player_store::HELLO_TIMEOUT))??;
    player_store::resolve_session(players, auth, first).await
}

/// Tell the client why (best effort), then close the connection.
async fn refuse_quic(connection: &quinn::Connection, why: &str) {
    log::warn!("[server] refusing a connection from {}: {why}", connection.remote_address());
    if let Ok(packet) = game::encode_packet(&ServerPacket::Refused(why.to_string())) {
        if let Ok(mut stream) = connection.open_uni().await {
            if stream.write_all(&packet).await.is_ok() && stream.finish().is_ok() {
                let _ = tokio::time::timeout(REFUSE_FLUSH, stream.stopped()).await;
            }
        }
    }
    connection.close(1u32.into(), b"refused");
}

pub fn run() -> claims::ServerExit {
    game::perf::pin_origin();
    // Per-entry undo verification is Full by default (HASH_MODE=delta|off
    // trades it for speed); client/server desync is checked by the
    // every-N-ticks state hash the server ships to clients — every 100 ticks
    // unless VOXEL_STATE_HASH_INTERVAL=N overrides it, 0 = off.
    game::set_hash_mode_from_env();
    game::set_state_hash_interval_from_env();

    // Continuous profiling: build with `--features pyroscope` and set PYROSCOPE
    // to push flamegraphs to a local Pyroscope (`just profile-server`). Off
    // unless both the feature and the env var are present, so release runs are
    // never instrumented.
    #[cfg(feature = "pyroscope")]
    let agent_running = if std::env::var("PYROSCOPE").is_ok() {
        use pyroscope::PyroscopeAgent;
        use pyroscope_pprofrs::{pprof_backend, PprofConfig};
        let agent = PyroscopeAgent::builder("http://localhost:4040", "server")
            .backend(pprof_backend(PprofConfig::new().sample_rate(100)))
            .build()
            .unwrap();
        Some(agent.start().unwrap())
    } else {
        None
    };

    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default()).unwrap();
    // `VOXEL_ROLE`: the whole world here (`all`), its regions only (`host`), its
    // coordinator only (`coordinator`, no client edge, no regions), or the fleet reconciler (`fleet`).
    let role = roles::Role::from_env().unwrap_or_else(|e| fatal(&e));
    if role == roles::Role::Coordinator {
        game::perf::init("coordinator");
        info!("[server] role: coordinator");
        return roles::run_coordinator();
    }
    if role == roles::Role::Fleet {
        game::perf::init("fleet");
        info!("[server] role: fleet");
        return roles::run_fleet();
    }
    game::perf::init("server");
    info!("[server] role: {role:?}");
    health::start_from_env("host").unwrap_or_else(|e| fatal(&e));

    let (client_packet_send, client_packet_recv) = crossbeam::channel::unbounded();
    let (server_send, server_recv) = crossbeam::channel::unbounded();

    let single_player = std::env::var(SINGLE_PLAYER_ENV).is_ok_and(|v| v == "1");
    let configured_port = std::env::var(SERVER_PORT_ENV).ok();
    let port = server_port_from(configured_port.as_deref()).unwrap_or_else(|message| panic!("{message}"));
    let wt_port = wt_port_from(std::env::var(WT_PORT_ENV).ok().as_deref()).unwrap_or_else(|message| panic!("{message}"));
    let configured_bind = std::env::var(BIND_ADDR_ENV).ok();
    let ip = bind_ip_from(configured_bind.as_deref()).unwrap_or_else(|message| panic!("{message}"));
    // The world is chosen before anything calls `forged::active()` (below), and
    // before the ingress starts: its player store is the world store.
    let world_source = world_file::resolve(
        std::env::var(worldgen::forged::WORLD_FILE_ENV).ok().as_deref(),
        std::env::var(world_file::WORLD_DB_ENV).ok().as_deref(),
    )
    .unwrap_or_else(|e| fatal(&e));
    role.check_store(&world_source).unwrap_or_else(|e| fatal(&e));
    let empty = world_file::empty_store_from_env().unwrap_or_else(|e| fatal(&e));
    // Only a process waiting for its world (`VOXEL_EMPTY_STORE=wait`, PID 1 in a pod, where an
    // unhandled SIGTERM is ignored) watches signals before the store opens; otherwise a signal
    // during a long forge or open acts as it always did (default action), and the watcher starts
    // below, where it always did.
    let early_stop = (empty == world_file::EmptyStore::Wait).then(watch_stop_signals_on_thread);
    // The players' certificate (QUIC and WebTransport), before any store work: a client pins
    // a self-signed one's hash to open a connection here (a certificate from files is a CA's:
    // no hash). Files another process has not written yet (a proxy obtaining them after this
    // start) are waited for; only the early watcher's stop (`EmptyStore::Wait`) ends the wait.
    let Some(client_cert) = client_cert::ClientCert::wait_from_env(|name| std::env::var(name).ok(), early_stop.as_deref())
        .unwrap_or_else(|e| fatal(&e))
    else {
        info!("[server] stop signal while waiting for the client certificate files: exiting");
        return claims::ServerExit::Clean;
    };
    let Some(mut opened) =
        world_file::open_world_with(&world_source, &worldgen::forge::ForgeParams::default(), empty, early_stop.as_deref())
            .unwrap_or_else(|e| fatal(&e))
    else {
        info!("[server] stop signal while waiting for a world: exiting");
        return claims::ServerExit::Clean;
    };
    let store = opened.store.clone();
    // Which regions this process simulates (`VOXEL_OWNERSHIP`), and the super-region side that
    // decides it: a bad setting stops the start, naming the variable. A host's ownership is
    // built from the id the store issues it at registration (below): no host number and no
    // host count is read (`VOXEL_HOST_ORDINAL` / `VOXEL_HOSTS` are ignored if set).
    let ownership_word = std::env::var(game::ownership::OWNERSHIP_ENV).unwrap_or_default();
    let crash_policy = game::crash_policy_from_value(Some(&ownership_word)).unwrap_or_else(|e| fatal(&e));
    roles::check_host_ownership(role, &ownership_word).unwrap_or_else(|e| fatal(&e));
    let super_region_regions = claims::super_region_regions_from(std::env::var(claims::SUPER_REGION_REGIONS_ENV).ok().as_deref())
        .unwrap_or_else(|e| fatal(&e));
    info!(
        "[server] ownership: {}, {super_region_regions} regions per super-region side",
        if ownership_word.trim().is_empty() { "allinone" } else { ownership_word.trim() }
    );
    // `claim` (R4): super-regions are assigned on demand by the coordinator (Elastic 3), the
    // directory read from the store — a host role only, and any number of hosts: peer links,
    // world cuts, the login relay and observer sessions. A `static` host is alone (the store
    // refuses a second one beside a live row).
    let claim_on_demand = crash_policy == game::CrashPolicy::Succession;
    // This host's name (`VOXEL_HOST_NAME`, default empty): labels, file names, `hosts.name`.
    let host_name = match role {
        roles::Role::Host => std::env::var(HOST_NAME_ENV).unwrap_or_default().trim().to_string(),
        _ => String::new(),
    };
    let players: player_store::SharedPlayerStore = match &store {
        None => player_store::shared(player_store::MemoryPlayerStore::default()),
        Some(store) => player_store::shared(player_store::StorePlayers(store.clone())),
    };
    // Who may connect (`VOXEL_AUTH`): a bad setting stops the start, naming
    // the variable. A single-player server is always local (player names).
    let (auth_mode, auth_warning) =
        auth_edge::AuthMode::resolve(single_player, |name| std::env::var(name).ok()).unwrap_or_else(|e| fatal(&e));
    if let Some(warning) = auth_warning {
        log::warn!("[server] {warning}");
    }
    match &auth_mode {
        auth_edge::AuthMode::Local => info!("auth: local player names (no login; anyone may claim any name)"),
        auth_edge::AuthMode::Jwt { world, login_url } => {
            info!("auth: login tickets for world {world:?} from {login_url}")
        }
    }
    // A `jwt` host with a service token takes the login service's push (`SpawnPlayer` /
    // `Kick`) and admits tickets only against its spawn orders; without a token it keeps
    // R2a's newest-epoch check only when `VOXEL_JWT_ADMISSION=newest` says so (registry
    // worlds without a `host_link`: `just dev-login`); otherwise it refuses to start.
    let spawn_orders = match &auth_mode {
        auth_edge::AuthMode::Jwt { .. } => {
            let token =
                login_link::service_token_if_configured(|name| std::env::var(name).ok()).unwrap_or_else(|e| fatal(&e));
            let admission_env = std::env::var(login_link::JWT_ADMISSION_ENV).ok();
            match login_link::jwt_admission(token.is_some(), admission_env.as_deref()).unwrap_or_else(|e| fatal(&e)) {
                login_link::JwtAdmission::SpawnOrders => {
                    let token = token.expect("spawn orders imply a configured token");
                    Some((Arc::new(login_link::SpawnOrders::new(auth_edge::system_clock())), token))
                }
                login_link::JwtAdmission::Newest => {
                    log::warn!(
                        "auth: jwt without a service token ({} / {} unset): NO login link — tickets are admitted by \
                         newest epoch (R2a), the login service cannot order spawns, and its Kick (logout everywhere, \
                         password reset) never reaches this host; set the token for a world registered with a host link",
                        svc_link::SERVICE_TOKEN_ENV,
                        svc_link::SERVICE_TOKEN_FILE_ENV
                    );
                    None
                }
            }
        }
        auth_edge::AuthMode::Local => None,
    };
    // `claim` (any number of hosts): only as `host`, on Postgres, with the login link (ids and
    // placement come from it).
    let postgres = matches!(&world_source, world_file::WorldSource::Store { url: world_store::StoreUrl::Postgres(_), .. });
    roles::check_claim_needs(role, crash_policy, postgres, spawn_orders.is_some()).unwrap_or_else(|e| fatal(&e));
    // Role `all` claims every super-region: refused (before it resolves, aborts or fences
    // anything) on a world whose other hosts ran in a split role and did not leave cleanly.
    if let Some(store) = &store {
        roles::check_all_role_beside_hosts(role, store, HostId::SOLO).unwrap_or_else(|e| fatal(&e));
    }
    // On Postgres, before it claims, registers or loads anything: the world cut parts a
    // crash left staged are finished first, whatever hosts staged them (a `claim` host: and
    // two-phase commit must be available).
    if let Some(store) = &store {
        roles::resolve_before_claim(postgres, store, &host_name).unwrap_or_else(|e| fatal(&e));
        roles::check_two_phase(crash_policy, store).unwrap_or_else(|e| fatal(&e));
    }
    // Every check and setting that needs no host id, BEFORE the registration (Elastic 1 final
    // review A-I2): a start-up failure after it leaves a dirty `hosts` row — a succession for a
    // configuration error, and a crash-looping pod's restarts can roll the world back.
    roles::check_before_registration(
        role,
        store.as_ref(),
        postgres,
        &ownership_word,
        claim_on_demand,
        super_region_regions,
        opened.world_tick,
    )
    .unwrap_or_else(|e| fatal(&e));
    let configured_worker_threads = std::env::var("TOKIO_WORKER_THREADS").ok();
    let worker_threads = server_worker_threads(configured_worker_threads.as_deref())
        .unwrap_or_else(|message| panic!("{message}"));
    let configured_lod_threads = std::env::var("LOD_BUILD_THREADS").ok();
    let lod_threads = lod_workers::lod_build_threads(configured_lod_threads.as_deref())
        .unwrap_or_else(|message| panic!("{message}"));
    let mut notify_mode = (claim_on_demand && store.is_some())
        .then(|| world_store::NotifyMode::from_env().unwrap_or_else(|e| fatal(&e)));
    let mut world_link_config = (role == roles::Role::Host).then(|| world_link_config().unwrap_or_else(|e| fatal(&e)));
    // SIGINT / SIGTERM (`just run` stops its server with TERM) end the main loop like a last
    // client leaving does, so the world is saved on the way out; a stop before a host
    // registered exits at once.
    let stop_signal = early_stop.unwrap_or_else(watch_stop_signals_on_thread);
    let boot = host_boot();
    // A host REGISTERS FIRST (Elastic 1): its peer listener bound (`claim`: the row names its
    // address and pin) and where players reach it, then the store issues its id — a new one at
    // every start, never one a process had before. Everything keyed by a host is built from that
    // id afterwards: its ownership, its fence and claims, its heartbeat, its hellos and links, its
    // cut parts. Nothing is claimed, sent or written under an id before it was issued.
    let mut heartbeat: Option<lease::Heartbeat> = None;
    let mut world_generation = claims::WorldGeneration::default();
    let mut peer_listener: Option<peer_link::PeerListener> = None;
    let host_id = match (&store, role) {
        (Some(store), roles::Role::Host) => {
            if claim_on_demand {
                let config = peer_link::PeerLinkConfig::from_env(&host_name).unwrap_or_else(|e| fatal(&e));
                let token =
                    svc_link::service_token_from_env().unwrap_or_else(|e| fatal(&format!("the service link's token: {e}")));
                let bind = config.bind;
                let listener = peer_link::PeerListener::bind(config, token)
                    .unwrap_or_else(|e| fatal(&format!("the peer listener on {bind}: {e}")));
                peer_listener = Some(listener);
            }
            let (peer_addr, peer_cert) = peer_listener.as_ref().map(peer_link::PeerListener::row_fields).unzip();
            let lookup = |name: &str| std::env::var(name).ok();
            let new_host = world_store::NewHost {
                name: host_name.clone(),
                boot,
                peer_addr,
                peer_cert,
                quic: Some(peer_link::advertise_quic_from(lookup(peer_link::ADVERTISE_QUIC_ENV).as_deref(), port)),
                webtransport: peer_link::advertise_wt_from(lookup(peer_link::ADVERTISE_WT_ENV).as_deref()),
                wt_cert_hash: client_cert.sha256_hex(),
            };
            // A `static` registration beside a live row (`NotAlone`) is refused here (exit 1): its
            // message names `VOXEL_OWNERSHIP=claim`.
            let registered = match lease::register(store, new_host, crash_policy, &stop_signal) {
                Ok(Some(registered)) => registered,
                Ok(None) => return claims::ServerExit::Clean,
                Err(e) => fatal(&lease::registration_refusal(&e)),
            };
            heartbeat = Some(lease::Heartbeat::new(store.clone(), registered.id, boot));
            info!(
                "[server] registered as host {} at world generation {}{}",
                host_label(registered.id, &host_name),
                registered.generation,
                if registered.bumped { " (raised: a total crash, this host took every dirty host's land)" } else { "" }
            );
            world_generation = claims::WorldGeneration::new(registered.generation);
            health::Health::global().world_generation(registered.generation);
            health::Health::global().set_registered(true);
            // A host is Ready once a coordinator replica acknowledged its attach.
            health::Health::global().require_attach();
            registered.id
        }
        _ => HostId::SOLO,
    };
    let ownership = game::ownership_for(&ownership_word, host_id).unwrap_or_else(|e| fatal(&e));
    // Role `host` (any number of hosts, `static` or `claim`): the login service is linked to the
    // coordinator, which hands each of its orders to one live host (the gate host); this host's
    // relay resolves the orders it is given, places spawns here or on the host holding the
    // player, and sends its answers up its own coordinator link (`login_link`). It never listens
    // for the login service itself.
    let mut login_relay = match (&spawn_orders, role) {
        (Some((orders, _)), roles::Role::Host) => {
            info!("[server] login relay: the login service is linked to the coordinator, which relays its orders here");
            Some(login_link::LoginRelay::new(orders.clone(), players.clone(), client_packet_send.clone()))
        }
        _ => None,
    };
    // Where players reach this host (`ServerPacket::Host`, its `hosts` row).
    let endpoint = host_endpoint(host_id, port, |name| std::env::var(name).ok(), client_cert.sha256_hex());
    let mut authenticator = auth_edge::Authenticator::for_mode(auth_mode);
    if let Some((orders, _)) = &spawn_orders {
        authenticator = authenticator.with_spawn_orders(orders.clone());
    }
    // Several hosts: a client another host sends here (`RegionElsewhere`) connects with the
    // ticket that host vouched for; the main loop records its orders.
    let observer_orders = claim_on_demand.then(|| Arc::new(auth_edge::ObserverOrders::new(auth_edge::system_clock())));
    if let Some(orders) = &observer_orders {
        authenticator = authenticator.with_observer_orders(orders.clone());
    }
    let auth = Arc::new(authenticator);
    let mut rgi = WorldIngress::new(ip, port, webtransport_enabled(single_player), players.clone(), auth.clone())
        .with_client_cert(client_cert)
        .with_webtransport_port(wt_port)
        .with_host_name(&host_name);
    let sinks = rgi.sinks();
    let rt = build_server_runtime(worker_threads);
    let cps = client_packet_send.clone();
    // The login service's push: a listener (dropped at the end of `run`, sync code) — role
    // `all` only (one process; split roles: the coordinator listens, see above).
    let _login_link = spawn_orders.filter(|_| role != roles::Role::Host).map(|(orders, token)| {
        let bind = login_link::bind_from(std::env::var(login_link::HOST_LINK_BIND_ENV).ok().as_deref())
            .unwrap_or_else(|e| fatal(&e));
        let pin_file =
            login_link::cert_hash_file_from(std::env::var(login_link::HOST_LINK_CERT_HASH_FILE_ENV).ok().as_deref());
        let world = login_link::link_world_from(std::env::var(auth_edge::WORLD_ID_ENV).ok().as_deref());
        let (pin, events) = (Some(pin_file.clone()), client_packet_send.clone());
        let link = login_link::start_in_world(bind, token, world, pin, orders, players.clone(), events)
            .unwrap_or_else(|e| fatal(&format!("the login link on {bind}: {e}")));
        info!(
            "auth: login tickets need the login service's spawn orders, pushed to {} (pin {} in {})",
            link.local_addr(),
            link.cert_sha256(),
            pin_file.display()
        );
        link
    });
    // The startup JWKS fetch, in the background: a login service that is
    // down does not hold the start up (the first connect fetches again).
    rt.spawn(async move { auth.prefetch().await });
    std::thread::spawn(move || rt.block_on(async move { rgi.listen(cps, server_recv).await }));

    let (region_out_send, region_out_recv) = crossbeam::channel::unbounded();
    // Block registry: prefer the runtime manifest, fall back to the copy
    // embedded at build time (same repo file) so headless runs never fail.
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/blocks/blocks.ron"),
    )
    .unwrap_or_else(|_| include_str!("../../../assets/blocks/blocks.ron").to_string());
    let registry = game::BlockRegistry::from_ron(&manifest).expect("invalid block manifest");
    game::install_block_props(registry.props());
    let resolve = |name: &str| {
        registry
            .id_of(name)
            .unwrap_or_else(|| panic!("block manifest must define \"{name}\""))
    };
    let materials = worldgen::material::Materials::from_names(resolve);
    match &world_source {
        world_file::WorldSource::Store { url, .. } => {
            info!("[server] world: forged, in the world store {url} (sampled from its tiles)")
        }
        world_file::WorldSource::Procedural => info!("[server] world: procedural (nothing is saved)"),
    }
    // The world map was built when the world was written and is read from
    // the store with it (procedural worlds: none).
    let world_map = world_file::install_world(&mut opened);
    let world_seed = worldgen::WORLD_SEED;
    // Prefabs, and (only when the environment asks) a dev structure source: before the cache key and
    // anything that can generate terrain.
    let dev_source_installed = {
        let landings = dev_structures::requested().then(|| world_map.clone()).flatten().map(std::sync::Arc::new);
        let landing_of: dev_structures::LandingOf = std::sync::Arc::new(move |rc: game::RegionCoords| {
            landings.as_ref().and_then(|m| m.tile(rc)).and_then(|t| t.landing)
        });
        match dev_structures::install_from_env(&registry, landing_of, world_seed, materials) {
            Ok(dev_structures::DevStructures::Off) => false,
            Ok(dev_structures::DevStructures::Region { x, z, placements }) => {
                info!("[structures] {placements} placements in region {x},{z} from VOXEL_DEV_STRUCTURES");
                true
            }
            Ok(dev_structures::DevStructures::EveryRegion { spec }) => {
                info!("[structures] every region: {spec} (from VOXEL_DEV_STRUCTURES)");
                true
            }
            Ok(dev_structures::DevStructures::Home { spec }) => {
                info!("[structures] home region: {spec} (from VOXEL_DEV_STRUCTURES)");
                true
            }
            Ok(dev_structures::DevStructures::Town { style }) => {
                info!("[structures] home region: a {style} town (from VOXEL_DEV_STRUCTURES)");
                true
            }
            Err(message) => panic!("{message}"),
        }
    };
    // A cached region file would carry or lack a dev prefab independently of this run.
    let gen_cache = if dev_source_installed {
        info!("[structures] worldgen disk cache off: a dev structure source is installed");
        worldgen::cache::WorldgenCache::disabled()
    } else {
        worldgen::cache::WorldgenCache::from_env(world_seed, &manifest)
    };
    let (lod_result_send, lod_result_recv) = crossbeam::channel::unbounded();
    info!(
        "[server] lod workers: {lod_threads} ({})",
        if configured_lod_threads.is_some() { "LOD_BUILD_THREADS" } else { "default: cores / 4, clamped 2..=6" }
    );
    let lod_builder: Box<dyn game::LodBuilder> = Box::new(lod_workers::ThreadLodBuilder::new(
        world_seed,
        materials,
        gen_cache.clone(),
        // Saved regions that have not run in this process: their LOD is
        // built from the store (procedural worlds: none).
        store.clone().map(|s| std::sync::Arc::new(world_store::TerrainFromStore(s)) as std::sync::Arc<dyn game::StoredTerrain>),
        lod_result_send,
        lod_threads,
    ));
    // Kept only to read the manager→writer backlog for the `[server]` line.
    let writer_backlog = server_send.clone();
    // A drained host's refusals of new sessions (`drain::idle_drained`): the manager is gone by then.
    let drained_out = server_send.clone();
    // Region starts generate on their own thread; the manager's copy of the
    // generator is only reached by paths that stay synchronous by design.
    let region_gen_cache = gen_cache.clone();
    // A world map is installed exactly for a forged world (from the store; procedural: none).
    let forged_world = world_map.is_some();
    let region_generator: region_threads::SharedRegionGenerator =
        std::sync::Arc::new(move |rc| {
            // The first region announced is the home region of a `home=` / `town=` dev source; far-terrain
            // and halo generation must not decide it, so only this generator announces. In a forged world
            // the spawn region, where a fresh session waits before its adventure starts, is never announced: the
            // home is the adventure's start region (`dev_structures::announces_region`).
            if dev_structures::announces_region(rc, forged_world) {
                worldgen::structures::active().region_requested(rc);
            }
            region_gen_cache.generate_cached(rc, world_seed, materials)
        });
    // The world clock: ms since `start` (the manager's `now_ms` base) -> world
    // tick, continuing from the last committed cut's tick (procedural worlds:
    // from 0, never saved).
    // Split roles share one wall-anchored clock stored in the world row (R3), so the
    // base instant is back-dated by the clock's phase: tick edges fall at the same wall
    // instants in every process.
    let (world_clock, clock_phase) =
        roles::start_clock(role, store.as_ref(), postgres, opened.world_tick).unwrap_or_else(|e| fatal(&e));
    let start = roles::back_dated_start(clock_phase);
    // Halo seeds of never-run neighbours: only their band columns, on the
    // starting region's thread (`game::halo`).
    // The region thread pinned its 3x3 neighbourhood's forged tiles before
    // it was built (a tile that would not load stopped it), so these reads
    // never load; a failure here is a bug, never an empty seam.
    let halo_generator: game::HaloGenerator = std::sync::Arc::new(move |rc, xs, zs| {
        let _pins = worldgen::prefetch_region(rc)
            .unwrap_or_else(|e| panic!("halo seed of {rc:?}: its forged world tiles do not load: {e}"));
        worldgen::generate_columns(rc, xs, zs, world_seed, materials)
    });
    // Each region thread pins the forged tiles of its 3x3 neighbourhood
    // before it is built; one that will not load stops the region
    // (`RegionOutput::LoadFailed`).
    // Every region thread notes its ticks here; each heartbeat reads the worst of its window.
    let tick_load = load::TickLoad::default();
    let mut spawner = region_threads::ThreadRegionSpawner::with_generator(region_generator, halo_generator)
        .with_tick_load(tick_load.clone())
        .with_prefetch(std::sync::Arc::new(worldgen::prefetch_region))
        .with_clock(world_clock, start);
    // Region loads and cut commits are fenced: the super-regions this process owns are
    // claimed now, once, as the id the store issued (procedural worlds keep nothing: no
    // claims). One claim is shared by the region threads and the cut writer.
    let claims = store.clone().map(|store| {
        let extent = world_map.as_ref().map(game::WorldMap::extent);
        let claimed = if claim_on_demand {
            // `claim` claims nothing at start: free land is claimed on demand (the directory
            // keeper below). Its super-region side was checked before the registration.
            claims::HostClaims::none(store, host_id)
        } else {
            claims::HostClaims::claim(store, ownership.as_ref(), extent, super_region_regions).unwrap_or_else(|e| fatal(&e))
        };
        // Only now, with every epoch bumped (any split host is fenced), may `all` forget the anchor.
        roles::clear_anchor_after_claim(role, Some(claimed.store()), postgres).unwrap_or_else(|e| fatal(&e));
        if !claim_on_demand {
            info!(
                "[server] claimed {} super-regions as host {}",
                claimed.fence().epochs.len(),
                host_label(claimed.fence().owner, &host_name)
            );
        } else {
            // What the registration gave this new id: nothing — or, a total crash (no other
            // host live), every dirty host's land at `epoch + 1`. Taken into the fence (its
            // epochs bumped again, so the processes before are fenced).
            let fenced_exit = |e: &dyn std::fmt::Display| -> ! {
                log::error!("[server] fenced while starting as host {}: {e}", host_label(host_id, &host_name));
                std::process::exit(claims::EXIT_FENCED)
            };
            match claimed.reclaim_own(boot) {
                Ok(0) => {}
                Ok(n) => info!("[server] took {n} super-regions the registration gave host {host_id} (a total crash)"),
                Err(e @ world_store::StoreError::Fenced { .. }) => fenced_exit(&e),
                Err(e) => fatal(&format!("taking the super-regions the registration gave this host: {e}")),
            }
            // The claim may have waited behind a row lock: the lease is beaten between the
            // boot's long steps (the main loop's pump comes much later).
            lease::beat_at_boot(&mut heartbeat, "taking its super-regions");
        }
        claimed
    });
    // `claim`: a link to every older live host, and the admission of newer ones (each one's
    // link made when it dials in). Dropped at the end of `run` (sync code).
    let mut peer_links: Option<peer_link::PeerLinks> = peer_listener.map(|listener| {
        let store = store.clone().expect("a host has a store");
        peer_link::PeerLinks::start(listener, host_id, boot, store)
            .unwrap_or_else(|e| fatal(&format!("starting the peer links: {e}")))
    });
    let fenced = claims.as_ref().map(|c| c.fenced().clone()).unwrap_or_default();
    if let Some(claims) = &claims {
        spawner = spawner.with_store(claims.clone());
    }
    let mut manager = game::WorldManager::new(
        spawner,
        // Unused on the server (regions and LOD generate on their own
        // threads); pinned like them all the same.
        Box::new(move |rc| {
            let _pins = worldgen::prefetch_region(rc)
                .unwrap_or_else(|e| panic!("region {rc:?}: its forged world tiles do not load: {e}"));
            gen_cache.generate_cached(rc, world_seed, materials)
        }),
        server_send,
        region_out_send,
        lod_builder,
        lod_result_recv,
    );
    manager.set_ownership(ownership, super_region_regions);
    let land_idle_ms = land_idle_ms_from(std::env::var(LAND_IDLE_MS_ENV).ok().as_deref()).unwrap_or_else(|message| fatal(&message));
    if land_idle_ms != game::LAND_IDLE_MS {
        info!("[server] land idle for {land_idle_ms} ms is released ({LAND_IDLE_MS_ENV})");
    }
    manager.set_land_idle_ms(land_idle_ms);
    // `claim`: the directory read whole before anything is placed or restored (until then
    // every owner is `Unknown`), and the loop's directory keeper (it fences assigned land).
    let mut directory_keeper = None;
    if let Some(claims) = claims.as_ref().filter(|_| claim_on_demand) {
        let (rows, live, draining) = claims::read_directory(claims.store(), host_id)
            .unwrap_or_else(|e| fatal(&format!("reading the ownership directory: {e}")));
        info!("[server] ownership directory: {} super-regions, live hosts {live:?}, draining {draining:?}", rows.len());
        manager.set_draining(draining);
        manager.directory_read(rows, live);
        lease::beat_at_boot(&mut heartbeat, "reading the ownership directory");
        directory_keeper = Some(claims::DirectoryKeeper::new(claims.clone()));
    }
    // `claim`: an heir's adoption reads (R4, a succession) — what it
    // inherits, read on the store thread while this loop (and its heartbeat) runs on.
    let mut adoption_reader =
        claims.as_ref().filter(|_| claim_on_demand).map(|claims| claims::AdoptionReader::new(claims.clone()));
    manager.set_endpoints(endpoint.clone(), os_nonces());
    // The coordinator refuses an `Attach` of an older world generation than the store's.
    manager.set_host_generation(world_generation.mine());
    // A session per other host, over its link; the links' wake-up (none with one host).
    let peer_hello = || game::PeerHello {
        id: host_id,
        boot,
        generation: world_generation.mine(),
        endpoint: endpoint.clone(),
        received_through: 0,
        received_from_boot: None,
    };
    let mut peer_wake = crossbeam::channel::never::<()>();
    if let Some(links) = peer_links.as_mut() {
        peer_wake = links.wake();
        for (other, link) in links.take() {
            manager.set_peer(other, game::PeerSession::new(peer_hello(), other), link);
        }
        // Before any placement: the hosts that left cleanly hold nobody (a spawn order does
        // not wait for them). A read that fails leaves every host expected (orders wait).
        if let Some(store) = &store {
            match peer_link::left_cleanly(store, host_id) {
                Ok(left) => manager.note_left_peers(left),
                Err(e) => log::warn!(
                    "[server] reading which hosts left cleanly: {e}; every host is awaited until the next directory read"
                ),
            }
        }
    }

    // The world coordinator's thread and its link's wake-up (none without a history).
    let mut coordinator: Option<coordinator_rt::CoordinatorThread> = None;
    let mut world_wake = crossbeam::channel::never::<()>();
    // The world map (stored with the forged world: per-region tile
    // summaries); procedural worlds have none.
    if let Some(map) = world_map {
        info!("[server] world map: {}x{} tiles, from the world store", map.width, map.height);
        // Forged worlds keep their history in the store (procedural: none): the
        // world coordinator, the only writer of history state, runs on its own
        // `coordinator` thread joined by a channel link (`all`), or is its own
        // process dialed over the service link (`host`).
        if role == roles::Role::Host {
            let config = world_link_config.take().expect("a host's world link was configured before its registration");
            let link = host_world_link(config, host_id);
            world_wake = link.wake();
            manager.set_world_link(Box::new(link), boot, 0);
        } else if let Some(store) = &store {
            let sqlite_db = match &world_source {
                world_file::WorldSource::Store { url: world_store::StoreUrl::Sqlite(db), .. } => Some(db.as_path()),
                _ => None,
            };
            if let Some(start) =
                world_file::ensure_coordinator_state(store, &map, sqlite_db).unwrap_or_else(|e| fatal(&e))
            {
                let (link, thread) =
                    coordinator_rt::start_all_in_one(store.clone(), start, fenced.clone()).unwrap_or_else(|e| fatal(&e));
                world_wake = link.wake();
                coordinator = Some(thread);
                manager.set_world_link(Box::new(link), host_boot(), 0);
            }
        }
        manager.set_world_map(map);
    }
    // What the store holds: which regions have stored state (their start
    // reads it), and what its last cut caught on its way (into the mailboxes).
    let mut cuts = claims.map(|claims| {
        let store = claims.store().clone();
        // The peer journal counts from the store's last committed world cut, before any peer
        // traffic (the restored in-flight rows below are its first entries). A read error is
        // fatal: a guessed base would be wrong (wait, never guess). One host, or SQLite:
        // `CutId(0)` — one-phase cuts record no decision, and there is no peer to journal.
        let journal_base = store.last_committed_cut().unwrap_or_else(|e| fatal(&format!("reading the last committed cut: {e}")));
        manager.start_peer_journal(journal_base);
        let stored = store.stored_regions().unwrap_or_else(|e| fatal(&e.to_string()));
        let in_flight =
            store.load_in_flight(claims.fence().in_flight_scope()).unwrap_or_else(|e| fatal(&e.to_string()));
        info!(
            "[server] world store: {} stored regions, {} crossings / intents in flight, world tick {}",
            stored.len(),
            in_flight.len(),
            opened.world_tick
        );
        manager.set_stored_regions(stored);
        manager.restore_in_flight(in_flight);
        lease::beat_at_boot(&mut heartbeat, "reading the stored regions and what was in flight");
        // Several hosts: the coordinator picks every cut (a world cut, staged and decided
        // together); one host cuts on its own (the one-phase cut).
        if claim_on_demand {
            cuts::CutWriter::with_world_cuts(claims, world_generation.clone())
        } else {
            cuts::CutWriter::new(claims)
        }
    });

    // Several hosts: other hosts' saves re-seed the halos of their parked regions next to
    // this host's (`VOXEL_TERRAIN_NOTIFY`, `VOXEL_WORLD_DB_DIRECT`; one host: nothing to learn).
    let mut terrain_watch = match (&cuts, claim_on_demand) {
        (Some(cuts), true) => {
            let mode = notify_mode.take().expect("parsed before the registration");
            info!("[server] stored terrain of other hosts: {mode:?} (a resync every {} s)", world_store::TERRAIN_POLL_SECS);
            Some(cuts::TerrainWatch::start(cuts.store().clone(), mode))
        }
        _ => None,
    };
    let mut report = perf::ServerReport::new();
    let mut ever_connected = false;
    let mut last_world_save = std::time::Instant::now();
    let save_every = std::time::Duration::from_secs(world_save_secs(std::env::var(WORLD_SAVE_SECS_ENV).ok().as_deref()));
    if let Some(cuts) = cuts.as_mut().filter(|c| c.world_cuts()) {
        cuts.set_world_cut_period(save_every);
    }
    let parent_gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if single_player {
        let gone = parent_gone.clone();
        std::thread::Builder::new()
            .name("parent-watch".into())
            .spawn(move || watch_parent(std::io::stdin(), &gone))
            .expect("spawn the parent watch thread");
    }
    // Role `all` registers nothing: ready once its main loop runs (a host already is).
    health::Health::global().set_registered(true);
    // The loop ended for a drain's deadline (`drain.rs`): after a committed leave the process
    // stays up drained instead of exiting.
    let mut drain_due = false;
    // A `static` host told the operator once that it ignores a drain mark.
    let mut static_drain_warned = false;
    'main: loop {
        health::Health::global().loop_beat();
        health::Health::global().set_attached(manager.world_attached());
        let now_ms = start.elapsed().as_millis() as u64;
        crossbeam::channel::select! {
            recv(client_packet_recv) -> ev => match ev {
                Ok(ev) => {
                    if !manager.handle_server_event(ev, now_ms) {
                        break 'main;
                    }
                }
                Err(_) => break 'main,
            },
            recv(region_out_recv) -> out => {
                if let Ok((rc, output)) = out {
                    manager.handle_region_output(rc, output, now_ms);
                }
            },
            // The coordinator sent something: `maintain` below reads it.
            recv(world_wake) -> _ => {},
            // Another host sent something: `maintain` pumps the peers.
            recv(peer_wake) -> _ => {},
            default(std::time::Duration::from_millis(200)) => {}
        }
        let sessions = manager.depths().sessions;
        ever_connected |= sessions > 0;
        let gone = parent_gone.load(std::sync::atomic::Ordering::SeqCst);
        if let Some(why) = single_player_exit(single_player, ever_connected, sessions, gone) {
            info!("[server] single-player: {why}, exiting");
            break 'main;
        }
        if fenced.is_set() {
            log::error!("[server] fenced: another process claimed this server's super-regions or its coordinator; stopping");
            break 'main;
        }
        if world_generation.is_set() {
            log::error!("[server] {}; stopping to restart from the same save", world_generation.why());
            break 'main;
        }
        if coordinator.as_ref().is_some_and(coordinator_rt::CoordinatorThread::is_finished) {
            log::error!("[server] the world coordinator thread stopped; stopping");
            break 'main;
        }
        if stop_signal.load(std::sync::atomic::Ordering::SeqCst) {
            info!("[server] stop signal: saving the world and exiting");
            break 'main;
        }
        // The lease is beaten from THIS loop on purpose (R4 decision 3): a hung main loop must
        // stop beating although its sockets live, so the coordinator sees it lapse — a timer
        // thread would keep beating for a host that no longer runs.
        if lease::pump_or_fence(&mut heartbeat, &fenced, std::time::Instant::now(), &mut || {
            load::host_load(&manager, &tick_load)
        }) {
            break 'main;
        }
        // A drain mark (`worldctl drain`), read by that beat: the deadline its players are told
        // (`claim` hosts; a `static` host is a world's only one and is not drained).
        if claim_on_demand {
            let now_tick = world_clock.tick_at(start.elapsed().as_millis() as u64);
            drain::note_drain(&mut manager, heartbeat.as_ref().and_then(lease::Heartbeat::drain_in_ms), now_tick);
        } else if drain::static_mark_first_seen(&mut static_drain_warned, heartbeat.as_ref().and_then(lease::Heartbeat::drain_in_ms)) {
            log::warn!("{}", drain::STATIC_HOST_IGNORES_DRAIN);
        }
        // A newer host dialed in: its session, over the link made for it.
        if let Some(links) = &peer_links {
            for (other, link) in links.take_arrived() {
                info!("[server] host {other} joined: a peer session with it");
                manager.set_peer(other, game::PeerSession::new(peer_hello(), other), link);
            }
        }
        manager.maintain(now_ms);
        // `claim`: claims the manager wants, directory reads (never blocks).
        if let Some(keeper) = directory_keeper.as_mut() {
            keeper.pump(&mut manager);
            // The `hosts` rows that read brought: an older host found since is dialed, and the
            // admission answers from them (Elastic 1 Task 4 — the directory's own period,
            // `world_store::TERRAIN_POLL_SECS`, or sooner when the manager wants a read).
            if let (Some(rows), Some(links)) = (keeper.take_hosts_read(), peer_links.as_mut()) {
                for (other, link) in links.refresh(&rows) {
                    info!("[server] host {other} found in the hosts table: a peer session with it");
                    manager.set_peer(other, game::PeerSession::new(peer_hello(), other), link);
                }
            }
        }
        // An heir: the store's half of an adoption (never blocks; `Fenced` sets the flag).
        if let Some(reader) = adoption_reader.as_mut() {
            reader.pump(&mut manager);
        }
        // Several hosts: the observer tickets other hosts vouched for since the last pass.
        observer_pass(&mut manager, observer_orders.as_deref(), &sinks);
        // The login relay (role `host`): the coordinator's orders resolved, spawn orders taken,
        // kicks relayed, answers up the coordinator link.
        match login_relay.as_mut() {
            Some(relay) => relay.pump(&mut manager),
            None => {
                // No spawn orders here (no service token, or role `all`, whose login link answers
                // itself): an order the coordinator relayed anyway cannot be taken.
                let dropped = manager.take_login_orders();
                if !dropped.is_empty() {
                    log::warn!("[server] {} login orders from the coordinator, and this host takes none (no service token): dropped", dropped.len());
                }
            }
        }
        // A peer runs a newer world generation: this host is stale (polled above).
        if let Some(generation) = manager.take_higher_generation() {
            world_generation.note(generation);
        }
        // Elastic 2a: state traded with a dead host no succession here accounts for: stop
        // without a save (the manager logged why); this host's lapse rolls the world back.
        if let Some(dead) = manager.take_unaccounted_dead() {
            log::error!("[server] state exchanged with dead host {dead} is accounted for nowhere: stopping without a save");
            world_generation.stop_for_rollback();
        }
        manager.pump_lod_results();
        if let Some(cuts) = cuts.as_mut() {
            // Backpressure: no new cut while the store still commits one. Under `claim` only
            // the coordinator starts cuts (`BeginCut`); while it is down there are none.
            if !cuts.world_cuts() && last_world_save.elapsed() >= save_every && cuts.in_flight() == 0 && !fenced.is_set() {
                last_world_save = std::time::Instant::now();
                // Regions, players and what is in flight, at one
                // world tick a lead ahead (`save_cut.rs`).
                manager.begin_cut(world_clock.tick_at(start.elapsed().as_millis() as u64));
            }
            cuts.pump(&mut manager);
        }
        if let Some(watch) = terrain_watch.as_mut() {
            watch.pump(&mut manager);
        }
        // The drain's deadline passed and the coordinator is attached: leave (below), then idle.
        if manager.drain_due() {
            info!("[server] the drain deadline passed: leaving the world");
            drain_due = true;
            break 'main;
        }
        report.maybe_report(
            &mut manager,
            perf::Backlog {
                client_events: client_packet_recv.len(),
                region_outputs: region_out_recv.len(),
                writer: writer_backlog.len(),
            },
            store.as_ref(),
            cuts.as_ref().map(|c| c.land()),
        );
    }
    // Stopping: no longer ready (and no longer judged by its loop's beat).
    health::Health::global().set_registered(false);

    // Orderly exit: park every player (regions answer `ParkPlayer` before
    // `Shutdown`), then everything else; the coordinator saves once more; a
    // region that does not park within `EXIT_PARK_TIMEOUT` is given up, and
    // then no last cut is written (the world restarts from its last periodic
    // save). A fenced process parks and
    // saves nothing: its clients are refused and it exits `EXIT_FENCED`.
    let had_sessions = manager.depths().sessions > 0;
    let outcome = cuts::exit_world(
        &mut manager,
        cuts::ExitParams {
            region_out: &region_out_recv,
            now_ms: || start.elapsed().as_millis() as u64,
            cuts,
            coordinator,
            fenced: &fenced,
            world_generation: &world_generation,
            park_timeout: cuts::EXIT_PARK_TIMEOUT,
            save_timeout: EXIT_SAVE_TIMEOUT,
            now_tick: || world_clock.tick_at(start.elapsed().as_millis() as u64),
        },
        // Under `claim` (`CrashPolicy::Succession`) a host of several keeps its lease while it
        // waits for its leave cut (R4): a lapse there would be declared a crash of a host whose
        // part the leave cut is staging; a `Replaced` answer sets the fenced flag the exit
        // watches. Under `Static` nothing beats during the exit: R3's exit exactly. Once the leave
        // committed nothing beats any more: the clean row may be pruned at once (a landless
        // leaver's), and a beat finding no row must not make a clean exit a fenced one.
        |m| {
            if claim_on_demand && m.leave_outcome() != Some(true) {
                lease::pump_or_fence(&mut heartbeat, &fenced, std::time::Instant::now(), &mut || load::host_load(m, &tick_load));
            }
        },
    );
    // A host that left (in a committed cut or not) told its clients `LEAVING_REFUSAL`.
    let left = outcome == claims::ServerExit::Clean && manager.leave_asked();
    if (left || matches!(outcome, claims::ServerExit::Fenced | claims::ServerExit::WorldRolledBack)) && had_sessions {
        // The refusals are on their way out through the writer task.
        std::thread::sleep(FENCED_EXIT_FLUSH);
    }
    let outcome = if drain::stays_drained(drain_due, outcome, manager.leave_outcome()) {
        // Drained (`drain.rs`): nothing of the world is held here any more. The lease stops (no
        // store call is made from now on: the clean row may be pruned), and the regions, the
        // peer and coordinator links, the claim loop and the login relay go with the manager —
        // what the leave settled stays settled. The edge stays up to refuse new sessions.
        drop(heartbeat);
        drop(directory_keeper);
        drop(adoption_reader);
        drop(terrain_watch);
        drop(login_relay);
        drop(manager);
        drop(peer_links);
        drain::idle_drained(&stop_signal, &client_packet_recv, &drained_out, health::Health::global())
    } else {
        if drain_due && outcome == claims::ServerExit::Clean {
            log::error!("[server] the drain's leave did not commit: exiting with this host's row dirty (a crash to the others)");
        }
        outcome
    };
    #[cfg(feature = "pyroscope")]
    if let Some(agent) = agent_running {
        let agent_ready = agent.stop().unwrap();
        agent_ready.shutdown();
    }
    outcome
}

/// Re-reads the players' certificate files every [`client_cert::CERT_POLL`]: a renewal is
/// served to the next handshake on both listeners; one that does not load is read again once
/// after [`client_cert::RELOAD_RETRY`], then keeps the old pair until the next poll.
async fn reload_client_cert(cert: client_cert::ClientCert) {
    loop {
        tokio::time::sleep(client_cert::CERT_POLL).await;
        match cert.reload_with_retry(client_cert::RELOAD_RETRY).await {
            Ok(true) => info!("client certificate reloaded (QUIC and WebTransport)"),
            Ok(false) => {}
            Err(e) => log::warn!("{e}; still serving the previous certificate"),
        }
    }
}

/// Where players reach host `ordinal` (`ServerPacket::Host`, its `hosts` row): the
/// advertised addresses ([`peer_link::ADVERTISE_QUIC_ENV`], [`peer_link::ADVERTISE_WT_ENV`]
/// through `lookup`) and the hash of the players' self-signed certificate (`cert_hash`,
/// `client_cert::ClientCert::sha256_hex`; QUIC and WebTransport serve it), which a client pins.
fn host_endpoint(
    ordinal: HostId,
    port: u16,
    lookup: impl Fn(&str) -> Option<String>,
    cert_hash: Option<String>,
) -> game::HostEndpoint {
    game::HostEndpoint {
        id: ordinal,
        quic: Some(peer_link::advertise_quic_from(lookup(peer_link::ADVERTISE_QUIC_ENV).as_deref(), port)),
        webtransport: peer_link::advertise_wt_from(lookup(peer_link::ADVERTISE_WT_ENV).as_deref()),
        cert_hash,
    }
}

/// The main loop's pass over the manager's observer bookkeeping (after `maintain`): the
/// orders other hosts sent go to the edge's book (`claim`: `orders`), and the
/// sessions that became (or started as) their home are revoked there — their tickets admit
/// nothing more — and marked on their connection, which no observer connection replaces.
pub fn observer_pass<S: game::RegionSpawner>(
    manager: &mut game::WorldManager<S>,
    orders: Option<&auth_edge::ObserverOrders>,
    sinks: &DashMap<ClientId, ClientSink>,
) {
    let taken = manager.take_observer_orders();
    if let Some(orders) = orders {
        orders.record_all(taken);
    }
    for client in manager.take_observer_revocations() {
        if let Some(orders) = orders {
            orders.revoke(client);
        }
        if let Some(sink) = sinks.get(&client) {
            sink.set_home();
        }
    }
}

/// How long a fenced server waits, before it exits, for its clients'
/// `Refused` packets to leave the writer task.
const FENCED_EXIT_FLUSH: std::time::Duration = std::time::Duration::from_millis(500);

/// How often the main loop starts a save cut of the world (regions, players,
/// the world tick and what is in flight, at one world tick) into the world
/// store; the coordinator saves the history itself. The orderly exit saves once
/// more after parking every region.
pub const WORLD_SAVE_SECS: u64 = 60;

/// How long the orderly exit waits for the world store to commit its last save.
pub const EXIT_SAVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A start-up error the server cannot run with: logged and printed, then the
/// process exits (status 1).
pub(crate) fn fatal(message: &str) -> ! {
    log::error!("[server] {message}");
    eprintln!("server: {message}");
    std::process::exit(1)
}

/// Overrides [`WORLD_SAVE_SECS`] (whole seconds, at least 1) — for testing.
pub const WORLD_SAVE_SECS_ENV: &str = "VOXEL_SAVE_SECS";

/// The autosave period from `VOXEL_SAVE_SECS`; unset, empty or invalid (a
/// warning) is [`WORLD_SAVE_SECS`].
pub fn world_save_secs(env_value: Option<&str>) -> u64 {
    match env_value.map(str::trim) {
        None | Some("") => WORLD_SAVE_SECS,
        Some(v) => match v.parse::<u64>() {
            Ok(secs) if secs >= 1 => secs,
            _ => {
                log::warn!("[server] {WORLD_SAVE_SECS_ENV}={v:?} is not a whole number of seconds >= 1; using {WORLD_SAVE_SECS}");
                WORLD_SAVE_SECS
            }
        },
    }
}

/// Starts [`watch_stop_signals`] on a thread of its own and returns its flag.
pub(crate) fn watch_stop_signals_on_thread() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::Builder::new()
        .name("stop-signals".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("signal runtime");
            rt.block_on(watch_stop_signals(flag));
        })
        .expect("spawn the stop-signal thread");
    stop
}

/// Sets `stop` on the first SIGINT / SIGTERM; a second one exits at once
/// (an orderly exit that hangs can still be interrupted).
pub(crate) async fn watch_stop_signals(stop: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    #[cfg(unix)]
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(term) => term,
        Err(e) => {
            log::error!("[server] cannot watch SIGTERM: {e}");
            return;
        }
    };
    loop {
        #[cfg(unix)]
        let got = tokio::select! {
            r = tokio::signal::ctrl_c() => r.is_ok(),
            r = term.recv() => r.is_some(),
        };
        #[cfg(not(unix))]
        let got = tokio::signal::ctrl_c().await.is_ok();
        if !got {
            return;
        }
        if stop.swap(true, std::sync::atomic::Ordering::SeqCst) {
            log::warn!("[server] second stop signal: exiting without saving");
            std::process::exit(130);
        }
    }
}

async fn handle_connection(
    connection: quinn::Connection,
    send: Sender<ServerEvent>,
    id: ClientId,
    sinks: Arc<DashMap<ClientId, ClientSink>>,
    mine: ClientSink,
) -> Result<(), ConnectionError> {
    loop {
        let send = send.clone();
        let (sinks, mine) = (sinks.clone(), mine.clone());
        let stream = connection.accept_uni().await;

        let mut stream = match stream {
            Err(quinn::ConnectionError::ApplicationClosed { .. }) => {
                info!("connection closed");
                return Ok(());
            }
            Err(e) => {
                return Err(e);
            }
            Ok(s) => s,
        };

        tokio::spawn(async move {
            let req = stream.read_to_end(usize::MAX).await.unwrap();
            let packet = game::decode_packet(&req[..]);
            let packet: ClientPacket = match packet {
                Ok(e) => e,
                Err(e) => {
                    info!("Failed deserializing packet {:?}", e);
                    return ();
                }
            };
            player_store::forward_packet(&sinks, &send, id, |live| live.same_connection(&mine), packet);
        });
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

    /// Regression: menu Load/New world failed with the user-visible error 'port 6466 is already in use' while a dev server ran (seen in play from the menu; c3f6d622 quotes the error)
    #[test]
    fn server_port_accepts_an_override() {
        assert_eq!(server_port_from(Some("40123")), Ok(40123));
        assert_eq!(server_port_from(Some(" 7000 ")), Ok(7000));
    }

    /// Regression: same in-play failure as row 7: the single-player server also took 6467 / overwrote the cert-hash file beside a running dev server (c3f6d622; rides on the seen port clash)
    #[test]
    fn single_player_has_no_webtransport_ingress() {
        assert!(webtransport_enabled(false));
        assert!(!webtransport_enabled(true));
    }
}

/// A host's link to the world coordinator process (`VOXEL_ROLE=host`): it calls the
/// coordinator's HTTPS API at `VOXEL_COORDINATOR_ADDR` (one URL per replica, tried in turn) with
/// the service token, pinning the coordinator's certificate (`VOXEL_COORDINATOR_CERT_SHA256`,
/// else the cert-hash file — waited for if the coordinator has not written it yet). Every batch
/// names host `ordinal`; the coordinator admits its `Attach` only while its `hosts` row is live.
fn host_world_link(config: WorldLinkConfig, ordinal: HostId) -> world_http::HttpWorldLink {
    let WorldLinkConfig { urls, token, cert_file, world } = config;
    info!("[server] the world coordinator is a separate process at {} (pin from {})", urls.join(","), cert_file.display());
    world_http::HttpWorldLink::connect(
        urls,
        move || svc_link::read_cert_hash(roles::COORDINATOR_CERT_SHA256_ENV, &cert_file),
        ordinal,
        token,
        world,
    )
}

/// What a host's link to its coordinator needs that no host id is part of, read before the
/// registration (a bad setting fails the start without a `hosts` row): the coordinator's
/// URLs (`VOXEL_COORDINATOR_ADDR`), the service token, the pin file and the world id.
struct WorldLinkConfig {
    urls: Vec<String>,
    token: String,
    cert_file: std::path::PathBuf,
    world: String,
}

fn world_link_config() -> Result<WorldLinkConfig, String> {
    let urls =
        roles::coordinator_urls_from(roles::COORDINATOR_ADDR_ENV, std::env::var(roles::COORDINATOR_ADDR_ENV).ok().as_deref())?;
    let token = svc_link::service_token_from_env().map_err(|e| format!("the service token: {e}"))?;
    let cert_file = roles::cert_hash_file_from(std::env::var(roles::COORDINATOR_CERT_HASH_FILE_ENV).ok().as_deref());
    let world = login_link::link_world_from(std::env::var(auth_edge::WORLD_ID_ENV).ok().as_deref());
    Ok(WorldLinkConfig { urls, token, cert_file, world })
}

/// This host's observer nonces: each one fresh from the OS's randomness (ring's
/// `SystemRandom`, through rustls's provider), so no number of earlier nonces predicts the
/// next. A failed read stops the process: a guessable nonce would let anyone observe.
fn os_nonces() -> game::NonceSource {
    let random = rustls::crypto::ring::default_provider().secure_random;
    Box::new(move || {
        let mut bytes = [0u8; 8];
        random.fill(&mut bytes).unwrap_or_else(|e| fatal(&format!("the OS's randomness failed: {e:?}")));
        u64::from_le_bytes(bytes)
    })
}

/// A number distinct per host process start (its world requests' `RequestId::boot`): the
/// wall clock's nanoseconds mixed with the process id.
fn host_boot() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos ^ (u64::from(std::process::id()) << 32)
}
