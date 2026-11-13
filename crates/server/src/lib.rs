//! Server
// #![deny(missing_docs)]
use std::sync::{atomic::AtomicU64, Arc};

use crossbeam::channel::{Receiver, Sender};
use dashmap::DashMap;
use game::{ClientId, ClientPacket, ServerEvent, ServerPacket};
use log::{error, info};
use quinn::{
    crypto::rustls::QuicServerConfig,
    rustls::{
        self,
        pki_types::{CertificateDer, PrivatePkcs8KeyDer},
    },
    ConnectionError, Endpoint, TransportConfig,
};

/// Set to `1` by the client that launched this server for one world: once a
/// client has connected and the last one has left, the server exits. It also
/// exits when its stdin closes: the launching client holds the pipe's write
/// end, which the OS closes however the client dies (even before it ever
/// connected). So a single-player server needs its stdin to stay open —
/// don't start one with stdin at `/dev/null`.
pub const SINGLE_PLAYER_ENV: &str = "VOXEL_SINGLE_PLAYER";

/// `send` / `rebuild`: whether join snapshots carry stud collider grids or leave them
/// to the receiver (`game::region_wire`). Unset: `send` for a single-player server,
/// `rebuild` otherwise.
pub const SNAPSHOT_COLLIDERS_ENV: &str = "VOXEL_SNAPSHOT_COLLIDERS";

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

/// The WebTransport ingress's port (unset or empty = [`WEBTRANSPORT_PORT`]): several hosts
/// on one machine (`just dev-r3`) each need their own.
pub const WT_PORT_ENV: &str = "VOXEL_WT_PORT";

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

/// Whether a single-player server should exit now (parent still there).
#[cfg(test)]
fn should_exit_single_player(enabled: bool, ever_connected: bool, sessions: usize) -> bool {
    single_player_exit(enabled, ever_connected, sessions, false).is_some()
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
pub mod claims;
pub mod coordinator_rt;
pub mod cuts;
pub mod lod_workers;
pub mod login_link;
pub mod peer_link;
pub mod perf;
pub mod player_store;
pub mod region_threads;
pub mod roles;
pub mod webtransport;
pub mod world_file;

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
    /// The WebTransport ingress's self-signed certificate, minted before the ingress starts
    /// (its hash is in this host's endpoint); `None`: the ingress reads its source itself.
    wt_cert: Option<webtransport::SelfSignedCert>,
    /// This host's ordinal: which dev cert-hash files the ingress writes
    /// (`webtransport::cert_hash_file_names`).
    host_ordinal: u32,
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
            wt_cert: None,
            host_ordinal: 0,
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

    /// Serve WebTransport with `cert` (minted ahead, so its hash could be advertised).
    pub fn with_webtransport_cert(mut self, cert: Option<webtransport::SelfSignedCert>) -> Self {
        self.wt_cert = cert;
        self
    }

    /// Serve as host `ordinal` of the world (0 unless several hosts).
    pub fn with_host_ordinal(mut self, ordinal: u32) -> Self {
        self.host_ordinal = ordinal;
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
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_der = CertificateDer::from(cert.cert);
        let priv_key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
        // The provider is named, not left to rustls's process default: the
        // workspace's unified features enable both `ring` and `aws-lc-rs`
        // (the login service's AWS SDK), and then `builder()` panics.
        let crypto = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], priv_key.into())
            .unwrap();

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
            let minted = self.wt_cert.take();
            let ordinal = self.host_ordinal;
            let certs = crate::webtransport::cert_source_from(
                std::env::var(crate::webtransport::WT_CERT_ENV).ok().as_deref(),
                std::env::var(crate::webtransport::WT_KEY_ENV).ok().as_deref(),
            )
            .unwrap_or_else(|message| panic!("{message}"));
            tokio::spawn(async move {
                let served = match minted {
                    Some(cert) => {
                        crate::webtransport::serve_minted(wt_addr, cert, ordinal, wt_send, wt_sinks, wt_next, wt_players, wt_auth).await
                    }
                    None => crate::webtransport::serve(wt_addr, certs, wt_send, wt_sinks, wt_next, wt_players, wt_auth).await,
                };
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
                    // And the login service, for a session its spawn order admitted (with
                    // several hosts the manager tells it, once the session left the world).
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
    // `VOXEL_ROLE`: the whole world here (`all`), its regions only (`host`), or its
    // coordinator only (`coordinator`, no client edge, no regions).
    let role = roles::Role::from_env().unwrap_or_else(|e| fatal(&e));
    if role == roles::Role::Coordinator {
        game::perf::init("coordinator");
        info!("[server] role: coordinator");
        return roles::run_coordinator();
    }
    game::perf::init("server");
    info!("[server] role: {role:?}");

    let (client_packet_send, client_packet_recv) = crossbeam::channel::unbounded();
    let (server_send, server_recv) = crossbeam::channel::unbounded();

    let single_player = std::env::var(SINGLE_PLAYER_ENV).is_ok_and(|v| v == "1");
    // Join snapshots: a remote client rebuilds stud collider grids from the bricks
    // (20x fewer bytes, a genesis-sized rebuild per region); on loopback the bytes
    // are free and the rebuild is not, so a single-player server sends them.
    let send_colliders = match std::env::var(SNAPSHOT_COLLIDERS_ENV).as_deref() {
        Ok("send") => true,
        Ok("rebuild") => false,
        _ => single_player,
    };
    game::set_snapshot_sends_colliders(send_colliders);
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
    let mut opened = world_file::open_world(&world_source, &worldgen::forge::ForgeParams::default())
        .unwrap_or_else(|e| fatal(&e));
    let store = opened.store.clone();
    // A stored world is tied to stud terrain before any region is loaded or saved; one
    // saved as a voxel world does not start (its regions would come back without
    // terrain), and nothing is written to it.
    if let Some(store) = &store {
        world_file::bind_world_terrain(store).unwrap_or_else(|e| fatal(&e));
    }
    // Which regions this process simulates, and the super-region side that
    // decides it: a bad setting stops the start, naming the variable.
    let ownership = game::Ownership::from_env().unwrap_or_else(|e| fatal(&e));
    let super_region_regions = claims::super_region_regions_from(std::env::var(claims::SUPER_REGION_REGIONS_ENV).ok().as_deref())
        .unwrap_or_else(|e| fatal(&e));
    info!("[server] ownership: {ownership:?}, {super_region_regions} regions per super-region side");
    if role == roles::Role::Host && ownership == game::Ownership::AllInOne {
        log::warn!(
            "[server] role host with {} unset (all): this host claims every super-region — right for R2's one \
             host per world (`just host` sets static 0 of 1)",
            game::ownership::OWNERSHIP_ENV
        );
    }
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
    // Several hosts of one world (`VOXEL_HOSTS` > 1): only as `host`, on Postgres, with the
    // login link (ids and placement come from it).
    let hosts = match ownership {
        game::Ownership::Static { hosts, .. } => hosts,
        game::Ownership::AllInOne => 1,
    };
    let postgres = matches!(&world_source, world_file::WorldSource::Store { url: world_store::StoreUrl::Postgres(_), .. });
    roles::check_several_hosts(role, hosts, postgres, spawn_orders.is_some()).unwrap_or_else(|e| fatal(&e));
    // Several hosts: the login service is linked to one of them (the entry host); every host
    // relays its spawn orders and kicks to the host holding the player (`login_link`). Set
    // before anything connects (sessions are announced with their login).
    let entry_host = (hosts > 1)
        .then(|| login_link::entry_host_from(std::env::var(login_link::ENTRY_HOST_ENV).ok().as_deref(), hosts))
        .transpose()
        .unwrap_or_else(|e| fatal(&e));
    let login_relay = match (&spawn_orders, entry_host) {
        (Some((orders, _)), Some(entry)) => {
            info!("[server] login relay: the login service is linked to host {entry}");
            Some(login_link::LoginRelay::new(orders.clone(), players.clone(), client_packet_send.clone()))
        }
        _ => None,
    };
    // On Postgres, before it claims, registers or loads anything: the world cut parts a
    // crash left staged are finished first (several hosts: and two-phase commit must be
    // available).
    if let Some(store) = &store {
        roles::resolve_before_claim(postgres, store, hosts).unwrap_or_else(|e| fatal(&e));
    }
    // The WebTransport ingress's self-signed certificate is minted now, before this host's
    // endpoint is known to anyone: a browser pins its hash to open a connection here (a
    // certificate from files is a CA's: no hash).
    let wt_cert = match webtransport_enabled(single_player) {
        true => match webtransport::cert_source_from(
            std::env::var(webtransport::WT_CERT_ENV).ok().as_deref(),
            std::env::var(webtransport::WT_KEY_ENV).ok().as_deref(),
        )
        .unwrap_or_else(|e| fatal(&e))
        {
            webtransport::CertSource::SelfSigned => Some(
                webtransport::SelfSignedCert::mint()
                    .unwrap_or_else(|e| fatal(&format!("minting the webtransport certificate: {e}"))),
            ),
            webtransport::CertSource::Files { .. } => None,
        },
        false => None,
    };
    // Where players reach this host (`ServerPacket::Host`, its `hosts` row).
    let endpoint = host_endpoint(ownership.ordinal(), port, |name| std::env::var(name).ok(), wt_cert.as_ref());
    let mut authenticator = auth_edge::Authenticator::for_mode(auth_mode);
    if let Some((orders, _)) = &spawn_orders {
        authenticator = authenticator.with_spawn_orders(orders.clone());
    }
    // Several hosts: a client another host sends here (`RegionElsewhere`) connects with the
    // ticket that host vouched for; the main loop records its orders.
    let observer_orders = (hosts > 1).then(|| Arc::new(auth_edge::ObserverOrders::new(auth_edge::system_clock())));
    if let Some(orders) = &observer_orders {
        authenticator = authenticator.with_observer_orders(orders.clone());
    }
    let auth = Arc::new(authenticator);
    let mut rgi = WorldIngress::new(ip, port, webtransport_enabled(single_player), players.clone(), auth.clone())
        .with_webtransport_cert(wt_cert)
        .with_webtransport_port(wt_port)
        .with_host_ordinal(ownership.ordinal());
    let sinks = rgi.sinks();
    let configured_worker_threads = std::env::var("TOKIO_WORKER_THREADS").ok();
    let worker_threads = server_worker_threads(configured_worker_threads.as_deref())
        .unwrap_or_else(|message| panic!("{message}"));
    let rt = build_server_runtime(worker_threads);
    let cps = client_packet_send.clone();
    // The login service's push: a listener (dropped at the end of `run`, sync code).
    // Several hosts: only the entry host listens (the others take what it relays).
    let listens = entry_host.is_none_or(|entry| entry == ownership.ordinal());
    let _login_link = spawn_orders.filter(|_| listens).map(|(orders, token)| {
        let bind = login_link::bind_from(std::env::var(login_link::HOST_LINK_BIND_ENV).ok().as_deref())
            .unwrap_or_else(|e| fatal(&e));
        let pin_file =
            login_link::cert_hash_file_from(std::env::var(login_link::HOST_LINK_CERT_HASH_FILE_ENV).ok().as_deref());
        let (pin, events) = (Some(pin_file.clone()), client_packet_send.clone());
        let link = match entry_host {
            Some(_) => login_link::start_relaying(bind, token, pin, orders, players.clone(), events, ownership.ordinal()),
            None => login_link::start(bind, token, pin, orders, players.clone(), events),
        }
        .unwrap_or_else(|e| fatal(&format!("the login link on {bind}: {e}")));
        info!(
            "auth: login tickets need the login service's spawn orders, pushed to {} (pin {} in {})",
            link.local_addr(),
            link.cert_sha256(),
            pin_file.display()
        );
        link
    });
    // SIGINT / SIGTERM (`just run` stops its server with TERM) end the main
    // loop like a last client leaving does, so the world is saved on the way out.
    let stop_signal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    rt.spawn(watch_stop_signals(stop_signal.clone()));
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
    // The game's own prefabs, the same way: the repo directory, else the embedded copy.
    game::install_prefabs(
        game::load_builtin_prefabs(Some(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(game::BUILTIN_PREFAB_DIR),
        ))
        .expect("invalid built-in prefab"),
    );
    match &world_source {
        world_file::WorldSource::Store { url, .. } => {
            info!("[server] world: forged, in the world store {url} (sampled from its tiles)")
        }
        world_file::WorldSource::Procedural => info!("[server] world: procedural (nothing is saved)"),
        world_file::WorldSource::Studs => info!("[server] world: studs (toy bricks, nothing is saved)"),
        world_file::WorldSource::PrefabEditor => {
            info!("[server] world: prefab editor (an empty baseplate, nothing is saved)")
        }
    }
    // The world map was built when the world was written and is read from
    // the store with it (procedural worlds: none).
    let world_map = world_file::install_world(&mut opened);
    let world_seed = worldgen::WORLD_SEED;
    let configured_lod_threads = std::env::var("LOD_BUILD_THREADS").ok();
    let lod_threads = lod_workers::lod_build_threads(configured_lod_threads.as_deref())
        .unwrap_or_else(|message| panic!("{message}"));
    info!(
        "[server] lod workers: {lod_threads} ({})",
        if configured_lod_threads.is_some() { "LOD_BUILD_THREADS" } else { "default: cores / 4, clamped 2..=6" }
    );
    // The two keyword worlds of 3x3 hand-made regions: the toy-brick test world and the
    // prefab editor's baseplate.
    let editor_world = matches!(world_source, world_file::WorldSource::PrefabEditor);
    let toy_studs = editor_world || matches!(world_source, world_file::WorldSource::Studs);
    let lod_builder: Box<dyn game::LodBuilder> = if toy_studs {
        // The 3x3 toy world has nothing beyond its regions.
        Box::new(game::NullLodBuilder::default())
    } else {
        // Far terrain: height-field tiles from the ground recipe (`game::stud_lod`),
        // for the procedural and the stored worlds alike.
        Box::new(lod_workers::ThreadStudLodBuilder::new(world_seed, lod_threads))
    };
    // Kept only to read the manager→writer backlog for the `[server]` line.
    let writer_backlog = server_send.clone();
    // The world clock: ms since `start` (the manager's `now_ms` base) -> world
    // tick, continuing from the last committed cut's tick (procedural worlds:
    // from 0, never saved).
    // Split roles share one wall-anchored clock stored in the world row (R3), so the
    // base instant is back-dated by the clock's phase: tick edges fall at the same wall
    // instants in every process.
    let (world_clock, clock_phase) =
        roles::start_clock(role, store.as_ref(), postgres, opened.world_tick).unwrap_or_else(|e| fatal(&e));
    let start = roles::back_dated_start(clock_phase);
    // Region starts generate on their own thread: a fresh region's stud terrain
    // (`worldgen::stud_gen`), and the halo seeds of never-run neighbours — only their
    // band columns, trees and ice included (`generate_stud_window` gives the bricks
    // the whole region's genesis gives those columns).
    let mut spawner = region_threads::ThreadRegionSpawner::default().with_clock(world_clock, start);
    spawner = if editor_world {
        spawner.with_genesis(std::sync::Arc::new(worldgen::prefab_world::region_seed))
    } else if toy_studs {
        spawner.with_genesis(std::sync::Arc::new(worldgen::stud_world::region_seed))
    } else {
        spawner
            .with_stud_halo(std::sync::Arc::new(move |rc, xs, zs| worldgen::stud_gen::generate_stud_window(rc, xs, zs, world_seed)))
            .with_genesis(std::sync::Arc::new(move |rc| worldgen::stud_gen::generate_stud_region(rc, world_seed)))
    };
    // A stored world's genesis and halo seeds sample the forged tiles: each region
    // thread pins the tiles of its 3x3 neighbourhood before it is built, so those
    // reads never load, and one that will not load stops the region
    // (`RegionOutput::LoadFailed`) — never an empty seam. The store-less worlds have
    // no forged world to pin.
    if store.is_some() {
        spawner = spawner.with_prefetch(std::sync::Arc::new(worldgen::prefetch_region));
    }
    // Region loads and cut commits are fenced: the super-regions this process
    // owns are claimed now, once (procedural worlds keep nothing: no claims).
    // One claim is shared by the region threads and the cut writer.
    let boot = host_boot();
    let mut world_generation = claims::WorldGeneration::default();
    // Several hosts: the peer listener and a link to every other host (dropped at the end
    // of `run`, sync code).
    let mut peer_links: Option<peer_link::PeerLinks> = None;
    let claims = store.clone().map(|store| {
        let extent = world_map.as_ref().map(game::WorldMap::extent);
        let claimed = claims::HostClaims::claim(store, ownership, extent, super_region_regions).unwrap_or_else(|e| fatal(&e));
        // Only now, with every epoch bumped (any split host is fenced), may `all` forget the anchor.
        roles::clear_anchor_after_claim(role, Some(claimed.store()), postgres).unwrap_or_else(|e| fatal(&e));
        info!(
            "[server] claimed {} super-regions as host {}",
            claimed.fence().epochs.len(),
            claimed.fence().owner
        );
        // A host registers once per boot, after its claim (and, with several hosts, once its
        // peer listener is bound: the row names its address and pin): a boot after one that
        // never left clean raises the world's generation, which every other host then adopts.
        if role == roles::Role::Host {
            let fenced_exit = |e: &dyn std::fmt::Display| -> ! {
                // Replaced between the claim and the registration: fenced, not a start-up error.
                log::error!("[server] fenced while registering as host {}: {e}", ownership.ordinal());
                std::process::exit(claims::EXIT_FENCED)
            };
            let registered = if hosts > 1 {
                let config = peer_link::PeerLinkConfig::from_env(ownership.ordinal()).unwrap_or_else(|e| fatal(&e));
                let token =
                    svc_link::service_token_from_env().unwrap_or_else(|e| fatal(&format!("the service link's token: {e}")));
                let bind = config.bind;
                let links = peer_link::PeerLinks::start(
                    ownership.ordinal(),
                    hosts,
                    boot,
                    endpoint.clone(),
                    token,
                    claimed.store().clone(),
                    claimed.fence(),
                    config,
                )
                .unwrap_or_else(|e| {
                    if peer_link::is_fenced(&e) {
                        fenced_exit(&e)
                    }
                    fatal(&format!("the peer links on {bind}: {e}"))
                });
                let registered = links.registered();
                peer_links = Some(links);
                registered
            } else {
                match peer_link::register(claimed.store(), claimed.fence(), ownership.ordinal(), boot, &endpoint, None) {
                    Ok(registered) => registered,
                    Err(e @ world_store::StoreError::Fenced { .. }) => fenced_exit(&e),
                    Err(e) => fatal(&e.to_string()),
                }
            };
            info!(
                "[server] registered host {} at world generation {}{}",
                ownership.ordinal(),
                registered.generation,
                if registered.bumped { " (raised: the previous boot did not exit clean)" } else { "" }
            );
            world_generation = claims::WorldGeneration::new(registered.generation);
        }
        claimed
    });
    let fenced = claims.as_ref().map(|c| c.fenced().clone()).unwrap_or_default();
    if let Some(claims) = &claims {
        spawner = spawner.with_store(claims.clone());
    }
    let mut manager = game::WorldManager::new(spawner, server_send, region_out_send, lod_builder);
    manager.set_ownership(ownership, super_region_regions);
    manager.set_endpoints(endpoint.clone(), os_nonces());
    if let Some(entry) = entry_host {
        manager.set_entry_host(entry);
    }
    // The coordinator refuses an `Attach` of an older world generation than the store's.
    manager.set_host_generation(world_generation.mine());
    // A session per other host, over its link; the links' wake-up (none with one host).
    let mut peer_wake = crossbeam::channel::never::<()>();
    if let Some(links) = peer_links.as_mut() {
        peer_wake = links.wake();
        for (other, link) in links.take() {
            let hello = game::PeerHello {
                ordinal: ownership.ordinal(),
                boot,
                generation: world_generation.mine(),
                endpoint: endpoint.clone(),
                received_through: 0,
                received_from_boot: None,
            };
            manager.set_peer(other, game::PeerSession::new(hello, other), link);
        }
        // Before any placement: the hosts that left cleanly hold nobody (a spawn order does
        // not wait for them). A read that fails leaves every host expected (orders wait).
        if let Some(store) = &store {
            match peer_link::left_cleanly(store, ownership.ordinal()) {
                Ok(left) => manager.note_left_peers(left),
                Err(e) => log::warn!("[server] reading which hosts left cleanly: {e}; every host is awaited"),
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
            let epoch = claims.as_ref().map_or(0, |c| c.fence().claim_epoch());
            let link = host_world_link(ownership.ordinal(), boot, epoch).unwrap_or_else(|e| fatal(&e));
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
        // Several hosts: the coordinator picks every cut (a world cut, staged and decided
        // together); one host cuts on its own (the one-phase cut).
        if hosts > 1 {
            cuts::CutWriter::with_world_cuts(claims, world_generation.clone())
        } else {
            cuts::CutWriter::new(claims)
        }
    });

    // Several hosts: other hosts' saves re-seed the halos of their parked regions next to
    // this host's (`VOXEL_TERRAIN_NOTIFY`, `VOXEL_WORLD_DB_DIRECT`; one host: nothing to learn).
    let mut terrain_watch = match (&cuts, hosts > 1) {
        (Some(cuts), true) => {
            let mode = world_store::NotifyMode::from_env().unwrap_or_else(|e| fatal(&e));
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
    'main: loop {
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
            log::error!("[server] a host restarted dirty (a higher world generation than {}); stopping to restart from the same save", world_generation.mine());
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
        manager.maintain(now_ms);
        // Several hosts: the observer tickets other hosts vouched for since the last pass.
        observer_pass(&mut manager, observer_orders.as_deref(), &sinks);
        // The login relay (several hosts): spawn orders taken, kicks relayed, answers out.
        if let Some(relay) = &login_relay {
            relay.pump(&mut manager);
        }
        // A peer runs a newer world generation: this host is stale (polled above).
        if let Some(generation) = manager.take_higher_generation() {
            world_generation.note(generation);
        }
        manager.pump_lod_results();
        if let Some(cuts) = cuts.as_mut() {
            // Backpressure: no new cut while the store still commits one. With several hosts
            // only the coordinator starts cuts (`BeginCut`); while it is down there are none.
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
        report.maybe_report(
            &mut manager,
            perf::Backlog {
                client_events: client_packet_recv.len(),
                region_outputs: region_out_recv.len(),
                writer: writer_backlog.len(),
            },
            store.as_ref(),
        );
    }

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
        |_| {},
    );
    if matches!(outcome, claims::ServerExit::Fenced | claims::ServerExit::WorldRolledBack) && had_sessions {
        // The refusals are on their way out through the writer task.
        std::thread::sleep(FENCED_EXIT_FLUSH);
    }
    #[cfg(feature = "pyroscope")]
    if let Some(agent) = agent_running {
        let agent_ready = agent.stop().unwrap();
        agent_ready.shutdown();
    }
    outcome
}

/// Where players reach host `ordinal` (`ServerPacket::Host`, its `hosts` row): the
/// advertised addresses ([`peer_link::ADVERTISE_QUIC_ENV`], [`peer_link::ADVERTISE_WT_ENV`]
/// through `lookup`) and the hash of the WebTransport ingress's self-signed certificate
/// (`wt_cert`, minted before this is called), which a browser pins.
fn host_endpoint(
    ordinal: u32,
    port: u16,
    lookup: impl Fn(&str) -> Option<String>,
    wt_cert: Option<&webtransport::SelfSignedCert>,
) -> game::HostEndpoint {
    game::HostEndpoint {
        ordinal,
        quic: Some(peer_link::advertise_quic_from(lookup(peer_link::ADVERTISE_QUIC_ENV).as_deref(), port)),
        webtransport: peer_link::advertise_wt_from(lookup(peer_link::ADVERTISE_WT_ENV).as_deref()),
        cert_hash: wt_cert.map(|c| c.sha256_hex().to_string()),
    }
}

/// The main loop's pass over the manager's observer bookkeeping (after `maintain`): the
/// orders other hosts sent go to the edge's book (several hosts: `orders`), and the
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
    use super::*;

    #[test]
    fn world_save_period_from_env() {
        assert_eq!(world_save_secs(None), WORLD_SAVE_SECS);
        assert_eq!(world_save_secs(Some("")), WORLD_SAVE_SECS);
        assert_eq!(world_save_secs(Some("5")), 5);
        assert_eq!(world_save_secs(Some("0")), WORLD_SAVE_SECS, "0 would save every loop");
        assert_eq!(world_save_secs(Some("soon")), WORLD_SAVE_SECS);
    }

    #[test]
    fn single_player_exits_only_after_last_client_leaves() {
        assert!(!should_exit_single_player(false, true, 0));
        assert!(!should_exit_single_player(true, false, 0));
        assert!(!should_exit_single_player(true, true, 1));
        assert!(should_exit_single_player(true, true, 0));
    }

    #[test]
    fn single_player_exits_when_the_parent_is_gone() {
        assert_eq!(single_player_exit(true, false, 0, true), Some("parent gone"), "before any connect");
        assert_eq!(single_player_exit(true, true, 1, true), Some("parent gone"), "with a client");
        assert_eq!(single_player_exit(false, false, 0, true), None, "not single-player");
        assert_eq!(single_player_exit(true, true, 0, false), Some("last client left"));
        assert_eq!(single_player_exit(true, false, 0, false), None);
    }

    #[test]
    fn single_player_parent_watch_fires_on_eof() {
        let gone = std::sync::atomic::AtomicBool::new(false);
        watch_parent(&b"anything"[..], &gone);
        assert!(gone.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn bind_address_defaults_to_loopback() {
        assert_eq!(bind_ip_from(None), Ok(std::net::IpAddr::from([127, 0, 0, 1])));
        assert_eq!(bind_ip_from(Some(" ")), Ok(std::net::IpAddr::from([127, 0, 0, 1])));
        assert_eq!(bind_ip_from(Some("0.0.0.0")), Ok(std::net::IpAddr::from([0, 0, 0, 0])));
        assert!(bind_ip_from(Some("everywhere")).is_err());
    }

    #[test]
    fn server_port_defaults_to_6466() {
        assert_eq!(server_port_from(None), Ok(DEFAULT_SERVER_PORT));
        assert_eq!(server_port_from(Some("")), Ok(DEFAULT_SERVER_PORT));
        assert_eq!(DEFAULT_SERVER_PORT, 6466);
    }

    #[test]
    fn server_port_accepts_an_override() {
        assert_eq!(server_port_from(Some("40123")), Ok(40123));
        assert_eq!(server_port_from(Some(" 7000 ")), Ok(7000));
    }

    #[test]
    fn server_port_rejects_invalid_values() {
        for bad in ["0", "65536", "port", "-1"] {
            let err = server_port_from(Some(bad)).unwrap_err();
            assert!(err.contains(SERVER_PORT_ENV), "{err}");
            assert!(err.contains(bad), "{err}");
        }
    }

    #[test]
    fn webtransport_port_defaults_to_6467_and_takes_an_override() {
        assert_eq!(wt_port_from(None), Ok(WEBTRANSPORT_PORT));
        assert_eq!(wt_port_from(Some(" ")), Ok(WEBTRANSPORT_PORT));
        assert_eq!(wt_port_from(Some("6477")), Ok(6477));
        for bad in ["0", "65536", "port"] {
            let err = wt_port_from(Some(bad)).unwrap_err();
            assert!(err.contains(WT_PORT_ENV) && err.contains(bad), "{err}");
        }
    }

    #[test]
    fn endpoint_carries_the_minted_webtransport_cert_hash() {
        let cert = webtransport::SelfSignedCert::mint().unwrap();
        let hex = cert.sha256_hex().to_string();
        assert_eq!(hex.len(), 64, "{hex}");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{hex}");
        let wt = |name: &str| (name == peer_link::ADVERTISE_WT_ENV).then(|| "https://h.example:6467".to_string());
        let endpoint = host_endpoint(3, 7000, wt, Some(&cert));
        assert_eq!(endpoint.ordinal, 3);
        assert_eq!(endpoint.cert_hash.as_deref(), Some(hex.as_str()));
        assert_eq!(endpoint.webtransport.as_deref(), Some("https://h.example:6467"));
        assert_eq!(host_endpoint(3, 7000, |_| None, None).cert_hash, None, "a certificate from files: no hash");
    }

    #[test]
    fn observer_nonces_come_from_the_os() {
        let (mut a, mut b) = (os_nonces(), os_nonces());
        let drawn: std::collections::BTreeSet<u64> = (0..1000).map(|_| a()).collect();
        assert_eq!(drawn.len(), 1000, "no repeats");
        // Not a seeded sequence: two sources (as two hosts, or two boots) do not agree.
        assert_ne!((a(), a()), (b(), b()));
    }

    #[test]
    fn single_player_has_no_webtransport_ingress() {
        assert!(webtransport_enabled(false));
        assert!(!webtransport_enabled(true));
    }

    #[test]
    fn server_worker_threads_default_runtime_has_four_workers() {
        let worker_threads = server_worker_threads(None).unwrap();
        let runtime = build_server_runtime(worker_threads);
        assert_eq!(runtime.metrics().num_workers(), 4);
    }

    #[test]
    fn server_worker_threads_accept_valid_override() {
        assert_eq!(server_worker_threads(Some("2")), Ok(2));
    }

    #[test]
    fn server_worker_threads_reject_invalid_values() {
        let zero = server_worker_threads(Some("0")).unwrap_err();
        assert!(zero.contains("TOKIO_WORKER_THREADS"));
        assert!(zero.contains("positive integer"));

        let malformed = server_worker_threads(Some("many")).unwrap_err();
        assert!(malformed.contains("TOKIO_WORKER_THREADS"));
        assert!(malformed.contains("positive integer"));
    }
}

/// A host's link to the world coordinator process (`VOXEL_ROLE=host`): it dials
/// `VOXEL_COORDINATOR_ADDR` with the service token, pinning the coordinator's
/// certificate (`VOXEL_COORDINATOR_CERT_SHA256`, else the cert-hash file — waited for
/// if the coordinator has not written it yet).
/// `epoch`: the host's claim epoch ([`world_store::Fence::claim_epoch`]), in its hello: the
/// coordinator keeps one connection per ordinal, the highest epoch's.
fn host_world_link(ordinal: u32, boot: u64, epoch: u64) -> Result<roles::QuicWorldLink, String> {
    let addr = roles::coordinator_addr_from(
        roles::COORDINATOR_ADDR_ENV,
        std::env::var(roles::COORDINATOR_ADDR_ENV).ok().as_deref(),
    )?;
    let token = svc_link::service_token_from_env().map_err(|e| format!("the service link's token: {e}"))?;
    let cert_file = roles::cert_hash_file_from(std::env::var(roles::COORDINATOR_CERT_HASH_FILE_ENV).ok().as_deref());
    let hello = svc_link::LinkHello::new(token, svc_link::PeerRole::Host { ordinal, boot, epoch });
    info!("[server] the world coordinator is a separate process at {addr} (pin from {})", cert_file.display());
    Ok(roles::QuicWorldLink::connect(
        addr,
        move || svc_link::read_cert_hash(roles::COORDINATOR_CERT_SHA256_ENV, &cert_file),
        hello,
    ))
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
