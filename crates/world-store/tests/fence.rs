//! Owner-epoch fencing: a claim bumps a super-region's (or the coordinator's)
//! epoch, so whoever held it before is refused — cut commits, region loads and
//! coordinator saves alike — and writes nothing. Read-only handles, and
//! Postgres opening without a world lock.

use crate::common::hid;
use std::time::Duration;

use game::{ChunkCoords, CutId, InFlightRow, PortableRegion, RegionCoords, WorldCut};
use sqlx::Connection;
use world_store::{Dials, FormatKind, OpenMode, Pool, StoreError, StoreHandle, StoreUrl, newest_migration, open_pool};

use crate::codec::{chunk, intent, meta, player, region_ground};
use crate::common;
use crate::handle::{TEST_SUPER_REGIONS, each_backend, open};

const A: RegionCoords = RegionCoords { x: 0, z: 0 };

/// How long a claim stuck behind a `FOR SHARE` row lock must stay stuck.
const BLOCKED_FOR: Duration = Duration::from_millis(300);

fn region_a() -> PortableRegion {
    PortableRegion {
        meta: meta(),
        chunks: [(ChunkCoords::new(0, 0, 0), chunk())].into_iter().collect(),
        ground: Some(region_ground()),
        entities: vec![],
        players: vec![],
    }
}

/// A cut writing region A, a player record and an in-flight row.
fn cut(h: &StoreHandle, id: u64) -> WorldCut {
    let (pid, _) = h.resolve_player("local:fenced").unwrap();
    WorldCut {
        id: CutId(id),
        host: hid(0),
        tick: 100 * id,
        regions: vec![(A, region_a())],
        in_flight: vec![InFlightRow::Intent { target: A, msg: intent() }],
        players: vec![(pid, player())],
        history: None,
        adopts: vec![],
        releases: vec![],
    }
}

/// Runs raw SQL on the store without migrating (a read-only pool: the
/// schema is not touched by the open).
fn raw_exec_unmigrated(url: &StoreUrl, sql: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (pool, _guard) = open_pool(url, Dials::BUILT, OpenMode::ReadOnly).await.unwrap();
        match &pool {
            Pool::Sqlite(p) => {
                sqlx::query(sql).execute(p).await.unwrap();
                p.close().await;
            }
            Pool::Pg(p) => {
                sqlx::query(sql).execute(p).await.unwrap();
                p.close().await;
            }
        }
    });
}

#[test]
fn stale_owner_commit_is_refused_and_writes_nothing() {
    each_backend("fence_commit", false, |url| {
        let h = open(url);
        let fence_a = h.claim_super_regions(hid(0), TEST_SUPER_REGIONS.to_vec()).unwrap();
        assert!(fence_a.epochs.values().all(|e| *e == 1), "{fence_a:?}");
        let fence_b = h.claim_super_regions(hid(1), TEST_SUPER_REGIONS.to_vec()).unwrap();
        assert!(fence_b.epochs.values().all(|e| *e == 2), "{fence_b:?}");
        assert_eq!(fence_b.owner, hid(1));

        let err = h.commit_cut(cut(&h, 1), fence_a.clone()).recv().unwrap().unwrap_err();
        assert!(matches!(err, StoreError::Fenced { sr: Some(_) }), "{err}");
        assert_eq!(h.load_region(A, &fence_b).unwrap(), None, "no region row");
        assert!(h.stored_regions().unwrap().is_empty());
        assert!(h.load_in_flight(None).unwrap().is_empty(), "no in-flight rows");
        assert_eq!(h.world_info().unwrap().world_tick, 0, "the world tick did not move");
        assert_eq!(h.resolve_player("local:fenced").unwrap().1, None, "no player record");

        // A fence missing a super-region the cut touches is refused too.
        let mut partial = fence_b.clone();
        partial.epochs.remove(&(0, 0));
        let err = h.commit_cut(cut(&h, 2), partial).recv().unwrap().unwrap_err();
        assert_eq!(err, StoreError::Fenced { sr: Some((0, 0)) });

        h.commit_cut(cut(&h, 3), fence_b.clone()).recv().unwrap().unwrap();
        assert!(h.load_region(A, &fence_b).unwrap().is_some());
        assert_eq!(h.world_info().unwrap().world_tick, 300);

        // A cut that touches nothing is still refused to a fenced host: every
        // super-region its fence claimed must still be held.
        let empty = WorldCut { regions: vec![], players: vec![], ..cut(&h, 4) };
        let err = h.commit_cut(empty, fence_a).recv().unwrap().unwrap_err();
        assert!(matches!(err, StoreError::Fenced { .. }), "{err}");
        assert_eq!(h.load_in_flight(None).unwrap().len(), 1, "fence B's in-flight row is kept");
        h.shutdown();
    });
}

/// A region far from the test claims: its super-region is another host's.
const FOREIGN: RegionCoords = RegionCoords { x: 1000, z: 1000 };

/// A host writes the record of a player it handed toward another host's region (the
/// record rides in its cut until that host has it): a record's region is not a
/// super-region the cut touches, so the commit is fenced by the host's own claim only.
#[test]
fn player_record_in_a_foreign_super_region_commits() {
    each_backend("fence_foreign_record", false, |url| {
        let h = open(url);
        let fence = h.claim_super_regions(hid(0), TEST_SUPER_REGIONS.to_vec()).unwrap();
        let foreign_sr = game::super_region_of(FOREIGN, h.super_region_regions());
        let other = h.claim_super_regions(hid(1), vec![foreign_sr]).unwrap();
        assert!(!fence.epochs.contains_key(&foreign_sr) && other.epochs.contains_key(&foreign_sr));
        let mut c = cut(&h, 1);
        c.players[0].1.region = FOREIGN;
        h.commit_cut(c, fence.clone()).recv().unwrap().expect("the record of a player headed elsewhere commits");
        let stored = h.resolve_player("local:fenced").unwrap().1.expect("the record is stored");
        assert_eq!(stored.region, FOREIGN);
        assert!(h.load_region(A, &fence).unwrap().is_some(), "and the cut's region with it");
        h.shutdown();
    });
}

/// The relaxation does not open a hole: a host fenced out of its own claim still writes
/// no player record, wherever the record's region is.
#[test]
fn stale_owner_still_cannot_write_a_player_record() {
    each_backend("fence_stale_record", false, |url| {
        let h = open(url);
        let stale = h.claim_super_regions(hid(0), TEST_SUPER_REGIONS.to_vec()).unwrap();
        let _newer = h.claim_super_regions(hid(1), TEST_SUPER_REGIONS.to_vec()).unwrap();
        for region in [A, FOREIGN] {
            let mut c = WorldCut { regions: vec![], in_flight: vec![], ..cut(&h, 1) };
            c.players[0].1.region = region;
            let err = h.commit_cut(c, stale.clone()).recv().unwrap().unwrap_err();
            assert!(matches!(err, StoreError::Fenced { .. }), "{region:?}: {err}");
            assert_eq!(h.resolve_player("local:fenced").unwrap().1, None, "{region:?}: no player record");
        }
        h.shutdown();
    });
}

#[test]
fn stale_owner_region_load_is_refused() {
    each_backend("fence_load", false, |url| {
        let h = open(url);
        let fence_a = h.claim_super_regions(hid(0), TEST_SUPER_REGIONS.to_vec()).unwrap();
        h.commit_cut(cut(&h, 1), fence_a.clone()).recv().unwrap().unwrap();
        assert!(h.load_region(A, &fence_a).unwrap().is_some());
        let fence_b = h.claim_super_regions(hid(0), vec![(0, 0)]).unwrap();
        assert_eq!(fence_b.epochs[&(0, 0)], 2, "the same owner claiming again also bumps the epoch");
        assert_eq!(h.load_region(A, &fence_a).unwrap_err(), StoreError::Fenced { sr: Some((0, 0)) });
        assert!(h.load_region(A, &fence_b).unwrap().is_some());
        // Unclaimed super-region: a load with no claim for it is refused.
        let far = RegionCoords::new(1000, 1000);
        assert!(matches!(h.load_region(far, &fence_b), Err(StoreError::Fenced { .. })));
        // Terrain is an unfenced read.
        assert!(h.load_terrain(A).unwrap().is_some());
        h.shutdown();
    });
}

/// Postgres has no world lock any more: two writers open one database (two
/// pools, as two processes), and the later claim fences the earlier.
#[test]
fn two_pg_writers_open_without_lock() {
    if common::pg_admin_url().is_none() { return common::skip_msg() }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(common::PgTestDb::create_async("mmo_test")).expect("pg env var set");
    let url = StoreUrl::Postgres(db.url.clone());
    let first = open(&url);
    let second = open(&url);
    let fence_1 = first.claim_super_regions(hid(0), TEST_SUPER_REGIONS.to_vec()).unwrap();
    let fence_2 = second.claim_super_regions(hid(1), TEST_SUPER_REGIONS.to_vec()).unwrap();
    assert!(matches!(first.commit_cut(cut(&first, 1), fence_1).recv().unwrap(), Err(StoreError::Fenced { .. })));
    second.commit_cut(cut(&second, 2), fence_2).recv().unwrap().unwrap();
    second.shutdown();
    first.shutdown();
    drop(db);
}

/// Two writers opening a fresh Postgres database at the same moment (no
/// world lock any more: coordinator and host start together) both succeed,
/// and the database holds one world row.
#[test]
fn two_pg_writers_open_a_fresh_database_at_once() {
    if common::pg_admin_url().is_none() { return common::skip_msg() }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(common::PgTestDb::create_async("mmo_test")).expect("pg env var set");
    let url = StoreUrl::Postgres(db.url.clone());
    let start = std::sync::Arc::new(std::sync::Barrier::new(2));
    let openers: Vec<_> = (0..2)
        .map(|_| {
            let (url, start) = (url.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                StoreHandle::open(&url, Dials::BUILT, OpenMode::Writer)
            })
        })
        .collect();
    let handles: Vec<StoreHandle> =
        openers.into_iter().map(|t| t.join().unwrap().expect("both writers open")).collect();
    let rows: i64 = rt.block_on(async {
        let (pool, _g) = open_pool(&url, Dials::BUILT, OpenMode::ReadOnly).await.unwrap();
        let Pool::Pg(p) = &pool else { panic!("expected postgres") };
        let n = sqlx::query_scalar("SELECT COUNT(*) FROM world").fetch_one(p).await.unwrap();
        p.close().await;
        n
    });
    assert_eq!(rows, 1);
    for h in handles {
        h.shutdown();
    }
    drop(db);
}

#[test]
fn read_only_refuses_writes_and_unknown_schema() {
    each_backend("fence_ro", true, |url| {
        let w = open(url);
        let fence = w.claim_super_regions(hid(0), TEST_SUPER_REGIONS.to_vec()).unwrap();
        w.commit_cut(cut(&w, 1), fence.clone()).recv().unwrap().unwrap();
        // Beside the open writer (SQLite: the writer's file lock is not taken).
        let r = StoreHandle::open(url, Dials::BUILT, OpenMode::ReadOnly).unwrap();
        assert!(r.load_region(A, &fence).unwrap().is_some(), "reads work");
        assert_eq!(r.world_info().unwrap().world_tick, 100);
        assert!(r.resolve_player("local:fenced").unwrap().1.is_some(), "a known player is a read");
        assert_eq!(r.resolve_player("local:new").unwrap_err(), StoreError::ReadOnly);
        assert_eq!(r.commit_cut(cut(&w, 2), fence.clone()).recv().unwrap().unwrap_err(), StoreError::ReadOnly);
        assert_eq!(r.claim_super_regions(hid(0), vec![(0, 0)]).unwrap_err(), StoreError::ReadOnly);
        assert_eq!(r.bootstrap_coordinator(vec![1], vec![]).unwrap_err(), StoreError::ReadOnly);
        let t = world_store::TransitionWrite {
            version: 1,
            core: vec![1],
            history: None,
            decisions: vec![],
            replica: game::coordinator::ReplicaId(1),
            ..Default::default()
        };
        let err = r.write_transition_later(t).recv().unwrap().unwrap_err();
        assert_eq!(err, StoreError::ReadOnly);
        assert!(w.load_region(A, &fence).unwrap().is_some(), "the refused writes changed nothing");
        r.shutdown();
        w.shutdown();

        // A schema at another migration than this build's newest is refused.
        let newest = newest_migration();
        raw_exec_unmigrated(url, &format!("DELETE FROM _sqlx_migrations WHERE version = {newest}"));
        let err = StoreHandle::open(url, Dials::BUILT, OpenMode::ReadOnly).err().unwrap();
        assert_eq!(err, StoreError::OtherFormat { found: (newest - 1) as u32, kind: FormatKind::Schema });
    });
    // A store that was never created is not created by a read-only open.
    let missing = std::env::temp_dir().join(format!("mmo_ws_fence_ro_missing_{}.sqlite", std::process::id()));
    assert!(StoreHandle::open(&StoreUrl::Sqlite(missing.clone()), Dials::BUILT, OpenMode::ReadOnly).is_err());
    assert!(!missing.exists(), "a read-only open created no file");
    // An empty database has no migrations at all.
    let err = StoreHandle::open(&StoreUrl::Memory, Dials::BUILT, OpenMode::ReadOnly).err().unwrap();
    assert_eq!(err, StoreError::OtherFormat { found: 0, kind: FormatKind::Schema });
}

/// A commit's `FOR SHARE` on its super-regions holds a claim back until the
/// commit ends; the claim then bumps the epoch and the old fence is refused.
/// The commit's lock is taken here by its own statement on a raw connection,
/// held open while the claim waits.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn concurrent_claim_and_commit_serialise() {
    if common::pg_admin_url().is_none() { return common::skip_msg() }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(common::PgTestDb::create_async("mmo_test")).expect("pg env var set");
    let url = StoreUrl::Postgres(db.url.clone());
    let h = open(&url);
    let old = h.claim_super_regions(hid(0), TEST_SUPER_REGIONS.to_vec()).unwrap();

    let mut conn = rt.block_on(sqlx::PgConnection::connect(&db.url)).unwrap();
    let mut tx = rt.block_on(conn.begin()).unwrap();
    rt.block_on(
        sqlx::query("SELECT sx, sz, owner, epoch FROM super_regions WHERE (sx = 0 AND sz = 0) ORDER BY sx, sz FOR SHARE")
            .fetch_all(&mut *tx),
    )
    .unwrap();

    let (done_tx, done_rx) = crossbeam::channel::bounded(1);
    let claimer = {
        let h = h.clone();
        std::thread::spawn(move || {
            let _ = done_tx.send(h.claim_super_regions(hid(1), TEST_SUPER_REGIONS.to_vec()));
        })
    };
    assert!(done_rx.recv_timeout(BLOCKED_FOR).is_err(), "the claim waits for the commit's row lock");
    rt.block_on(tx.commit()).unwrap();
    let new = done_rx.recv_timeout(Duration::from_secs(10)).expect("the claim finishes after the commit").unwrap();
    claimer.join().unwrap();
    assert_eq!(new.epochs[&(0, 0)], old.epochs[&(0, 0)] + 1);

    let err = h.commit_cut(cut(&h, 1), old).recv().unwrap().unwrap_err();
    assert!(matches!(err, StoreError::Fenced { .. }), "{err}");
    h.commit_cut(cut(&h, 2), new).recv().unwrap().unwrap();
    h.shutdown();
    rt.block_on(conn.close()).unwrap();
    drop(db);
}

/// Elastic 3: land a committed part released is free at `epoch + 1`, so a region load under a
/// fence that still lists it at its old epoch is refused like any other stale one.
#[test]
fn a_region_load_in_released_land_is_fenced() {
    let Some(w) = crate::two_phase::pg_world() else { return };
    let h = open(&w.url);
    let fence = h.claim_super_regions(hid(0), vec![(0, 0)]).unwrap();
    h.prepare_cut(WorldCut { releases: vec![(0, 0)], ..cut(&h, 1) }, fence.clone(), 0).recv().unwrap().unwrap();
    assert!(h.decide_cut(CutId(1), 100, true, vec![hid(0)], vec![], 0, vec![]).unwrap());
    assert_eq!(h.cut_decided(CutId(1)).unwrap(), Some(true));
    assert_eq!(h.load_region(A, &fence).unwrap_err(), StoreError::Fenced { sr: Some((0, 0)) });
    assert!(h.load_terrain(A).unwrap().is_some(), "the part's region was saved (terrain is an unfenced read)");
    h.shutdown();
}
