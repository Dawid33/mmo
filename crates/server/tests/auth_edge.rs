//! The connect edge's credential check (`VOXEL_AUTH`): a `jwt` host admits a
//! connection only with a valid login ticket for its world, a `token` host
//! (single-player / dev) only with a player token. Driven through
//! `player_store::resolve_hello` — the very function both ingress paths call —
//! plus one end-to-end refusal over real QUIC.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use auth::{Jwks, Signer, SigningKey};
use game::{ClientPacket, Credential, PlayerId, PlayerToken};
use login_api::{JoinClaims, HOST_AUDIENCE, JOIN_JWT_TTL_SECS};
use server::auth_edge::{AuthMode, Authenticator, Clock, JwksCache, JWKS_REFETCH_MIN_SECS};
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

fn hello_token(token: PlayerToken) -> ClientPacket {
    ClientPacket::Hello { credential: Credential::Token(token) }
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
async fn jwt_host_refuses_token_credential() {
    let auth = jwt_host(signer().jwks());
    let why = resolve_hello(&store(), &auth, hello_token(token(1))).await.unwrap_err();
    assert_eq!(why, "this server requires login");
}

#[tokio::test(flavor = "multi_thread")]
async fn token_host_refuses_jwt() {
    let signer = signer();
    let auth = Authenticator::token();
    let why = admit(&store(), &auth, signer.sign(&claims(&token(1), 1))).await.unwrap_err();
    assert!(why.contains("single-player / dev"), "{why}");
}

#[tokio::test(flavor = "multi_thread")]
async fn token_host_unchanged() {
    let auth = Authenticator::token();
    let store = store();
    let (a, record) = resolve_hello(&store, &auth, hello_token(token(1))).await.expect("a valid token");
    assert!(record.is_none());
    let (b, _) = resolve_hello(&store, &auth, hello_token(token(1))).await.unwrap();
    assert_eq!(a, b);
    let why = resolve_hello(&store, &auth, hello_token(PlayerToken("nope".into()))).await.unwrap_err();
    assert!(why.contains("invalid player token"), "{why}");
    let why = resolve_hello(&store, &auth, ClientPacket::RequestPlayerRegion).await.unwrap_err();
    assert!(why.contains("Hello"), "{why}");
}

fn mode_of(vars: &[(&str, &str)]) -> Result<AuthMode, String> {
    AuthMode::from_lookup(|name| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string()))
}

#[test]
fn auth_mode_from_env() {
    assert_eq!(mode_of(&[]), Ok(AuthMode::Token));
    assert_eq!(mode_of(&[("VOXEL_AUTH", "")]), Ok(AuthMode::Token));
    assert_eq!(mode_of(&[("VOXEL_AUTH", "token")]), Ok(AuthMode::Token));
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

/// End to end: a `jwt` host refuses a token `Hello` over real QUIC with
/// `Refused` and closes the connection; the world never hears of it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "slow: >100ms — run via just test-full"]
async fn jwt_host_refuses_token_hello_over_quic() {
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
    hello.write_all(&game::encode_packet(&hello_token(token(1))).unwrap()).await.unwrap();
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
