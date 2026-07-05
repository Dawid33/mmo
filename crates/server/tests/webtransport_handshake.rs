//! End-to-end: a wtransport client (the same library the browser-facing
//! endpoint uses) performs the full join handshake against the real router
//! loop, proving Region snapshots and GameEvent echoes flow over WebTransport.
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use game::{ChunkCoords, ClientPacket, ServerPacket};
use server::{webtransport, ClientSink, ServerEvent};

async fn recv_packet(conn: &wtransport::Connection) -> ServerPacket {
    let mut stream = conn.accept_uni().await.expect("server closed stream");
    let mut buf = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf)
        .await
        .expect("read failed");
    bincode::deserialize(&buf).expect("bad packet")
}

async fn send_packet(conn: &wtransport::Connection, packet: &ClientPacket) {
    let mut stream = conn.open_uni().await.unwrap().await.unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut stream, &bincode::serialize(packet).unwrap())
        .await
        .unwrap();
    stream.finish().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn webtransport_client_full_handshake() {
    let (event_send, event_recv) = crossbeam::channel::unbounded();
    let (server_send, server_recv) = crossbeam::channel::unbounded();
    let sinks: Arc<DashMap<usize, ClientSink>> = Arc::new(DashMap::new());
    let next_id = Arc::new(AtomicUsize::new(0));

    // Writer task (Task 1's) — drains router output into sinks.
    let writer_sinks = sinks.clone();
    tokio::spawn(async move {
        while let Ok((target, event)) = server_recv.recv() {
            let packet = bincode::serialize(&event).unwrap();
            match target {
                Some(id) => {
                    let sink = writer_sinks.get(&id).map(|e| e.value().clone());
                    if let Some(sink) = sink {
                        sink.send_packet(packet.clone()).await;
                    }
                }
                None => {
                    for entry in writer_sinks.iter() {
                        entry.value().send_packet(packet.clone()).await;
                    }
                }
            }
        }
    });

    // WebTransport ingress on an ephemeral-ish test port.
    let bind = "127.0.0.1:16467".parse().unwrap();
    let ingress_sinks = sinks.clone();
    tokio::spawn(async move {
        webtransport::serve(bind, event_send, ingress_sinks, next_id)
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(300)).await; // endpoint up + hash file written

    // Router loop on a plain thread, same shape as server::run's tail.
    // (No results_buffer here, unlike server::run's tail: ServerTickTimer
    // is a no-op in this test — nothing ever drives progress_world_one_tick,
    // so the buffer the brief sketched would be dead and fail to type-infer.)
    std::thread::spawn(move || {
        let mut world = game::World::basic();
        while let Ok(event) = event_recv.recv() {
            match event {
                ServerEvent::ClientPacket(packet, client_id) => match packet {
                    ClientPacket::RequestPlayerRegion => {
                        let id = world.find_player(&client_id);
                        server_send
                            .send((Some(client_id), ServerPacket::PlayerRegion(id, client_id)))
                            .unwrap();
                    }
                    ClientPacket::RequestRegionConnection(id) => {
                        server_send
                            .send((Some(client_id), world.build_region_server_packet(&id)))
                            .unwrap();
                    }
                    ClientPacket::GameEvent(ev) => {
                        let out = world.handle_region_event(ev.kind, ev.region_id).unwrap();
                        world.forget_last_event(&ev.region_id);
                        server_send.send((None, ServerPacket::GameEvent(out))).unwrap();
                    }
                },
                ServerEvent::ClientConnected(client_id) => {
                    if world.find_player(&client_id).is_none() {
                        let region = ChunkCoords::new(0, 0, 0);
                        let ev = world
                            .handle_region_event(
                                game::GameEventKind::CreateClient(client_id),
                                region,
                            )
                            .unwrap();
                        world.forget_last_event(&region);
                        server_send.send((None, ServerPacket::GameEvent(ev))).unwrap();
                    }
                }
                ServerEvent::ServerTickTimer => {}
            }
        }
    });

    // Client side: read the hash file the ingress just wrote, connect.
    // write_cert_hash_file resolves via CARGO_MANIFEST_DIR (crates/server),
    // and cargo test's cwd for integration tests is that same manifest dir,
    // so "../../assets/..." is the one path that always matches.
    let hash_json = std::fs::read_to_string("../../assets/webtransport-cert-hash.json")
        .expect("cert hash file written by serve()");
    assert!(hash_json.contains("sha256_hex"));

    let config = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation() // test-only; the browser path uses the hash
        .build();
    let conn = wtransport::Endpoint::client(config)
        .unwrap()
        .connect("https://127.0.0.1:16467")
        .await
        .expect("connect failed");

    // CreateClient broadcast arrives first (ClientConnected fired on accept).
    let first = recv_packet(&conn).await;
    assert!(matches!(first, ServerPacket::GameEvent(_)), "got {first:?}");

    send_packet(&conn, &ClientPacket::RequestPlayerRegion).await;
    let player_region = recv_packet(&conn).await;
    let region_id = match player_region {
        ServerPacket::PlayerRegion(id, client_id) => {
            assert_eq!(client_id, 0);
            id.unwrap_or(ChunkCoords::new(0, 0, 0))
        }
        p => panic!("expected PlayerRegion, got {p:?}"),
    };

    send_packet(&conn, &ClientPacket::RequestRegionConnection(region_id)).await;
    let snapshot = recv_packet(&conn).await;
    assert!(matches!(snapshot, ServerPacket::Region(..)), "got {snapshot:?}");
}
