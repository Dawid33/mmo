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

pub mod lod_workers;
pub mod perf;
pub mod player_store;
pub mod region_store;
pub mod region_threads;
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
}

impl WorldIngress {
    pub fn new(
        ip: std::net::IpAddr,
        port: u16,
        webtransport: bool,
        players: player_store::SharedPlayerStore,
    ) -> Self {
        Self { ip, port, webtransport, players }
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
        let crypto = rustls::ServerConfig::builder()
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
            let id = next_client_id.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                info!("accepting connection");
                let connection = match conn.await {
                    Ok(connection) => connection,
                    Err(e) => return log::warn!("quic connection failed: {e}"),
                };
                // The handshake comes first: no session (and no sink) exists
                // until the connection's Hello has resolved to a player.
                let (player, record) = match quic_hello(&connection, &players).await {
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
) -> Result<(game::PlayerId, Option<game::PlayerRecord>), String> {
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
    player_store::resolve_hello(players, first).await
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

pub fn run() {
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
    game::perf::init("server");

    let (client_packet_send, client_packet_recv) = crossbeam::channel::unbounded();
    let (server_send, server_recv) = crossbeam::channel::unbounded();

    let single_player = std::env::var(SINGLE_PLAYER_ENV).is_ok_and(|v| v == "1");
    let configured_port = std::env::var(SERVER_PORT_ENV).ok();
    let port = server_port_from(configured_port.as_deref()).unwrap_or_else(|message| panic!("{message}"));
    let configured_bind = std::env::var(BIND_ADDR_ENV).ok();
    let ip = bind_ip_from(configured_bind.as_deref()).unwrap_or_else(|message| panic!("{message}"));
    // The world is chosen before anything calls `forged::active()` (below), and
    // before the ingress starts: its player store sits next to a forged world.
    let world_source = world_file::resolve(std::env::var(worldgen::forged::WORLD_FILE_ENV).ok().as_deref());
    let players: player_store::SharedPlayerStore = match &world_source {
        world_file::WorldSource::Procedural => player_store::shared(player_store::MemoryPlayerStore::default()),
        world_file::WorldSource::Forged(path) => {
            let dir = player_store::players_dir_for(&world_file::from_repo_root(path));
            let store = player_store::FilePlayerStore::open(dir.clone()).unwrap_or_else(|e| {
                panic!("cannot open the player store {}: {e} — move it away to start without players", dir.display())
            });
            info!("[server] players: {}", dir.display());
            player_store::shared(store)
        }
    };
    let mut rgi = WorldIngress::new(ip, port, webtransport_enabled(single_player), players.clone());
    let configured_worker_threads = std::env::var("TOKIO_WORKER_THREADS").ok();
    let worker_threads = server_worker_threads(configured_worker_threads.as_deref())
        .unwrap_or_else(|message| panic!("{message}"));
    let rt = build_server_runtime(worker_threads);
    let cps = client_packet_send.clone();
    // SIGINT / SIGTERM (`just run` stops its server with TERM) end the main
    // loop like a last client leaving does, so the world is saved on the way out.
    let stop_signal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    rt.spawn(watch_stop_signals(stop_signal.clone()));
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
    let mut history_world_path: Option<std::path::PathBuf> = None;
    match world_source {
        world_file::WorldSource::Procedural => {
            info!("[server] world: procedural");
            worldgen::forged::install(None);
        }
        world_file::WorldSource::Forged(path) => {
            let path = world_file::from_repo_root(&path);
            world_file::ensure_forged(&path, &worldgen::forge::ForgeParams::default())
                .unwrap_or_else(|e| panic!("{e}"));
            let world = worldgen::forged::ForgedWorld::load(&path).unwrap_or_else(|e| {
                panic!("{}: {e} — delete it to forge a new default world", path.display())
            });
            info!("[server] world: forged {}", path.display());
            worldgen::forged::install(Some(world));
            history_world_path = Some(path);
        }
    }
    // World autosave: forged worlds keep their regions next to the world file
    // (procedural worlds, like their missing history, keep nothing).
    let region_store = history_world_path.as_ref().map(|path| {
        let world = worldgen::forged::active().expect("a forged world is installed");
        let store = region_store::RegionStore::new(region_store::regions_dir_for(path), world.content_crc());
        info!("[server] region saves: {}", store.dir().display());
        store
    });
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
    // tick, continuing from the saved tick (procedural worlds: from 0, never saved).
    let start = std::time::Instant::now();
    let world_clock = game::WorldClock::starting_at(
        0,
        region_store.as_ref().and_then(|s| s.load_world_tick()).unwrap_or(0),
    );
    // Halo seeds of never-run neighbours: only their band columns, on the
    // starting region's thread (`game::halo`).
    let halo_generator: game::HaloGenerator = std::sync::Arc::new(move |rc, xs, zs| {
        worldgen::generate_columns(rc, xs, zs, world_seed, materials)
    });
    let mut spawner = region_threads::ThreadRegionSpawner::with_generator(region_generator, halo_generator)
        .with_clock(world_clock, start);
    let tick_store = region_store.clone();
    let save_world_tick = move |start: std::time::Instant| {
        if let Some(store) = &tick_store {
            let tick = world_clock.tick_at(start.elapsed().as_millis() as u64);
            if let Err(e) = store.save_world_tick(tick) {
                log::error!("[server] cannot save the world tick: {e}");
            }
        }
    };
    if let Some(store) = &region_store {
        spawner = spawner.with_store(store.clone());
    }
    let saver = region_store.map(region_store::spawn_saver);
    // Parked players and the autosave's online players go to the player store
    // on their own thread (procedural worlds: the in-memory store).
    let player_saver = player_store::spawn_saver(players.clone());
    let mut manager = game::WorldManager::new(
        spawner,
        Box::new(move |rc| gen_cache.generate_cached(rc, world_seed, materials)),
        server_send,
        region_out_send,
        lod_builder,
        lod_result_recv,
    );

    // The world map is the forged artifact's rasters summarised per region
    // tile — no region is generated; procedural worlds have none.
    if let Some(world) = worldgen::forged::active() {
        let built = std::time::Instant::now();
        let map = worldgen::tile_map::build_world_map(world);
        info!(
            "[server] world map: {}x{} tiles in {} ms",
            map.width,
            map.height,
            built.elapsed().as_millis()
        );
        // Forged worlds have a history next to the world file (procedural: none).
        if let Some(path) = &history_world_path {
            if let Some(history) = world_file::ensure_history(path, &map).unwrap_or_else(|e| panic!("{e}")) {
                manager.set_history(history, 0);
            }
        }
        manager.set_world_map(map);
    }

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
            default(std::time::Duration::from_millis(200)) => {}
        }
        let sessions = manager.depths().sessions;
        ever_connected |= sessions > 0;
        let gone = parent_gone.load(std::sync::atomic::Ordering::SeqCst);
        if let Some(why) = single_player_exit(single_player, ever_connected, sessions, gone) {
            info!("[server] single-player: {why}, exiting");
            break 'main;
        }
        if stop_signal.load(std::sync::atomic::Ordering::SeqCst) {
            info!("[server] stop signal: saving the world and exiting");
            break 'main;
        }
        manager.maintain(now_ms);
        manager.pump_lod_results();
        if last_world_save.elapsed() >= save_every {
            last_world_save = std::time::Instant::now();
            save_history(&history_world_path, &manager);
            save_world_tick(start);
            manager.request_save();
        }
        forward_saves(&saver, &mut manager);
        forward_player_saves(&player_saver.0, &mut manager);
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
    // `Shutdown`), then everything else, and join region threads.
    manager.park_all_players(start.elapsed().as_millis() as u64);
    manager.shutdown_all();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !manager.running_regions().is_empty() && std::time::Instant::now() < deadline {
        let now_ms = start.elapsed().as_millis() as u64;
        if let Ok((rc, output)) =
            region_out_recv.recv_timeout(std::time::Duration::from_millis(100))
        {
            manager.handle_region_output(rc, output, now_ms);
        }
        // A park queued for a region that was mid-stop woke it again.
        manager.shutdown_all();
    }
    let unparked = manager.running_regions();
    if !unparked.is_empty() {
        log::warn!("[server] {} regions did not park in time and are not saved: {unparked:?}", unparked.len());
    }
    // Every region that parked is saved (its blob since the last autosave).
    save_world_tick(start);
    manager.request_save();
    forward_saves(&saver, &mut manager);
    if let Some((send, handle)) = saver {
        drop(send);
        if handle.join().is_err() {
            log::error!("[server] the world saver panicked");
        }
    }
    // Every player parked: their records are written before the process ends.
    forward_player_saves(&player_saver.0, &mut manager);
    let (send, handle) = player_saver;
    drop(send);
    if handle.join().is_err() {
        log::error!("[server] the player saver panicked");
    }
    // Every region is parked, so its people are back in the history.
    save_history(&history_world_path, &manager);
    #[cfg(feature = "pyroscope")]
    if let Some(agent) = agent_running {
        let agent_ready = agent.stop().unwrap();
        agent_ready.shutdown();
    }
}

/// How often the main loop saves the world — the history and every region
/// (running regions' live state; parked ones once after they park). Both are
/// also saved after the orderly-exit loop has parked every region.
pub const WORLD_SAVE_SECS: u64 = 60;

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

/// Hand the manager's collected region states to the saver thread.
fn forward_saves<S: game::RegionSpawner>(
    saver: &Option<(
        crossbeam::channel::Sender<(game::RegionCoords, game::RegionSave)>,
        std::thread::JoinHandle<()>,
    )>,
    manager: &mut game::WorldManager<S>,
) {
    let saves = manager.take_saves();
    if let Some((send, _)) = saver {
        for save in saves {
            let _ = send.send(save);
        }
    }
}

/// Hand the manager's collected player records to the player-saver thread.
fn forward_player_saves<S: game::RegionSpawner>(
    send: &crossbeam::channel::Sender<player_store::PlayerSaves>,
    manager: &mut game::WorldManager<S>,
) {
    let saves = manager.take_player_saves();
    if !saves.is_empty() {
        let _ = send.send(saves);
    }
}

/// Sets `stop` on the first SIGINT / SIGTERM; a second one exits at once
/// (an orderly exit that hangs can still be interrupted).
async fn watch_stop_signals(stop: std::sync::Arc<std::sync::atomic::AtomicBool>) {
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

fn save_history<S: game::RegionSpawner>(
    path: &Option<std::path::PathBuf>,
    manager: &game::WorldManager<S>,
) {
    if let (Some(path), Some(history)) = (path, manager.history()) {
        world_file::save_history(path, history);
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
