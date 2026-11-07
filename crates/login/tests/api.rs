//! The HTTP API end to end, against `router()` on an in-memory SQLite store.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use auth::{Jwks, Signer, SigningKey, Verifier};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use login::mail::LogMailer;
use login::{AppState, Hasher, LoginConfig, LoginStore, WorldRow, router};
use login_api::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tower::ServiceExt;

const T0: u64 = 1_800_000_000;
const PASSWORD: &str = "correct horse battery";
const EMAIL: &str = "Dwarf@Example.org";
const ORIGIN_OK: &str = "http://127.0.0.1:1334";
const BODY_LIMIT: usize = 1 << 20;

struct App {
    router: Router,
    mailer: LogMailer,
    clock: Arc<AtomicU64>,
    store: LoginStore,
}

async fn app_with(require_verified: bool) -> App {
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
            })
            .await
            .unwrap();
    }
    let mut config = LoginConfig::from_lookup(|_| None).unwrap();
    config.require_verified = require_verified;
    let mailer = LogMailer::default();
    let clock = Arc::new(AtomicU64::new(T0));
    let c = clock.clone();
    let (key, _pem) = SigningKey::generate();
    let state = AppState::new(
        store.clone(),
        Arc::new(Signer::new(vec![key]).ok().unwrap()),
        Hasher::for_tests(),
        Arc::new(mailer.clone()),
        Arc::new(config),
        Arc::new(move || c.load(Ordering::SeqCst)),
    );
    App { router: router(state), mailer, clock, store }
}

async fn app() -> App {
    app_with(false).await
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn error(&self) -> ApiErrorCode {
        self.json::<ApiError>().code
    }
}

impl App {
    async fn send(&self, req: Request<Body>) -> Resp {
        let r = self.router.clone().oneshot(req).await.unwrap();
        let status = r.status();
        let headers = r.headers().clone();
        let body = to_bytes(r.into_body(), BODY_LIMIT).await.unwrap().to_vec();
        Resp { status, headers, body }
    }

    async fn post<B: Serialize>(&self, path: &str, body: &B) -> Resp {
        let req = Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap();
        self.send(req).await
    }

    async fn get(&self, path: &str) -> Resp {
        self.send(Request::get(path).body(Body::empty()).unwrap()).await
    }

    /// The token of the last mail's link with `path` (`/verify` or `/reset`).
    fn mailed_token(&self, path: &str) -> String {
        let needle = format!("{path}?token=");
        let mails = self.mailer.sent();
        let text = &mails.last().expect("a mail was sent").text;
        let at = text.find(&needle).unwrap_or_else(|| panic!("no {needle} link in {text}"));
        text[at + needle.len()..].split_whitespace().next().unwrap().to_string()
    }

    async fn register(&self, email: &str, password: &str) -> Resp {
        self.post(PATH_REGISTER, &RegisterRequest { email: email.into(), password: password.into() }).await
    }

    async fn login(&self, email: &str, password: &str) -> Resp {
        self.post(PATH_LOGIN, &LoginRequest { email: email.into(), password: password.into() }).await
    }

    /// Register + verify + login; the refresh token.
    async fn account(&self) -> String {
        assert_eq!(self.register(EMAIL, PASSWORD).await.status, StatusCode::ACCEPTED);
        let token = self.mailed_token("/verify");
        assert_eq!(self.post(PATH_VERIFY, &VerifyRequest { token }).await.status, StatusCode::NO_CONTENT);
        let r = self.login(EMAIL, PASSWORD).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.json::<LoginResponse>().refresh_token
    }

    async fn join(&self, refresh: &str, world: &str) -> Resp {
        self.post(PATH_JOIN, &JoinRequest { refresh_token: refresh.into(), world: world.into() }).await
    }

    async fn verifier(&self) -> Verifier {
        let r = self.get(PATH_JWKS).await;
        assert_eq!(r.status, StatusCode::OK);
        Verifier::from_jwks(&r.json::<Jwks>())
    }

    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn register_login_join_happy_path() {
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
async fn login_unknown_email_and_bad_password_look_the_same() {
    let a = app().await;
    a.account().await;
    let unknown = a.login("nobody@example.org", PASSWORD).await;
    let wrong = a.login(EMAIL, "wrong password!!").await;
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.status, wrong.status);
    assert_eq!(unknown.body, wrong.body);
    assert_eq!(unknown.error(), ApiErrorCode::BadCredentials);
}

#[tokio::test]
async fn register_existing_email_sends_already_registered_and_202() {
    let a = app().await;
    a.account().await;
    let before = a.mailer.sent().len();
    let r = a.register(" DWARF@example.org ", "another password").await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let mails = a.mailer.sent();
    assert_eq!(mails.len(), before + 1);
    let m = mails.last().unwrap();
    assert_eq!(m.to, "dwarf@example.org");
    assert_eq!(m.subject, "You already have an account");
    assert!(!m.text.contains("token="));
    // The password did not change.
    assert_eq!(a.login(EMAIL, PASSWORD).await.status, StatusCode::OK);
    assert_eq!(a.login(EMAIL, "another password").await.status, StatusCode::UNAUTHORIZED);
    // A bad password is refused before anything else.
    let short = a.register("new@example.org", "short").await;
    assert_eq!((short.status, short.error()), (StatusCode::BAD_REQUEST, ApiErrorCode::BadRequest));
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
    let t = a.login(EMAIL, PASSWORD).await.json::<LoginResponse>().refresh_token;
    let out = a.post(PATH_LOGOUT, &RefreshRequest { refresh_token: t.clone() }).await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    assert_eq!(a.post(PATH_REFRESH, &RefreshRequest { refresh_token: t }).await.status, StatusCode::UNAUTHORIZED);
    // Refresh tokens expire.
    let t = a.login(EMAIL, PASSWORD).await.json::<LoginResponse>().refresh_token;
    a.clock.fetch_add(REFRESH_TTL_DAYS * 86_400 + 1, Ordering::SeqCst);
    assert_eq!(a.post(PATH_REFRESH, &RefreshRequest { refresh_token: t }).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn require_verified_refuses_unverified() {
    let a = app_with(true).await;
    assert_eq!(a.register(EMAIL, PASSWORD).await.status, StatusCode::ACCEPTED);
    let r = a.login(EMAIL, PASSWORD).await;
    assert_eq!((r.status, r.error()), (StatusCode::FORBIDDEN, ApiErrorCode::NotVerified));
    // A wrong password still answers bad credentials (no hint that the account exists).
    assert_eq!(a.login(EMAIL, "wrong password!!").await.error(), ApiErrorCode::BadCredentials);
    let token = a.mailed_token("/verify");
    assert_eq!(a.post(PATH_VERIFY, &VerifyRequest { token: token.clone() }).await.status, StatusCode::NO_CONTENT);
    // Single use.
    assert_eq!(a.post(PATH_VERIFY, &VerifyRequest { token }).await.status, StatusCode::BAD_REQUEST);
    let r = a.login(EMAIL, PASSWORD).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json::<LoginResponse>().email_verified);

    // Without require_verified an unverified account may log in.
    let b = app().await;
    b.register(EMAIL, PASSWORD).await;
    let r = b.login(EMAIL, PASSWORD).await;
    assert_eq!(r.status, StatusCode::OK);
    let r: LoginResponse = r.json();
    assert!(!r.email_verified);
    assert_eq!(r.account_email, "dwarf@example.org");
}

#[tokio::test]
async fn reset_sets_password_and_revokes_refresh() {
    let a = app().await;
    let refresh = a.account().await;
    let mails = a.mailer.sent().len();
    // Unknown address: 202 and no mail.
    let r = a.post(PATH_RESET_REQUEST, &ResetRequest { email: "nobody@example.org".into() }).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    assert_eq!(a.mailer.sent().len(), mails);
    let r = a.post(PATH_RESET_REQUEST, &ResetRequest { email: EMAIL.into() }).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let token = a.mailed_token("/reset");
    // Policy first: the token is not spent by a too-short password.
    let short = a.post(PATH_RESET_CONFIRM, &ResetConfirm { token: token.clone(), password: "short".into() }).await;
    assert_eq!(short.status, StatusCode::BAD_REQUEST);
    let ok = a.post(PATH_RESET_CONFIRM, &ResetConfirm { token: token.clone(), password: "a brand new password".into() }).await;
    assert_eq!(ok.status, StatusCode::NO_CONTENT, "{}", ok.text());
    let again = a.post(PATH_RESET_CONFIRM, &ResetConfirm { token, password: "yet another password".into() }).await;
    assert_eq!((again.status, again.error()), (StatusCode::BAD_REQUEST, ApiErrorCode::BadRequest));
    assert_eq!(a.login(EMAIL, PASSWORD).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(a.login(EMAIL, "a brand new password").await.status, StatusCode::OK);
    assert_eq!(a.post(PATH_REFRESH, &RefreshRequest { refresh_token: refresh }).await.status, StatusCode::UNAUTHORIZED);
    // Reset tokens expire.
    a.post(PATH_RESET_REQUEST, &ResetRequest { email: EMAIL.into() }).await;
    let token = a.mailed_token("/reset");
    a.clock.fetch_add(login::mail::RESET_TOKEN_TTL_SECS as u64 + 1, Ordering::SeqCst);
    let late = a.post(PATH_RESET_CONFIRM, &ResetConfirm { token, password: "too late password".into() }).await;
    assert_eq!(late.status, StatusCode::BAD_REQUEST);
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
            .uri(PATH_LOGIN)
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

fn form_post(path: &str, body: String) -> Request<Body> {
    Request::post(path)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn reset_page_form_round_trip() {
    let a = app().await;
    a.account().await;
    a.post(PATH_RESET_REQUEST, &ResetRequest { email: EMAIL.into() }).await;
    let token = a.mailed_token("/reset");
    let page = a.get(&format!("/reset?token={token}")).await;
    assert_eq!(page.status, StatusCode::OK);
    let html = page.text();
    assert!(page.headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
    // Relative, so it works behind a path prefix (`/login/reset` posts to `/login/reset`).
    assert!(html.contains("<form") && html.contains("method=\"post\"") && html.contains("action=\"reset\""), "{html}");
    // The URL carries the token: never cached, never sent on as a Referer.
    assert_eq!(page.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(page.headers[header::REFERRER_POLICY], "no-referrer");
    assert!(html.contains(&format!("value=\"{token}\"")));
    assert!(!html.contains("<script"));
    let done = a.send(form_post("/reset", format!("token={token}&password=a+brand+new+password"))).await;
    assert_eq!(done.status, StatusCode::OK);
    assert!(done.text().to_lowercase().contains("password changed"), "{}", done.text());
    assert_eq!(a.login(EMAIL, "a brand new password").await.status, StatusCode::OK);
    // Used twice: an error page, not a success.
    let again = a.send(form_post("/reset", format!("token={token}&password=a+brand+new+password"))).await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    assert!(!again.text().to_lowercase().contains("password changed"));
    // Echoed values are escaped.
    let page = a.get("/reset?token=%3Cscript%3E").await;
    assert!(!page.text().contains("<script>") && page.text().contains("&lt;script&gt;"));
}

#[tokio::test]
async fn verify_page_confirms() {
    let a = app_with(true).await;
    a.register(EMAIL, PASSWORD).await;
    let token = a.mailed_token("/verify");
    let page = a.get(&format!("/verify?token={token}")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(page.headers[header::REFERRER_POLICY], "no-referrer");
    assert!(page.text().to_lowercase().contains("verified"), "{}", page.text());
    assert_eq!(a.login(EMAIL, PASSWORD).await.status, StatusCode::OK);
    let again = a.get(&format!("/verify?token={token}")).await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
}

#[test]
fn config_defaults_and_overrides() {
    use login::MailerKind;
    let d = LoginConfig::from_lookup(|_| None).unwrap();
    assert_eq!(d.bind.to_string(), "127.0.0.1:8790");
    assert_eq!(d.db_url, "target/dev-login/login.sqlite");
    assert_eq!(d.keys_dir, std::path::PathBuf::from("target/dev-login/keys"));
    assert_eq!(d.public_url, "http://127.0.0.1:8790");
    assert_eq!(d.cors_origins, vec![ORIGIN_OK.to_string(), "http://localhost:1334".to_string()]);
    assert!(!d.require_verified);
    assert_eq!(d.mailer, MailerKind::Log);
    let env = |k: &str| {
        match k {
            "VOXEL_LOGIN_BIND" => Some("0.0.0.0:9000"),
            "VOXEL_LOGIN_CORS_ORIGINS" => Some("https://a.example, https://b.example"),
            "VOXEL_LOGIN_REQUIRE_VERIFIED" => Some("1"),
            "VOXEL_MAILER" => Some("ses"),
            "VOXEL_MAIL_FROM" => Some("noreply@example.org"),
            _ => None,
        }
        .map(String::from)
    };
    let c = LoginConfig::from_lookup(env).unwrap();
    assert_eq!(c.bind.to_string(), "0.0.0.0:9000");
    assert_eq!(c.cors_origins, vec!["https://a.example".to_string(), "https://b.example".to_string()]);
    assert!(c.require_verified);
    assert_eq!(c.mailer, MailerKind::Ses { from: "noreply@example.org".into() });
    // SES needs a sender; a bad bind address or mailer is refused.
    assert!(LoginConfig::from_lookup(|k| (k == "VOXEL_MAILER").then(|| "ses".to_string())).is_err());
    assert!(LoginConfig::from_lookup(|k| (k == "VOXEL_MAILER").then(|| "smtp".to_string())).is_err());
    assert!(LoginConfig::from_lookup(|k| (k == "VOXEL_LOGIN_BIND").then(|| "nope".to_string())).is_err());
}
