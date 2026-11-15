//! End-to-end: a wtransport client (the same library the browser-facing
//! endpoint uses) performs the full join handshake against the real router
//! loop, proving Region snapshots and GameEvent echoes flow over WebTransport.
use std::sync::atomic::AtomicU64;
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

fn self_signed() -> server::client_cert::ClientCert {
    server::client_cert::ClientCert::from_env(|_| None).unwrap()
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
    let sinks: Arc<DashMap<game::ClientId, ClientSink>> = Arc::new(DashMap::new());
    let next_id = Arc::new(AtomicU64::new(0));

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
        webtransport::serve(bind, self_signed(), 0, event_send, ingress_sinks, next_id, memory_store(), local_auth())
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
        world.load(&origin, game::Region::from_chunks(origin, Vec::new()));
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
                ServerEvent::ClientDisconnected(_)
                | ServerEvent::Supersede { .. }
                | ServerEvent::ObserverConnected { .. }
                | ServerEvent::SpawnOrder { .. }
                | ServerEvent::LoginConnected { .. }
                | ServerEvent::Kick { .. }
                | ServerEvent::SessionEnded { .. } => {}
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
    let sinks: Arc<DashMap<game::ClientId, ClientSink>> = Arc::new(DashMap::new());
    let ingress_sinks = sinks.clone();
    tokio::spawn(async move {
        let bind = format!("127.0.0.1:{PORT}").parse().unwrap();
        webtransport::serve(
            bind,
            self_signed(),
            0,
            event_send,
            ingress_sinks,
            Arc::new(AtomicU64::new(0)),
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
    // The refused connections took no id: a session gets one once authenticated.
    for client in [0, 1] {
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
                assert_eq!(c, client, "client ids count authenticated sessions");
                assert!(record.is_none());
                players.push(player);
            }
            other => panic!("expected ClientConnected, got {other:?}"),
        }
        drop(conn);
    }
    assert_eq!(players[0], players[1], "one name, one player");
}

/// One certificate for both listeners: a native client (QUIC) and a browser (WebTransport)
/// are shown the same leaf, whose SHA-256 is the hash the host advertises — and a client
/// that pins that hash, as a browser does, connects.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "slow: >100ms — run via just test-full"]
async fn quic_and_webtransport_show_the_same_leaf() {
    let free = || std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (quic_port, wt_port) = (free(), free());
    let cert = self_signed();
    let cert_hash = cert.sha256_hex().expect("self-signed: pinned by hash");
    let (event_send, _event_recv) = crossbeam::channel::unbounded();
    let (_server_send, server_recv) = crossbeam::channel::unbounded();
    let mut ingress = server::WorldIngress::new("127.0.0.1".parse().unwrap(), quic_port, true, memory_store(), local_auth())
        .with_webtransport_port(wt_port)
        .with_client_cert(cert);
    tokio::spawn(async move { ingress.listen(event_send, server_recv).await });
    tokio::time::sleep(Duration::from_millis(300)).await;

    let quic_leaf = leaf::over_quic(quic_port).await;
    let wt_leaf = leaf::over_webtransport(wt_port).await;
    assert_eq!(quic_leaf, wt_leaf, "one certificate for both listeners");
    let hash = wtransport::tls::Certificate::from_der(quic_leaf).unwrap().hash();
    let hex: String = hash.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, cert_hash, "the advertised hash is the served leaf's");

    // The browser's way: pin the advertised hash (ECDSA, valid <= 14 days, or it is refused).
    let pinned = wtransport::ClientConfig::builder().with_bind_default().with_server_certificate_hashes([hash]).build();
    wtransport::Endpoint::client(pinned)
        .unwrap()
        .connect(format!("https://127.0.0.1:{wt_port}"))
        .await
        .expect("a client pinning the advertised hash connects");
}

/// Test clients that accept any certificate and record the leaf they were shown.
mod leaf {
    use std::sync::{Arc, Mutex};

    use quinn::crypto::rustls::QuicClientConfig;
    use quinn::rustls::{
        self,
        client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        pki_types::{CertificateDer, ServerName, UnixTime},
        DigitallySignedStruct, SignatureScheme,
    };

    #[derive(Debug)]
    struct Recording {
        provider: Arc<rustls::crypto::CryptoProvider>,
        leaf: Mutex<Option<Vec<u8>>>,
    }

    impl ServerCertVerifier for Recording {
        fn verify_server_cert(
            &self,
            leaf: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            *self.leaf.lock().unwrap() = Some(leaf.to_vec());
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
        }
        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.provider.signature_verification_algorithms.supported_schemes()
        }
    }

    fn recording_tls(alpn: Option<&[u8]>) -> (rustls::ClientConfig, Arc<Recording>) {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = Arc::new(Recording { provider: provider.clone(), leaf: Mutex::new(None) });
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(verifier.clone())
            .with_no_client_auth();
        tls.alpn_protocols = alpn.into_iter().map(<[u8]>::to_vec).collect();
        (tls, verifier)
    }

    fn recorded(verifier: &Recording) -> Vec<u8> {
        verifier.leaf.lock().unwrap().clone().expect("the handshake showed a certificate")
    }

    pub async fn over_quic(port: u16) -> Vec<u8> {
        let (tls, verifier) = recording_tls(None);
        let config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls).unwrap()));
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(config);
        let connection =
            endpoint.connect(format!("127.0.0.1:{port}").parse().unwrap(), "localhost").unwrap().await.expect("QUIC connects");
        connection.close(0u32.into(), b"done");
        recorded(&verifier)
    }

    pub async fn over_webtransport(port: u16) -> Vec<u8> {
        let (tls, verifier) = recording_tls(Some(wtransport::tls::WEBTRANSPORT_ALPN));
        let config = wtransport::ClientConfig::builder().with_bind_default().with_custom_tls(tls).build();
        let connection = wtransport::Endpoint::client(config)
            .unwrap()
            .connect(format!("https://127.0.0.1:{port}"))
            .await
            .expect("WebTransport connects");
        connection.close(wtransport::VarInt::from_u32(0), b"done");
        recorded(&verifier)
    }
}
