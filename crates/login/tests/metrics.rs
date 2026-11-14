//! `GET /metrics` and the counters behind it, through the test app of `tests/api.rs`.

use axum::http::StatusCode;
use login_api::*;

use crate::api::{App, OIDC_ISSUER, app, oidc_app, oidc_claims};

async fn scrape(a: &App) -> String {
    let r = a.get("/metrics").await;
    assert_eq!(r.status, StatusCode::OK);
    r.text()
}

fn token(claims: serde_json::Value) -> String {
    crate::common::oidc_token(claims, "test-1")
}

#[tokio::test]
async fn metrics_endpoint_serves_text_format() {
    let a = app().await;
    // A labelled family shows up once it has a series: count one request first.
    a.get(PATH_WORLDS).await;
    let r = a.get("/metrics").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers[axum::http::header::CONTENT_TYPE], "text/plain; version=0.0.4");
    let body = r.text();
    assert!(body.contains("login_build_info{version=\""), "{body}");
    assert!(body.contains("# TYPE login_http_requests_total counter"), "{body}");
}

#[tokio::test]
async fn an_oidc_login_is_counted_by_outcome() {
    let a = oidc_app().await;
    assert_eq!(a.login_oidc(token(oidc_claims(OIDC_ISSUER, "u-1"))).await.status, StatusCode::OK);
    let mut bad = oidc_claims(OIDC_ISSUER, "u-1");
    bad["aud"] = serde_json::json!("account");
    assert_eq!(a.login_oidc(token(bad)).await.status, StatusCode::UNAUTHORIZED);
    let body = scrape(&a).await;
    assert!(body.contains("login_oidc_logins_total{outcome=\"created\"} 1"), "{body}");
    assert!(body.contains("login_oidc_logins_total{outcome=\"refused\"} 1"), "{body}");
}

#[tokio::test]
async fn requests_are_counted_by_route_template_and_status() {
    let a = app().await;
    a.get(PATH_WORLDS).await;
    a.get(PATH_WORLDS).await;
    let body = scrape(&a).await;
    assert!(body.contains("login_http_requests_total{route=\"/v1/worlds\",status=\"200\"} 2"), "{body}");
    assert!(body.contains("login_http_request_duration_seconds_count{route=\"/v1/worlds\"} 2"), "{body}");
}

#[tokio::test]
async fn unmatched_paths_share_one_route_label() {
    let a = app().await;
    for p in ["/a", "/b/c", "/wp-admin/x.php"] {
        assert_eq!(a.get(p).await.status, StatusCode::NOT_FOUND);
    }
    let body = scrape(&a).await;
    let series: Vec<_> = body.lines().filter(|l| l.starts_with("login_http_requests_total") && l.contains("route=\"other\"")).collect();
    assert_eq!(series, ["login_http_requests_total{route=\"other\",status=\"404\"} 3"], "{body}");
    for p in ["/a", "/b/c", "wp-admin", "x.php"] {
        assert!(!body.contains(p), "{p} leaked: {body}");
    }
}

#[tokio::test]
async fn scrapes_and_probes_are_not_counted() {
    let a = app().await;
    for _ in 0..3 {
        a.get("/metrics").await;
    }
    a.get("/healthz").await;
    let body = scrape(&a).await;
    assert!(!body.contains("route=\"/metrics\""), "{body}");
    assert!(!body.contains("route=\"/healthz\""), "{body}");
}

#[tokio::test]
async fn metrics_never_carry_identities() {
    let a = oidc_app().await;
    let r = a.login_oidc(token(oidc_claims(OIDC_ISSUER, "u-1"))).await;
    let login: LoginResponse = r.json();
    let again = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: login.refresh_token.clone() }).await;
    assert_eq!(again.status, StatusCode::OK);
    let body = scrape(&a).await;
    for secret in ["ann", "x.io", "u-1", login.refresh_token.as_str()] {
        assert!(!body.contains(secret), "{secret} leaked: {body}");
    }
}

#[tokio::test]
async fn a_refresh_and_a_join_are_counted() {
    let a = app().await;
    let refresh = a.account().await;
    let r = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: refresh }).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: "garbage".into() }).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // The closed world, as `join_closed_world_403` asserts: 403 WorldClosed.
    let live: LoginResponse = a.post(PATH_REFRESH, &RefreshRequest { refresh_token: a.account().await }).await.json();
    assert_eq!(a.join(&live.refresh_token, "shut").await.status, StatusCode::FORBIDDEN);
    let body = scrape(&a).await;
    assert!(body.contains("login_refresh_total{outcome=\"ok\"} 2"), "{body}");
    assert!(body.contains("login_refresh_total{outcome=\"refused\"} 1"), "{body}");
    assert!(body.contains("login_joins_total{outcome=\"refused\"} 1"), "{body}");
}
