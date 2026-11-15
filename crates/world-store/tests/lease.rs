//! The lease on a `hosts` row (R4): `register_host` stamps `seen_ms` and `state`;
//! `heartbeat` moves `seen_ms` for the live boot only.

use crate::common::hid;
use game::CrashPolicy;
use world_store::{Beat, HOST_LIVE, StoreError};

use crate::handle::{each_backend, open, raw_exec, register};

#[test]
fn register_stamps_seen_and_live() {
    each_backend("lease_register", false, |url| {
        let h = open(url);
        let a = register(&h, "a", 10).unwrap();
        let b = register(&h, "b", 11).unwrap();
        assert_eq!((a.id, b.id), (hid(1), hid(2)));
        let now = h.db_now_ms().unwrap();
        let rows = h.hosts().unwrap();
        assert_eq!(rows.iter().map(|r| (r.id, r.state, r.clean)).collect::<Vec<_>>(), vec![(hid(1), HOST_LIVE, false), (hid(2), HOST_LIVE, false)]);
        for r in &rows {
            assert!(r.seen_ms.abs_diff(now) < 5_000, "seen_ms {} vs db now {now}", r.seen_ms);
        }
        h.shutdown();
    });
}

#[test]
fn heartbeat_moves_seen_ms_for_the_live_boot_only() {
    each_backend("lease_heartbeat", true, |url| {
        let h = open(url);
        let a = register(&h, "a", 10).unwrap().id;
        let seen = || h.hosts().unwrap()[0].seen_ms;
        let first = seen();
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert!(h.heartbeat(a, 10).unwrap().live);
        let second = seen();
        assert!(second > first, "{second} > {first}");
        // Another boot under the id, or an id that was never issued: replaced.
        assert!(!h.heartbeat(a, 11).unwrap().live);
        assert!(!h.heartbeat(hid(5), 10).unwrap().live);
        assert_eq!(seen(), second);
        // A dead row no longer takes beats.
        h.shutdown();
        raw_exec(url, "UPDATE hosts SET state = 1");
        let h = open(url);
        let before = h.hosts().unwrap()[0].seen_ms;
        assert!(!h.heartbeat(a, 10).unwrap().live);
        assert_eq!(h.hosts().unwrap()[0].seen_ms, before);
        h.shutdown();
    });
}

/// Elastic 1 Task 7: a beat reports the drain mark of its row — the ms until the host leaves,
/// computed by the database — and `None` while there is none.
#[test]
fn a_beat_reports_the_drain_and_its_remaining_time() {
    each_backend("lease_drain_beat", false, |url| {
        let h = open(url);
        let a = register(&h, "a", 10).unwrap().id;
        assert_eq!(h.heartbeat(a, 10).unwrap(), Beat { live: true, drain_in_ms: None });
        h.drain_host(a, 60_000).unwrap();
        let beat = h.heartbeat(a, 10).unwrap();
        assert!(beat.live);
        let d = beat.drain_in_ms.expect("draining");
        assert!((55_000..=60_000).contains(&d), "{d}");
        // Another boot under the id is not that live row: nothing to report.
        assert_eq!(h.heartbeat(a, 11).unwrap(), Beat { live: false, drain_in_ms: None });
        h.shutdown();
    });
}

/// A drain mark only ever moves EARLIER: marking again with less time brings the deadline
/// forward (to now with 0), with more time it is left as it was (no postponement, no
/// un-drain). Only a dirty live row can be marked: a clean one, a dead one and an absent id
/// are refused (`NotDrainable`, naming why), nothing written.
#[test]
fn a_drain_mark_only_moves_earlier_and_needs_a_live_row() {
    each_backend("lease_drain_mark", true, |url| {
        let h = open(url);
        let a = register(&h, "a", 10).unwrap().id;
        let b = register(&h, "b", 11).unwrap().id;
        let c = register(&h, "c", 12).unwrap().id;
        let left = |h: &world_store::StoreHandle, id| h.hosts().unwrap().into_iter().find(|r| r.id == id).unwrap().drain_in_ms;
        h.drain_host(a, 60_000).unwrap();
        h.drain_host(a, 600_000).unwrap();
        let d = left(&h, a).expect("draining");
        assert!((55_000..=60_000).contains(&d), "not postponed: {d}");
        h.drain_host(a, 30_000).unwrap();
        let d = left(&h, a).expect("draining");
        assert!((25_000..=30_000).contains(&d), "brought forward: {d}");
        h.drain_host(a, 0).unwrap();
        let d = left(&h, a).expect("draining");
        assert!((-5_000..=0).contains(&d), "due now: {d}");
        h.shutdown();
        raw_exec(url, &format!("UPDATE hosts SET clean = 1 WHERE id = {}", b.0));
        raw_exec(url, &format!("UPDATE hosts SET state = 1 WHERE id = {}", c.0));
        let h = open(url);
        for (id, why) in [(b, "left cleanly"), (c, "dead"), (hid(9), "no hosts row")] {
            match h.drain_host(id, 60_000) {
                Err(StoreError::NotDrainable(text)) => {
                    assert!(text.contains(why) && text.contains(&format!("host {}", id.0)), "{id}: {text}")
                }
                other => panic!("{id}: {other:?}"),
            }
        }
        let rows = h.hosts().unwrap();
        assert!(rows.iter().filter(|r| r.id != a).all(|r| r.drain_in_ms.is_none()), "nothing written: {rows:?}");
        assert!(rows.iter().all(|r| r.id != hid(9)), "no row made up");
        h.shutdown();
    });
}

/// Postgres: a held lock on the host's own row makes a beat fail with an error after the
/// lock timeout — it neither hangs nor answers `false` (which means "replaced").
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_heartbeat_behind_a_held_row_lock_errors_instead_of_waiting() {
    use sqlx::Connection;
    use world_store::{HEARTBEAT_LOCK_TIMEOUT_MS, StoreUrl};

    if crate::common::pg_admin_url().is_none() {
        return crate::common::skip_msg();
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(crate::common::PgTestDb::create_async("mmo_test")).expect("pg env var set");
    let h = open(&StoreUrl::Postgres(db.url.clone()));
    let a = register(&h, "a", 10).unwrap().id;
    rt.block_on(async {
        let mut conn = sqlx::PgConnection::connect(&db.url).await.unwrap();
        let mut tx = conn.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM hosts WHERE id = 1 FOR UPDATE").execute(&mut *tx).await.unwrap();
        let started = std::time::Instant::now();
        let h2 = h.clone();
        let beat = tokio::task::spawn_blocking(move || h2.heartbeat(a, 10)).await.unwrap();
        let waited = started.elapsed();
        assert!(matches!(&beat, Err(e) if e.is_lock_conflict()), "a beat behind a row lock is a lock conflict, got {beat:?}");
        assert!(waited < std::time::Duration::from_millis(HEARTBEAT_LOCK_TIMEOUT_MS * 3), "waited {waited:?}");
        tx.rollback().await.unwrap();
        // Released: the beat goes through again.
        let h2 = h.clone();
        assert!(tokio::task::spawn_blocking(move || h2.heartbeat(a, 10)).await.unwrap().unwrap().live);
    });
    h.shutdown();
    drop(db);
}

/// Final review A-I2 (Postgres): a beat never waits for a connection of the store's shared
/// pool — a burst of region loads, a long cut part and a claim waiting on a row lock can
/// hold all of them for longer than the lease, and a healthy host would be declared dead.
/// It goes out on the heartbeat's own connection.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_heartbeat_does_not_wait_for_the_shared_pool() {
    use world_store::{STORE_POOL_MAX, StoreUrl};

    if crate::common::pg_admin_url().is_none() {
        return crate::common::skip_msg();
    }
    const HELD_MS: u64 = 3_000;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(crate::common::PgTestDb::create_async("mmo_test")).expect("pg env var set");
    let h = open(&StoreUrl::Postgres(db.url.clone()));
    let a = register(&h, "a", 10).unwrap().id;
    let before = h.hosts().unwrap()[0].seen_ms;
    std::thread::sleep(std::time::Duration::from_millis(30));
    h.hold_pool_connections(STORE_POOL_MAX, HELD_MS).unwrap();
    let started = std::time::Instant::now();
    assert!(h.heartbeat(a, 10).unwrap().live, "the live boot's beat");
    let waited = started.elapsed();
    assert!(waited < std::time::Duration::from_millis(HELD_MS / 2), "the beat waited {waited:?} for the busy shared pool");
    // Another boot under the id is still answered `false` on that connection.
    assert!(!h.heartbeat(a, 11).unwrap().live);
    // (The read waits for the pool; the beat above did not.)
    assert!(h.hosts().unwrap()[0].seen_ms > before);
    h.shutdown();
    drop(db);
}

/// R4's lone `claim` host (its coordinator runs `Rollback`: nobody to inherit) restarted right
/// after a beat — its old row dirty, live and fresh. Elastic 1: it never waits; its old row,
/// of the same name, is vouched first and so not live: the registration is a total crash that
/// takes the old row's land back (`hosts::a_lone_named_host_restarted_within_the_lease_…`).
#[test]
fn a_lone_claim_host_restarts_after_a_crash_without_waiting() {
    each_backend("lease_lone_claim", false, |url| {
        let h = open(url);
        let first = h.register_host(crate::hosts::new_host("host", 10), CrashPolicy::Succession).unwrap();
        assert!(h.heartbeat(first.id, 10).unwrap().live);
        let started = std::time::Instant::now();
        let again = h.register_host(crate::hosts::new_host("host", 20), CrashPolicy::Succession).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "registered at once");
        assert_eq!((again.id, again.generation, again.bumped), (hid(2), 1, true));
        let rows = h.hosts().unwrap();
        assert_eq!(
            rows.iter().map(|r| (r.id, r.boot, r.state, r.clean)).collect::<Vec<_>>(),
            vec![(again.id, 20, HOST_LIVE, false)],
            "the old row, dead and landless, is pruned"
        );
        h.shutdown();
    });
}
