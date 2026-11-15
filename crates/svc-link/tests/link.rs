//! The service link end to end on localhost (port 0). Slow-tagged: each opens real QUIC sockets.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crossbeam::channel::Receiver;
use serde::{Deserialize, Serialize};
use svc_link::{LinkDialer, LinkEvent, LinkHello, LinkListener, PeerId, PeerRole};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Msg {
    Num(u64),
    Blob(Vec<u8>),
}

const TOKEN: &str = "test-token";
const WAIT: Duration = Duration::from_secs(10);

fn loopback() -> SocketAddr {
    ([127, 0, 0, 1], 0).into()
}

fn hello(token: &str) -> LinkHello {
    LinkHello::new(token.to_string(), PeerRole::Coordinator, "")
}

fn next<T>(rx: &Receiver<T>) -> T {
    rx.recv_timeout(WAIT).expect("an event within the timeout")
}

fn nothing<T: std::fmt::Debug>(rx: &Receiver<T>) {
    if let Ok(ev) = rx.recv_timeout(Duration::from_millis(700)) {
        panic!("expected silence, got {ev:?}");
    }
}

fn temp_hash_file(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("svc-link-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("link.sha256")
}

fn expect_up(rx: &Receiver<LinkEvent<Msg>>) {
    match next(rx) {
        LinkEvent::Up(_) => {}
        other => panic!("expected Up, got {other:?}"),
    }
}

fn expect_peer_up(rx: &Receiver<(PeerId, LinkEvent<Msg>)>) -> PeerId {
    match next(rx) {
        (peer, LinkEvent::Up(_)) => peer,
        other => panic!("expected Up, got {other:?}"),
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn round_trip_both_directions() {
    let (listener, from_peers, senders) = LinkListener::bind::<Msg, Msg>(loopback(), TOKEN.into(), String::new(), None).unwrap();
    let (to_listener, from_listener) = LinkDialer::connect::<Msg, Msg>(
        listener.local_addr(),
        listener.cert_sha256().to_string(),
        hello(TOKEN),
    );
    expect_up(&from_listener);
    let peer = expect_peer_up(&from_peers);

    to_listener.send(Msg::Num(7)).unwrap();
    match next(&from_peers) {
        (p, LinkEvent::Msg(Msg::Num(7))) => assert_eq!(p, peer),
        other => panic!("{other:?}"),
    }
    assert!(senders.send(peer, Msg::Num(9)));
    senders.broadcast(Msg::Num(10));
    assert!(matches!(next(&from_listener), LinkEvent::Msg(Msg::Num(9))));
    assert!(matches!(next(&from_listener), LinkEvent::Msg(Msg::Num(10))));
    assert!(!senders.send(PeerId(999), Msg::Num(1)));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn wrong_token_refused() {
    let (listener, from_peers, _senders) = LinkListener::bind::<Msg, Msg>(loopback(), TOKEN.into(), String::new(), None).unwrap();
    let (_tx, rx) = LinkDialer::connect::<Msg, Msg>(
        listener.local_addr(),
        listener.cert_sha256().to_string(),
        hello("not-the-token"),
    );
    match next(&rx) {
        LinkEvent::Down(why) => assert!(why.contains("token"), "reason names the token: {why}"),
        other => panic!("expected Down, got {other:?}"),
    }
    nothing(&from_peers);
}

/// Elastic 1 Task 6: a hello of another world is refused, whichever side names one; the
/// empty id (no `VOXEL_WORLD_ID`) matches only the empty id; the same world is admitted.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_hello_for_another_world_is_refused() {
    for (ours, theirs) in [("alpha", "beta"), ("alpha", ""), ("", "alpha")] {
        let (listener, from_peers, _senders) = LinkListener::bind::<Msg, Msg>(loopback(), TOKEN.into(), ours.into(), None).unwrap();
        let h = LinkHello::new(TOKEN.into(), PeerRole::Coordinator, theirs);
        let (_tx, rx) = LinkDialer::connect::<Msg, Msg>(listener.local_addr(), listener.cert_sha256().to_string(), h);
        match next(&rx) {
            LinkEvent::Down(why) => assert!(why.contains("world"), "listener {ours:?}, peer {theirs:?}: the reason names the world: {why}"),
            other => panic!("listener {ours:?}, peer {theirs:?}: expected Down, got {other:?}"),
        }
        nothing(&from_peers);
    }
    let (listener, from_peers, _senders) = LinkListener::bind::<Msg, Msg>(loopback(), TOKEN.into(), "alpha".into(), None).unwrap();
    let h = LinkHello::new(TOKEN.into(), PeerRole::Coordinator, "alpha");
    let (_tx, rx) = LinkDialer::connect::<Msg, Msg>(listener.local_addr(), listener.cert_sha256().to_string(), h);
    expect_up(&rx);
    assert_eq!(expect_peer_up_hello(&from_peers).world, "alpha");
}

/// The hello of the next peer to come up.
fn expect_peer_up_hello(rx: &Receiver<(PeerId, LinkEvent<Msg>)>) -> LinkHello {
    match next(rx) {
        (_, LinkEvent::Up(hello)) => hello,
        other => panic!("expected Up, got {other:?}"),
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn wrong_cert_hash_refused() {
    let (listener, from_peers, _senders) = LinkListener::bind::<Msg, Msg>(loopback(), TOKEN.into(), String::new(), None).unwrap();
    let (_tx, rx) = LinkDialer::connect::<Msg, Msg>(listener.local_addr(), "00".repeat(32), hello(TOKEN));
    assert!(matches!(next(&rx), LinkEvent::Down(_)));
    nothing(&from_peers);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn mismatched_dials_refused() {
    let (listener, from_peers, _senders) = LinkListener::bind::<Msg, Msg>(loopback(), TOKEN.into(), String::new(), None).unwrap();
    let pin = listener.cert_sha256().to_string();
    for (label, edit) in [
        ("region", (|h: &mut LinkHello| h.region_blocks += 1) as fn(&mut LinkHello)),
        ("chunk", |h| h.chunk_blocks += 1),
        ("protocol", |h| h.protocol += 1),
    ] {
        let mut h = hello(TOKEN);
        edit(&mut h);
        let (_tx, rx) = LinkDialer::connect::<Msg, Msg>(listener.local_addr(), pin.clone(), h);
        match next(&rx) {
            LinkEvent::Down(why) => assert!(!why.is_empty(), "{label}: a reason"),
            other => panic!("{label}: expected Down, got {other:?}"),
        }
    }
    nothing(&from_peers);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn dialer_reconnects_after_listener_restart() {
    let hash_file = temp_hash_file("restart");
    let (listener, from_peers, senders) =
        LinkListener::bind::<Msg, Msg>(loopback(), TOKEN.into(), String::new(), Some(hash_file.clone())).unwrap();
    let addr = listener.local_addr();
    let pin = svc_link::read_cert_hash("SVC_LINK_TEST_UNSET_VAR", &hash_file).unwrap();
    assert_eq!(pin, listener.cert_sha256());
    let (to_listener, from_listener) = LinkDialer::connect::<Msg, Msg>(addr, pin, hello(TOKEN));
    expect_up(&from_listener);
    let peer = expect_peer_up(&from_peers);
    assert!(senders.send(peer, Msg::Num(1)));
    assert!(matches!(next(&from_listener), LinkEvent::Msg(Msg::Num(1))));

    drop((listener, from_peers, senders));
    assert!(matches!(next(&from_listener), LinkEvent::Down(_)));
    // Sent while down: dropped, never delivered after the reconnect.
    to_listener.send(Msg::Num(100)).unwrap();

    // Same port and same hash file: the pin the dialer holds still matches.
    let (listener, from_peers, senders) = {
        let mut tries = 0;
        loop {
            match LinkListener::bind::<Msg, Msg>(addr, TOKEN.into(), String::new(), Some(hash_file.clone())) {
                Ok(parts) => break parts,
                Err(e) if tries < 40 => {
                    tries += 1;
                    eprintln!("rebind: {e}");
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => panic!("rebind failed: {e}"),
            }
        }
    };
    expect_up(&from_listener);
    let peer = expect_peer_up(&from_peers);
    for n in 0..20u64 {
        to_listener.send(Msg::Num(n)).unwrap();
    }
    for n in 0..20u64 {
        match next(&from_peers) {
            (_, LinkEvent::Msg(Msg::Num(got))) => assert_eq!(got, n, "in order, nothing from the down period"),
            other => panic!("{other:?}"),
        }
    }
    assert!(senders.send(peer, Msg::Num(2)));
    assert!(matches!(next(&from_listener), LinkEvent::Msg(Msg::Num(2))));
    drop(listener);
}

/// Incompressible bytes, so the frame is as large as the payload.
fn noise(len: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn large_frame_ok_and_oversize_refused() {
    let (listener, from_peers, _senders) = LinkListener::bind::<Msg, Msg>(loopback(), TOKEN.into(), String::new(), None).unwrap();
    let (to_listener, from_listener) = LinkDialer::connect::<Msg, Msg>(
        listener.local_addr(),
        listener.cert_sha256().to_string(),
        hello(TOKEN),
    );
    expect_up(&from_listener);
    expect_peer_up(&from_peers);

    let big = Msg::Blob(noise(8 * 1024 * 1024));
    to_listener.send(big.clone()).unwrap();
    match next(&from_peers) {
        (_, LinkEvent::Msg(got)) => assert_eq!(got, big),
        other => panic!("{other:?}"),
    }

    // Over the limit: refused at the sender, the link survives and the next message flows.
    to_listener.send(Msg::Blob(noise(svc_link::LINK_MAX_FRAME_BYTES + 1024))).unwrap();
    to_listener.send(Msg::Num(5)).unwrap();
    match next(&from_peers) {
        (_, LinkEvent::Msg(Msg::Num(5))) => {}
        other => panic!("the oversize frame must not arrive: {other:?}"),
    }
}

/// A raw QUIC client config that accepts any certificate (the tests that use it play an
/// unauthenticated peer; no pin check is needed).
fn raw_client_config() -> quinn::ClientConfig {
    use quinn::rustls;
    #[derive(Debug)]
    struct AcceptAny(std::sync::Arc<rustls::crypto::CryptoProvider>);
    impl rustls::client::danger::ServerCertVerifier for AcceptAny {
        fn verify_server_cert(
            &self,
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &[rustls::pki_types::CertificateDer<'_>],
            _: &rustls::pki_types::ServerName<'_>,
            _: &[u8],
            _: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            m: &[u8],
            c: &rustls::pki_types::CertificateDer<'_>,
            d: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
        }
        fn verify_tls13_signature(
            &self,
            m: &[u8],
            c: &rustls::pki_types::CertificateDer<'_>,
            d: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(AcceptAny(provider)))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![b"mmo-svc-link/1".to_vec()];
    quinn::ClientConfig::new(std::sync::Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap()))
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn forged_compressed_hello_refused_before_decoding() {
    // A raw QUIC client sends a hello tagged lz4 that claims a huge output: the listener
    // must refuse it without decoding.
    let (listener, from_peers, _senders) = LinkListener::bind::<Msg, Msg>(loopback(), TOKEN.into(), String::new(), None).unwrap();
    let addr = listener.local_addr();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let endpoint = quinn::Endpoint::client(([127, 0, 0, 1], 0).into()).unwrap();
        let conn = endpoint.connect_with(raw_client_config(), addr, "localhost").unwrap().await.unwrap();
        let (mut send, _recv) = conn.open_bi().await.unwrap();
        // tag 1 (lz4), claimed output 0x7fff_ffff bytes, no payload.
        let body = [1u8, 0xff, 0xff, 0xff, 0x7f];
        send.write_all(&(body.len() as u32).to_be_bytes()).await.unwrap();
        send.write_all(&body).await.unwrap();
        let closed = tokio::time::timeout(WAIT, conn.closed()).await.expect("listener closes the connection");
        assert!(closed.to_string().contains("plain"), "refused for its framing: {closed}");
    });
    nothing(&from_peers);
}

/// An unauthenticated peer gets one bidirectional stream and no unidirectional one (it
/// cannot make the listener buffer per-stream windows by the hundred), and at most
/// [`LINK_MAX_PENDING_HANDSHAKES`] connections wait in the hello at once: one more is refused.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn unauthenticated_peers_are_bounded() {
    const PENDING: usize = 16; // svc_link's LINK_MAX_PENDING_HANDSHAKES
    let (listener, from_peers, _senders) = LinkListener::bind::<Msg, Msg>(loopback(), TOKEN.into(), String::new(), None).unwrap();
    let addr = listener.local_addr();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let endpoint = quinn::Endpoint::client(([127, 0, 0, 1], 0).into()).unwrap();
        let mut held = Vec::new();
        for _ in 0..PENDING {
            held.push(endpoint.connect_with(raw_client_config(), addr, "localhost").unwrap().await.unwrap());
        }
        let first = &held[0];
        let short = Duration::from_millis(300);
        assert!(tokio::time::timeout(short, first.open_uni()).await.is_err(), "no unidirectional stream is allowed");
        let _bi = first.open_bi().await.expect("one bidirectional stream");
        assert!(tokio::time::timeout(short, first.open_bi()).await.is_err(), "a second one is not allowed");
        let over = endpoint.connect_with(raw_client_config(), addr, "localhost").unwrap();
        let refused = tokio::time::timeout(WAIT, over).await.expect("answered");
        assert!(refused.is_err(), "the {}th pending handshake is refused", PENDING + 1);
    });
    nothing(&from_peers);
}

/// The listener closes one peer's connection: the dialer sees its link go down.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn listener_closes_a_peer() {
    let (listener, from_peers, senders) = LinkListener::bind::<Msg, Msg>(loopback(), TOKEN.into(), String::new(), None).unwrap();
    let (_tx, rx) = LinkDialer::connect::<Msg, Msg>(listener.local_addr(), listener.cert_sha256().to_string(), hello(TOKEN));
    expect_up(&rx);
    let peer = expect_peer_up(&from_peers);
    assert!(senders.close(peer));
    assert!(matches!(next(&rx), LinkEvent::Down(_)), "the dialer's link went down");
    assert!(matches!(next(&from_peers), (p, LinkEvent::Down(_)) if p == peer));
    assert!(!senders.send(peer, Msg::Num(1)), "closed");
}

#[test]
fn hello_debug_never_shows_the_token() {
    let h = LinkHello::new("s3cret-token".into(), PeerRole::Host { id: game::HostId(0), boot: 1, reg: 2 }, "");
    let shown = format!("{h:?}");
    assert!(!shown.contains("s3cret"), "{shown}");
    assert!(shown.contains("redacted") && shown.contains("reg: 2"), "{shown}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn messages_sent_right_after_up_are_never_lost() {
    let (listener, from_peers, _senders) = LinkListener::bind::<Msg, Msg>(loopback(), TOKEN.into(), String::new(), None).unwrap();
    let pin = listener.cert_sha256().to_string();
    // Send the instant Up is seen, over many fresh links: the slot must already be open.
    for round in 0..25u64 {
        let (tx, rx) = LinkDialer::connect::<Msg, Msg>(listener.local_addr(), pin.clone(), hello(TOKEN));
        expect_up(&rx);
        tx.send(Msg::Num(round)).unwrap();
        loop {
            match next(&from_peers) {
                (_, LinkEvent::Msg(Msg::Num(n))) => {
                    assert_eq!(n, round);
                    break;
                }
                _ => {}
            }
        }
    }
}
