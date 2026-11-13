use login::ctl::run_ctl;
use login::LoginStore;
use login_api::WorldStatus;

fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

fn run(db: &str, line: &str) -> Result<String, String> {
    let mut out = Vec::new();
    run_ctl(&args(&format!("--db {db} {line}")), &mut out)?;
    Ok(String::from_utf8(out).unwrap())
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("login-ctl-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn keygen_numbers_files_and_signs_with_newest() {
    let root = temp_dir("keygen");
    let keys = root.join("keys"); // does not exist yet
    let db = root.join("l.sqlite");
    let line = format!("keygen --dir {}", keys.display());
    let kid1 = run(db.to_str().unwrap(), &line).unwrap();
    let kid2 = run(db.to_str().unwrap(), &line).unwrap();
    assert_ne!(kid1.trim(), kid2.trim());
    let mut names: Vec<_> = std::fs::read_dir(&keys)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["0001.pem", "0002.pem"]);
    // Never prints key material.
    assert!(!kid1.contains("PRIVATE"));
    let signer = auth::Signer::load_dir(&keys).unwrap();
    assert_eq!(signer.signing_key().kid(), kid2.trim());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(keys.join("0002.pem")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn world_add_list_remove() {
    let root = temp_dir("world");
    let db = root.join("l.sqlite");
    let db = db.to_str().unwrap().to_string();
    let d = db.clone();
    tokio::task::spawn_blocking(move || {
        run(&d, "world add dev --name Dev --quic 127.0.0.1:6466 --wt https://127.0.0.1:6467/").unwrap();
        let list = run(&d, "world list").unwrap();
        assert!(list.contains("dev") && list.contains("open") && list.contains("127.0.0.1:6466"), "{list}");
        run(&d, "world close dev").unwrap();
        assert!(run(&d, "world list").unwrap().contains("closed"));
        run(&d, "world open dev").unwrap();
        assert!(run(&d, "world list").unwrap().contains("open"));
        assert!(run(&d, "world close nope").is_err());
        // The host's service link: both flags or neither; a pin is 64 hex digits.
        let pin = "AB".repeat(32);
        let linked = format!("world add dev --name Dev --link 127.0.0.1:6490 --link-cert {pin}");
        run(&d, &linked).unwrap();
        assert!(run(&d, "world list").unwrap().contains("link=127.0.0.1:6490"));
        assert!(run(&d, "world add dev --name Dev --link 127.0.0.1:6490").is_err());
        assert!(run(&d, "world add dev --name Dev --link nope --link-cert 00").is_err());
        assert!(run(&d, &format!("world add dev --name Dev --link 127.0.0.1:6490 --link-cert {}", "zz".repeat(32))).is_err());
        run(&d, "world close dev").unwrap();
        assert!(run(&d, "world list").unwrap().contains("link=127.0.0.1:6490"), "close keeps the link");
        let readded = run(&d, "world add dev --name Dev").unwrap();
        assert!(readded.contains("warning") && readded.contains("127.0.0.1:6490"), "the cleared link is said: {readded}");
        assert!(run(&d, "world list").unwrap().contains("link=-"), "re-adding without a link clears it");
        assert!(!run(&d, "world add dev --name Dev").unwrap().contains("warning"), "nothing to clear now");
        run(&d, "world remove dev").unwrap();
        assert!(run(&d, "world remove dev").is_err());
        assert!(!run(&d, "world list").unwrap().contains("dev"));
    })
    .await
    .unwrap();
    let store = LoginStore::open(&db).await.unwrap();
    assert!(store.worlds().await.unwrap().is_empty());
    let _ = WorldStatus::Open;
    let _ = std::fs::remove_dir_all(&root);
}

/// `POST /v1/refresh` with `token` through the real router over the store at `db`.
async fn refresh_status(db: &str, token: &str) -> axum::http::StatusCode {
    use axum::body::Body;
    use axum::http::{Request, header};
    use tower::ServiceExt;

    let store = LoginStore::open(db).await.unwrap();
    let (key, _pem) = auth::SigningKey::generate();
    let state = login::AppState::new(
        store,
        std::sync::Arc::new(auth::Signer::new(vec![key]).ok().unwrap()),
        std::sync::Arc::new(login::LoginConfig::from_lookup(|_| None).unwrap()),
        login::system_clock(),
    );
    let body = serde_json::to_vec(&login_api::RefreshRequest { refresh_token: token.into() }).unwrap();
    let req = Request::post(login_api::PATH_REFRESH)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    login::router(state).oneshot(req).await.unwrap().status()
}

/// Runs `issue-refresh` for `email` off the async runtime; the one printed line, unquoted.
async fn issue(db: &str, email: &str) -> String {
    let (d, line) = (db.to_string(), format!("account issue-refresh {email}"));
    let out = tokio::task::spawn_blocking(move || run(&d, &line)).await.unwrap().unwrap();
    assert!(out.ends_with('\n') && out.matches('\n').count() == 1, "one line: {out:?}");
    out.trim_end().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn issue_refresh_creates_a_verified_account_and_a_working_token() {
    let root = temp_dir("issue");
    let db = root.join("l.sqlite");
    let db = db.to_str().unwrap().to_string();
    let token = issue(&db, "dev@localhost").await;
    let store = LoginStore::open(&db).await.unwrap();
    let row = store.account_by_email("dev@localhost").await.unwrap().unwrap();
    assert!(row.email_verified_at.is_some());
    assert_eq!(row.subject, None);
    drop(store);
    assert_eq!(refresh_status(&db, &token).await, axum::http::StatusCode::OK);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn issue_refresh_twice_keeps_one_account_and_both_tokens_work() {
    let root = temp_dir("issue2");
    let db = root.join("l.sqlite");
    let db = db.to_str().unwrap().to_string();
    let first = issue(&db, "Dev@Localhost").await;
    let second = issue(&db, "dev@localhost").await;
    assert_ne!(first, second);
    // The first token is still good: a second family, not a rotation of the first.
    assert_eq!(refresh_status(&db, &first).await, axum::http::StatusCode::OK);
    assert_eq!(refresh_status(&db, &second).await, axum::http::StatusCode::OK);
    let store = LoginStore::open(&db).await.unwrap();
    let a = store.account_by_email("dev@localhost").await.unwrap().unwrap();
    assert!(a.email_verified_at.is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn issue_refresh_prints_only_the_token() {
    let root = temp_dir("issue3");
    let db = root.join("l.sqlite");
    let db = db.to_str().unwrap().to_string();
    let token = issue(&db, "dev@localhost").await;
    assert!(!token.contains(char::is_whitespace), "{token:?}");
    // The same length as a refresh token a login hands out.
    let minted = login::api::mint_refresh(&LoginStore::open(&db).await.unwrap(), 1, 0).await.unwrap();
    assert_eq!(token.len(), minted.len());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn account_revoke_ends_sessions() {
    let root = temp_dir("revoke");
    let db = root.join("l.sqlite");
    let db = db.to_str().unwrap().to_string();
    let token = issue(&db, "dev@localhost").await;
    // A session in a host-linked world: `revoke` queues a kick for it.
    let store = LoginStore::open(&db).await.unwrap();
    let id = store.account_by_email("dev@localhost").await.unwrap().unwrap().account_id;
    store
        .put_world(&login::WorldRow {
            world_id: "linked".into(),
            name: "L".into(),
            status: WorldStatus::Open,
            quic: None,
            webtransport: None,
            cert_hash: None,
            created_at: 1,
            host_link: Some("127.0.0.1:6490".into()),
            host_link_cert: Some("ab".repeat(32)),
        })
        .await
        .unwrap();
    store.player_token_for(id, "linked", 0, || "tok".into()).await.unwrap();
    assert_eq!(store.bump_session(id, "linked", 0).await.unwrap(), 1);
    drop(store);
    assert_eq!(refresh_status(&db, &token).await, axum::http::StatusCode::OK);
    // The refresh above rotated the token; a second one is a new family: both die with the account's tokens.
    let second = issue(&db, "dev@localhost").await;

    let d = db.clone();
    let out = tokio::task::spawn_blocking(move || run(&d, "account revoke Dev@Localhost")).await.unwrap().unwrap();
    assert!(out.contains("1 session(s) kicked"), "{out}");
    assert!(!out.contains(&second), "no token is printed: {out}");
    assert_eq!(refresh_status(&db, &second).await, axum::http::StatusCode::UNAUTHORIZED);
    let store = LoginStore::open(&db).await.unwrap();
    let kicks = store.pending_kicks().await.unwrap();
    assert_eq!(kicks.len(), 1);
    assert_eq!((kicks[0].world_id.as_str(), kicks[0].player_token.as_str(), kicks[0].session_epoch), ("linked", "tok", 1));
    assert_eq!(kicks[0].reason, login::api::KICK_ACCOUNT_REVOKED);
    drop(store);
    let d = db.clone();
    let missing = tokio::task::spawn_blocking(move || run(&d, "account revoke nobody@localhost")).await.unwrap();
    assert!(missing.is_err());
    let _ = std::fs::remove_dir_all(&root);
}
