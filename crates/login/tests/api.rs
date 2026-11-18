//! The HTTP API end to end, against `router()` on an in-memory SQLite store.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use auth::{Jwks, Signer, SigningKey, Verifier};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use login::config::OidcConfig;
use login::oidc::OidcVerifier;
use login::{AppState, LoginConfig, LoginStore, WorldRow, router};
use login_api::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tower::ServiceExt;

const T0: u64 = 1_800_000_000;
const EMAIL: &str = "Dwarf@Example.org";
const ORIGIN_OK: &str = "http://127.0.0.1:1334";
const BODY_LIMIT: usize = 1 << 20;

pub(crate) struct App {
    pub(crate) router: Router,
    clock: Arc<AtomicU64>,
    store: LoginStore,
}

/// An app whose state `adjust` changes (host links, identity provider).
pub(crate) async fn app_from(adjust: impl FnOnce(AppState) -> AppState) -> App {
    let store = LoginStore::open("sqlite::memory:").await.unwrap();
    for (id, status) in [("w1", WorldStatus::Open), ("w2", WorldStatus::Open), ("shut", WorldStatus::Closed)] {
        store
            .put_world(&WorldRow {
                world_id: id.into(),
                name: format!("World {id}"),
                status,
                quic: Some(format!("{id}.example:6466")),
                webtransport: Some(format!("https://{id}.example:6467/")),
                cert_hash: None,
                created_at: 1,
                host_link: None,
                host_link_cert: None,
            })
            .await
            .unwrap();
    }
    let config = LoginConfig::from_lookup(|_| None).unwrap();
    let clock = Arc::new(AtomicU64::new(T0));
    let c = clock.clone();
    let (key, _pem) = SigningKey::generate();
    let state = AppState::new(
        store.clone(),
        Arc::new(Signer::new(vec![key]).ok().unwrap()),
        Arc::new(config),
        Arc::new(move || c.load(Ordering::SeqCst)),
    );
    App { router: router(adjust(state)), clock, store }
}

pub(crate) async fn app() -> App {
    app_from(|state| state).await
}

pub(crate) struct Resp {
    pub(crate) status: StatusCode,
    pub(crate) headers: axum::http::HeaderMap,
    pub(crate) body: Vec<u8>,
}

impl Resp {
    pub(crate) fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub(crate) fn error(&self) -> ApiErrorCode {
        self.json::<ApiError>().code
    }
}

impl App {
    pub(crate) async fn send(&self, req: Request<Body>) -> Resp {
        let r = self.router.clone().oneshot(req).await.unwrap();
        let status = r.status();
        let headers = r.headers().clone();
        let body = to_bytes(r.into_body(), BODY_LIMIT).await.unwrap().to_vec();
        Resp { status, headers, body }
    }

    pub(crate) async fn post<B: Serialize>(&self, path: &str, body: &B) -> Resp {
        let req = Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap();
        self.send(req).await
    }

    pub(crate) async fn get(&self, path: &str) -> Resp {
        self.send(Request::get(path).body(Body::empty()).unwrap()).await
    }

    /// A new refresh token (a new family) of the account `EMAIL`, made verified when missing:
    /// what a login with the identity provider hands out.
    pub(crate) async fn account(&self) -> String {
        let email = login::normalize_email(EMAIL).unwrap();
        let id = self.store.ensure_verified_account(&email, self.now() as i64).await.unwrap();
        login::api::mint_refresh(&self.store, id, self.now() as i64).await.unwrap()
    }

    pub(crate) async fn join(&self, refresh: &str, world: &str) -> Resp {
        self.post(PATH_JOIN, &JoinRequest { refresh_token: refresh.into(), world: world.into() }).await
    }

    async fn verifier(&self) -> Verifier {
        let r = self.get(PATH_JWKS).await;
        assert_eq!(r.status, StatusCode::OK);
        Verifier::from_jwks(&r.json::<Jwks>())
    }

    pub(crate) fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn join_happy_path() {
    let a = app().await;
    let refresh = a.account().await;
    let me = a.store.account_by_email("dwarf@example.org").await.unwrap().unwrap();
    assert!(me.email_verified_at.is_some());

    let worlds = a.get(PATH_WORLDS).await;
    assert_eq!(worlds.status, StatusCode::OK);
    let worlds: Vec<WorldEntry> = worlds.json();
    assert!(worlds.iter().any(|w| w.id == "w1" && w.name == "World w1" && w.status == WorldStatus::Open));
    assert!(worlds.iter().any(|w| w.id == "shut" && w.status == WorldStatus::Closed));

    let r = a.join(&refresh, "w1").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j: JoinResponse = r.json();
    assert_ne!(j.refresh_token, refresh, "join rotates the refresh token");
    assert_eq!(j.ticket.world, "w1");
    assert_eq!(j.ticket.quic.as_deref(), Some("w1.example:6466"));
    assert_eq!(j.ticket.webtransport.as_deref(), Some("https://w1.example:6467/"));
    assert_eq!(j.ticket.expires_in_secs, JOIN_JWT_TTL_SECS);
    let claims = a.verifier().await.verify(&j.ticket.jwt, a.now(), "w1").unwrap();
    assert_eq!(claims.sub, me.account_id.to_string());
    assert_eq!(claims.session_epoch, 1);
    assert_eq!(claims.aud, HOST_AUDIENCE);
    assert_eq!((claims.iat, claims.exp), (a.now(), a.now() + JOIN_JWT_TTL_SECS));
    let stored = a.store.player_token_for(me.account_id, "w1", 0, || unreachable!()).await.unwrap();
    assert_eq!(claims.player_token, stored);
    assert_eq!(stored.len(), 32);
    assert!(stored.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    // The ticket expires.
    assert!(a.verifier().await.verify(&j.ticket.jwt, a.now() + JOIN_JWT_TTL_SECS + 31, "w1").is_err());
    // The old refresh token is spent; the rotated one works.
    assert_eq!(a.post(PATH_REFRESH, &RefreshRequest { refresh_token: j.refresh_token.clone() }).await.status, StatusCode::OK);
    assert_eq!(a.get("/healthz").await.status, StatusCode::OK);
}

#[tokio::test]
async fn second_join_bumps_epoch_same_world() {
    let a = app().await;
    let refresh = a.account().await;
    let v = a.verifier().await;
    let j1: JoinResponse = a.join(&refresh, "w1").await.json();
    let j2: JoinResponse = a.join(&j1.refresh_token, "w1").await.json();
    let c1 = v.verify(&j1.ticket.jwt, a.now(), "w1").unwrap();
    let c2 = v.verify(&j2.ticket.jwt, a.now(), "w1").unwrap();
    assert_eq!((c1.session_epoch, c2.session_epoch), (1, 2));
    assert_eq!(c1.player_token, c2.player_token, "one player per (account, world)");
}

#[tokio::test]
async fn two_worlds_independent() {
    let a = app().await;
    let refresh = a.account().await;
    let v = a.verifier().await;
    let j1: JoinResponse = a.join(&refresh, "w1").await.json();
    let j2: JoinResponse = a.join(&j1.refresh_token, "w2").await.json();
    let c1 = v.verify(&j1.ticket.jwt, a.now(), "w1").unwrap();
    let c2 = v.verify(&j2.ticket.jwt, a.now(), "w2").unwrap();
    assert_eq!((c1.session_epoch, c2.session_epoch), (1, 1));
    assert_ne!(c1.player_token, c2.player_token);
    assert!(v.verify(&j1.ticket.jwt, a.now(), "w2").is_err());
    assert!(v.verify(&j2.ticket.jwt, a.now(), "w1").is_err());
}

#[tokio::test]
async fn refresh_reuse_revokes_family() {
    let a = app().await;
    let t0 = a.account().await;
    let first = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: t0.clone() }).await;
    assert_eq!(first.status, StatusCode::OK);
    let t1 = first.json::<LoginResponse>().refresh_token;
    let again = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: t0 }).await;
    assert_eq!((again.status, again.error()), (StatusCode::UNAUTHORIZED, ApiErrorCode::Revoked));
    let t1r = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: t1 }).await;
    assert_eq!((t1r.status, t1r.error()), (StatusCode::UNAUTHORIZED, ApiErrorCode::Revoked));
    // Garbage is refused the same way, and a malformed body is a JSON 400.
    let junk = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: "junk".into() }).await;
    assert_eq!(junk.status, StatusCode::UNAUTHORIZED);
    let bad = a
        .send(
            Request::post(PATH_REFRESH)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{not json"))
                .unwrap(),
        )
        .await;
    assert_eq!((bad.status, bad.error()), (StatusCode::BAD_REQUEST, ApiErrorCode::BadRequest));
    // Logout of a fresh family: its token stops working.
    let t = a.account().await;
    let out = a.post(PATH_LOGOUT, &RefreshRequest { refresh_token: t.clone() }).await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    assert_eq!(a.post(PATH_REFRESH, &RefreshRequest { refresh_token: t }).await.status, StatusCode::UNAUTHORIZED);
    // Refresh tokens expire.
    let t = a.account().await;
    a.clock.fetch_add(REFRESH_TTL_DAYS * 86_400 + 1, Ordering::SeqCst);
    assert_eq!(a.post(PATH_REFRESH, &RefreshRequest { refresh_token: t }).await.status, StatusCode::UNAUTHORIZED);
}

/// Keycloak owns passwords: the service has no register / password-login / verify / reset route.
#[tokio::test]
async fn password_routes_are_gone() {
    let a = app().await;
    for path in ["/v1/login", "/v1/register", "/v1/reset/request", "/v1/reset/confirm", "/v1/verify"] {
        let r = a.post(path, &serde_json::json!({ "email": EMAIL, "password": "correct horse battery" })).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "POST {path}");
    }
    for path in ["/verify?token=x", "/reset?token=x"] {
        assert_eq!(a.get(path).await.status, StatusCode::NOT_FOUND, "GET {path}");
    }
}

#[tokio::test]
async fn join_closed_world_403() {
    let a = app().await;
    let refresh = a.account().await;
    let r = a.join(&refresh, "shut").await;
    assert_eq!((r.status, r.error()), (StatusCode::FORBIDDEN, ApiErrorCode::WorldClosed));
    // The refresh token was not spent.
    assert_eq!(a.join(&refresh, "w1").await.status, StatusCode::OK);
}

#[tokio::test]
async fn join_unknown_world_404() {
    let a = app().await;
    let refresh = a.account().await;
    let r = a.join(&refresh, "nowhere").await;
    assert_eq!((r.status, r.error()), (StatusCode::NOT_FOUND, ApiErrorCode::UnknownWorld));
    assert_eq!(a.join(&refresh, "w1").await.status, StatusCode::OK);
    // A spent token cannot join.
    let r = a.join(&refresh, "w1").await;
    assert_eq!((r.status, r.error()), (StatusCode::UNAUTHORIZED, ApiErrorCode::Revoked));
}

async fn preflight(a: &App, origin: &str) -> Resp {
    a.send(
        Request::builder()
            .method(Method::OPTIONS)
            .uri(PATH_REFRESH)
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn cors_allows_configured_origin_only() {
    let a = app().await;
    let ok = preflight(&a, ORIGIN_OK).await;
    assert_eq!(
        ok.headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).map(|v| v.to_str().unwrap()),
        Some(ORIGIN_OK)
    );
    let methods = ok.headers.get(header::ACCESS_CONTROL_ALLOW_METHODS).unwrap().to_str().unwrap();
    assert!(methods.contains("POST") && methods.contains("GET"), "{methods}");
    let evil = preflight(&a, "http://evil").await;
    assert!(evil.headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
}

#[test]
fn config_defaults_and_overrides() {
    let d = LoginConfig::from_lookup(|_| None).unwrap();
    assert_eq!(d.bind.to_string(), "127.0.0.1:8790");
    assert_eq!(d.db_url, "target/dev-login/login.sqlite");
    assert_eq!(d.keys_dir, std::path::PathBuf::from("target/dev-login/keys"));
    assert_eq!(d.cors_origins, vec![ORIGIN_OK.to_string(), "http://localhost:1334".to_string()]);
    assert_eq!(d.oidc, None);
    let oidc = LoginConfig::from_lookup(|k| (k == "VOXEL_OIDC_ISSUER").then(|| "https://auth.example/realms/mmo".into())).unwrap();
    assert_eq!(oidc.oidc, Some(oidc_config("https://auth.example/realms/mmo")));
    let env = |k: &str| {
        match k {
            "VOXEL_LOGIN_BIND" => Some("0.0.0.0:9000"),
            "VOXEL_LOGIN_CORS_ORIGINS" => Some("https://a.example, https://b.example"),
            _ => None,
        }
        .map(String::from)
    };
    let c = LoginConfig::from_lookup(env).unwrap();
    assert_eq!(c.bind.to_string(), "0.0.0.0:9000");
    assert_eq!(c.cors_origins, vec!["https://a.example".to_string(), "https://b.example".to_string()]);
    // A bad bind address is refused.
    assert!(LoginConfig::from_lookup(|k| (k == "VOXEL_LOGIN_BIND").then(|| "nope".to_string())).is_err());
}

// ---- Host links: SpawnPlayer / Kick / SessionEnded (R2b) --------------------------------------

mod host_link {
    use std::sync::Mutex;
    use std::time::Duration;

    use crossbeam::channel::Receiver;
    use login::host_link::{HostLinks, SpawnPlace};
    use svc_link::{LinkEvent, LinkListener, PeerId, PeerRole, PeerSenders};

    use super::*;

    const SERVICE_TOKEN: &str = "test-service-token";
    /// How long a test waits for something to arrive over a real link.
    const LINK_WAIT: Duration = Duration::from_secs(5);

    /// How a fake host answers a spawn order: `Some(ok)` acks, `None` stays silent.
    type AckPolicy = Box<dyn Fn(&str, u64) -> Option<bool> + Send>;

    /// A world's login link on localhost (its coordinator's, or a role-`all` server's) for ONE
    /// world id (the hello names it, Elastic 1 Task 6): records every message, acks by policy.
    struct FakeHost {
        world_id: &'static str,
        link: String,
        cert: String,
        seen: Arc<Mutex<Vec<LoginToHost>>>,
        peers: PeerSenders<HostToLogin>,
        listener: Option<LinkListener>,
        /// Login services linked to it now.
        connected: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FakeHost {
        fn start(world_id: &'static str, ack: AckPolicy) -> FakeHost {
            Self::start_with(world_id, "127.0.0.1:0", None, ack)
        }

        /// On `bind`, with its cert kept beside `pin_file` (a restart keeps its pin).
        fn start_with(world_id: &'static str, bind: &str, pin_file: Option<std::path::PathBuf>, ack: AckPolicy) -> FakeHost {
            Self::start_placing(world_id, bind, pin_file, ack, SpawnPlace::default())
        }

        /// [`Self::start_with`], its acks naming `place` (another host of the world took the
        /// order).
        fn start_placing(
            world_id: &'static str,
            bind: &str,
            pin_file: Option<std::path::PathBuf>,
            ack: AckPolicy,
            place: SpawnPlace,
        ) -> FakeHost {
            let (listener, events, peers) = LinkListener::bind::<LoginToHost, HostToLogin>(
                bind.parse().unwrap(),
                SERVICE_TOKEN.into(),
                world_id.into(),
                pin_file,
            )
            .unwrap();
            let seen: Arc<Mutex<Vec<LoginToHost>>> = Arc::default();
            let connected: Arc<std::sync::atomic::AtomicUsize> = Arc::default();
            let (record, answer, count) = (seen.clone(), peers.clone(), connected.clone());
            std::thread::spawn(move || serve(events, answer, record, ack, place, count));
            FakeHost {
                world_id,
                link: listener.local_addr().to_string(),
                cert: listener.cert_sha256().to_string(),
                seen,
                peers,
                listener: Some(listener),
                connected,
            }
        }

        fn acking(world_id: &'static str) -> FakeHost {
            Self::start(world_id, Box::new(|_, _| Some(true)))
        }

        fn world(&self, id: &str) -> WorldRow {
            assert_eq!(id, self.world_id, "a fake host serves one world");
            WorldRow {
                world_id: id.into(),
                name: format!("World {id}"),
                status: WorldStatus::Open,
                quic: Some(format!("{id}.example:6466")),
                webtransport: None,
                cert_hash: None,
                created_at: 1,
                host_link: Some(self.link.clone()),
                host_link_cert: Some(self.cert.clone()),
            }
        }

        fn seen(&self) -> Vec<LoginToHost> {
            self.seen.lock().unwrap().clone()
        }

        /// Wait until `want` holds for what this host has seen.
        async fn wait_seen(&self, want: impl Fn(&[LoginToHost]) -> bool) -> Vec<LoginToHost> {
            let deadline = tokio::time::Instant::now() + LINK_WAIT;
            loop {
                let seen = self.seen();
                if want(&seen) || tokio::time::Instant::now() >= deadline {
                    return seen;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    impl Drop for FakeHost {
        fn drop(&mut self) {
            // The listener owns a multi-thread runtime: drop it off the test's runtime.
            if let Some(listener) = self.listener.take() {
                std::thread::spawn(move || drop(listener)).join().unwrap();
            }
        }
    }

    fn serve(
        events: Receiver<(PeerId, LinkEvent<LoginToHost>)>,
        peers: PeerSenders<HostToLogin>,
        seen: Arc<Mutex<Vec<LoginToHost>>>,
        ack: AckPolicy,
        place: SpawnPlace,
        connected: Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::sync::atomic::Ordering::SeqCst;
        for (peer, event) in events.iter() {
            match event {
                LinkEvent::Up(hello) => {
                    assert_eq!(hello.role, PeerRole::Login);
                    connected.fetch_add(1, SeqCst);
                }
                LinkEvent::Down(_) => {
                    connected.fetch_sub(1, SeqCst);
                }
                LinkEvent::Msg(msg) => {
                    seen.lock().unwrap().push(msg.clone());
                    match msg {
                        LoginToHost::SpawnPlayer { player_token, session_epoch } => {
                            if let Some(ok) = ack(&player_token, session_epoch) {
                                let SpawnPlace { quic, webtransport, cert_hash } = place.clone();
                                peers.send(peer, HostToLogin::SpawnAck { player_token, session_epoch, ok, quic, webtransport, cert_hash });
                            }
                        }
                        // Kicks are always applied and acked.
                        LoginToHost::Kick { player_token, session_epoch, .. } => {
                            peers.send(peer, HostToLogin::KickAck { player_token, session_epoch });
                        }
                    }
                }
            }
        }
    }

    /// An app whose host links dial with the service token; `ack_timeout` bounds each join.
    async fn linked_app(worlds: &[WorldRow], ack_timeout: Duration) -> App {
        let a = app_from(|state| {
            let links = HostLinks::new(Some(SERVICE_TOKEN.into()), state.store.clone(), state.clock.clone())
                .with_ack_timeout(ack_timeout);
            state.with_host_links(links)
        })
        .await;
        for w in worlds {
            a.store.put_world(w).await.unwrap();
        }
        a
    }

    fn epoch_of(a: &App, verifier: &Verifier, j: &JoinResponse) -> (String, u64) {
        let c = verifier.verify(&j.ticket.jwt, a.now(), &j.ticket.world).unwrap();
        (c.player_token, c.session_epoch)
    }

    #[tokio::test]
    async fn join_pushes_spawn_player_and_waits_for_its_ack() {
        let host = FakeHost::acking("lw");
        let a = linked_app(&[host.world("lw")], Duration::from_millis(SPAWN_ACK_TIMEOUT_MS)).await;
        let refresh = a.account().await;
        let r = a.join(&refresh, "lw").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let j: JoinResponse = r.json();
        let (token, epoch) = epoch_of(&a, &a.verifier().await, &j);
        assert_eq!(epoch, 1);
        // The order reached the host before the ticket was handed out.
        assert_eq!(host.seen(), vec![LoginToHost::SpawnPlayer { player_token: token.clone(), session_epoch: 1 }]);
        // A world without a host link pushes nothing (R2a).
        let r = a.join(&j.refresh_token, "w1").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(host.seen().len(), 1);
        // The host reports the session's end: it is no longer open (no kick goes there).
        let account = a.store.account_by_email("dwarf@example.org").await.unwrap().unwrap().account_id;
        assert_eq!(a.store.open_sessions(account).await.unwrap().len(), 2);
        host.peers.broadcast(HostToLogin::SessionEnded { player_token: token, session_epoch: 1 });
        let deadline = tokio::time::Instant::now() + LINK_WAIT;
        while a.store.open_sessions(account).await.unwrap().len() == 2 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let open: Vec<String> = a.store.open_sessions(account).await.unwrap().into_iter().map(|s| s.world_id).collect();
        assert_eq!(open, vec!["w1".to_string()]);
    }

    /// Several hosts: the linked host's ack names the host that took the order; the ticket
    /// sends the client there, not to the world row's address.
    #[tokio::test]
    async fn join_uses_the_acknowledged_endpoint() {
        let place = SpawnPlace {
            quic: Some("host-1.example:7401".into()),
            webtransport: Some("https://host-1.example:7501".into()),
            cert_hash: Some("cd".repeat(32)),
        };
        let host = FakeHost::start_placing("lw", "127.0.0.1:0", None, Box::new(|_, _| Some(true)), place.clone());
        let a = linked_app(&[host.world("lw")], Duration::from_millis(SPAWN_ACK_TIMEOUT_MS)).await;
        let refresh = a.account().await;
        let r = a.join(&refresh, "lw").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let j: JoinResponse = r.json();
        assert_eq!(
            (j.ticket.quic, j.ticket.webtransport, j.ticket.cert_hash),
            (place.quic, place.webtransport, place.cert_hash),
            "the acknowledged host's addresses, not the world row's ({:?})",
            host.world("lw").quic
        );
        // An ack that names nothing keeps the world row's addresses.
        let plain = FakeHost::acking("pw");
        let world = plain.world("pw");
        let b = linked_app(&[world.clone()], Duration::from_millis(SPAWN_ACK_TIMEOUT_MS)).await;
        let refresh = b.account().await;
        let j: JoinResponse = b.join(&refresh, "pw").await.json();
        assert_eq!((j.ticket.quic, j.ticket.webtransport, j.ticket.cert_hash), (world.quic, world.webtransport, world.cert_hash));
    }

    /// The world row has no addresses (registered with `--link` only) and the ack names none: the
    /// ticket would point nowhere, so the join is refused, not answered with an empty ticket.
    #[tokio::test]
    async fn a_spawn_ack_without_an_endpoint_is_world_unavailable() {
        let host = FakeHost::acking("ne");
        let world = WorldRow { quic: None, webtransport: None, cert_hash: None, ..host.world("ne") };
        let a = linked_app(&[world], Duration::from_millis(SPAWN_ACK_TIMEOUT_MS)).await;
        let refresh = a.account().await;
        let r = a.join(&refresh, "ne").await;
        assert_eq!((r.status, r.error()), (StatusCode::SERVICE_UNAVAILABLE, ApiErrorCode::WorldUnavailable));
        // The refresh token was not spent: the client may try again.
        let r = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: refresh }).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }

    #[tokio::test]
    async fn join_fails_503_when_host_link_down() {
        // A link to a port nobody listens on (a pin that matches nothing).
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap().to_string();
        drop(probe);
        let world = WorldRow {
            host_link: Some(addr),
            host_link_cert: Some("ab".repeat(32)),
            ..FakeHost::acking("down").world("down")
        };
        let a = linked_app(&[world], Duration::from_millis(50)).await;
        let refresh = a.account().await;
        let r = a.join(&refresh, "down").await;
        assert_eq!((r.status, r.error()), (StatusCode::SERVICE_UNAVAILABLE, ApiErrorCode::WorldUnavailable));
        // The refresh token was not spent: the client may simply try again.
        let r = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: refresh }).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }

    #[tokio::test]
    async fn join_fails_503_when_the_host_refuses_or_has_no_service_token() {
        let host = FakeHost::start("lw", Box::new(|_, _| Some(false)));
        let a = linked_app(&[host.world("lw")], Duration::from_millis(SPAWN_ACK_TIMEOUT_MS)).await;
        let refresh = a.account().await;
        let r = a.join(&refresh, "lw").await;
        assert_eq!((r.status, r.error()), (StatusCode::SERVICE_UNAVAILABLE, ApiErrorCode::WorldUnavailable));
        // Without a service token a linked world cannot be joined (and nothing is dialed).
        let b = app().await;
        b.store.put_world(&host.world("lw")).await.unwrap();
        let refresh = b.account().await;
        let r = b.join(&refresh, "lw").await;
        assert_eq!((r.status, r.error()), (StatusCode::SERVICE_UNAVAILABLE, ApiErrorCode::WorldUnavailable));
    }

    /// How many pump threads of world `w1` are alive (Linux: thread names in /proc).
    fn w1_pumps() -> usize {
        std::fs::read_dir("/proc/self/task")
            .unwrap()
            .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
            .filter(|name| name.trim() == "host-link-w1")
            .count()
    }

    async fn wait_for(what: &str, mut ok: impl AsyncFnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + LINK_WAIT;
        while !ok().await {
            assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    #[ignore = "slow: >100ms — run via just test-full"]
    async fn host_link_gauge_follows_the_current_link() {
        const UP: &str = "login_host_link_up{world=\"w1\"} 1";
        const DOWN: &str = "login_host_link_up{world=\"w1\"} 0";
        // Host A never acks, so a join waiting on it keeps A's link (and its pump) alive.
        let host_a = FakeHost::start("w1", Box::new(|_, _| None));
        let a = linked_app(&[host_a.world("w1")], Duration::from_millis(600)).await;
        let (first, refresh_b) = (a.account().await, a.account().await);
        let stuck = a.join(&first, "w1");
        let moved = async {
            wait_for("link A up", async || a.get("/metrics").await.text().contains(UP)).await;
            // The world moves to host B: the replacement link reports from now on.
            let host_b = FakeHost::acking("w1");
            a.store.put_world(&host_b.world("w1")).await.unwrap();
            assert_eq!(a.join(&refresh_b, "w1").await.status, StatusCode::OK);
            assert!(a.get("/metrics").await.text().contains(UP));
            host_b
        };
        let (stuck, host_b) = tokio::join!(stuck, moved);
        // The join on A gave up, so A's replaced pump ends AFTER B is up: it must not read as down.
        assert_eq!(stuck.status, StatusCode::SERVICE_UNAVAILABLE);
        drop(host_a);
        wait_for("A's pump to end", async || w1_pumps() == 1).await;
        let body = a.get("/metrics").await.text();
        assert!(body.contains(UP), "{body}");
        // The world loses its host link: the series reads down, once.
        let mut row = host_b.world("w1");
        (row.host_link, row.host_link_cert) = (None, None);
        a.store.put_world(&row).await.unwrap();
        assert_eq!(a.join(&a.account().await, "w1").await.status, StatusCode::OK);
        let body = a.get("/metrics").await.text();
        assert!(body.contains(DOWN), "{body}");
    }

    #[tokio::test]
    #[ignore = "slow: >100ms — run via just test-full"]
    async fn late_ack_never_answers_a_later_join() {
        // The host acks nothing on its own; the test sends acks by hand.
        let host = FakeHost::start("lw", Box::new(|_, _| None));
        let a = linked_app(&[host.world("lw")], Duration::from_millis(300)).await;
        let refresh = a.account().await;
        let r = a.join(&refresh, "lw").await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
        let seen = host.wait_seen(|s| !s.is_empty()).await;
        let LoginToHost::SpawnPlayer { player_token, session_epoch } = seen[0].clone() else { panic!("{seen:?}") };
        assert_eq!(session_epoch, 1);
        // Epoch 1's ack arrives late, while the next join (epoch 2) waits: it must not count.
        let (peers, token) = (host.peers.clone(), player_token.clone());
        let late = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            peers.broadcast(HostToLogin::SpawnAck {
                player_token: token,
                session_epoch: 1,
                ok: true,
                quic: None,
                webtransport: None,
                cert_hash: None,
            });
        });
        let r = a.join(&refresh, "lw").await;
        late.await.unwrap();
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
        assert!(host.seen().contains(&LoginToHost::SpawnPlayer { player_token, session_epoch: 2 }));
    }

    #[tokio::test]
    async fn logout_all_sends_kick_to_each_world_with_a_session() {
        let (h1, h2, h3) = (FakeHost::acking("a"), FakeHost::acking("b"), FakeHost::acking("c"));
        let a = linked_app(&[h1.world("a"), h2.world("b"), h3.world("c")], Duration::from_millis(SPAWN_ACK_TIMEOUT_MS)).await;
        let refresh = a.account().await;
        let v = a.verifier().await;
        let ja: JoinResponse = a.join(&refresh, "a").await.json();
        let jb: JoinResponse = a.join(&ja.refresh_token, "b").await.json();
        let jw: JoinResponse = a.join(&jb.refresh_token, "w1").await.json();
        let (ta, tb) = (epoch_of(&a, &v, &ja).0, epoch_of(&a, &v, &jb).0);
        // A plain logout kicks nobody.
        let other = a.account().await;
        let out = a.post(PATH_LOGOUT, &LogoutRequest { refresh_token: other, all: false }).await;
        assert_eq!(out.status, StatusCode::NO_CONTENT);
        let out = a.post(PATH_LOGOUT, &LogoutRequest { refresh_token: jw.refresh_token.clone(), all: true }).await;
        assert_eq!(out.status, StatusCode::NO_CONTENT);
        let kick = |token: &str| LoginToHost::Kick {
            player_token: token.into(),
            session_epoch: 1,
            reason: login::api::KICK_LOGOUT_ALL.into(),
        };
        assert!(h1.wait_seen(|s| s.contains(&kick(&ta))).await.contains(&kick(&ta)));
        assert!(h2.wait_seen(|s| s.contains(&kick(&tb))).await.contains(&kick(&tb)));
        assert!(h3.seen().is_empty(), "no session in world c: {:?}", h3.seen());
        // Every refresh token of the account is revoked.
        let r = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: jw.refresh_token }).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        // The hosts' acks clear the queue.
        assert!(wait_no_pending_kicks(&a).await, "{:?}", a.store.pending_kicks().await.unwrap());
    }

    /// C-I4: logout-all with a token already rotated (a thief may hold the live one) still
    /// signs out everywhere: every session is kicked.
    #[tokio::test]
    async fn logout_all_with_a_reused_token_still_kicks() {
        let host = FakeHost::acking("a");
        let a = linked_app(&[host.world("a")], Duration::from_millis(SPAWN_ACK_TIMEOUT_MS)).await;
        let refresh = a.account().await;
        let j: JoinResponse = a.join(&refresh, "a").await.json(); // rotates `refresh`
        let token = epoch_of(&a, &a.verifier().await, &j).0;
        let out = a.post(PATH_LOGOUT, &LogoutRequest { refresh_token: refresh, all: true }).await;
        assert_eq!(out.status, StatusCode::NO_CONTENT);
        let kick = LoginToHost::Kick { player_token: token, session_epoch: 1, reason: login::api::KICK_LOGOUT_ALL.into() };
        assert!(host.wait_seen(|s| s.contains(&kick)).await.contains(&kick), "{:?}", host.seen());
        let r = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: j.refresh_token }).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "the live token is revoked too");
    }

    /// Wait until every kick was acknowledged; whether it was.
    async fn wait_no_pending_kicks(a: &App) -> bool {
        let deadline = tokio::time::Instant::now() + LINK_WAIT * 2;
        while !a.store.pending_kicks().await.unwrap().is_empty() {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        true
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "slow: >100ms — run via just test-full"]
    async fn kick_sent_while_the_link_is_down_goes_out_when_it_comes_back() {
        // A host that has run before (its pin is known), down now.
        let dir = std::env::temp_dir().join(format!("login-kick-durable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pin = dir.join("host.cert-sha256");
        let first = FakeHost::start_with("a", "127.0.0.1:0", Some(pin.clone()), Box::new(|_, _| Some(true)));
        let (bind, world) = (first.link.clone(), first.world("a"));
        drop(first);
        let a = linked_app(&[world], Duration::from_millis(SPAWN_ACK_TIMEOUT_MS)).await;
        let refresh = a.account().await;
        // The account has a session there (its join happened while the host was up).
        let account = a.store.account_by_email("dwarf@example.org").await.unwrap().unwrap().account_id;
        let token = a.store.player_token_for(account, "a", 0, || "ab".repeat(16)).await.unwrap();
        assert_eq!(a.store.bump_session(account, "a", 0).await.unwrap(), 1);
        // Signed out everywhere while the host is down: the kick waits, durably.
        let out = a.post(PATH_LOGOUT, &LogoutRequest { refresh_token: refresh, all: true }).await;
        assert_eq!(out.status, StatusCode::NO_CONTENT);
        tokio::time::sleep(Duration::from_millis(300)).await; // the sweeper dials and fails
        assert_eq!(a.store.pending_kicks().await.unwrap().len(), 1, "the kick is durable");
        // The host comes back on the same address and pin: the kick goes out and is acked.
        let host = FakeHost::start_with("a", &bind, Some(pin), Box::new(|_, _| Some(true)));
        let kick = LoginToHost::Kick { player_token: token, session_epoch: 1, reason: login::api::KICK_LOGOUT_ALL.into() };
        let deadline = tokio::time::Instant::now() + LINK_WAIT * 2;
        while !host.seen().contains(&kick) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(host.seen().contains(&kick), "{:?}", host.seen());
        assert!(wait_no_pending_kicks(&a).await, "the ack deletes it");
        drop(host);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Elastic 2b Task 8: the login service holds a link per address of the world's
    /// `host_link` (each a coordinator replica, one shared identity), re-read at every join: a
    /// new address is dialled, a gone one's link is retired — its dialer stops, so that replica
    /// sees the login service leave.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "slow: >100ms — run via just test-full"]
    async fn a_new_replica_address_is_dialled_and_a_gone_one_retired() {
        use std::sync::atomic::Ordering::SeqCst;
        let dir = std::env::temp_dir().join(format!("login-replicas-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pin = dir.join("replica.cert-sha256");
        let replica = || FakeHost::start_with("r", "127.0.0.1:0", Some(pin.clone()), Box::new(|_, _| Some(true)));
        let (ra, rb, rc) = (replica(), replica(), replica());
        assert!(ra.cert == rb.cert && rb.cert == rc.cert, "one identity");
        let mut row = ra.world("r");
        row.host_link = Some(format!("{},{}", ra.link, rb.link));
        let mut held = None;
        let a = app_from(|state| {
            let links = HostLinks::new(Some(SERVICE_TOKEN.into()), state.store.clone(), state.clock.clone());
            let state = state.with_host_links(links);
            held = Some(state.host_links.clone());
            state
        })
        .await;
        let links = held.expect("the app's host links");
        a.store.put_world(&row).await.unwrap();
        let refresh = a.account().await;
        let r = a.join(&refresh, "r").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let j: JoinResponse = r.json();
        let states = async |row: &WorldRow| -> Vec<String> {
            links.link_states(row).await.unwrap().into_iter().map(|(addr, _)| addr).collect()
        };
        assert_eq!(states(&row).await, vec![ra.link.clone(), rb.link.clone()], "a link per replica");
        wait_for("both replicas linked", async || ra.connected.load(SeqCst) == 1 && rb.connected.load(SeqCst) == 1).await;
        assert_eq!(ra.seen().len() + rb.seen().len(), 1, "the order went out on one link");

        // The row now names B and C: C is dialled, A's link retired.
        row.host_link = Some(format!("{},{}", rb.link, rc.link));
        a.store.put_world(&row).await.unwrap();
        let r = a.join(&j.refresh_token, "r").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(states(&row).await, vec![rb.link.clone(), rc.link.clone()]);
        wait_for("C linked", async || rc.connected.load(SeqCst) == 1).await;
        wait_for("A's link retired: its dialer left", async || ra.connected.load(SeqCst) == 0).await;
        assert_eq!(rb.connected.load(SeqCst), 1, "B's link is kept, not redialled");
        drop((ra, rb, rc));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---- OIDC login ----

pub(crate) const OIDC_ISSUER: &str = "https://auth.example/realms/mmo";

fn oidc_config(issuer: &str) -> OidcConfig {
    OidcConfig { issuer: issuer.into(), audience: "mmo-login".into(), client_id: "game".into() }
}

/// `state` with OIDC on, verifying against `verifier`.
fn with_oidc(mut state: AppState, issuer: &str, verifier: OidcVerifier) -> AppState {
    let mut config = (*state.config).clone();
    config.oidc = Some(oidc_config(issuer));
    state.config = Arc::new(config);
    state.with_oidc(Arc::new(verifier))
}

pub(crate) async fn oidc_app() -> App {
    app_from(|state| {
        let verifier = OidcVerifier::from_jwks(OIDC_ISSUER, "mmo-login", crate::common::OIDC_JWKS).unwrap();
        with_oidc(state, OIDC_ISSUER, verifier)
    })
    .await
}

pub(crate) fn oidc_claims(issuer: &str, sub: &str) -> serde_json::Value {
    serde_json::json!({
        "iss": issuer, "aud": "mmo-login", "exp": T0 + 300, "sub": sub,
        "email": "Ann@X.io", "email_verified": true, "preferred_username": "ann",
    })
}

impl App {
    pub(crate) async fn login_oidc(&self, token: String) -> Resp {
        self.post(PATH_LOGIN_OIDC, &OidcLoginRequest { access_token: token }).await
    }
}

#[tokio::test]
async fn oidc_login_creates_account_and_returns_refresh() {
    let a = oidc_app().await;
    let r = a.login_oidc(crate::common::oidc_token(oidc_claims(OIDC_ISSUER, "u-1"), "test-1")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let login: LoginResponse = r.json();
    assert!(!login.refresh_token.is_empty());
    assert_eq!(login.account_name, "ann");
    assert_eq!(login.account_email, "ann@x.io");
    assert!(login.email_verified);

    // The refresh token joins like any other, and rotation keeps the name.
    let j = a.join(&login.refresh_token, "w1").await;
    assert_eq!(j.status, StatusCode::OK, "{}", j.text());
    let j: JoinResponse = j.json();
    assert_ne!(j.refresh_token, login.refresh_token);
    let again = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: j.refresh_token }).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json::<LoginResponse>().account_name, "ann");
}

#[tokio::test]
async fn oidc_login_with_foreign_audience_is_401() {
    let a = oidc_app().await;
    let mut claims = oidc_claims(OIDC_ISSUER, "u-1");
    claims["aud"] = serde_json::json!("account");
    let r = a.login_oidc(crate::common::oidc_token(claims, "test-1")).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.error(), ApiErrorCode::IdentityRefused);
}

#[tokio::test]
async fn oidc_login_email_conflict_is_409() {
    let a = oidc_app().await;
    let first = a.login_oidc(crate::common::oidc_token(oidc_claims(OIDC_ISSUER, "u-1"), "test-1")).await;
    assert_eq!(first.status, StatusCode::OK);
    let r = a.login_oidc(crate::common::oidc_token(oidc_claims(OIDC_ISSUER, "u-2"), "test-1")).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.error(), ApiErrorCode::IdentityConflict);
}

#[tokio::test]
async fn oidc_routes_are_404_when_not_configured() {
    let a = app().await;
    let r = a.login_oidc("x".into()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(a.get(PATH_CONFIG).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn config_reports_issuer_and_client() {
    let a = oidc_app().await;
    let r = a.get(PATH_CONFIG).await;
    assert_eq!(r.status, StatusCode::OK);
    let v: serde_json::Value = r.json();
    assert_eq!(v, serde_json::json!({ "oidc_issuer": OIDC_ISSUER, "oidc_client_id": "game" }));
}

/// Serves discovery and a JWKS that gains kid `test-2` (the same key) after its first request.
async fn serve_provider() -> String {
    use std::sync::atomic::AtomicUsize;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let discovery = serde_json::json!({ "issuer": base, "jwks_uri": format!("{base}/jwks") });
    let jwks = move || {
        let mut doc: serde_json::Value = serde_json::from_str(crate::common::OIDC_JWKS).unwrap();
        if hits.fetch_add(1, Ordering::SeqCst) >= 1 {
            let mut second = doc["keys"][0].clone();
            second["kid"] = "test-2".into();
            doc["keys"].as_array_mut().unwrap().push(second);
        }
        axum::Json(doc)
    };
    let provider = Router::new()
        .route("/.well-known/openid-configuration", axum::routing::get(move || async move { axum::Json(discovery) }))
        .route("/jwks", axum::routing::get(move || async move { jwks() }));
    tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    base
}

#[tokio::test]
async fn oidc_login_refetches_keys_on_unknown_kid() {
    let issuer = serve_provider().await;
    let verifier = OidcVerifier::discover(&issuer, "mmo-login").await.unwrap();
    let a = app_from(|state| with_oidc(state, &issuer, verifier)).await;
    let r = a.login_oidc(crate::common::oidc_token(oidc_claims(&issuer, "u-1"), "test-2")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// A provider whose JWKS answers once (discovery), then only 500s.
async fn serve_failing_provider() -> String {
    use axum::response::IntoResponse;
    use std::sync::atomic::AtomicUsize;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let discovery = serde_json::json!({ "issuer": base, "jwks_uri": format!("{base}/jwks") });
    let jwks = move || {
        if hits.fetch_add(1, Ordering::SeqCst) >= 1 {
            return (StatusCode::INTERNAL_SERVER_ERROR, "down").into_response();
        }
        axum::Json(serde_json::from_str::<serde_json::Value>(crate::common::OIDC_JWKS).unwrap()).into_response()
    };
    let provider = Router::new()
        .route("/.well-known/openid-configuration", axum::routing::get(move || async move { axum::Json(discovery) }))
        .route("/jwks", axum::routing::get(move || async move { jwks() }));
    tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    base
}

/// The provider's keys cannot be fetched: the service has a problem, not the player — 503
/// with the internal code, never a refused identity.
#[tokio::test]
async fn oidc_login_when_keys_cannot_be_fetched_is_503() {
    let issuer = serve_failing_provider().await;
    let verifier = OidcVerifier::discover(&issuer, "mmo-login").await.unwrap();
    let a = app_from(|state| with_oidc(state, &issuer, verifier)).await;
    let r = a.login_oidc(crate::common::oidc_token(oidc_claims(&issuer, "u-1"), "test-2")).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.error(), ApiErrorCode::Internal);
}
