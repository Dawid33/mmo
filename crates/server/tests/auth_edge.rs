//! The connect edge's credential check (`VOXEL_AUTH`): a `jwt` host admits a
//! connection only with a valid login ticket for its world, a `local` host
//! (single-player / LAN / dev) only with a player name. Driven through
//! `player_store::resolve_hello` — the very function both ingress paths call —
//! plus one end-to-end refusal over real QUIC.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use auth::{Jwks, Signer, SigningKey};
use game::{ClientPacket, Credential, PlayerId, PlayerName, PlayerToken};
use login_api::{JoinClaims, HOST_AUDIENCE, JOIN_JWT_TTL_SECS};
use server::auth_edge::{AuthMode, Authenticator, Clock, JwksCache, JWKS_REFETCH_MIN_SECS, MAX_KID_SHOWN_CHARS};
use server::player_store::{self, resolve_hello, SharedPlayerStore};

const NOW: u64 = 1_800_000_000;
const WORLD: &str = "alpha";

fn token(n: u8) -> PlayerToken {
    PlayerToken::generate([n; 16])
}

fn claims(player: &PlayerToken, epoch: u64) -> JoinClaims {
    JoinClaims {
        sub: "42".into(),
        world: WORLD.into(),
        player_token: player.0.clone(),
        session_epoch: epoch,
        aud: HOST_AUDIENCE.into(),
        iat: NOW,
        exp: NOW + JOIN_JWT_TTL_SECS,
    }
}

fn signer() -> Signer {
    Signer::new(vec![SigningKey::generate().0]).unwrap_or_else(|_| unreachable!())
}

/// A settable clock, in seconds since the Unix epoch.
fn clock(at: u64) -> (Arc<AtomicU64>, Clock) {
    let now = Arc::new(AtomicU64::new(at));
    let read = now.clone();
    (now, Arc::new(move || read.load(Ordering::SeqCst)))
}

fn store() -> SharedPlayerStore {
    player_store::shared(player_store::MemoryPlayerStore::default())
}

fn jwt_host(jwks: Jwks) -> Authenticator {
    Authenticator::jwt(WORLD, JwksCache::fixed(jwks), clock(NOW).1)
}

fn hello_jwt(jwt: String) -> ClientPacket {
    ClientPacket::Hello { credential: Credential::Jwt(jwt) }
}

fn hello_name(name: &str) -> ClientPacket {
    ClientPacket::Hello { credential: Credential::Local(PlayerName(name.into())) }
}

async fn admit(store: &SharedPlayerStore, auth: &Authenticator, jwt: String) -> Result<PlayerId, String> {
    resolve_hello(store, auth, hello_jwt(jwt)).await.map(|(id, _)| id)
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_accepts_valid_ticket() {
    let signer = signer();
    let auth = jwt_host(signer.jwks());
    let store = store();
    let first = admit(&store, &auth, signer.sign(&claims(&token(1), 1))).await.expect("a valid ticket");
    let again = admit(&store, &auth, signer.sign(&claims(&token(1), 2))).await.expect("a newer ticket");
    assert_eq!(first, again, "one player_token is one player");
    // A reconnect with the same ticket (same epoch) within its TTL is fine.
    let same = admit(&store, &auth, signer.sign(&claims(&token(1), 2))).await.expect("same epoch");
    assert_eq!(first, same);
    let other = admit(&store, &auth, signer.sign(&claims(&token(2), 1))).await.expect("another player");
    assert_ne!(first, other);
}

/// The login service mints a ticket's `player_token` itself
/// (`crates/login/src/api.rs` `new_player_token`: `PLAYER_TOKEN_BYTES` random
/// bytes as lowercase hex), while hosts parse it as a `game::PlayerToken`
/// (`crates/game/src/player.rs`). The two definitions are kept apart on
/// purpose (no `login` dependency here): this pins that a token in login's
/// format is one the host accepts.
#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_accepts_a_player_token_in_the_login_services_format() {
    // Login's format, spelled out: 16 bytes, each as two lowercase hex digits.
    const LOGIN_PLAYER_TOKEN_BYTES: usize = 16;
    assert_eq!(LOGIN_PLAYER_TOKEN_BYTES, game::PLAYER_TOKEN_BYTES, "login and game agree on the size");
    let bytes: [u8; LOGIN_PLAYER_TOKEN_BYTES] = std::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(0xA5));
    let minted: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert!(PlayerToken(minted.clone()).is_valid(), "{minted}");
    assert_eq!(PlayerToken::generate(bytes).0, minted, "the same text as game's own generator");

    let signer = signer();
    let auth = jwt_host(signer.jwks());
    let store = store();
    let mut ticket = claims(&token(0), 1);
    ticket.player_token = minted;
    admit(&store, &auth, signer.sign(&ticket)).await.expect("a login-minted player token is admitted");
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_refuses_expired() {
    let signer = signer();
    let auth = jwt_host(signer.jwks());
    let mut old = claims(&token(1), 1);
    old.iat = NOW - 2 * JOIN_JWT_TTL_SECS;
    old.exp = NOW - JOIN_JWT_TTL_SECS;
    let why = admit(&store(), &auth, signer.sign(&old)).await.unwrap_err();
    assert!(why.contains("expired"), "{why}");
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_refuses_wrong_world() {
    let signer = signer();
    let auth = jwt_host(signer.jwks());
    let mut elsewhere = claims(&token(1), 1);
    elsewhere.world = "beta".into();
    let why = admit(&store(), &auth, signer.sign(&elsewhere)).await.unwrap_err();
    assert!(why.contains("another world"), "{why}");
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_refuses_wrong_audience() {
    let signer = signer();
    let auth = jwt_host(signer.jwks());
    let mut wrong = claims(&token(1), 1);
    wrong.aud = "someone-else".into();
    let why = admit(&store(), &auth, signer.sign(&wrong)).await.unwrap_err();
    assert!(why.contains("not for a game host"), "{why}");
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_refuses_bad_signature() {
    let signer = signer();
    let auth = jwt_host(signer.jwks());
    // The payload of one ticket under the signature of another (same key).
    let mine = signer.sign(&claims(&token(1), 1));
    let theirs = signer.sign(&claims(&token(2), 1));
    let (body, _) = mine.rsplit_once('.').unwrap();
    let (_, signature) = theirs.rsplit_once('.').unwrap();
    let why = admit(&store(), &auth, format!("{body}.{signature}")).await.unwrap_err();
    assert!(why.contains("signature is invalid"), "{why}");
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_refuses_malformed() {
    let auth = jwt_host(signer().jwks());
    let why = admit(&store(), &auth, "not.a.jwt".into()).await.unwrap_err();
    assert!(why.contains("malformed"), "{why}");
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_refuses_unknown_kid() {
    let published = signer();
    let stranger = signer();
    let (now, clock) = clock(NOW);
    let auth = Authenticator::jwt(WORLD, JwksCache::fixed(published.jwks()), clock);
    let store = store();
    let ticket = stranger.sign(&claims(&token(1), 1));
    let why = admit(&store, &auth, ticket.clone()).await.unwrap_err();
    assert!(why.contains("unknown key"), "{why}");
    let jwks = auth.jwks().expect("a jwt host has a JWKS cache");
    assert_eq!(jwks.fetches(), 1, "an unknown kid refetches the JWKS once");
    // Within the throttle the refetch is not repeated.
    now.store(NOW + JWKS_REFETCH_MIN_SECS - 1, Ordering::SeqCst);
    admit(&store, &auth, ticket.clone()).await.unwrap_err();
    assert_eq!(jwks.fetches(), 1, "refetches are throttled");
    // After it, it is.
    now.store(NOW + JWKS_REFETCH_MIN_SECS, Ordering::SeqCst);
    let why = admit(&store, &auth, ticket).await.unwrap_err();
    assert!(why.contains("unknown key"), "{why}");
    assert_eq!(jwks.fetches(), 2);
}

/// A key the login service rotated in after the host started is picked up
/// by the refetch an unknown kid triggers.
#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_refetch_finds_a_rotated_key() {
    let old = signer();
    let new = signer();
    let mut both = old.jwks();
    both.keys.extend(new.jwks().keys);
    let cache = JwksCache::with_fetch(Box::new(move || {
        let both = both.clone();
        Box::pin(async move { Ok(both) })
    }));
    let auth = Authenticator::jwt(WORLD, cache, clock(NOW).1);
    let store = store();
    admit(&store, &auth, new.sign(&claims(&token(1), 1))).await.expect("the rotated key is fetched");
    admit(&store, &auth, old.sign(&claims(&token(1), 1))).await.expect("the old key is still published");
    assert_eq!(auth.jwks().unwrap().fetches(), 1);
}

/// A failing fetch keeps the keys already cached.
#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_keeps_keys_when_a_fetch_fails() {
    let signer = signer();
    let jwks = signer.jwks();
    let calls = Arc::new(AtomicU64::new(0));
    let counted = calls.clone();
    let cache = JwksCache::with_fetch(Box::new(move || {
        let first = counted.fetch_add(1, Ordering::SeqCst) == 0;
        let jwks = jwks.clone();
        Box::pin(async move { if first { Ok(jwks) } else { Err("login service down".to_string()) } })
    }));
    let (now, clock) = clock(NOW);
    let auth = Authenticator::jwt(WORLD, cache, clock);
    let store = store();
    auth.prefetch().await;
    admit(&store, &auth, signer.sign(&claims(&token(1), 1))).await.expect("the fetched key");
    // An unknown kid after the throttle: the refetch fails, the old key stays.
    now.store(NOW + JWKS_REFETCH_MIN_SECS, Ordering::SeqCst);
    admit(&store, &auth, self::signer().sign(&claims(&token(1), 1))).await.unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    admit(&store, &auth, signer.sign(&claims(&token(1), 1))).await.expect("cached keys survive a failed fetch");
}

/// A refetch in flight (an unknown kid, a slow login service) never stalls a
/// connect whose key is already known.
#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_known_kid_does_not_wait_for_a_refetch() {
    use std::time::Duration;
    let signer = signer();
    let jwks = signer.jwks();
    let gate = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicU64::new(0));
    let (fetch_gate, counted) = (gate.clone(), calls.clone());
    let cache = JwksCache::with_fetch(Box::new(move || {
        let first = counted.fetch_add(1, Ordering::SeqCst) == 0;
        let (jwks, gate) = (jwks.clone(), fetch_gate.clone());
        Box::pin(async move {
            if !first {
                gate.notified().await; // the login service hangs
            }
            Ok(jwks)
        })
    }));
    let (now, clock) = clock(NOW);
    let auth = Arc::new(Authenticator::jwt(WORLD, cache, clock));
    let store = store();
    auth.prefetch().await;
    now.store(NOW + JWKS_REFETCH_MIN_SECS, Ordering::SeqCst);

    let stranger = self::signer().sign(&claims(&token(1), 1));
    let unknown = tokio::spawn({
        let (auth, store) = (auth.clone(), store.clone());
        async move { admit(&store, &auth, stranger).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the unknown kid starts a refetch");

    let known = tokio::time::timeout(Duration::from_secs(2), admit(&store, &auth, signer.sign(&claims(&token(2), 1))))
        .await
        .expect("a known kid is admitted while the refetch hangs");
    known.expect("a valid ticket");
    assert!(!unknown.is_finished(), "the refetch is still in flight");

    gate.notify_one();
    let why = unknown.await.unwrap().unwrap_err();
    assert!(why.contains("unknown key"), "{why}");
}

/// An attacker-chosen kid is cut short in the refusal.
#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_cuts_a_long_kid_in_the_refusal() {
    use base64::Engine;
    let signer = signer();
    let auth = jwt_host(signer.jwks());
    let ticket = signer.sign(&claims(&token(1), 1));
    let (_, rest) = ticket.split_once('.').unwrap();
    let kid = "k".repeat(1000);
    let header = format!(r#"{{"alg":"EdDSA","typ":"JWT","kid":"{kid}"}}"#);
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header);
    let why = admit(&store(), &auth, format!("{header}.{rest}")).await.unwrap_err();
    assert!(why.contains("unknown key"), "{why}");
    assert!(why.contains(&"k".repeat(MAX_KID_SHOWN_CHARS)), "{why}");
    assert!(!why.contains(&"k".repeat(MAX_KID_SHOWN_CHARS + 1)), "{why}");
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_refuses_older_epoch_after_newer() {
    let signer = signer();
    let auth = jwt_host(signer.jwks());
    let store = store();
    let older = signer.sign(&claims(&token(1), 1));
    admit(&store, &auth, signer.sign(&claims(&token(1), 2))).await.expect("epoch 2");
    let why = admit(&store, &auth, older).await.unwrap_err();
    assert!(why.contains("superseded by a newer login"), "{why}");
    // Epochs are per player: another player's epoch 1 is fine.
    admit(&store, &auth, signer.sign(&claims(&token(2), 1))).await.expect("another player");
}

/// Only a ticket that passed every check records its epoch.
#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_bad_ticket_does_not_raise_the_epoch() {
    let signer = signer();
    let auth = jwt_host(signer.jwks());
    let store = store();
    let mut elsewhere = claims(&token(1), 9);
    elsewhere.world = "beta".into();
    admit(&store, &auth, signer.sign(&elsewhere)).await.unwrap_err();
    let mut expired = claims(&token(1), 8);
    expired.exp = NOW - JOIN_JWT_TTL_SECS;
    admit(&store, &auth, signer.sign(&expired)).await.unwrap_err();
    let mut bad_token = claims(&token(1), 7);
    bad_token.player_token = "NOT-HEX".into();
    admit(&store, &auth, signer.sign(&bad_token)).await.unwrap_err();
    admit(&store, &auth, signer.sign(&claims(&token(1), 2))).await.expect("epoch 2 is still the newest");
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_refuses_a_malformed_player_token() {
    let signer = signer();
    let auth = jwt_host(signer.jwks());
    let mut bad = claims(&token(1), 1);
    bad.player_token = "0011".into();
    let why = admit(&store(), &auth, signer.sign(&bad)).await.unwrap_err();
    assert!(why.contains("player token"), "{why}");
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_refuses_an_oversized_ticket() {
    let auth = jwt_host(signer().jwks());
    let why = admit(&store(), &auth, "a".repeat(game::MAX_JWT_BYTES + 1)).await.unwrap_err();
    assert!(why.contains("too large"), "{why}");
    assert_eq!(auth.jwks().unwrap().fetches(), 0, "nothing was parsed");
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_host_refuses_a_player_name() {
    let auth = jwt_host(signer().jwks());
    let why = resolve_hello(&store(), &auth, hello_name("bob")).await.unwrap_err();
    assert_eq!(why, "this server requires login");
}

#[tokio::test(flavor = "multi_thread")]
async fn local_host_refuses_jwt() {
    let signer = signer();
    let auth = Authenticator::local();
    let why = admit(&store(), &auth, signer.sign(&claims(&token(1), 1))).await.unwrap_err();
    assert!(why.contains("single-player / dev"), "{why}");
}

#[tokio::test(flavor = "multi_thread")]
async fn local_host_admits_any_valid_name() {
    let auth = Authenticator::local();
    let store = store();
    let (a, record) = resolve_hello(&store, &auth, hello_name("bob")).await.expect("a valid name");
    assert!(record.is_none());
    // The same name is the same player (no secret: Factorio's LAN model).
    let (b, _) = resolve_hello(&store, &auth, hello_name("bob")).await.unwrap();
    assert_eq!(a, b);
    // The server re-validates: the wire form bypasses `PlayerName::parse`.
    let (c, _) = resolve_hello(&store, &auth, hello_name("  bob ")).await.unwrap();
    assert_eq!(a, c, "trimmed on the server too");
    let (d, _) = resolve_hello(&store, &auth, hello_name("Żółw")).await.unwrap();
    assert_ne!(a, d);
    for bad in ["", "   ", "a\u{0}b", &"x".repeat(game::PLAYER_NAME_MAX_CHARS + 1)] {
        let why = resolve_hello(&store, &auth, hello_name(bad)).await.unwrap_err();
        assert!(why.contains("invalid player name"), "{bad:?}: {why}");
    }
    let why = resolve_hello(&store, &auth, ClientPacket::RequestPlayerRegion).await.unwrap_err();
    assert!(why.contains("Hello"), "{why}");
}

fn mode_of(vars: &[(&str, &str)]) -> Result<AuthMode, String> {
    AuthMode::from_lookup(|name| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string()))
}

#[test]
fn auth_mode_from_env() {
    assert_eq!(mode_of(&[]), Ok(AuthMode::Local));
    assert_eq!(mode_of(&[("VOXEL_AUTH", "")]), Ok(AuthMode::Local));
    assert_eq!(mode_of(&[("VOXEL_AUTH", "local")]), Ok(AuthMode::Local));
    assert_eq!(mode_of(&[("VOXEL_AUTH", "token")]), Ok(AuthMode::Local), "the old name is an alias");
    assert_eq!(
        mode_of(&[("VOXEL_AUTH", "jwt"), ("VOXEL_WORLD_ID", "dev"), ("VOXEL_LOGIN_URL", "http://127.0.0.1:8790")]),
        Ok(AuthMode::Jwt { world: "dev".into(), login_url: "http://127.0.0.1:8790".into() })
    );
    let why = mode_of(&[("VOXEL_AUTH", "jwt"), ("VOXEL_LOGIN_URL", "http://x")]).unwrap_err();
    assert!(why.contains("VOXEL_WORLD_ID"), "{why}");
    let why = mode_of(&[("VOXEL_AUTH", "jwt"), ("VOXEL_WORLD_ID", "dev"), ("VOXEL_LOGIN_URL", "")]).unwrap_err();
    assert!(why.contains("VOXEL_LOGIN_URL"), "{why}");
    let why = mode_of(&[("VOXEL_AUTH", "password")]).unwrap_err();
    assert!(why.contains("VOXEL_AUTH"), "{why}");
}

fn resolved(single_player: bool, vars: &[(&str, &str)]) -> Result<(AuthMode, Option<String>), String> {
    AuthMode::resolve(single_player, |name| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string()))
}

/// A single-player server inherits the client's environment: an exported
/// `VOXEL_AUTH=jwt` must not make it need a login service.
#[test]
fn single_player_is_always_local_mode() {
    let jwt = [("VOXEL_AUTH", "jwt"), ("VOXEL_WORLD_ID", "dev"), ("VOXEL_LOGIN_URL", "http://127.0.0.1:8790")];
    let (mode, warning) = resolved(true, &jwt).unwrap();
    assert_eq!(mode, AuthMode::Local);
    assert!(warning.expect("the override is reported").contains("VOXEL_AUTH"));
    // Even an incomplete or unknown setting does not stop it.
    assert_eq!(resolved(true, &[("VOXEL_AUTH", "jwt")]).unwrap().0, AuthMode::Local);
    assert_eq!(resolved(true, &[("VOXEL_AUTH", "password")]).unwrap().0, AuthMode::Local);
    assert_eq!(resolved(true, &[]).unwrap(), (AuthMode::Local, None));
    assert_eq!(resolved(true, &[("VOXEL_AUTH", "token")]).unwrap(), (AuthMode::Local, None));
    assert_eq!(resolved(true, &[("VOXEL_AUTH", "local")]).unwrap(), (AuthMode::Local, None));
    // A dedicated server honours it.
    assert_eq!(
        resolved(false, &jwt).unwrap(),
        (AuthMode::Jwt { world: "dev".into(), login_url: "http://127.0.0.1:8790".into() }, None)
    );
    assert!(resolved(false, &[("VOXEL_AUTH", "jwt")]).is_err());
}

/// The production fetch: `GET {login_url}/.well-known/jwks.json` over HTTP.
#[tokio::test(flavor = "multi_thread")]
async fn jwks_is_fetched_from_the_login_service() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let signer = signer();
    let body = serde_json::to_string(&signer.jwks()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0u8; 4096];
        let n = socket.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..n]).to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        request
    });
    let auth = Authenticator::jwt(WORLD, JwksCache::from_login(&format!("http://{addr}/")), clock(NOW).1);
    admit(&store(), &auth, signer.sign(&claims(&token(1), 1))).await.expect("the fetched key verifies");
    let request = served.await.unwrap();
    assert!(request.starts_with("GET /.well-known/jwks.json "), "{request}");
}

/// End to end: a `jwt` host refuses a player-name `Hello` over real QUIC with
/// `Refused` and closes the connection; the world never hears of it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "slow: >100ms — run via just test-full"]
async fn jwt_host_refuses_name_hello_over_quic() {
    use std::time::Duration;
    let port = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let auth = Arc::new(jwt_host(signer().jwks()));
    let (event_send, event_recv) = crossbeam::channel::unbounded();
    let (_server_send, server_recv) = crossbeam::channel::unbounded();
    let mut ingress = server::WorldIngress::new("127.0.0.1".parse().unwrap(), port, false, store(), auth);
    tokio::spawn(async move { ingress.listen(event_send, server_recv).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let connection = quic::connect(port).await;
    let mut hello = connection.open_uni().await.unwrap();
    hello.write_all(&game::encode_packet(&hello_name("bob")).unwrap()).await.unwrap();
    hello.finish().unwrap();
    let mut answer = tokio::time::timeout(Duration::from_secs(5), connection.accept_uni())
        .await
        .expect("an answer")
        .expect("a stream");
    let bytes = answer.read_to_end(4096).await.unwrap();
    let packet: game::ServerPacket = game::decode_packet(&bytes).unwrap();
    assert!(
        matches!(&packet, game::ServerPacket::Refused(why) if why == "this server requires login"),
        "got {packet:?}"
    );
    tokio::time::timeout(Duration::from_secs(5), connection.closed()).await.expect("the server closes");
    assert!(event_recv.try_recv().is_err(), "a refused connection is never announced");
}

/// A test QUIC client that accepts any certificate (the server's is self-signed).
mod quic {
    use std::sync::Arc;

    use quinn::crypto::rustls::QuicClientConfig;
    use quinn::rustls::{
        self,
        client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        pki_types::{CertificateDer, ServerName, UnixTime},
        DigitallySignedStruct, SignatureScheme,
    };

    #[derive(Debug)]
    struct AnyCert(Arc<rustls::crypto::CryptoProvider>);

    impl ServerCertVerifier for AnyCert {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
        }
        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    pub async fn connect(port: u16) -> quinn::Connection {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let crypto = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCert(provider)))
            .with_no_client_auth();
        let config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto).unwrap()));
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(config);
        endpoint
            .connect(format!("127.0.0.1:{port}").parse().unwrap(), "localhost")
            .unwrap()
            .await
            .expect("connect")
    }
}
