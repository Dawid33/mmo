//! The lease on a `hosts` row (R4): `register_host` stamps `seen_ms`, `state` and a rising
//! `reg`; `heartbeat` moves `seen_ms` for the live boot only.

use world_store::{HOST_LIVE, HostRow};

use crate::handle::{TEST_SUPER_REGIONS, each_backend, open, raw_exec};

fn row(ordinal: u32, boot: u64) -> HostRow {
    HostRow {
        ordinal,
        boot,
        // Ignored by `register_host`.
        generation: 99,
        clean: true,
        state: 7,
        seen_ms: 1,
        reg: 42,
        peer_addr: None,
        peer_cert: None,
        quic: None,
        webtransport: None,
        wt_cert_hash: None,
    }
}

#[test]
fn register_stamps_seen_and_a_rising_reg() {
    each_backend("lease_register", false, |url| {
        let h = open(url);
        let a = h.claim_super_regions(0, vec![TEST_SUPER_REGIONS[0]]).unwrap();
        let b = h.claim_super_regions(1, vec![TEST_SUPER_REGIONS[1]]).unwrap();
        let boot0 = h.register_host(row(0, 10), &a).unwrap();
        let boot1 = h.register_host(row(1, 11), &b).unwrap();
        assert_eq!((boot0.reg, boot1.reg), (1, 2));
        let now = h.db_now_ms().unwrap();
        let rows = h.hosts().unwrap();
        assert_eq!(rows.iter().map(|r| (r.ordinal, r.reg, r.state)).collect::<Vec<_>>(), vec![(0, 1, HOST_LIVE), (1, 2, HOST_LIVE)]);
        for r in &rows {
            assert!(r.seen_ms.abs_diff(now) < 5_000, "seen_ms {} vs db now {now}", r.seen_ms);
        }
        // A re-registration takes the next number.
        assert_eq!(h.register_host(row(0, 12), &a).unwrap().reg, 3);
        h.shutdown();
    });
}

#[test]
fn heartbeat_moves_seen_ms_for_the_live_boot_only() {
    each_backend("lease_heartbeat", true, |url| {
        let h = open(url);
        let a = h.claim_super_regions(0, vec![TEST_SUPER_REGIONS[0]]).unwrap();
        h.register_host(row(0, 10), &a).unwrap();
        let seen = || h.hosts().unwrap()[0].seen_ms;
        let first = seen();
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert!(h.heartbeat(0, 10).unwrap());
        let second = seen();
        assert!(second > first, "{second} > {first}");
        // Another boot of the ordinal, or an ordinal that never registered: replaced.
        assert!(!h.heartbeat(0, 11).unwrap());
        assert!(!h.heartbeat(5, 10).unwrap());
        assert_eq!(seen(), second);
        // A dead row no longer takes beats.
        h.shutdown();
        raw_exec(url, "UPDATE hosts SET state = 1");
        let h = open(url);
        let before = h.hosts().unwrap()[0].seen_ms;
        assert!(!h.heartbeat(0, 10).unwrap());
        assert_eq!(h.hosts().unwrap()[0].seen_ms, before);
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
    let a = h.claim_super_regions(0, vec![TEST_SUPER_REGIONS[0]]).unwrap();
    h.register_host(row(0, 10), &a).unwrap();
    rt.block_on(async {
        let mut conn = sqlx::PgConnection::connect(&db.url).await.unwrap();
        let mut tx = conn.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM hosts WHERE ordinal = 0 FOR UPDATE").execute(&mut *tx).await.unwrap();
        let started = std::time::Instant::now();
        let h2 = h.clone();
        let beat = tokio::task::spawn_blocking(move || h2.heartbeat(0, 10)).await.unwrap();
        let waited = started.elapsed();
        assert!(beat.is_err(), "a beat behind a row lock is an error, got {beat:?}");
        assert!(waited < std::time::Duration::from_millis(HEARTBEAT_LOCK_TIMEOUT_MS * 3), "waited {waited:?}");
        tx.rollback().await.unwrap();
        // Released: the beat goes through again.
        let h2 = h.clone();
        assert!(tokio::task::spawn_blocking(move || h2.heartbeat(0, 10)).await.unwrap().unwrap());
    });
    h.shutdown();
    drop(db);
}
