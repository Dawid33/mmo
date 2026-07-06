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

pub mod region_threads;
pub mod webtransport;

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
        let priv_key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
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
            async fn send_bounded(sink: ClientSink, packet: Vec<u8>) {
                let deadline = std::time::Duration::from_secs(2);
                if tokio::time::timeout(deadline, sink.send_packet(packet))
                    .await
                    .is_err()
                {
                    log::warn!("send to client timed out; skipping (peer stalled or gone)");
                }
            }

            while let Ok((target, event)) = server_recv.recv() {
                let packet = bincode::serialize(&event).unwrap();
                match target {
                    // Directed packet: if the client is gone, drop it.
                    Some(id) => {
                        let sink = writer_sinks.get(&id).map(|e| e.value().clone());
                        if let Some(sink) = sink {
                            send_bounded(sink, packet.clone()).await;
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
    // Debug builds keep per-transaction hash self-verification (the rollback
    // bar); release skips the O(state) walk — state restore is identical.
    #[cfg(not(debug_assertions))]
    game::set_hash_verification(false);

    // use pyroscope::PyroscopeAgent;
    // use pyroscope_pprofrs::{pprof_backend, PprofConfig};
    // let agent = PyroscopeAgent::builder("http://localhost:4040", "server")
    //     .backend(pprof_backend(PprofConfig::new().sample_rate(100)))
    //     .build()
    //     .unwrap();
    // let agent_running = agent.start().unwrap();

    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default()).unwrap();

    let (client_packet_send, client_packet_recv) = crossbeam::channel::unbounded();
    let (server_send, server_recv) = crossbeam::channel::unbounded();

    let mut rgi = WorldIngress::new();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
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
    let stone = registry.id_of("stone").expect("block manifest must define \"stone\"");
    let mut manager = game::WorldManager::new(
        region_threads::ThreadRegionSpawner::default(),
        Box::new(move |rc| worldgen::generate_region(rc, stone)),
        server_send,
        region_out_send,
    );

    let start = std::time::Instant::now();
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
    }

    // Orderly exit: park everything, join region threads.
    manager.shutdown_all();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !manager.running_regions().is_empty() && std::time::Instant::now() < deadline {
        let now_ms = start.elapsed().as_millis() as u64;
        if let Ok((rc, output)) = region_out_recv.recv_timeout(std::time::Duration::from_millis(100)) {
            manager.handle_region_output(rc, output, now_ms);
        }
    }
    // let agent_ready = agent_running.stop().unwrap();
    // agent_ready.shutdown();
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
            let packet = bincode::deserialize(&req[..]);
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
