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
pub struct WorldIngress {}

impl WorldIngress {
    pub fn new() -> Self {
        Self {}
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
        let endpoint = Endpoint::server(config, "127.0.0.1:6466".parse().unwrap()).unwrap();

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

            while let Ok((target, event)) = server_recv.recv() {
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

        let wt_send = send.clone();
        let wt_sinks = sinks.clone();
        let wt_next = next_client_id.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::webtransport::serve(
                "127.0.0.1:6467".parse().unwrap(),
                wt_send,
                wt_sinks,
                wt_next,
            )
            .await
            {
                log::error!("webtransport ingress died: {e:?}");
            }
        });

        while let Some(conn) = endpoint.accept().await {
            let send = send.clone();
            let sinks = sinks.clone();
            let id = next_client_id.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                info!("accepting connection");
                let connection = conn.await.unwrap();
                game::perf_event!("client_connected", "client={id} transport=quic");
                sinks.insert(id, ClientSink::Quinn(connection.clone()));
                // Announce before any of this client's packets can be read:
                // guarantees the player exists before its first request is
                // served.
                send.send(ServerEvent::ClientConnected(id)).unwrap();

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

pub fn run() {
    game::perf::pin_origin();
    // Per-entry verification is off by default (desync is caught by the
    // periodic tick-interval state hash); HASH_MODE=delta re-enables
    // footprint-digest self-checks, HASH_MODE=full whole-field verification
    // when chasing a footprint/capture gap. Tests and the convergence
    // harnesses pin HashMode::Full themselves.
    game::set_hash_mode_from_env();

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

    let mut rgi = WorldIngress::new();
    let configured_worker_threads = std::env::var("TOKIO_WORKER_THREADS").ok();
    let worker_threads = server_worker_threads(configured_worker_threads.as_deref())
        .unwrap_or_else(|message| panic!("{message}"));
    let rt = build_server_runtime(worker_threads);
    let cps = client_packet_send.clone();
    std::thread::spawn(move || rt.block_on(async move { rgi.listen(cps, server_recv).await }));

    let (region_out_send, region_out_recv) = crossbeam::channel::unbounded();
    // Block registry: prefer the runtime manifest, fall back to the copy
    // embedded at build time (same repo file) so headless runs never fail.
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/blocks/blocks.ron"),
    )
    .unwrap_or_else(|_| include_str!("../../../assets/blocks/blocks.ron").to_string());
    let registry = game::BlockRegistry::from_ron(&manifest).expect("invalid block manifest");
    let resolve = |name: &str| {
        registry
            .id_of(name)
            .unwrap_or_else(|| panic!("block manifest must define \"{name}\""))
    };
    let materials = worldgen::material::Materials::from_names(resolve);
    // The world must be chosen before anything calls `forged::active()`.
    match world_file::resolve(std::env::var(worldgen::forged::WORLD_FILE_ENV).ok().as_deref()) {
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
        }
    }
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
    let mut manager = game::WorldManager::new(
        region_threads::ThreadRegionSpawner::with_generator(region_generator),
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
        manager.set_world_map(map);
    }

    let start = std::time::Instant::now();
    let mut report = perf::ServerReport::new();
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
        manager.maintain(now_ms);
        manager.pump_lod_results();
        report.maybe_report(
            &mut manager,
            perf::Backlog {
                client_events: client_packet_recv.len(),
                region_outputs: region_out_recv.len(),
                writer: writer_backlog.len(),
            },
        );
    }

    // Orderly exit: park everything, join region threads.
    manager.shutdown_all();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !manager.running_regions().is_empty() && std::time::Instant::now() < deadline {
        let now_ms = start.elapsed().as_millis() as u64;
        if let Ok((rc, output)) =
            region_out_recv.recv_timeout(std::time::Duration::from_millis(100))
        {
            manager.handle_region_output(rc, output, now_ms);
        }
    }
    #[cfg(feature = "pyroscope")]
    if let Some(agent) = agent_running {
        let agent_ready = agent.stop().unwrap();
        agent_ready.shutdown();
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
