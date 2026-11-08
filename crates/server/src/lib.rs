//! Server
// #![deny(missing_docs)]
use std::sync::{atomic::AtomicUsize, atomic::Ordering, Arc};

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
pub mod perf;
pub mod player_store;
pub mod region_threads;
pub mod roles;
pub mod webtransport;
pub mod world_file;

/// One connected client's outgoing half, transport-agnostic. The router
/// writes bincode ServerPackets; each variant frames them as one
/// uni-stream per packet.
#[derive(Clone)]
pub enum ClientSink {
    Quinn(quinn::Connection),
    Web(wtransport::Connection),
}

impl ClientSink {
    /// Send one packet; a failed/vanished client is logged and skipped so a
    /// single dead connection can't kill the shared writer task.
    pub async fn send_packet(&self, packet: Vec<u8>) {
        match self {
            ClientSink::Quinn(conn) => {
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
            ClientSink::Web(session) => {
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
        match self {
            ClientSink::Quinn(conn) => conn.close(1u32.into(), b"refused"),
            ClientSink::Web(session) => session.close(wtransport::VarInt::from_u32(1), b"refused"),
        }
    }
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
    /// Whether the WebTransport ingress (`ip`:[`WEBTRANSPORT_PORT`]) runs.
    webtransport: bool,
    /// Resolves each connection's `Hello` token before its session starts.
    players: player_store::SharedPlayerStore,
    /// Checks each `Hello`'s credential first (`VOXEL_AUTH`).
    auth: Arc<auth_edge::Authenticator>,
}

impl WorldIngress {
    pub fn new(
        ip: std::net::IpAddr,
        port: u16,
        webtransport: bool,
        players: player_store::SharedPlayerStore,
        auth: Arc<auth_edge::Authenticator>,
    ) -> Self {
        Self { ip, port, webtransport, players, auth }
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

        let sinks: Arc<DashMap<ClientId, ClientSink>> = Arc::new(DashMap::new());
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
                        let sink = writer_sinks.get(&id).map(|e| e.value().clone());
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
                            writer_sinks.iter().map(|e| e.value().clone()).collect();
                        for sink in targets {
                            send_bounded(sink, packet.clone()).await;
                        }
                    }
                }
            }
        });

        let next_client_id = Arc::new(AtomicUsize::new(0));

        if self.webtransport {
            let wt_send = send.clone();
            let wt_sinks = sinks.clone();
            let wt_next = next_client_id.clone();
            let wt_players = self.players.clone();
            let wt_auth = self.auth.clone();
            let wt_addr = std::net::SocketAddr::new(self.ip, WEBTRANSPORT_PORT);
            let certs = crate::webtransport::cert_source_from(
                std::env::var(crate::webtransport::WT_CERT_ENV).ok().as_deref(),
                std::env::var(crate::webtransport::WT_KEY_ENV).ok().as_deref(),
            )
            .unwrap_or_else(|message| panic!("{message}"));
            tokio::spawn(async move {
                if let Err(e) = crate::webtransport::serve(
                    wt_addr,
                    certs,
                    wt_send,
                    wt_sinks,
                    wt_next,
                    wt_players,
                    wt_auth,
                )
                .await
                {
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
            let id = next_client_id.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                info!("accepting connection");
                let connection = match conn.await {
                    Ok(connection) => connection,
                    Err(e) => return log::warn!("quic connection failed: {e}"),
                };
                // The handshake comes first: no session (and no sink) exists
                // until the connection's Hello has resolved to a player.
                let player_store::HelloSession { player, record, ticket } =
                    match quic_hello(&connection, &players, &auth).await {
                        Ok(resolved) => resolved,
                        Err(why) => return refuse_quic(&connection, &why).await,
                    };
                game::perf_event!("client_connected", "client={id} player={} transport=quic", player.0);
                sinks.insert(id, ClientSink::Quinn(connection.clone()));
                // Announce before any of this client's other packets can be
                // read: guarantees the player exists before its first request
                // is served.
                send.send(ServerEvent::ClientConnected { client: id, player, record }).unwrap();

                let fut = handle_connection(connection, send.clone(), id);
                tokio::spawn(async move {
                    if let Err(e) = fut.await {
                        error!("connection failed: {reason}", reason = e.to_string())
                    }
                    sinks.remove(&id);
                    // The world must know: unsubscribe everywhere, let the
                    // home region's grace timer start.
                    let _ = send.send(ServerEvent::ClientDisconnected(id));
                    // And the login service, for a session its spawn order admitted.
                    if let Some(ticket) = &ticket {
                        auth.session_ended(ticket);
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
    let configured_port = std::env::var(SERVER_PORT_ENV).ok();
    let port = server_port_from(configured_port.as_deref()).unwrap_or_else(|message| panic!("{message}"));
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
    // R2a's newest-epoch check (registry worlds without a `host_link`: `just dev-login`).
    let spawn_orders = match &auth_mode {
        auth_edge::AuthMode::Jwt { .. } => {
            match login_link::service_token_if_configured(|name| std::env::var(name).ok()).unwrap_or_else(|e| fatal(&e)) {
                Some(token) => Some((Arc::new(login_link::SpawnOrders::new(auth_edge::system_clock())), token)),
                None => {
                    info!("auth: no service token, so no login link: tickets are admitted by newest epoch (R2a)");
                    None
                }
            }
        }
        auth_edge::AuthMode::Local => None,
    };
    let mut authenticator = auth_edge::Authenticator::for_mode(auth_mode);
    if let Some((orders, _)) = &spawn_orders {
        authenticator = authenticator.with_spawn_orders(orders.clone());
    }
    let auth = Arc::new(authenticator);
    let mut rgi = WorldIngress::new(ip, port, webtransport_enabled(single_player), players.clone(), auth.clone());
    let configured_worker_threads = std::env::var("TOKIO_WORKER_THREADS").ok();
    let worker_threads = server_worker_threads(configured_worker_threads.as_deref())
        .unwrap_or_else(|message| panic!("{message}"));
    let rt = build_server_runtime(worker_threads);
    let cps = client_packet_send.clone();
    // The login service's push: a listener (dropped at the end of `run`, sync code).
    let _login_link = spawn_orders.map(|(orders, token)| {
        let bind = login_link::bind_from(std::env::var(login_link::HOST_LINK_BIND_ENV).ok().as_deref())
            .unwrap_or_else(|e| fatal(&e));
        let pin_file =
            login_link::cert_hash_file_from(std::env::var(login_link::HOST_LINK_CERT_HASH_FILE_ENV).ok().as_deref());
        let link = login_link::start(bind, token, Some(pin_file.clone()), orders, players.clone(), client_packet_send.clone())
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
    let resolve = |name: &str| {
        registry
            .id_of(name)
            .unwrap_or_else(|| panic!("block manifest must define \"{name}\""))
    };
    let materials = worldgen::material::Materials::from_names(resolve);
    match &world_source {
        world_file::WorldSource::Store { url, .. } => {
            info!("[server] world: forged, in the world store {url:?} (sampled from its tiles)")
        }
        world_file::WorldSource::Procedural => info!("[server] world: procedural (nothing is saved)"),
    }
    // The world map was built when the world was written and is read from
    // the store with it (procedural worlds: none).
    let world_map = world_file::install_world(&mut opened);
    let world_seed = worldgen::WORLD_SEED;
    let gen_cache = worldgen::cache::WorldgenCache::from_env(world_seed, &manifest);
    let (lod_result_send, lod_result_recv) = crossbeam::channel::unbounded();
    let configured_lod_threads = std::env::var("LOD_BUILD_THREADS").ok();
    let lod_threads = lod_workers::lod_build_threads(configured_lod_threads.as_deref())
        .unwrap_or_else(|message| panic!("{message}"));
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
    // Region starts generate on their own thread; the manager's copy of the
    // generator is only reached by paths that stay synchronous by design.
    let region_gen_cache = gen_cache.clone();
    let region_generator: region_threads::SharedRegionGenerator =
        std::sync::Arc::new(move |rc| region_gen_cache.generate_cached(rc, world_seed, materials));
    // The world clock: ms since `start` (the manager's `now_ms` base) -> world
    // tick, continuing from the last committed cut's tick (procedural worlds:
    // from 0, never saved).
    let start = std::time::Instant::now();
    let world_clock = game::WorldClock::starting_at(0, opened.world_tick);
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
    let mut spawner = region_threads::ThreadRegionSpawner::with_generator(region_generator, halo_generator)
        .with_prefetch(std::sync::Arc::new(worldgen::prefetch_region))
        .with_clock(world_clock, start);
    // Region loads and cut commits are fenced: the super-regions this process
    // owns are claimed now, once (procedural worlds keep nothing: no claims).
    // One claim is shared by the region threads and the cut writer.
    let claims = store.clone().map(|store| {
        let extent = world_map.as_ref().map(game::WorldMap::extent);
        let claimed = claims::HostClaims::claim(store, ownership, extent, super_region_regions).unwrap_or_else(|e| fatal(&e));
        info!(
            "[server] claimed {} super-regions as host {}",
            claimed.fence().epochs.len(),
            claimed.fence().owner
        );
        claimed
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
            let boot = host_boot();
            let link = host_world_link(ownership.ordinal(), boot).unwrap_or_else(|e| fatal(&e));
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
        let in_flight = store.load_in_flight().unwrap_or_else(|e| fatal(&e.to_string()));
        info!(
            "[server] world store: {} stored regions, {} crossings / intents in flight, world tick {}",
            stored.len(),
            in_flight.len(),
            opened.world_tick
        );
        manager.set_stored_regions(stored);
        manager.restore_in_flight(in_flight);
        cuts::CutWriter::new(claims)
    });

    let mut report = perf::ServerReport::new();
    let mut ever_connected = false;
    let mut last_world_save = std::time::Instant::now();
    let save_every = std::time::Duration::from_secs(world_save_secs(std::env::var(WORLD_SAVE_SECS_ENV).ok().as_deref()));
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
        if coordinator.as_ref().is_some_and(coordinator_rt::CoordinatorThread::is_finished) {
            log::error!("[server] the world coordinator thread stopped; stopping");
            break 'main;
        }
        if stop_signal.load(std::sync::atomic::Ordering::SeqCst) {
            info!("[server] stop signal: saving the world and exiting");
            break 'main;
        }
        manager.maintain(now_ms);
        manager.pump_lod_results();
        if let Some(cuts) = cuts.as_mut() {
            // Backpressure: no new cut while the store still commits one.
            if last_world_save.elapsed() >= save_every && cuts.in_flight() == 0 && !fenced.is_set() {
                last_world_save = std::time::Instant::now();
                // Regions, players and what is in flight, at one
                // world tick a lead ahead (`save_cut.rs`).
                manager.begin_cut(world_clock.tick_at(start.elapsed().as_millis() as u64));
            }
            cuts.pump(&mut manager);
        }
        report.maybe_report(
            &mut manager,
            perf::Backlog {
                client_events: client_packet_recv.len(),
                region_outputs: region_out_recv.len(),
                writer: writer_backlog.len(),
            },
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
            park_timeout: cuts::EXIT_PARK_TIMEOUT,
            save_timeout: EXIT_SAVE_TIMEOUT,
            now_tick: || world_clock.tick_at(start.elapsed().as_millis() as u64),
        },
        |_| {},
    );
    if outcome == claims::ServerExit::Fenced && had_sessions {
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
) -> Result<(), ConnectionError> {
    loop {
        let send = send.clone();
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
            send.send(ServerEvent::ClientPacket(packet, id)).unwrap();
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
fn host_world_link(ordinal: u32, boot: u64) -> Result<roles::QuicWorldLink, String> {
    let addr = roles::coordinator_addr_from(
        roles::COORDINATOR_ADDR_ENV,
        std::env::var(roles::COORDINATOR_ADDR_ENV).ok().as_deref(),
    )?;
    let token = svc_link::service_token_from_env().map_err(|e| format!("the service link's token: {e}"))?;
    let cert_file = roles::cert_hash_file_from(std::env::var(roles::COORDINATOR_CERT_HASH_FILE_ENV).ok().as_deref());
    let hello = svc_link::LinkHello::new(token, svc_link::PeerRole::Host { ordinal, boot });
    info!("[server] the world coordinator is a separate process at {addr} (pin from {})", cert_file.display());
    Ok(roles::QuicWorldLink::connect(
        addr,
        move || svc_link::read_cert_hash(roles::COORDINATOR_CERT_SHA256_ENV, &cert_file),
        hello,
    ))
}

/// A number distinct per host process start (its world requests' `RequestId::boot`): the
/// wall clock's nanoseconds mixed with the process id.
fn host_boot() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos ^ (u64::from(std::process::id()) << 32)
}
