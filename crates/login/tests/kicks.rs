//! Pending kicks end with their session, or after `PENDING_KICK_TTL_SECS`.

use login::{LoginStore, WorldRow};
use login_api::WorldStatus;

const TTL: u64 = 86_400;

/// A store with a host-linked world "linked", account 1 joined there (token "tok"), session epoch `epochs`.
async fn linked(s: &LoginStore, epochs: u64) {
    let row = WorldRow {
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
    s.put_world(&row).await.unwrap();
    s.player_token_for(1, "linked", 0, || "tok".into()).await.unwrap();
    for _ in 0..epochs {
        s.bump_session(1, "linked", 0).await.unwrap();
    }
}

async fn sqlite() -> LoginStore {
    LoginStore::open("sqlite::memory:").await.unwrap()
}

async fn ended_session_drops_its_pending_kick(s: LoginStore) {
    linked(&s, 3).await;
    assert_eq!(s.revoke_account_with_kicks(1, 10, "bye").await.unwrap(), 1);
    assert_eq!(s.pending_kicks().await.unwrap().len(), 1);
    assert!(s.end_session("linked", "tok", 3, 11).await.unwrap());
    assert!(s.pending_kicks().await.unwrap().is_empty());
}

async fn newer_session_keeps_its_kick(s: LoginStore) {
    linked(&s, 4).await;
    s.revoke_account_with_kicks(1, 10, "bye").await.unwrap();
    s.end_session("linked", "tok", 3, 11).await.unwrap();
    let kicks = s.pending_kicks().await.unwrap();
    assert_eq!(kicks.len(), 1);
    assert_eq!(kicks[0].session_epoch, 4);
}

async fn old_kick_expires_once(s: LoginStore) {
    linked(&s, 1).await;
    let now: i64 = 1_000_000;
    s.revoke_account_with_kicks(1, now - 86_400, "bye").await.unwrap();
    assert!(s.expire_pending_kicks(now as u64 - 1, TTL).await.unwrap().is_empty(), "one second younger stays");
    assert_eq!(s.pending_kicks().await.unwrap().len(), 1);
    let gone = s.expire_pending_kicks(now as u64, TTL).await.unwrap();
    assert_eq!(gone.len(), 1);
    assert_eq!((gone[0].world_id.as_str(), gone[0].player_token.as_str(), gone[0].session_epoch), ("linked", "tok", 1));
    assert!(s.pending_kicks().await.unwrap().is_empty());
    assert!(s.expire_pending_kicks(now as u64, TTL).await.unwrap().is_empty(), "reported once");
}

macro_rules! both {
    ($name:ident) => {
        mod $name {
            #[tokio::test]
            async fn sqlite() {
                super::$name(super::sqlite().await).await;
            }
            #[tokio::test]
            async fn postgres() {
                if crate::common::pg_admin_url().is_none() { return crate::common::skip_msg() }
                let db = crate::common::PgTestDb::create_async("mmo_test").await.expect("pg env var set");
                super::$name(super::LoginStore::open(&db.url).await.unwrap()).await;
                drop(db);
            }
        }
    };
}
both!(ended_session_drops_its_pending_kick);
both!(newer_session_keeps_its_kick);
both!(old_kick_expires_once);
