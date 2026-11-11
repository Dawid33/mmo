//! End-to-end: a wtransport client (the same library the browser-facing
//! endpoint uses) performs the full join handshake against the real router
//! loop, proving Region snapshots and GameEvent echoes flow over WebTransport.
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use game::{ClientPacket, PlayerName, RegionCoords, ServerEvent, ServerPacket};
use server::{player_store, webtransport, ClientSink};

fn memory_store() -> player_store::SharedPlayerStore {
    player_store::shared(player_store::MemoryPlayerStore::default())
}

fn local_auth() -> Arc<server::auth_edge::Authenticator> {
    Arc::new(server::auth_edge::Authenticator::local())
}

fn hello(name: &str) -> ClientPacket {
    ClientPacket::Hello { credential: game::Credential::Local(PlayerName(name.into())) }
}

async fn connect(port: u16) -> wtransport::Connection {
    let config = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation() // test-only; the browser path uses the hash
        .build();
    wtransport::Endpoint::client(config)
        .unwrap()
        .connect(format!("https://127.0.0.1:{port}"))
        .await
        .expect("connect failed")
}

async fn recv_packet(conn: &wtransport::Connection) -> ServerPacket {
    let mut stream = conn.accept_uni().await.expect("server closed stream");
    let mut buf = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf)
        .await
        .expect("read failed");
    game::decode_packet(&buf).expect("bad packet")
}

async fn send_packet(conn: &wtransport::Connection, packet: &ClientPacket) {
    let mut stream = conn.open_uni().await.unwrap().await.unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut stream, &game::encode_packet(packet).unwrap())
        .await
        .unwrap();
    stream.finish().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "slow: >100ms — run via just test-full"]
async fn webtransport_client_full_handshake() {
    let (event_send, event_recv) = crossbeam::channel::unbounded();
    let (server_send, server_recv) = crossbeam::channel::unbounded();
    let sinks: Arc<DashMap<usize, ClientSink>> = Arc::new(DashMap::new());
    let next_id = Arc::new(AtomicUsize::new(0));

    // Writer task (Task 1's) — drains router output into sinks.
    let writer_sinks = sinks.clone();
    tokio::spawn(async move {
        while let Ok((target, event)) = server_recv.recv() {
            let packet = game::encode_packet(&event).unwrap();
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
        webtransport::serve(bind, webtransport::CertSource::SelfSigned, event_send, ingress_sinks, next_id, memory_store(), local_auth())
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(300)).await; // endpoint up + hash file written

    // Router loop on a plain thread, same shape as server::run's tail.
    // (No results_buffer here, unlike server::run's tail: ServerTickTimer
    // is a no-op in this test — nothing ever drives progress_world_one_tick,
    // so the buffer the brief sketched would be dead and fail to type-infer.)
    std::thread::spawn(move || {
        let origin = RegionCoords::new(0, 0);
        let mut world = game::World::new();
        world.load(&origin, game::Region::empty(origin));
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
                    // The tick stamp only matters to a ticking server; this one never ticks.
                    ClientPacket::GameEvent(ev) | ClientPacket::GameEventAfterTick(ev, _) => {
                        let out = world.handle_region_event(ev.kind, ev.region_id).unwrap();
                        world.forget_last_event(&ev.region_id);
                        server_send
                            .send((None, ServerPacket::GameEvent(out)))
                            .unwrap();
                    }
                    ClientPacket::ReleaseRegionConnection(_) | ClientPacket::Hello { .. } => {}
                    ClientPacket::RequestLod(_)
                    | ClientPacket::ReleaseLod(_)
                    | ClientPacket::PlayRegion(_)
                    | ClientPacket::Leave
                    | ClientPacket::RequestEmbarkOptions
                    | ClientPacket::Embark(_)
                    | ClientPacket::Retire
                    | ClientPacket::ReturnTo(_)
                    | ClientPacket::RequestPersonInfo(_)
                    | ClientPacket::RequestLegends(_)
                    | ClientPacket::StartAdventure(_)
                    | ClientPacket::EndAdventure => {}
                },
                ServerEvent::ClientConnected { client: client_id, .. } => {
                    if world.find_player(&client_id).is_none() {
                        let region = RegionCoords::new(0, 0);
                        let ev = world
                            .handle_region_event(
                                game::GameEventKind::CreateClient(client_id),
                                region,
                            )
                            .unwrap();
                        world.forget_last_event(&region);
                        server_send
                            .send((None, ServerPacket::GameEvent(ev)))
                            .unwrap();
                    }
                }
                ServerEvent::ClientDisconnected(_) | ServerEvent::Supersede { .. } => {}
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

    // The handshake: nothing happens until the Hello resolves a player.
    send_packet(&conn, &hello("alice")).await;
    // CreateClient broadcast arrives first (ClientConnected fired on Hello).
    let first = recv_packet(&conn).await;
    assert!(matches!(first, ServerPacket::GameEvent(_)), "got {first:?}");

    send_packet(&conn, &ClientPacket::RequestPlayerRegion).await;
    let player_region = recv_packet(&conn).await;
    let region_id = match player_region {
        ServerPacket::PlayerRegion(id, client_id) => {
            assert_eq!(client_id, 0);
            id.unwrap_or(RegionCoords::new(0, 0))
        }
        p => panic!("expected PlayerRegion, got {p:?}"),
    };

    send_packet(&conn, &ClientPacket::RequestRegionConnection(region_id)).await;
    let snapshot = recv_packet(&conn).await;
    assert!(
        matches!(snapshot, ServerPacket::Region(..)),
        "got {snapshot:?}"
    );
}

/// A connection whose first packet is not `Hello` is refused and closed, and
/// the world never hears of it; one with a valid `Hello` becomes a session
/// whose player the store resolved.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "slow: >100ms — run via just test-full"]
async fn first_packet_not_hello_closes() {
    const PORT: u16 = 16468;
    let (event_send, event_recv) = crossbeam::channel::unbounded();
    let sinks: Arc<DashMap<usize, ClientSink>> = Arc::new(DashMap::new());
    let ingress_sinks = sinks.clone();
    tokio::spawn(async move {
        let bind = format!("127.0.0.1:{PORT}").parse().unwrap();
        webtransport::serve(
            bind,
            webtransport::CertSource::SelfSigned,
            event_send,
            ingress_sinks,
            Arc::new(AtomicUsize::new(0)),
            memory_store(),
            local_auth(),
        )
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Not a Hello: refused, closed, no session.
    let conn = connect(PORT).await;
    send_packet(&conn, &ClientPacket::RequestPlayerRegion).await;
    let refused = tokio::time::timeout(Duration::from_secs(5), recv_packet(&conn)).await.expect("an answer");
    assert!(matches!(&refused, ServerPacket::Refused(why) if why.contains("Hello")), "got {refused:?}");
    tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.expect("the server closes the session");
    assert!(event_recv.try_recv().is_err(), "a refused connection is never announced");
    assert!(sinks.is_empty());

    // An invalid name: refused too.
    let conn = connect(PORT).await;
    send_packet(&conn, &hello("   ")).await;
    let refused = tokio::time::timeout(Duration::from_secs(5), recv_packet(&conn)).await.expect("an answer");
    assert!(matches!(refused, ServerPacket::Refused(_)), "got {refused:?}");
    tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.expect("the server closes the session");
    assert!(event_recv.try_recv().is_err());

    // A Hello: announced with its player, and the same name is the same player.
    let mut players = Vec::new();
    // The refused connections took ids 0 and 1.
    for client in [2, 3] {
        let conn = connect(PORT).await;
        send_packet(&conn, &hello("bob")).await;
        let event = loop {
            match event_recv.recv_timeout(Duration::from_secs(5)).expect("the session is announced") {
                ServerEvent::ClientDisconnected(_) => continue, // the previous one
                event => break event,
            }
        };
        match event {
            ServerEvent::ClientConnected { client: c, player, record } => {
                assert_eq!(c, client, "client ids count every connection");
                assert!(record.is_none());
                players.push(player);
            }
            other => panic!("expected ClientConnected, got {other:?}"),
        }
        drop(conn);
    }
    assert_eq!(players[0], players[1], "one name, one player");
}
