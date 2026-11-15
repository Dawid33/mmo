//! Stored-terrain notices (R3 Task 19): `notify_cut` after a committed cut, a listener per
//! host (`listen_cuts`: `Cut` per notification, `Resync` on every (re)connect and every poll
//! period), and `stored_regions_since` for what a notice names. Postgres only, except the
//! SQLite no-ops.

use crate::common::hid;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crossbeam::channel::Receiver;
use game::{ChunkCoords, CutId, PortableRegion, RegionCoords, WorldCut};
use world_store::{CutNotice, Dials, NotifyMode, OpenMode, StoreHandle, StoreUrl, TERRAIN_LISTENER_APP};

use crate::codec::{chunk, meta, region_ground};
use crate::common;
use crate::handle::{fence, open};

const A: RegionCoords = RegionCoords { x: 0, z: 0 };
const B: RegionCoords = RegionCoords { x: 1, z: -1 };

/// How long a notice may take to arrive.
const NOTICE_WAIT: Duration = Duration::from_secs(5);

fn pg_db(tag: &str) -> Option<(common::PgTestDb, StoreUrl)> {
    if common::pg_admin_url().is_none() {
        common::skip_msg();
        return None;
    }
    let db = common::PgTestDb::create(&format!("mmo_test_notify_{tag}")).expect("pg env var set");
    let url = StoreUrl::Postgres(db.url.clone());
    Some((db, url))
}

/// A region's save at `tick`: one chunk row (a terrain change), with its ground on
/// the first save.
fn region(tick: u64, ground: bool) -> PortableRegion {
    let mut m = meta();
    m.tick = tick;
    PortableRegion {
        meta: m,
        ground: ground.then(region_ground),
        chunks: [(ChunkCoords::new(0, 0, 0), chunk())].into_iter().collect(),
        ..Default::default()
    }
}

fn cut(id: u64, tick: u64, regions: Vec<(RegionCoords, PortableRegion)>) -> WorldCut {
    WorldCut { id: CutId(id), host: hid(0), tick, regions, in_flight: vec![], players: vec![], history: None, adopts: vec![] }
}

/// The next notice that is not a `Resync`, within `within`.
fn next_cut(rx: &Receiver<CutNotice>, within: Duration) -> Option<CutNotice> {
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        match rx.recv_timeout(left) {
            Ok(CutNotice::Resync) => continue,
            Ok(n) => return Some(n),
            Err(_) => return None,
        }
    }
}

/// Waits for the listener's first `Resync` (it is connected and listening).
fn until_resync(rx: &Receiver<CutNotice>, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else { return false };
        match rx.recv_timeout(left) {
            Ok(CutNotice::Resync) => return true,
            Ok(_) => continue,
            Err(_) => return false,
        }
    }
}

/// Runs `sql` on its own connection to `url`: the first column of every row.
fn query_i64s(url: &str, sql: &str) -> Vec<i64> {
    let (url, sql) = (url.to_string(), sql.to_string());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use sqlx::Connection;
            let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
            let v: Vec<i64> = sqlx::query_scalar(&sql).fetch_all(&mut c).await.unwrap();
            c.close().await.unwrap();
            v
        })
    })
    .join()
    .unwrap()
}

/// A cut's notice reaches a listener of the same world: `Cut` with the cut's tick.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cut_notice_reaches_a_listener() {
    let Some((db, url)) = pg_db("reach") else { return };
    let h = open(&url);
    let notices = h.listen_cuts(NotifyMode::Listen);
    assert!(until_resync(&notices, NOTICE_WAIT), "a Resync once connected");
    h.notify_cut(CutId(7), 1234).unwrap();
    assert_eq!(next_cut(&notices, NOTICE_WAIT), Some(CutNotice::Cut { tick: 1234 }));
    // A second handle (another host) hears it too.
    let other = open(&url);
    let theirs = other.listen_cuts(NotifyMode::Listen);
    assert!(until_resync(&theirs, NOTICE_WAIT));
    h.notify_cut(CutId(8), 1300).unwrap();
    assert_eq!(next_cut(&theirs, NOTICE_WAIT), Some(CutNotice::Cut { tick: 1300 }));
    assert_eq!(next_cut(&notices, NOTICE_WAIT), Some(CutNotice::Cut { tick: 1300 }));
    drop((notices, theirs));
    other.shutdown();
    h.shutdown();
    drop(db);
}

/// The listener's backend is terminated (only that one, found by its application name in
/// this test's own database): a `Resync` follows within 5 s, and later notices still arrive.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn listener_reconnect_emits_resync() {
    let Some((db, url)) = pg_db("reconn") else { return };
    let h = open(&url);
    let notices = h.listen_cuts(NotifyMode::Listen);
    assert!(until_resync(&notices, NOTICE_WAIT), "a Resync once connected");
    let pid_sql = format!(
        "SELECT pid::bigint FROM pg_stat_activity WHERE datname = current_database() AND application_name = '{TERRAIN_LISTENER_APP}'"
    );
    let pids = query_i64s(&db.url, &pid_sql);
    assert_eq!(pids.len(), 1, "one listener backend in this database: {pids:?}");
    let killed = query_i64s(&db.url, &format!("SELECT pg_terminate_backend({})::int::bigint", pids[0]));
    assert_eq!(killed, vec![1], "the listener's backend was terminated");
    assert!(until_resync(&notices, NOTICE_WAIT), "a Resync within 5 s of the reconnect");
    // The terminated backend may linger in `pg_stat_activity` for a moment: wait for the
    // listener's new backend alone.
    let deadline = Instant::now() + NOTICE_WAIT;
    let again = loop {
        let again = query_i64s(&db.url, &pid_sql);
        if (again.len() == 1 && again != pids) || Instant::now() >= deadline {
            break again;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(again.len(), 1, "listening again, on one backend: {again:?}");
    assert_ne!(again, pids, "a new backend");
    h.notify_cut(CutId(9), 4321).unwrap();
    assert_eq!(next_cut(&notices, NOTICE_WAIT), Some(CutNotice::Cut { tick: 4321 }), "later notices still arrive");
    drop(notices);
    h.shutdown();
    drop(db);
}

/// `Poll` mode: no `LISTEN` — only the timer's `Resync`s (its period set to 1 s by the
/// test setter), and a notification is not heard.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn poll_mode_resyncs_without_notify() {
    let Some((db, url)) = pg_db("poll") else { return };
    let h = open(&url);
    h.set_terrain_poll_period(Duration::from_secs(1));
    let notices = h.listen_cuts(NotifyMode::Poll);
    let start = Instant::now();
    let mut resyncs = 0;
    while resyncs < 3 {
        match notices.recv_timeout(Duration::from_secs(3)) {
            Ok(CutNotice::Resync) => resyncs += 1,
            other => panic!("only Resyncs in poll mode, each within the period: {other:?}"),
        }
        if resyncs == 1 {
            h.notify_cut(CutId(3), 300).unwrap();
        }
    }
    assert!(start.elapsed() >= Duration::from_millis(1500), "paced by the period: {:?}", start.elapsed());
    let listeners = query_i64s(
        &db.url,
        &format!("SELECT COUNT(*) FROM pg_stat_activity WHERE datname = current_database() AND application_name = '{TERRAIN_LISTENER_APP}'"),
    );
    assert_eq!(listeners, vec![0], "poll mode holds no listener connection");
    drop(notices);
    h.shutdown();
    drop(db);
}

/// `stored_regions_since(tick)`: the regions a cut at `tick` or later saved, with their
/// terrain versions (`saved_tick >= tick`).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stored_regions_since_returns_only_newer_saves() {
    let Some((db, url)) = pg_db("since") else { return };
    let h = open(&url);
    let f = fence(&h);
    h.commit_cut(cut(1, 1000, vec![(A, region(1000, true)), (B, region(1000, true))]), f.clone()).recv().unwrap().unwrap();
    h.commit_cut(cut(2, 1100, vec![(A, region(1100, false))]), f.clone()).recv().unwrap().unwrap();
    assert_eq!(h.stored_regions_since(1000).unwrap(), BTreeMap::from([(A, 2), (B, 1)]));
    assert_eq!(h.stored_regions_since(1100).unwrap(), BTreeMap::from([(A, 2)]));
    assert!(h.stored_regions_since(1101).unwrap().is_empty());
    assert_eq!(h.stored_regions_since_later(1001).recv().unwrap().unwrap(), BTreeMap::from([(A, 2)]));
    h.shutdown();
    drop(db);
}

/// SQLite: `notify_cut` is a no-op, `listen_cuts` an empty receiver; a read-only handle
/// refuses `notify_cut` (it writes nothing, a notice included).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn sqlite_notify_is_a_no_op_and_read_only_refuses_it() {
    let dir = std::env::temp_dir().join(format!(
        "mmo_ws_notify_{}_{}",
        std::process::id(),
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let url = StoreUrl::Sqlite(dir.join("world.sqlite"));
    let h = open(&url);
    h.notify_cut(CutId(1), 10).unwrap();
    let notices = h.listen_cuts(NotifyMode::Listen);
    assert!(notices.recv_timeout(Duration::from_millis(50)).is_err(), "nothing on SQLite");
    assert!(h.stored_regions_since(0).unwrap().is_empty());
    h.shutdown();
    let ro = StoreHandle::open(&url, Dials::BUILT, OpenMode::ReadOnly).unwrap();
    assert_eq!(ro.notify_cut(CutId(1), 10), Err(world_store::StoreError::ReadOnly));
    ro.shutdown();
    let _ = std::fs::remove_dir_all(dir);
}
