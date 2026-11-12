use login::{LoginStore, RefreshOutcome, WorldRow};
use login_api::WorldStatus;

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
    s.set_password(id, "new").await.unwrap();
    s.mark_verified(id, 20).await.unwrap();
    let row = s.account(id).await.unwrap().unwrap();
    assert_eq!((row.password_hash.as_str(), row.email_verified_at), ("new", Some(20)));
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
async fn email_token_single_use_and_expiry() {
    let s = store().await;
    let a = s.create_account("a@b.c", "h", 0).await.unwrap().unwrap();
    s.insert_email_token([1; 32], a, 0, 100).await.unwrap();
    assert_eq!(s.use_email_token([1; 32], 1, 50).await.unwrap(), None, "wrong kind");
    assert_eq!(s.use_email_token([1; 32], 0, 50).await.unwrap(), Some(a));
    assert_eq!(s.use_email_token([1; 32], 0, 51).await.unwrap(), None);
    s.insert_email_token([2; 32], a, 1, 100).await.unwrap();
    assert_eq!(s.use_email_token([2; 32], 1, 100).await.unwrap(), None, "expired");
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

/// `RACERS` concurrent takes of one token / email token: exactly one wins.
async fn race_takes(store: LoginStore) {
    let a = store.create_account("a@b.c", "h", 0).await.unwrap().unwrap();
    store.insert_refresh([1; 32], a, [9; 16], 1000).await.unwrap();
    store.insert_email_token([2; 32], a, 0, 1000).await.unwrap();
    let mut refresh = Vec::new();
    let mut email = Vec::new();
    for _ in 0..RACERS {
        let (s1, s2) = (store.clone(), store.clone());
        refresh.push(tokio::spawn(async move { s1.take_refresh([1; 32], 10).await.unwrap() }));
        email.push(tokio::spawn(async move { s2.use_email_token([2; 32], 0, 10).await.unwrap() }));
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
    let mut used = 0;
    for h in email {
        used += usize::from(h.await.unwrap().is_some());
    }
    assert_eq!(used, 1, "exactly one concurrent email-token use succeeds");
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

/// A password reset is one transaction: when a later step fails (here the kick queue is
/// gone), the password is unchanged, the reset token is not spent and the account's refresh
/// tokens stay — never a changed password with the old sessions alive.
#[tokio::test]
async fn reset_password_is_all_or_nothing() {
    let dir = std::env::temp_dir().join(format!("login-reset-tx-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("login.sqlite");
    let _ = std::fs::remove_file(&db);
    let s = LoginStore::open(db.to_str().unwrap()).await.unwrap();
    let a = s.create_account("a@b.c", "old", 0).await.unwrap().unwrap();
    s.insert_email_token([7; 32], a, 1, 1000).await.unwrap();
    s.insert_refresh([1; 32], a, [9; 16], 1000).await.unwrap();
    // Break the transaction's last step from outside.
    let raw = sqlx::SqlitePool::connect(&format!("sqlite:{}", db.display())).await.unwrap();
    sqlx::query("DROP TABLE pending_kicks").execute(&raw).await.unwrap();
    raw.close().await;
    assert!(s.reset_password([7; 32], 1, "new", 10, "reset").await.is_err());
    assert_eq!(s.account(a).await.unwrap().unwrap().password_hash, "old", "the password is unchanged");
    assert_eq!(s.peek_email_token([7; 32], 1, 10).await.unwrap(), Some(a), "the reset token is not spent");
    assert_eq!(s.take_refresh([1; 32], 10).await.unwrap(), RefreshOutcome::Valid { account: a, family: [9; 16] });
    let _ = std::fs::remove_dir_all(&dir);
}

/// The happy path: token spent, password set, verified, refresh tokens revoked; a dead
/// token changes nothing.
#[tokio::test]
async fn reset_password_spends_sets_and_revokes() {
    let s = store().await;
    let a = s.create_account("a@b.c", "old", 0).await.unwrap().unwrap();
    s.insert_email_token([7; 32], a, 1, 1000).await.unwrap();
    s.insert_refresh([1; 32], a, [9; 16], 1000).await.unwrap();
    assert_eq!(s.reset_password([8; 32], 1, "new", 10, "reset").await.unwrap(), None, "unknown token");
    assert_eq!(s.account(a).await.unwrap().unwrap().password_hash, "old");
    assert_eq!(s.reset_password([7; 32], 1, "new", 10, "reset").await.unwrap(), Some((a, 0)));
    let row = s.account(a).await.unwrap().unwrap();
    assert_eq!((row.password_hash.as_str(), row.email_verified_at), ("new", Some(10)));
    assert_eq!(s.peek_email_token([7; 32], 1, 11).await.unwrap(), None, "spent");
    assert_ne!(s.take_refresh([1; 32], 11).await.unwrap(), RefreshOutcome::Valid { account: a, family: [9; 16] });
    assert_eq!(s.set_password_with_kicks(a, "newer", 12, "set").await.unwrap(), 0);
    assert_eq!(s.account(a).await.unwrap().unwrap().password_hash, "newer");
}
