use login::{DISPLAY_NAME_MAX_CHARS, LoginStore, OidcUpsert, RefreshOutcome, WorldRow, normalize_email};
use login_api::WorldStatus;

#[test]
fn email_normalized() {
    assert_eq!(normalize_email(" A@B.c ").unwrap(), "a@b.c");
    for bad in ["a@@b", "@b", "a@", "ab", "a@b@c", ""] {
        assert!(normalize_email(bad).is_err(), "{bad:?}");
    }
    let long = format!("{}@x.y", "a".repeat(251));
    assert!(normalize_email(&long).is_err());
}

async fn store() -> LoginStore {
    LoginStore::open("sqlite::memory:").await.unwrap()
}

#[tokio::test]
async fn create_account_refuses_duplicate_email() {
    let s = store().await;
    let id = s.create_account("a@b.c", "h", 10).await.unwrap().unwrap();
    assert_eq!(s.create_account("a@b.c", "h2", 11).await.unwrap(), None);
    let row = s.account_by_email("a@b.c").await.unwrap().unwrap();
    assert_eq!((row.account_id, row.password_hash.as_str(), row.created_at), (id, "h", 10));
    assert_eq!(s.account(id).await.unwrap().unwrap(), row);
    assert!(s.account_by_email("x@y.z").await.unwrap().is_none());
}

#[tokio::test]
async fn ensure_verified_account_makes_or_finds_one() {
    let s = store().await;
    let id = s.ensure_verified_account("a@b.c", 10).await.unwrap();
    let row = s.account(id).await.unwrap().unwrap();
    assert_eq!((row.password_hash.as_str(), row.email_verified_at), ("", Some(10)));
    // Again: the same account, its verified time kept.
    assert_eq!(s.ensure_verified_account("a@b.c", 20).await.unwrap(), id);
    assert_eq!(s.account(id).await.unwrap().unwrap().email_verified_at, Some(10));
    // An unverified account (made another way) is found but stays unverified: an operator
    // must not vouch for an email the command did not set up.
    let other = s.create_account("o@b.c", "h", 1).await.unwrap().unwrap();
    assert_eq!(s.ensure_verified_account("o@b.c", 30).await.unwrap(), other);
    assert_eq!(s.account(other).await.unwrap().unwrap().email_verified_at, None);
}

#[tokio::test]
async fn refresh_rotation() {
    let s = store().await;
    let a = s.create_account("a@b.c", "h", 0).await.unwrap().unwrap();
    s.insert_refresh([1; 32], a, [9; 16], 1000).await.unwrap();
    assert_eq!(s.take_refresh([1; 32], 10).await.unwrap(), RefreshOutcome::Valid { account: a, family: [9; 16] });
    assert_eq!(s.take_refresh([1; 32], 11).await.unwrap(), RefreshOutcome::Reused { account: a, family: [9; 16] });
    assert_eq!(s.take_refresh([2; 32], 11).await.unwrap(), RefreshOutcome::Unknown);
    // Expired and never used: unknown.
    s.insert_refresh([3; 32], a, [9; 16], 50).await.unwrap();
    assert_eq!(s.take_refresh([3; 32], 50).await.unwrap(), RefreshOutcome::Unknown);
}

#[tokio::test]
async fn revoke_family_revokes_all_of_it() {
    let s = store().await;
    let a = s.create_account("a@b.c", "h", 0).await.unwrap().unwrap();
    s.insert_refresh([1; 32], a, [7; 16], 1000).await.unwrap();
    s.insert_refresh([2; 32], a, [7; 16], 1000).await.unwrap();
    s.insert_refresh([3; 32], a, [8; 16], 1000).await.unwrap();
    s.revoke_family([7; 16], 5).await.unwrap();
    assert!(matches!(s.take_refresh([1; 32], 6).await.unwrap(), RefreshOutcome::Reused { .. }));
    assert!(matches!(s.take_refresh([2; 32], 6).await.unwrap(), RefreshOutcome::Reused { .. }));
    assert!(matches!(s.take_refresh([3; 32], 6).await.unwrap(), RefreshOutcome::Valid { .. }));
    s.insert_refresh([4; 32], a, [8; 16], 1000).await.unwrap();
    s.revoke_account_refresh(a, 7).await.unwrap();
    assert!(matches!(s.take_refresh([4; 32], 8).await.unwrap(), RefreshOutcome::Reused { .. }));
}

#[tokio::test]
async fn bump_session_is_per_account_and_world() {
    let s = store().await;
    assert_eq!(s.bump_session(1, "w1", 0).await.unwrap(), 1);
    assert_eq!(s.bump_session(1, "w1", 1).await.unwrap(), 2);
    assert_eq!(s.bump_session(1, "w2", 2).await.unwrap(), 1);
    assert_eq!(s.bump_session(2, "w1", 3).await.unwrap(), 1);
}

#[tokio::test]
async fn player_token_stable_per_world() {
    let s = store().await;
    let t1 = s.player_token_for(1, "w1", 0, || "aaa".into()).await.unwrap();
    let again = s.player_token_for(1, "w1", 1, || panic!("must not mint again")).await.unwrap();
    let other = s.player_token_for(1, "w2", 2, || "bbb".into()).await.unwrap();
    assert_eq!((t1.as_str(), again.as_str(), other.as_str()), ("aaa", "aaa", "bbb"));
}

#[tokio::test]
async fn world_registry() {
    let s = store().await;
    let w = WorldRow {
        world_id: "w1".into(),
        name: "One".into(),
        status: WorldStatus::Open,
        quic: Some("h:1".into()),
        webtransport: None,
        cert_hash: None,
        created_at: 5,
        host_link: Some("127.0.0.1:6490".into()),
        host_link_cert: Some("ab".repeat(32)),
    };
    s.put_world(&w).await.unwrap();
    assert_eq!(s.world("w1").await.unwrap().unwrap(), w);
    let closed = WorldRow { status: WorldStatus::Closed, name: "Uno".into(), ..w.clone() };
    s.put_world(&closed).await.unwrap();
    assert_eq!(s.worlds().await.unwrap(), vec![closed]);
    assert!(s.remove_world("w1").await.unwrap());
    assert!(!s.remove_world("w1").await.unwrap());
    assert!(s.world("w1").await.unwrap().is_none());
}

#[tokio::test]
async fn file_database_persists() {
    let dir = std::env::temp_dir().join(format!("login-test-{}", std::process::id()));
    let url = dir.join("sub/login.sqlite");
    let url = url.to_str().unwrap();
    {
        let s = LoginStore::open(url).await.unwrap();
        s.create_account("a@b.c", "h", 0).await.unwrap().unwrap();
    }
    let s = LoginStore::open(url).await.unwrap();
    assert!(s.account_by_email("a@b.c").await.unwrap().is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

const RACERS: usize = 8;

/// `RACERS` concurrent takes of one token: exactly one wins.
async fn race_takes(store: LoginStore) {
    let a = store.create_account("a@b.c", "h", 0).await.unwrap().unwrap();
    store.insert_refresh([1; 32], a, [9; 16], 1000).await.unwrap();
    let mut refresh = Vec::new();
    for _ in 0..RACERS {
        let s1 = store.clone();
        refresh.push(tokio::spawn(async move { s1.take_refresh([1; 32], 10).await.unwrap() }));
    }
    let mut valid = 0;
    for h in refresh {
        match h.await.unwrap() {
            RefreshOutcome::Valid { .. } => valid += 1,
            RefreshOutcome::Reused { .. } => {}
            RefreshOutcome::Unknown => panic!("a racer saw Unknown"),
        }
    }
    assert_eq!(valid, 1, "exactly one concurrent refresh take is Valid");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_takes_sqlite_file() {
    let dir = std::env::temp_dir().join(format!("login-race-{}", std::process::id()));
    let path = dir.join("login.sqlite");
    race_takes(LoginStore::open(path.to_str().unwrap()).await.unwrap()).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_takes_postgres() {
    if crate::common::pg_admin_url().is_none() { return crate::common::skip_msg() }
    let db = crate::common::PgTestDb::create_async("mmo_test").await.expect("pg env var set");
    race_takes(LoginStore::open(&db.url).await.unwrap()).await;
    drop(db);
}

/// `RACERS` concurrent rotations of one token, each with its own successor: exactly one is
/// `Valid`, and once every `Reused` racer has revoked the family (as the handler does), no token
/// of that family is live — the winner's successor included.
async fn race_rotations(store: LoginStore) {
    const FAMILY: [u8; 16] = [5; 16];
    let a = store.create_account("r@b.c", "h", 0).await.unwrap().unwrap();
    store.insert_refresh([1; 32], a, FAMILY, 1000).await.unwrap();
    let mut racers = Vec::new();
    for i in 0..RACERS {
        let s = store.clone();
        racers.push(tokio::spawn(async move {
            let successor = [100 + i as u8; 32];
            let out = s.rotate_refresh([1; 32], successor, 1000, 10).await.unwrap();
            if let RefreshOutcome::Reused { family, .. } = &out {
                s.revoke_family(*family, 10).await.unwrap();
            }
            (successor, out)
        }));
    }
    let mut winners = Vec::new();
    for h in racers {
        match h.await.unwrap() {
            (succ, RefreshOutcome::Valid { account, family }) => {
                assert_eq!((account, family), (a, FAMILY));
                winners.push(succ);
            }
            (_, RefreshOutcome::Reused { family, .. }) => assert_eq!(family, FAMILY),
            (_, RefreshOutcome::Unknown) => panic!("a racer saw Unknown"),
        }
    }
    assert_eq!(winners.len(), 1, "exactly one concurrent rotation is Valid");
    // Every successor: the winner's is revoked, the losers' were never inserted.
    for i in 0..RACERS {
        let succ = [100 + i as u8; 32];
        let out = store.take_refresh(succ, 11).await.unwrap();
        if succ == winners[0] {
            assert_eq!(out, RefreshOutcome::Reused { account: a, family: FAMILY }, "winner's successor revoked");
        } else {
            assert_eq!(out, RefreshOutcome::Unknown, "a loser's successor was inserted");
        }
    }
}

#[tokio::test]
async fn rotate_refresh_inserts_successor_in_family() {
    let s = store().await;
    let a = s.create_account("a@b.c", "h", 0).await.unwrap().unwrap();
    s.insert_refresh([1; 32], a, [9; 16], 1000).await.unwrap();
    assert_eq!(s.rotate_refresh([1; 32], [2; 32], 2000, 10).await.unwrap(), RefreshOutcome::Valid { account: a, family: [9; 16] });
    assert_eq!(s.rotate_refresh([1; 32], [3; 32], 2000, 11).await.unwrap(), RefreshOutcome::Reused { account: a, family: [9; 16] });
    assert_eq!(s.rotate_refresh([7; 32], [4; 32], 2000, 11).await.unwrap(), RefreshOutcome::Unknown);
    // Only the Valid rotation inserted its successor, in the old family.
    assert_eq!(s.take_refresh([3; 32], 12).await.unwrap(), RefreshOutcome::Unknown);
    assert_eq!(s.take_refresh([4; 32], 12).await.unwrap(), RefreshOutcome::Unknown);
    assert_eq!(s.take_refresh([2; 32], 12).await.unwrap(), RefreshOutcome::Valid { account: a, family: [9; 16] });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_rotations_sqlite_file() {
    let dir = std::env::temp_dir().join(format!("login-rot-race-{}", std::process::id()));
    let path = dir.join("login.sqlite");
    race_rotations(LoginStore::open(path.to_str().unwrap()).await.unwrap()).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_rotations_postgres() {
    if crate::common::pg_admin_url().is_none() { return crate::common::skip_msg() }
    let db = crate::common::PgTestDb::create_async("mmo_test").await.expect("pg env var set");
    race_rotations(LoginStore::open(&db.url).await.unwrap()).await;
    drop(db);
}

/// Sessions end per epoch (a late end never closes a newer join), and a join re-opens one.
async fn session_ends(s: LoginStore) {
    let t = s.player_token_for(1, "w1", 0, || "aaa".into()).await.unwrap();
    s.player_token_for(1, "w2", 0, || "bbb".into()).await.unwrap();
    assert_eq!(s.bump_session(1, "w1", 1).await.unwrap(), 1);
    assert_eq!(s.bump_session(1, "w2", 1).await.unwrap(), 1);
    let worlds = |v: Vec<login::OpenSession>| v.into_iter().map(|o| (o.world_id, o.player_token, o.session_epoch)).collect::<Vec<_>>();
    assert_eq!(worlds(s.open_sessions(1).await.unwrap()), vec![("w1".into(), t.clone(), 1), ("w2".into(), "bbb".into(), 1)]);
    assert_eq!(s.bump_session(1, "w1", 2).await.unwrap(), 2);
    assert!(!s.end_session("w1", &t, 1, 3).await.unwrap(), "epoch 1's end is late: epoch 2 is open");
    assert!(!s.end_session("w1", "nobody", 2, 3).await.unwrap());
    assert!(s.end_session("w1", &t, 2, 3).await.unwrap());
    assert!(!s.end_session("w1", &t, 2, 4).await.unwrap(), "once");
    assert_eq!(worlds(s.open_sessions(1).await.unwrap()), vec![("w2".into(), "bbb".into(), 1)]);
    assert_eq!(s.bump_session(1, "w1", 5).await.unwrap(), 3);
    assert_eq!(s.open_sessions(1).await.unwrap().len(), 2, "a join re-opens it");
    assert!(s.open_sessions(2).await.unwrap().is_empty());
}

#[tokio::test]
async fn session_ends_sqlite() {
    session_ends(store().await).await;
}

#[tokio::test]
async fn session_ends_postgres() {
    if crate::common::pg_admin_url().is_none() { return crate::common::skip_msg() }
    let db = crate::common::PgTestDb::create_async("mmo_test").await.expect("pg env var set");
    session_ends(LoginStore::open(&db.url).await.unwrap()).await;
    drop(db);
}

/// A refresh token's account is read without spending it.
#[tokio::test]
async fn live_refresh_account_reads_without_spending() {
    let s = store().await;
    s.insert_refresh([1; 32], 7, [9; 16], 100).await.unwrap();
    assert_eq!(s.live_refresh_account([1; 32], 50).await.unwrap(), Some(7));
    assert_eq!(s.live_refresh_account([1; 32], 100).await.unwrap(), None, "expired");
    assert_eq!(s.live_refresh_account([2; 32], 50).await.unwrap(), None);
    assert!(matches!(s.take_refresh([1; 32], 50).await.unwrap(), RefreshOutcome::Valid { account: 7, .. }));
    assert_eq!(s.live_refresh_account([1; 32], 51).await.unwrap(), None, "spent");
}

/// Revocation and its kicks commit together; an ack forgets only up to its epoch.
async fn kick_queue(s: LoginStore) {
    let link = WorldRow {
        world_id: "linked".into(),
        name: "L".into(),
        status: WorldStatus::Open,
        quic: None,
        webtransport: None,
        cert_hash: None,
        created_at: 1,
        host_link: Some("127.0.0.1:6490".into()),
        host_link_cert: Some("ab".repeat(32)),
    };
    s.put_world(&link).await.unwrap();
    s.put_world(&WorldRow { world_id: "plain".into(), host_link: None, host_link_cert: None, ..link.clone() }).await.unwrap();
    for w in ["linked", "plain"] {
        s.player_token_for(1, w, 0, || format!("tok-{w}")).await.unwrap();
        s.bump_session(1, w, 0).await.unwrap();
    }
    s.bump_session(1, "linked", 0).await.unwrap(); // epoch 2
    s.insert_refresh([1; 32], 1, [9; 16], 100).await.unwrap();
    assert_eq!(s.revoke_account_with_kicks(1, 10, "bye").await.unwrap(), 1, "only the linked world");
    assert!(matches!(s.take_refresh([1; 32], 11).await.unwrap(), RefreshOutcome::Reused { .. }), "revoked");
    let kick = login::PendingKick { world_id: "linked".into(), player_token: "tok-linked".into(), session_epoch: 2, reason: "bye".into() };
    assert_eq!(s.pending_kicks().await.unwrap(), vec![kick.clone()]);
    // Queued again (a second revocation): replaced, not duplicated.
    s.bump_session(1, "linked", 0).await.unwrap(); // epoch 3
    s.revoke_account_with_kicks(1, 12, "again").await.unwrap();
    let newer = login::PendingKick { session_epoch: 3, reason: "again".into(), ..kick };
    assert_eq!(s.pending_kicks().await.unwrap(), vec![newer]);
    assert!(!s.kick_acked("linked", "tok-linked", 2).await.unwrap(), "an older kick's ack keeps the newer");
    assert!(s.kick_acked("linked", "tok-linked", 3).await.unwrap());
    assert!(s.pending_kicks().await.unwrap().is_empty());
}

#[tokio::test]
async fn kick_queue_sqlite() {
    kick_queue(store().await).await;
}

#[tokio::test]
async fn kick_queue_postgres() {
    if crate::common::pg_admin_url().is_none() { return crate::common::skip_msg() }
    let db = crate::common::PgTestDb::create_async("mmo_test").await.expect("pg env var set");
    kick_queue(LoginStore::open(&db.url).await.unwrap()).await;
    drop(db);
}

/// Revoking an account is one transaction: when a later step fails (here the kick queue is
/// gone), the account's refresh tokens stay — never revoked tokens without their kicks.
#[tokio::test]
async fn revoke_account_with_kicks_is_all_or_nothing() {
    let dir = std::env::temp_dir().join(format!("login-revoke-tx-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("login.sqlite");
    let _ = std::fs::remove_file(&db);
    let s = LoginStore::open(db.to_str().unwrap()).await.unwrap();
    let a = s.create_account("a@b.c", "", 0).await.unwrap().unwrap();
    s.insert_refresh([1; 32], a, [9; 16], 1000).await.unwrap();
    // Break the transaction's last step from outside.
    let raw = sqlx::SqlitePool::connect(&format!("sqlite:{}", db.display())).await.unwrap();
    sqlx::query("DROP TABLE pending_kicks").execute(&raw).await.unwrap();
    raw.close().await;
    assert!(s.revoke_account_with_kicks(a, 10, "revoked").await.is_err());
    assert_eq!(s.take_refresh([1; 32], 10).await.unwrap(), RefreshOutcome::Valid { account: a, family: [9; 16] });
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn oidc_first_login_creates_account() {
    let s = store().await;
    let OidcUpsert::Created(id) = s.upsert_oidc_account("s1", "a@x.io", "ann", 10).await.unwrap() else {
        panic!("expected Created")
    };
    let row = s.account_by_subject("s1").await.unwrap().unwrap();
    assert_eq!((row.account_id, row.display_name.as_str()), (id, "ann"));
    assert_eq!((row.password_hash.as_str(), row.subject.as_deref()), ("", Some("s1")));
    assert!(row.email_verified_at.is_some());
}

#[tokio::test]
async fn oidc_second_login_updates_name_and_email() {
    let s = store().await;
    let OidcUpsert::Created(id) = s.upsert_oidc_account("s1", "a@x.io", "ann", 10).await.unwrap() else {
        panic!("expected Created")
    };
    assert_eq!(s.upsert_oidc_account("s1", "b@x.io", "ann2", 11).await.unwrap(), OidcUpsert::Existing(id));
    let row = s.account_by_subject("s1").await.unwrap().unwrap();
    assert_eq!((row.display_name.as_str(), row.email.as_str()), ("ann2", "b@x.io"));
}

#[tokio::test]
async fn oidc_adopts_password_account_by_email() {
    let s = store().await;
    let id = s.create_account("a@x.io", "hash", 5).await.unwrap().unwrap();
    let token = s.player_token_for(id, "w1", 6, || "tok".into()).await.unwrap();
    assert_eq!(s.upsert_oidc_account("s1", "a@x.io", "ann", 10).await.unwrap(), OidcUpsert::Adopted(id));
    assert_eq!(s.player_token_for(id, "w1", 11, || panic!("must not mint again")).await.unwrap(), token);
    let row = s.account_by_subject("s1").await.unwrap().unwrap();
    assert_eq!((row.account_id, row.email_verified_at, row.password_hash.as_str()), (id, Some(10), ""));
}

/// A world with a host link, where a revocation queues kicks.
fn linked_world() -> WorldRow {
    WorldRow {
        world_id: "linked".into(),
        name: "L".into(),
        status: WorldStatus::Open,
        quic: None,
        webtransport: None,
        cert_hash: None,
        created_at: 1,
        host_link: Some("127.0.0.1:6490".into()),
        host_link_cert: Some("ab".repeat(32)),
    }
}

/// Someone registered this email before the identity provider and never verified it: when the
/// real owner signs in, the squatter's refresh token dies and its live session is kicked, in the
/// same transaction as the adoption. The account's world players stay.
async fn adopt_unverified(s: LoginStore) {
    s.put_world(&linked_world()).await.unwrap();
    let id = s.create_account("a@x.io", "hash", 5).await.unwrap().unwrap();
    let token = s.player_token_for(id, "linked", 6, || "tok".into()).await.unwrap();
    assert_eq!(s.bump_session(id, "linked", 6).await.unwrap(), 1);
    s.insert_refresh([1; 32], id, [9; 16], 1000).await.unwrap();
    assert_eq!(s.upsert_oidc_account("s1", "a@x.io", "ann", 10).await.unwrap(), OidcUpsert::Adopted(id));
    assert!(
        matches!(s.rotate_refresh([1; 32], [2; 32], 1000, 11).await.unwrap(), RefreshOutcome::Reused { .. }),
        "the squatter's token no longer rotates"
    );
    let kick = login::PendingKick {
        world_id: "linked".into(),
        player_token: token.clone(),
        session_epoch: 1,
        reason: "signed in again through the identity provider".into(),
    };
    assert_eq!(s.pending_kicks().await.unwrap(), vec![kick]);
    assert_eq!(s.player_token_for(id, "linked", 12, || panic!("must not mint again")).await.unwrap(), token);
}

#[tokio::test]
async fn oidc_adopting_an_unverified_account_revokes_its_tokens() {
    adopt_unverified(store().await).await;
}

#[tokio::test]
async fn oidc_adopting_an_unverified_account_revokes_its_tokens_postgres() {
    if crate::common::pg_admin_url().is_none() { return crate::common::skip_msg() }
    let db = crate::common::PgTestDb::create_async("mmo_test").await.expect("pg env var set");
    adopt_unverified(LoginStore::open(&db.url).await.unwrap()).await;
    drop(db);
}

/// An account whose email was already verified belongs to that email's owner: adopting it
/// keeps its refresh tokens and its sessions.
async fn adopt_verified(s: LoginStore) {
    s.put_world(&linked_world()).await.unwrap();
    let id = s.ensure_verified_account("a@x.io", 5).await.unwrap();
    s.player_token_for(id, "linked", 6, || "tok".into()).await.unwrap();
    s.bump_session(id, "linked", 6).await.unwrap();
    s.insert_refresh([1; 32], id, [9; 16], 1000).await.unwrap();
    assert_eq!(s.upsert_oidc_account("s1", "a@x.io", "ann", 10).await.unwrap(), OidcUpsert::Adopted(id));
    assert_eq!(
        s.rotate_refresh([1; 32], [2; 32], 1000, 11).await.unwrap(),
        RefreshOutcome::Valid { account: id, family: [9; 16] }
    );
    assert!(s.pending_kicks().await.unwrap().is_empty());
    assert_eq!(s.account(id).await.unwrap().unwrap().email_verified_at, Some(5), "verified time kept");
}

#[tokio::test]
async fn oidc_adopting_a_verified_account_keeps_its_tokens() {
    adopt_verified(store().await).await;
}

#[tokio::test]
async fn oidc_adopting_a_verified_account_keeps_its_tokens_postgres() {
    if crate::common::pg_admin_url().is_none() { return crate::common::skip_msg() }
    let db = crate::common::PgTestDb::create_async("mmo_test").await.expect("pg env var set");
    adopt_verified(LoginStore::open(&db.url).await.unwrap()).await;
    drop(db);
}

#[tokio::test]
async fn oidc_email_of_another_subject_is_refused() {
    let s = store().await;
    s.upsert_oidc_account("s1", "a@x.io", "ann", 10).await.unwrap();
    assert_eq!(s.upsert_oidc_account("s2", "a@x.io", "bob", 11).await.unwrap(), OidcUpsert::EmailTaken);
    assert!(s.account_by_subject("s2").await.unwrap().is_none());
}

#[tokio::test]
async fn oidc_display_name_is_capped() {
    let s = store().await;
    s.upsert_oidc_account("s1", "a@x.io", &"é".repeat(200), 10).await.unwrap();
    let row = s.account_by_subject("s1").await.unwrap().unwrap();
    assert_eq!(row.display_name.chars().count(), DISPLAY_NAME_MAX_CHARS);
}

#[tokio::test]
async fn oidc_existing_subject_moving_to_another_accounts_email_is_refused() {
    let s = store().await;
    s.upsert_oidc_account("s1", "a@x.io", "ann", 10).await.unwrap();
    s.upsert_oidc_account("s2", "b@x.io", "bob", 11).await.unwrap();
    assert_eq!(s.upsert_oidc_account("s1", "b@x.io", "ann", 12).await.unwrap(), OidcUpsert::EmailTaken);
    let row = s.account_by_subject("s1").await.unwrap().unwrap();
    assert_eq!(row.email, "a@x.io");
}
