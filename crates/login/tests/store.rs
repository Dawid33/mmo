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
    let Some(admin) = crate::common::pg_admin_url() else { return crate::common::skip_msg() };
    let db = crate::common::PgTestDb::create(&admin).await;
    race_takes(LoginStore::open(&db.url).await.unwrap()).await;
    db.drop_db().await;
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
    let Some(admin) = crate::common::pg_admin_url() else { return crate::common::skip_msg() };
    let db = crate::common::PgTestDb::create(&admin).await;
    race_rotations(LoginStore::open(&db.url).await.unwrap()).await;
    db.drop_db().await;
}
