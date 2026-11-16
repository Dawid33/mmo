//! Two-phase save cuts (R3): each host prepares its fenced part of a cut
//! (`PREPARE TRANSACTION`), one decision row commits or aborts every part, and a
//! recovery sweep finishes whatever a crash left prepared. Postgres only; SQLite
//! refuses all of it.

use crate::common::hid;
use std::time::Duration;

use game::{ChunkCoords, CutId, InFlightRow, PortableRegion, RegionCoords, WorldCut};
use sqlx::Connection;
use world_store::{Dials, Fence, OpenMode, StoreError, StoreHandle, StoreUrl, cut_gid};

use crate::codec::{chunk, intent, meta, region_ground};
use crate::common;
use crate::handle::{each_backend, open, register};

/// Host 0's region (super-region (0, 0)).
const A: RegionCoords = RegionCoords { x: 0, z: 0 };
/// Host 1's region (super-region (0, -1)).
const B: RegionCoords = RegionCoords { x: 1, z: -1 };

/// How long a claim stuck behind a prepared part's row lock must stay stuck.
const BLOCKED_FOR: Duration = Duration::from_millis(300);

fn region() -> PortableRegion {
    PortableRegion {
        meta: meta(),
        chunks: [(ChunkCoords::new(0, 0, 0), chunk())].into_iter().collect(),
        ground: Some(region_ground()),
        ..Default::default()
    }
}

/// Host `host`'s part of cut `id`: its region and one in-flight row.
fn part(id: u64, host: game::HostId) -> WorldCut {
    let rc = if host == hid(0) { A } else { B };
    WorldCut {
        id: CutId(id),
        host,
        tick: 100 * id,
        regions: vec![(rc, region())],
        in_flight: vec![InFlightRow::Intent { target: rc, msg: intent() }],
        players: vec![],
        history: None,
        adopts: vec![],
    }
}

/// A throwaway Postgres world. Its drop rolls back any prepared transaction a
/// failed test left behind (the database could not be dropped otherwise).
pub(crate) struct PgWorld {
    db: common::PgTestDb,
    pub(crate) url: StoreUrl,
}

impl Drop for PgWorld {
    fn drop(&mut self) {
        let url = self.db.url.clone();
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let Ok(mut c) = sqlx::PgConnection::connect(&url).await else { return };
                let gids: Vec<String> =
                    sqlx::query_scalar("SELECT gid FROM pg_prepared_xacts WHERE database = current_database()")
                        .fetch_all(&mut c)
                        .await
                        .unwrap_or_default();
                for gid in gids {
                    eprintln!("two_phase: rolling back leftover prepared transaction {gid}");
                    let _ = sqlx::raw_sql(&format!("ROLLBACK PREPARED '{gid}'")).execute(&mut c).await;
                }
                let _ = c.close().await;
            });
        })
        .join();
    }
}

pub(crate) fn pg_world() -> Option<PgWorld> {
    if common::pg_admin_url().is_none() {
        common::skip_msg();
        return None;
    }
    let db = common::PgTestDb::create("mmo_test_2pc").expect("pg env var set");
    let url = StoreUrl::Postgres(db.url.clone());
    Some(PgWorld { db, url })
}

/// Runs `sql` on its own connection and returns the first column of the first row.
pub(crate) fn scalar_i64(w: &PgWorld, sql: &str) -> i64 {
    let url = w.db.url.clone();
    let sql = sql.to_string();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
            let v: i64 = sqlx::query_scalar(&sql).fetch_one(&mut c).await.unwrap();
            c.close().await.unwrap();
            v
        })
    })
    .join()
    .unwrap()
}

pub(crate) fn exec(w: &PgWorld, sql: &str) {
    let url = w.db.url.clone();
    let sql = sql.to_string();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
            sqlx::query(&sql).execute(&mut c).await.unwrap();
            c.close().await.unwrap();
        })
    })
    .join()
    .unwrap()
}

/// The prepared transactions of this test's database.
pub(crate) fn prepared(w: &PgWorld) -> i64 {
    scalar_i64(w, "SELECT COUNT(*) FROM pg_prepared_xacts WHERE database = current_database()")
}

/// Host 0 holds super-region (0, 0), host 1 holds (0, -1).
fn fences(h: &StoreHandle) -> (Fence, Fence) {
    let s = h.super_region_regions();
    let f0 = h.claim_super_regions(hid(0), vec![game::super_region_of(A, s)]).unwrap();
    let f1 = h.claim_super_regions(hid(1), vec![game::super_region_of(B, s)]).unwrap();
    (f0, f1)
}

/// Registers hosts `0..n` in order (ids issued from 0: `world.next_host_id` set to 0 first, as
/// a world migrated from fixed ordinals continues) — these tests are about cuts, not ids.
fn register_from_zero(w: &PgWorld, h: &StoreHandle, n: u64) {
    exec(w, "UPDATE world SET next_host_id = 0");
    for o in 0..n {
        assert_eq!(register(h, &format!("host-{o}"), 1).unwrap().id, hid(o), "issued in order");
    }
}

/// Prepares host 0's part of cut `id` on `h0` and host 1's on `h1` (each host has
/// its own store handle: a handle stages one undecided part at a time), at generation 0.
fn prepare_both(h0: &StoreHandle, h1: &StoreHandle, id: u64, f0: &Fence, f1: &Fence) {
    h0.prepare_cut(part(id, hid(0)), f0.clone(), 0).recv().unwrap().unwrap();
    h1.prepare_cut(part(id, hid(1)), f1.clone(), 0).recv().unwrap().unwrap();
}

/// Backends of this test's database inside a transaction block that is idle: a
/// connection handed back to a pool with its transaction still open shows up here.
fn idle_in_transaction(w: &PgWorld) -> i64 {
    scalar_i64(
        w,
        "SELECT COUNT(*) FROM pg_stat_activity WHERE datname = current_database() \
         AND state LIKE 'idle in transaction%'",
    )
}

#[test]
fn gid_names_world_cut_and_host() {
    assert_eq!(cut_gid(16_384, CutId(7), hid(2)), "mmo-cut-16384-7-2");
}

#[test]
fn prepared_part_is_invisible_until_decided() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    h.two_phase_ready().expect("the dev Postgres runs with max_prepared_transactions > 0");
    let (f0, _) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let done = h.prepare_cut(part(1, hid(0)), f0.clone(), 0).recv().unwrap().unwrap();
    assert_eq!(done.id, CutId(1));
    assert_eq!(prepared(&w), 1);
    // Another process sees nothing of it.
    let other = open(&w.url);
    assert_eq!(other.load_region(A, &f0).unwrap(), None);
    assert!(other.stored_regions().unwrap().is_empty());
    assert!(other.load_in_flight(None).unwrap().is_empty());
    assert_eq!(other.world_info().unwrap().world_tick, 0);
    assert_eq!(other.cut_decision(CutId(1)).unwrap(), None);
    assert!(h.decide_cut(CutId(1), 100, true, vec![hid(0)], vec![], c, 0, vec![]).unwrap());
    assert!(other.load_region(A, &f0).unwrap().is_some());
    assert_eq!(other.world_info().unwrap().world_tick, 100);
    assert_eq!(prepared(&w), 0);
    other.shutdown();
    h.shutdown();
}

#[test]
fn commit_decision_applies_every_part_and_the_world_tick() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let h1 = open(&w.url);
    prepare_both(&h, &h1, 2, &f0, &f1);
    assert_eq!(prepared(&w), 2);
    assert!(h.decide_cut(CutId(2), 200, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    assert!(h.load_region(A, &f0).unwrap().is_some());
    assert!(h.load_region(B, &f1).unwrap().is_some());
    assert_eq!(h.load_in_flight(None).unwrap().len(), 2, "both hosts' in-flight rows");
    assert_eq!(h.world_info().unwrap().world_tick, 200, "the decision writes the world tick");
    assert_eq!(h.cut_decision(CutId(2)).unwrap(), Some(true));
    assert_eq!(h.last_committed_cut().unwrap(), CutId(2), "the peer journal's boot base");
    assert_eq!(prepared(&w), 0);
    // Deciding again changes nothing and finds nothing left to commit.
    assert!(h.decide_cut(CutId(2), 200, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    h.shutdown();
}

/// A world cut taken while the same chunk is edited on two hosts, each in its own
/// region: each host's part writes only its own region's `chunks` rows, and a part
/// staged on a stale epoch is refused (`Fenced`) and writes no `chunks` row.
#[test]
fn a_cut_part_writes_only_its_own_regions_chunks() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let h1 = open(&w.url);
    let at = ChunkCoords::new(0, 0, 0);
    let edited = |id: u16| {
        let mut ch = chunk();
        ch.set_block(9, 9, 9, game::Block::new(game::BlockId(id)));
        ch
    };
    let with_chunks = |mut p: WorldCut, chunks: Vec<(ChunkCoords, game::Chunk)>| {
        p.regions[0].1.chunks = chunks.into_iter().collect();
        p
    };
    h.prepare_cut(with_chunks(part(3, hid(0)), vec![(at, edited(5))]), f0.clone(), 0).recv().unwrap().unwrap();
    h1.prepare_cut(with_chunks(part(3, hid(1)), vec![(at, edited(6))]), f1.clone(), 0).recv().unwrap().unwrap();
    assert!(h.decide_cut(CutId(3), 300, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    assert_eq!(h.load_region(A, &f0).unwrap().unwrap().chunks, vec![(at, edited(5))], "host 0's edit in A");
    assert_eq!(h.load_region(B, &f1).unwrap().unwrap().chunks, vec![(at, edited(6))], "host 1's edit in B");
    assert_eq!(scalar_i64(&w, "SELECT COUNT(*) FROM chunks WHERE rx = 0 AND rz = 0"), 1);
    assert_eq!(scalar_i64(&w, "SELECT COUNT(*) FROM chunks WHERE rx = 1 AND rz = -1"), 1);
    assert_eq!(scalar_i64(&w, "SELECT COUNT(*) FROM chunks"), 2, "no part wrote a row of the other's region");

    // Host 0's super-region is claimed by another owner since: host 0's next part, on its
    // old epoch, edits A's chunk again and adds one — refused, nothing written.
    let newer = h.claim_super_regions(hid(2), f0.epochs.keys().copied().collect()).unwrap();
    let stale = open(&w.url);
    let part4 = with_chunks(part(4, hid(0)), vec![(at, edited(7)), (ChunkCoords::new(0, 1, 0), edited(8))]);
    let err = stale.prepare_cut(part4, f0.clone(), 0).recv().unwrap().unwrap_err();
    assert!(matches!(err, StoreError::Fenced { sr: Some(_) }), "{err}");
    assert_eq!(prepared(&w), 0, "rolled back, not prepared");
    assert_eq!(scalar_i64(&w, "SELECT COUNT(*) FROM chunks"), 2, "the stale part wrote no chunks row");
    assert_eq!(h.load_region(A, &newer).unwrap().unwrap().chunks, vec![(at, edited(5))], "A's chunk unchanged");
    stale.shutdown();
    h1.shutdown();
    h.shutdown();
}

#[test]
fn abort_decision_leaves_nothing_and_no_prepared_xact() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let h1 = open(&w.url);
    prepare_both(&h, &h1, 3, &f0, &f1);
    assert!(!h.decide_cut(CutId(3), 300, false, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    assert!(h.stored_regions().unwrap().is_empty());
    assert!(h.load_in_flight(None).unwrap().is_empty());
    assert_eq!(h.world_info().unwrap().world_tick, 0);
    assert_eq!(h.cut_decision(CutId(3)).unwrap(), Some(false));
    assert_eq!(h.last_committed_cut().unwrap(), CutId(0), "an aborted cut is no journal base");
    assert_eq!(prepared(&w), 0);
    h.shutdown();
}

#[test]
fn first_decision_wins() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let h1 = open(&w.url);
    prepare_both(&h, &h1, 4, &f0, &f1);
    assert!(!h.abort_cut(CutId(4), 400).unwrap(), "a host's abort is the first decision");
    assert!(!h.abort_cut(CutId(4), 400).unwrap());
    assert!(!h.decide_cut(CutId(4), 400, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap(), "the later commit loses");
    assert!(h.stored_regions().unwrap().is_empty());
    assert_eq!(h.world_info().unwrap().world_tick, 0);
    assert_eq!(prepared(&w), 0, "the losing commit rolled the parts back");
    // Each host hears the cut is done; its next part is accepted.
    assert_eq!(h.cut_decided(CutId(4)).unwrap(), Some(false));
    assert_eq!(h1.cut_decided(CutId(4)).unwrap(), Some(false));
    // And the other way round: a decided commit is not undone by a late abort.
    prepare_both(&h, &h1, 5, &f0, &f1);
    assert!(h.decide_cut(CutId(5), 500, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    assert!(h.abort_cut(CutId(5), 500).unwrap());
    h.shutdown();
}

#[test]
fn resolve_finishes_a_decided_cut_stopped_between_commits() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let h1 = open(&w.url);
    prepare_both(&h, &h1, 6, &f0, &f1);
    h.stop_after_commit_prepared(1);
    let err = h.decide_cut(CutId(6), 600, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap_err();
    assert_eq!(err, StoreError::Backend("test stop".into()));
    assert_eq!(prepared(&w), 1, "one part is still prepared");
    // A process starting later (here: another handle) finishes it.
    let other = open(&w.url);
    assert_eq!(other.resolve_prepared().unwrap(), vec![(CutId(6), true)]);
    assert!(other.load_region(A, &f0).unwrap().is_some());
    assert!(other.load_region(B, &f1).unwrap().is_some());
    assert_eq!(other.world_info().unwrap().world_tick, 600);
    assert_eq!(prepared(&w), 0);
    assert!(other.resolve_prepared().unwrap().is_empty(), "nothing left to resolve");
    other.shutdown();
    h.shutdown();
}

#[test]
fn resolve_aborts_an_undecided_cut() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    let h1 = open(&w.url);
    prepare_both(&h, &h1, 7, &f0, &f1);
    assert_eq!(h.resolve_prepared().unwrap(), vec![(CutId(7), false)]);
    assert_eq!(h.cut_decision(CutId(7)).unwrap(), Some(false), "the abort is recorded first");
    assert!(h.stored_regions().unwrap().is_empty());
    assert_eq!(prepared(&w), 0);
    h.shutdown();
}

/// Elastic 1: the staged parts are found by their prepared transactions' names alone, for
/// any host id — no host count says which hosts to look for. SQLite stages nothing.
#[test]
fn staged_parts_of_unknown_hosts_are_found() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let h41 = open(&w.url);
    let s = h.super_region_regions();
    let f3 = h.claim_super_regions(hid(3), vec![game::super_region_of(A, s)]).unwrap();
    let f41 = h.claim_super_regions(hid(41), vec![game::super_region_of(B, s)]).unwrap();
    let mut p3 = part(5, hid(3));
    p3.regions = vec![(A, region())];
    p3.in_flight = vec![InFlightRow::Intent { target: A, msg: intent() }];
    h.prepare_cut(p3, f3, 0).recv().unwrap().unwrap();
    h41.prepare_cut(part(5, hid(41)), f41, 0).recv().unwrap().unwrap();
    let mut found = h.staged_cut_parts().unwrap();
    found.sort();
    assert_eq!(found, vec![(CutId(5), hid(3)), (CutId(5), hid(41))]);
    assert_eq!(h.resolve_prepared().unwrap(), vec![(CutId(5), false)], "both resolved");
    assert!(h.staged_cut_parts().unwrap().is_empty());
    assert_eq!(prepared(&w), 0);
    h41.shutdown();
    h.shutdown();
    let sqlite = open(&StoreUrl::Memory);
    assert_eq!(sqlite.staged_cut_parts().unwrap(), vec![], "SQLite stages nothing");
    sqlite.shutdown();
}

/// Only this database's prepared cuts are touched: another world's stay prepared.
#[test]
fn resolve_touches_only_its_own_world() {
    let Some(w) = pg_world() else { return };
    let Some(w2) = pg_world() else { return };
    let h = open(&w.url);
    let h2 = open(&w2.url);
    let (f0, _) = fences(&h);
    let (g0, _) = fences(&h2);
    h.prepare_cut(part(8, hid(0)), f0, 0).recv().unwrap().unwrap();
    h2.prepare_cut(part(8, hid(0)), g0, 0).recv().unwrap().unwrap();
    assert_eq!(h.resolve_prepared().unwrap(), vec![(CutId(8), false)]);
    assert_eq!(prepared(&w), 0);
    assert_eq!(prepared(&w2), 1, "the other world's part of the same cut id is not touched");
    assert_eq!(h2.cut_decision(CutId(8)).unwrap(), None);
    assert_eq!(h2.resolve_prepared().unwrap(), vec![(CutId(8), false)]);
    assert_eq!(prepared(&w2), 0);
    h2.shutdown();
    h.shutdown();
}

#[test]
fn fenced_host_cannot_prepare() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (stale, _) = fences(&h);
    let newer = h.claim_super_regions(hid(1), stale.epochs.keys().copied().collect()).unwrap();
    let err = h.prepare_cut(part(9, hid(0)), stale, 0).recv().unwrap().unwrap_err();
    assert!(matches!(err, StoreError::Fenced { sr: Some(_) }), "{err}");
    assert_eq!(prepared(&w), 0, "rolled back, not prepared");
    assert!(h.stored_regions().unwrap().is_empty());
    // Every connection of every pool on this database (the store's 4 among them) is
    // outside a transaction block: the refused prepare's ROLLBACK ran on its own
    // connection before it went back to the pool.
    assert_eq!(idle_in_transaction(&w), 0, "no connection was handed back inside the transaction");
    // A refusal that staged nothing settles the part: the next one is accepted.
    let mut fresh = part(10, hid(1));
    fresh.regions = vec![(A, region())];
    h.prepare_cut(fresh, newer, 0).recv().unwrap().unwrap();
    assert_eq!(prepared(&w), 1);
    assert_eq!(h.resolve_prepared().unwrap(), vec![(CutId(10), false)]);
    h.shutdown();
}

#[test]
fn stale_generation_cannot_prepare() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, _) = fences(&h);
    let err = h.prepare_cut(part(11, hid(0)), f0.clone(), 1).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::StaleGeneration { stored: 0, mine: 1 });
    assert_eq!(prepared(&w), 0);
    assert_eq!(idle_in_transaction(&w), 0);
    assert!(h.stored_regions().unwrap().is_empty());
    h.prepare_cut(part(12, hid(0)), f0, 0).recv().unwrap().unwrap();
    assert_eq!(h.resolve_prepared().unwrap(), vec![(CutId(12), false)]);
    h.shutdown();
}

/// A part prepared at a generation the world has since left is never committed:
/// the decision checks the generation under the world row's lock and records nothing.
#[test]
fn stale_generation_cannot_decide_a_commit() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, _) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    h.prepare_cut(part(13, hid(0)), f0, 0).recv().unwrap().unwrap();
    exec(&w, "UPDATE world SET generation = 1 WHERE id = 1");
    let err = h.decide_cut(CutId(13), 1300, true, vec![hid(0)], vec![], c, 0, vec![]).unwrap_err();
    assert_eq!(err, StoreError::StaleGeneration { stored: 1, mine: 0 });
    assert_eq!(h.cut_decision(CutId(13)).unwrap(), None, "nothing recorded");
    assert_eq!(prepared(&w), 1, "nothing committed");
    assert_eq!(h.world_info().unwrap().world_tick, 0);
    assert_eq!(h.resolve_prepared().unwrap(), vec![(CutId(13), false)]);
    assert!(h.stored_regions().unwrap().is_empty());
    h.shutdown();
}

#[test]
fn stale_coordinator_cannot_decide() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, _) = fences(&h);
    let stale = h.claim_coordinator().unwrap();
    let newer = h.claim_coordinator().unwrap();
    h.prepare_cut(part(14, hid(0)), f0, 0).recv().unwrap().unwrap();
    let err = h.decide_cut(CutId(14), 1400, true, vec![hid(0)], vec![], stale, 0, vec![]).unwrap_err();
    assert_eq!(err, StoreError::Fenced { sr: None });
    assert_eq!(h.cut_decision(CutId(14)).unwrap(), None, "nothing recorded");
    assert_eq!(prepared(&w), 1, "nothing committed or rolled back");
    let err = h.decide_cut(CutId(14), 1400, false, vec![hid(0)], vec![], stale, 0, vec![]).unwrap_err();
    assert_eq!(err, StoreError::Fenced { sr: None }, "an abort decision is fenced too");
    assert_eq!(prepared(&w), 1);
    assert!(h.decide_cut(CutId(14), 1400, true, vec![hid(0)], vec![], newer, 0, vec![]).unwrap());
    assert_eq!(prepared(&w), 0);
    h.shutdown();
}

/// The prepared part keeps its `FOR SHARE` on the host's super-region rows: a claim of
/// them by another process waits until the decision, then bumps the epoch.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn claim_waits_for_the_prepared_part_and_proceeds_after_the_decision() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, _) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    h.prepare_cut(part(15, hid(0)), f0.clone(), 0).recv().unwrap().unwrap();
    let other = open(&w.url);
    let (done_tx, done_rx) = crossbeam::channel::bounded(1);
    let claimer = {
        let other = other.clone();
        let srs: Vec<_> = f0.epochs.keys().copied().collect();
        std::thread::spawn(move || {
            let _ = done_tx.send(other.claim_super_regions(hid(2), srs));
        })
    };
    assert!(done_rx.recv_timeout(BLOCKED_FOR).is_err(), "the claim waits for the prepared part");
    assert!(h.decide_cut(CutId(15), 1500, true, vec![hid(0)], vec![], c, 0, vec![]).unwrap());
    let new = done_rx.recv_timeout(Duration::from_secs(10)).expect("the claim finishes after the decision").unwrap();
    claimer.join().unwrap();
    let sr = game::super_region_of(A, h.super_region_regions());
    assert_eq!(new.epochs[&sr], f0.epochs[&sr] + 1);
    assert!(other.load_region(A, &new).unwrap().is_some(), "the part was committed before the claim");
    assert_eq!(prepared(&w), 0);
    other.shutdown();
    h.shutdown();
}

#[test]
fn leaving_host_is_clean_only_if_committed() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    register_from_zero(&w, &h, 2);
    let c = h.claim_coordinator().unwrap();
    let clean = |h: &StoreHandle| h.hosts().unwrap().iter().map(|r| (r.id, r.clean)).collect::<Vec<_>>();
    let h1 = open(&w.url);
    prepare_both(&h, &h1, 16, &f0, &f1);
    assert!(!h.decide_cut(CutId(16), 1600, false, vec![hid(0), hid(1)], vec![hid(1)], c, 0, vec![]).unwrap());
    assert_eq!(clean(&h), vec![(hid(0), false), (hid(1), false)], "an aborted cut leaves nobody clean");
    h.cut_decided(CutId(16)).unwrap();
    h1.cut_decided(CutId(16)).unwrap();
    prepare_both(&h, &h1, 17, &f0, &f1);
    assert!(h.decide_cut(CutId(17), 1700, true, vec![hid(0), hid(1)], vec![hid(1)], c, 0, vec![]).unwrap());
    assert_eq!(clean(&h), vec![(hid(0), false), (hid(1), true)], "only the leaving host");
    assert_eq!(prepared(&w), 0);
    h.shutdown();
}

/// A commit decision must list every host whose `hosts` row is not clean: host 2
/// registered (row dirty) while a cut of hosts {0, 1} was pending, so it may hold state
/// that cut does not save (an entity it acked from a peer, one it sent away) → abort.
#[test]
fn commit_beside_a_dirty_unlisted_host_becomes_an_abort() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let h1 = open(&w.url);
    let (f0, f1) = fences(&h);
    h.claim_super_regions(hid(2), vec![(7, 7)]).unwrap();
    register_from_zero(&w, &h, 3);
    let c = h.claim_coordinator().unwrap();
    prepare_both(&h, &h1, 40, &f0, &f1);
    assert!(!h.decide_cut(CutId(40), 4000, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    assert_eq!(h.cut_decision(CutId(40)).unwrap(), Some(false), "an abort is recorded");
    assert!(h.stored_regions().unwrap().is_empty(), "nothing visible");
    assert_eq!(h.world_info().unwrap().world_tick, 0);
    assert_eq!(prepared(&w), 0, "both parts are rolled back");
    h1.shutdown();
    h.shutdown();
}

/// A leave cut (the leaver listed, dirty until this very decision) commits; a host that
/// registers while a cut is pending (a restarted leaver is a new host) aborts a commit that
/// does not list it; listing every dirty host commits.
#[test]
fn host_returning_from_a_leave_aborts_a_commit_that_excludes_it() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let h1 = open(&w.url);
    let (f0, f1) = fences(&h);
    register_from_zero(&w, &h, 2);
    let c = h.claim_coordinator().unwrap();
    let settle = |cut: u64| {
        h.cut_decided(CutId(cut)).unwrap();
        h1.cut_decided(CutId(cut)).unwrap();
    };
    // Host 1 leaves: listed, dirty until this decision marks it clean.
    prepare_both(&h, &h1, 50, &f0, &f1);
    assert!(h.decide_cut(CutId(50), 5000, true, vec![hid(0), hid(1)], vec![hid(1)], c, 0, vec![]).unwrap(), "the leave cut commits");
    settle(50);
    // Host 0 alone: host 1's row is clean, unlisted is fine.
    h.prepare_cut(part(51, hid(0)), f0.clone(), 0).recv().unwrap().unwrap();
    assert!(h.decide_cut(CutId(51), 5100, true, vec![hid(0)], vec![], c, 0, vec![]).unwrap(), "a clean row need not be listed");
    settle(51);
    // Host 1's process comes back — a new host, id 2 — while cut 52 of host 0 is pending.
    h.prepare_cut(part(52, hid(0)), f0.clone(), 0).recv().unwrap().unwrap();
    let back = register(&h1, "host-1", 2).unwrap();
    assert_eq!((back.id, back.bumped), (hid(2), false));
    assert!(!h.decide_cut(CutId(52), 5200, true, vec![hid(0)], vec![], c, 0, vec![]).unwrap(), "host 2 is dirty and unlisted");
    settle(52);
    assert_eq!(h.world_info().unwrap().world_tick, 5100, "cut 52 is not visible");
    // The next cut lists it (its part saves no region: it owns no land): commits.
    h.prepare_cut(part(53, hid(0)), f0.clone(), 0).recv().unwrap().unwrap();
    let landless = Fence { owner: back.id, epochs: Default::default(), all_hosts: false };
    let empty = WorldCut { regions: vec![], in_flight: vec![], ..part(53, back.id) };
    h1.prepare_cut(empty, landless, 0).recv().unwrap().unwrap();
    assert!(h.decide_cut(CutId(53), 5300, true, vec![hid(0), back.id], vec![], c, 0, vec![]).unwrap());
    settle(53);
    assert_eq!(prepared(&w), 0);
    h1.shutdown();
    h.shutdown();
}

/// Elastic 1 Task 4: a host registered AFTER a cut's begin was recorded (`record_cut_begin`:
/// its id at or above the recorded bound) may be absent from the cut's decision — it owned
/// nothing at the cut's tick; one registered before the record must be listed (an abort
/// otherwise); a cut with no begin recorded keeps the rule before. The record is fenced on
/// the coordinator's epoch and its first writer wins.
#[test]
fn a_host_registered_after_the_cuts_begin_need_not_be_listed() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let h1 = open(&w.url);
    let (f0, f1) = fences(&h);
    register_from_zero(&w, &h, 2);
    let c = h.claim_coordinator().unwrap();
    // Cut 60: begun with hosts 0 and 1 issued (bound 2); host 2 registers after it.
    assert_eq!(h.record_cut_begin(CutId(60), c).unwrap(), hid(2), "the next id to issue");
    let late = register(&h1, "late", 3).unwrap();
    assert_eq!(late.id, hid(2));
    assert_eq!(h.record_cut_begin(CutId(60), c).unwrap(), hid(2), "first writer wins");
    prepare_both(&h, &h1, 60, &f0, &f1);
    assert!(h.decide_cut(CutId(60), 6000, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap(), "host 2 joined after the begin");
    h.cut_decided(CutId(60)).unwrap();
    h1.cut_decided(CutId(60)).unwrap();
    // Cut 61: begun AFTER host 2 registered (bound 3): host 2 must be listed.
    assert_eq!(h.record_cut_begin(CutId(61), c).unwrap(), hid(3));
    prepare_both(&h, &h1, 61, &f0, &f1);
    assert!(!h.decide_cut(CutId(61), 6100, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap(), "host 2 registered before the begin");
    h.cut_decided(CutId(61)).unwrap();
    h1.cut_decided(CutId(61)).unwrap();
    // Cut 62: no begin recorded — every dirty host listed, as before.
    prepare_both(&h, &h1, 62, &f0, &f1);
    assert!(!h.decide_cut(CutId(62), 6200, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap(), "no begin: the rule before");
    h.cut_decided(CutId(62)).unwrap();
    h1.cut_decided(CutId(62)).unwrap();
    assert_eq!(h.world_info().unwrap().world_tick, 6000, "only cut 60 is visible");
    // A stale coordinator records nothing.
    let newer = h1.claim_coordinator().unwrap();
    assert!(matches!(h.record_cut_begin(CutId(63), c), Err(StoreError::Fenced { .. })));
    assert_eq!(h.record_cut_begin(CutId(63), newer).unwrap(), hid(3));
    assert_eq!(prepared(&w), 0);
    h1.shutdown();
    h.shutdown();
}

/// Elastic 2a: a decision prunes every `cut_begins` row of an OLDER cut (never read again) and
/// keeps its own and newer ones; every `cut_decisions` row is kept.
#[test]
fn a_decision_prunes_older_begins_and_keeps_every_decision() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let h1 = open(&w.url);
    let (f0, f1) = fences(&h);
    register_from_zero(&w, &h, 2);
    let c = h.claim_coordinator().unwrap();
    for cut in [70, 71] {
        h.record_cut_begin(CutId(cut), c).unwrap();
        prepare_both(&h, &h1, cut, &f0, &f1);
        assert!(h.decide_cut(CutId(cut), 100 * cut, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
        h.cut_decided(CutId(cut)).unwrap();
        h1.cut_decided(CutId(cut)).unwrap();
    }
    // A begin recorded for a later cut is not touched by an older decision.
    h.record_cut_begin(CutId(73), c).unwrap();
    h.record_cut_begin(CutId(72), c).unwrap();
    prepare_both(&h, &h1, 72, &f0, &f1);
    assert!(h.decide_cut(CutId(72), 7200, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    h.cut_decided(CutId(72)).unwrap();
    h1.cut_decided(CutId(72)).unwrap();
    let begins = |cut: u64| scalar_i64(&w, &format!("SELECT COUNT(*) FROM cut_begins WHERE cut_id = {cut}"));
    assert_eq!((begins(70), begins(71)), (0, 0), "older begins are pruned");
    assert_eq!((begins(72), begins(73)), (1, 1), "the decided cut's own begin and a newer one stay");
    assert_eq!(scalar_i64(&w, "SELECT COUNT(*) FROM cut_decisions WHERE cut_id IN (70, 71, 72)"), 3, "every decision is kept");
    assert_eq!(prepared(&w), 0);
    h1.shutdown();
    h.shutdown();
}

#[test]
fn sqlite_is_not_two_phase_ready() {
    let h = open(&StoreUrl::Memory);
    let f = h.claim_super_regions(hid(0), vec![(0, 0)]).unwrap();
    let c = h.claim_coordinator().unwrap();
    fn unsupported<T>(r: Result<T, StoreError>) -> bool {
        matches!(r, Err(StoreError::Unsupported(_)))
    }
    let err = h.two_phase_ready().unwrap_err();
    assert!(err.to_string().contains("max_prepared_transactions"), "{err}");
    assert!(matches!(err, StoreError::Unsupported(_)));
    assert!(unsupported(h.prepare_cut(part(1, hid(0)), f, 0).recv().unwrap()));
    assert!(unsupported(h.decide_cut(CutId(1), 100, true, vec![hid(0)], vec![], c, 0, vec![])));
    assert!(unsupported(h.abort_cut(CutId(1), 100)));
    assert!(unsupported(h.resolve_prepared()));
    assert_eq!(h.cut_decision(CutId(1)).unwrap(), None);
    assert!(h.stored_regions().unwrap().is_empty());
    h.shutdown();
}

/// A read-only handle reads decisions and nothing else.
#[test]
fn read_only_refuses_two_phase_writes() {
    each_backend("two_phase_ro", true, |url| {
        open(url).shutdown();
        let r = StoreHandle::open(url, Dials::BUILT, OpenMode::ReadOnly).unwrap();
        let c = world_store::CoordinatorFence { epoch: 1 };
        assert_eq!(r.prepare_cut(part(1, hid(0)), Fence::default(), 0).recv().unwrap().unwrap_err(), StoreError::ReadOnly);
        assert_eq!(r.decide_cut(CutId(1), 100, true, vec![hid(0)], vec![], c, 0, vec![]).unwrap_err(), StoreError::ReadOnly);
        assert_eq!(r.abort_cut(CutId(1), 100).unwrap_err(), StoreError::ReadOnly);
        assert_eq!(r.resolve_prepared().unwrap_err(), StoreError::ReadOnly);
        assert_eq!(r.cut_decision(CutId(1)).unwrap(), None);
        r.shutdown();
    });
}

/// A commit decision needs every listed part staged: with host 1's part missing (still
/// queued, mid-PREPARE, never prepared) the decision becomes an abort, and host 0's
/// part is rolled back instead of committing half the cut.
#[test]
fn commit_with_a_missing_part_becomes_an_abort() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, _) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    h.prepare_cut(part(20, hid(0)), f0, 0).recv().unwrap().unwrap();
    assert!(!h.decide_cut(CutId(20), 2000, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    assert_eq!(h.cut_decision(CutId(20)).unwrap(), Some(false), "an abort is recorded");
    assert!(h.stored_regions().unwrap().is_empty(), "nothing visible");
    assert_eq!(h.world_info().unwrap().world_tick, 0);
    assert_eq!(prepared(&w), 0, "the prepared part is rolled back");
    h.shutdown();
}

/// A staged part from a host the decision does not list also turns a commit into an
/// abort, and is rolled back with the rest.
#[test]
fn commit_with_an_unlisted_part_becomes_an_abort() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let h1 = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    prepare_both(&h, &h1, 21, &f0, &f1);
    assert!(!h.decide_cut(CutId(21), 2100, true, vec![hid(0)], vec![], c, 0, vec![]).unwrap());
    assert_eq!(h.cut_decision(CutId(21)).unwrap(), Some(false));
    assert!(h.stored_regions().unwrap().is_empty(), "nothing visible");
    assert_eq!(prepared(&w), 0, "the listed and the unlisted part are rolled back");
    h1.shutdown();
    h.shutdown();
}

/// An abort decision sweeps every staged part of the cut, listed or not.
#[test]
fn decide_finishes_parts_outside_the_listed_hosts() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let h1 = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    prepare_both(&h, &h1, 22, &f0, &f1);
    assert!(!h.decide_cut(CutId(22), 2200, false, vec![hid(0)], vec![], c, 0, vec![]).unwrap());
    assert_eq!(prepared(&w), 0, "host 1's unlisted part is rolled back too");
    h1.shutdown();
    h.shutdown();
}

#[test]
fn abort_cut_rolls_back_staged_parts() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let h1 = open(&w.url);
    let (f0, f1) = fences(&h);
    prepare_both(&h, &h1, 23, &f0, &f1);
    assert!(!h1.abort_cut(CutId(23), 2300).unwrap());
    assert_eq!(prepared(&w), 0, "abort_cut finishes every staged part of the cut");
    assert!(h.stored_regions().unwrap().is_empty());
    h1.shutdown();
    h.shutdown();
}

/// The coordinator gave up and decided abort before this host's PREPARE ran (its
/// rollback pass found nothing): the part sees the decision and rolls itself back.
#[test]
fn part_prepared_after_its_abort_rolls_itself_back() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, _) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    assert!(!h.decide_cut(CutId(24), 2400, false, vec![hid(0)], vec![], c, 0, vec![]).unwrap());
    let err = h.prepare_cut(part(24, hid(0)), f0.clone(), 0).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::CutAborted(CutId(24)));
    assert_eq!(prepared(&w), 0, "nothing staged");
    assert!(h.stored_regions().unwrap().is_empty(), "nothing visible");
    // Settled: the next cut is accepted.
    h.prepare_cut(part(25, hid(0)), f0, 0).recv().unwrap().unwrap();
    assert_eq!(h.resolve_prepared().unwrap(), vec![(CutId(25), false)]);
    h.shutdown();
}

/// While this handle's part is undecided its next cut — either kind — is refused at
/// once (it would wait on the part's row locks inside the committer).
#[test]
fn second_cut_while_a_part_is_undecided_is_refused() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, _) = fences(&h);
    h.prepare_cut(part(30, hid(0)), f0.clone(), 0).recv().unwrap().unwrap();
    let err = h.commit_cut(part(31, hid(0)), f0.clone()).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::CutPending(CutId(30)));
    let err = h.prepare_cut(part(31, hid(0)), f0.clone(), 0).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::CutPending(CutId(30)));
    let clone = h.clone();
    let err = clone.commit_cut(part(31, hid(0)), f0).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::CutPending(CutId(30)), "every clone of the handle");
    assert_eq!(prepared(&w), 1, "only the first part is staged");
    assert_eq!(h.resolve_prepared().unwrap(), vec![(CutId(30), false)]);
    h.shutdown();
}

#[test]
fn cut_decided_clears_the_pending_part() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, _) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    h.prepare_cut(part(32, hid(0)), f0.clone(), 0).recv().unwrap().unwrap();
    assert_eq!(h.cut_decided(CutId(32)).unwrap(), None, "undecided: the part stays pending");
    assert!(matches!(h.commit_cut(part(33, hid(0)), f0.clone()).recv().unwrap(), Err(StoreError::CutPending(_))));
    assert!(h.decide_cut(CutId(32), 3200, true, vec![hid(0)], vec![], c, 0, vec![]).unwrap());
    assert_eq!(h.cut_decided(CutId(32)).unwrap(), Some(true));
    h.commit_cut(part(33, hid(0)), f0.clone()).recv().unwrap().expect("accepted once the cut is seen decided");
    // A decision recorded while this handle's part is still staged (the decider died
    // before finishing it): `cut_decided` finishes the part as decided.
    h.prepare_cut(part(34, hid(0)), f0, 0).recv().unwrap().unwrap();
    exec(&w, "INSERT INTO cut_decisions (cut_id, tick, committed, decided_utc) VALUES (34, 3400, 0, 'test')");
    assert_eq!(prepared(&w), 1);
    assert_eq!(h.cut_decided(CutId(34)).unwrap(), Some(false));
    assert_eq!(prepared(&w), 0, "the handle's own part is rolled back");
    h.shutdown();
}

/// A cut transaction waits at most its `lock_timeout` for a row lock (here one held by
/// a raw transaction on the host's super-region row), then fails with a clear error and
/// leaves nothing staged — a refused prepare is settled, so the next cut is accepted.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_cut_blocked_on_a_foreign_lock_times_out() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, _) = fences(&h);
    h.set_cut_lock_timeout_ms(200);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut conn = rt.block_on(sqlx::PgConnection::connect(&w.db.url)).unwrap();
    let mut tx = rt.block_on(conn.begin()).unwrap();
    rt.block_on(sqlx::query("SELECT owner FROM super_regions WHERE sx = 0 AND sz = 0 FOR UPDATE").fetch_all(&mut *tx))
        .unwrap();
    for prepare in [false, true] {
        let rx = if prepare { h.prepare_cut(part(40, hid(0)), f0.clone(), 0) } else { h.commit_cut(part(41, hid(0)), f0.clone()) };
        let err = rx.recv_timeout(Duration::from_secs(5)).expect("the cut gives up").unwrap_err();
        assert!(matches!(&err, StoreError::LockConflict { sqlstate, detail } if sqlstate == "55P03" && detail.contains("lock_timeout")), "prepare={prepare}: {err}");
        assert!(err.is_lock_conflict());
    }
    assert_eq!(prepared(&w), 0);
    rt.block_on(tx.rollback()).unwrap();
    h.commit_cut(part(42, hid(0)), f0).recv().unwrap().expect("nothing pending after the refused prepare");
    rt.block_on(conn.close()).unwrap();
    h.shutdown();
}

/// A two-phase part is per host and never carries the history: refused at once, and
/// nothing is left pending.
#[test]
fn prepare_refuses_an_all_hosts_fence_and_the_history() {
    let h = open(&StoreUrl::Memory);
    let f = h.claim_super_regions(hid(0), vec![(0, 0)]).unwrap();
    let all = Fence { all_hosts: true, ..f.clone() };
    let err = h.prepare_cut(part(1, hid(0)), all, 0).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::Unsupported("a two-phase cut needs a per-host fence".into()));
    let with_history = WorldCut { history: Some(vec![1, 2, 3]), ..part(2, hid(0)) };
    let err = h.prepare_cut(with_history, f.clone(), 0).recv().unwrap().unwrap_err();
    assert_eq!(
        err,
        StoreError::Unsupported("the history is the coordinator's; a host's cut part never carries it".into())
    );
    // Not pending: SQLite answers the next one itself.
    let err = h.prepare_cut(part(3, hid(0)), f.clone(), 0).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::Unsupported("several hosts need max_prepared_transactions > 0 (Postgres)".into()));
    h.commit_cut(part(4, hid(0)), f).recv().unwrap().expect("nor is a one-phase commit refused");
    h.shutdown();
}

/// Entity ids stored in more than one region (`entities` is keyed per region, so the
/// database itself no longer refuses one): must stay 0 after every committed cut.
pub(crate) const ENTITIES_STORED_TWICE: &str = "SELECT COUNT(*) FROM (SELECT birth_rx, birth_rz, serial FROM entities \
     GROUP BY birth_rx, birth_rz, serial HAVING COUNT(*) > 1) twice";

/// Both handles see cut `cut` decided (so each accepts its next part).
fn settled(h0: &StoreHandle, h1: &StoreHandle, cut: CutId) {
    assert_eq!(h0.cut_decided(cut).unwrap(), Some(true));
    assert_eq!(h1.cut_decided(cut).unwrap(), Some(true));
}

/// The defect of Task 14b: an entity stored in host 0's region by a committed cut
/// crosses into host 1's region before the next cut. In that cut host 0's part deletes it
/// from its region's rows and host 1's part inserts it into its own: with rows keyed per
/// region the two prepared parts touch no common row, so neither waits for the other, the
/// decision commits both, and the entity is stored once, in host 1's region.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn entity_crossing_between_two_parts_prepares_without_waiting() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let h1 = open(&w.url);
    let e = crate::codec::bundle();
    let with = |id: u64, host: game::HostId, entities: Vec<game::EntityBundle>| {
        let mut p = part(id, host);
        p.regions[0].1.entities = entities;
        p
    };
    h.prepare_cut(with(1, hid(0), vec![e.clone()]), f0.clone(), 0).recv().unwrap().unwrap();
    h1.prepare_cut(with(1, hid(1), vec![]), f1.clone(), 0).recv().unwrap().unwrap();
    assert!(h.decide_cut(CutId(1), 100, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    settled(&h, &h1, CutId(1));
    assert_eq!(h.load_region(A, &f0).unwrap().unwrap().entities, vec![e.clone()], "stored in host 0's region");
    // Cut 2: host 0's part first (it holds its deleted rows' locks while prepared), then
    // host 1's, which must not wait on them.
    h.set_cut_lock_timeout_ms(500);
    h1.set_cut_lock_timeout_ms(500);
    h.prepare_cut(with(2, hid(0), vec![]), f0.clone(), 0).recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    h1.prepare_cut(with(2, hid(1), vec![e.clone()]), f1.clone(), 0)
        .recv_timeout(Duration::from_secs(5))
        .expect("host 1's part is answered")
        .expect("host 1's part prepares beside host 0's");
    assert_eq!(prepared(&w), 2);
    assert!(h.decide_cut(CutId(2), 200, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    settled(&h, &h1, CutId(2));
    assert_eq!(prepared(&w), 0);
    assert!(h.load_region(A, &f0).unwrap().unwrap().entities.is_empty(), "gone from host 0's region");
    assert_eq!(h.load_region(B, &f1).unwrap().unwrap().entities, vec![e.clone()], "in host 1's region");
    assert_eq!(scalar_i64(&w, ENTITIES_STORED_TWICE), 0, "no entity stored in two regions");
    until_checked(&h, 2);
    assert_eq!(h.cuts_with_entities_twice(), 0, "a healthy crossing is not reported");
    // And back the other way, host 1's part prepared first.
    h1.prepare_cut(with(3, hid(1), vec![]), f1.clone(), 0).recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    h.prepare_cut(with(3, hid(0), vec![e.clone()]), f0.clone(), 0).recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert!(h.decide_cut(CutId(3), 300, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    settled(&h, &h1, CutId(3));
    assert_eq!(h.load_region(A, &f0).unwrap().unwrap().entities, vec![e]);
    assert!(h.load_region(B, &f1).unwrap().unwrap().entities.is_empty());
    assert_eq!(scalar_i64(&w, "SELECT COUNT(*) FROM entities"), 1);
    assert_eq!(scalar_i64(&w, ENTITIES_STORED_TWICE), 0);
    until_checked(&h, 3);
    assert_eq!(h.cuts_with_entities_twice(), 0);
    assert_eq!(prepared(&w), 0);
    h1.shutdown();
    h.shutdown();
}

/// A prepared part holding an entity in two of its regions is refused before anything is
/// staged, and the next part is accepted.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn prepared_part_with_an_entity_twice_is_refused() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let f = h.claim_super_regions(hid(0), vec![(0, 0), (0, -1)]).unwrap();
    let mut twice = part(1, hid(0));
    twice.regions[0].1.entities = vec![crate::codec::bundle()];
    let mut other = region();
    other.entities = vec![crate::codec::bundle()];
    twice.regions.push((B, other));
    let err = h.prepare_cut(twice, f.clone(), 0).recv().unwrap().unwrap_err();
    assert!(matches!(&err, StoreError::Corrupt(m) if m.contains("two regions")), "{err}");
    assert_eq!(prepared(&w), 0, "nothing staged");
    h.prepare_cut(part(2, hid(0)), f, 0).recv().unwrap().expect("the next part is accepted");
    assert!(!h.abort_cut(CutId(2), 200).unwrap());
    assert_eq!(prepared(&w), 0);
    h.shutdown();
}

/// The world-row lock matrix of the cut path (Task 12b follow-up): a claim's `FOR KEY
/// SHARE` on the world row (held while it waits for a prepared part's `super_regions`
/// rows) blocks neither a commit decision nor a host boot (both `FOR NO KEY UPDATE`);
/// `worldctl super-region`'s `FOR UPDATE` still does block a decision (it is serialised).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn claim_holding_the_world_row_does_not_block_a_decision() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    // The boot below is host 0, whose part the decisions list.
    exec(&w, "UPDATE world SET next_host_id = 0");
    let (f0, _) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut conn = rt.block_on(sqlx::PgConnection::connect(&w.db.url)).unwrap();
    for (id, lock, blocks) in [(50u64, "FOR KEY SHARE", false), (52, "FOR UPDATE", true)] {
        h.prepare_cut(part(id, hid(0)), f0.clone(), 0).recv().unwrap().unwrap();
        let mut tx = rt.block_on(conn.begin()).unwrap();
        rt.block_on(sqlx::query(&format!("SELECT super_region_regions FROM world WHERE id = 1 {lock}")).fetch_all(&mut *tx))
            .unwrap();
        let decided = h.decide_cut_later(CutId(id), 100 * id, true, vec![hid(0)], vec![], c, 0, vec![]);
        if blocks {
            assert!(decided.recv_timeout(BLOCKED_FOR).is_err(), "{lock}: the decision waits");
            rt.block_on(tx.rollback()).unwrap();
            assert!(decided.recv_timeout(Duration::from_secs(10)).expect("decided after the lock goes").unwrap());
        } else {
            let done = decided.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| panic!("{lock}: the decision waits"));
            assert!(done.unwrap(), "committed");
            // A host boot (its generation check) does not wait either.
            let (tx_boot, rx_boot) = crossbeam::channel::bounded(1);
            let booter = {
                let h = h.clone();
                std::thread::spawn(move || {
                    let _ = tx_boot.send(register(&h, "host-0", 1));
                })
            };
            rx_boot.recv_timeout(Duration::from_secs(5)).expect("the boot does not wait").unwrap();
            booter.join().unwrap();
            rt.block_on(tx.rollback()).unwrap();
        }
        assert_eq!(h.cut_decided(CutId(id)).unwrap(), Some(true));
        assert_eq!(prepared(&w), 0);
    }
    rt.block_on(conn.close()).unwrap();
    h.shutdown();
}

/// Host 0's second region (same super-region as `A`).
const A2: RegionCoords = RegionCoords { x: 1, z: 0 };

/// Fix round 1, path (a) in a prepared part: an entity stored in host 0's region A, then
/// saved by host 0's next part in its region A2 with A left out — refused at prepare
/// (A is host 0's: its row cannot be deleted by any other part of the cut).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn prepared_part_saving_an_entity_its_host_still_stores_elsewhere_is_refused() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, _) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let e = crate::codec::bundle();
    let mut first = part(1, hid(0));
    first.regions[0].1.entities = vec![e.clone()];
    h.prepare_cut(first, f0.clone(), 0).recv().unwrap().unwrap();
    assert!(h.decide_cut(CutId(1), 100, true, vec![hid(0)], vec![], c, 0, vec![]).unwrap());
    assert_eq!(h.cut_decided(CutId(1)).unwrap(), Some(true));
    let mut second = part(2, hid(0));
    let mut other = region();
    other.entities = vec![e.clone()];
    second.regions = vec![(A2, other)];
    let err = h.prepare_cut(second, f0.clone(), 0).recv().unwrap().unwrap_err();
    assert!(matches!(&err, StoreError::Corrupt(m) if m.contains("still stored in") && m.contains("conservation bug")), "{err}");
    assert_eq!(prepared(&w), 0, "nothing staged");
    assert_eq!(h.load_region(A, &f0).unwrap().unwrap().entities, vec![e]);
    h.shutdown();
}

/// Waits until `h` has checked `n` committed cuts for entities stored twice.
fn until_checked(h: &StoreHandle, n: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while h.cuts_checked_for_entities_twice() < n {
        assert!(std::time::Instant::now() < deadline, "the post-commit check of cut {n} ran");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Fix round 1, path (b): host 0's region A still stores an entity (e.g. respawned from
/// the store) that now lives on host 1. Host 0's part leaves A out, host 1's part saves
/// the entity in B: neither part can refuse it (a row in a peer's region may be deleted by
/// the peer's part of the same cut), so the cut commits — and the decider reports it once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stale_row_across_hosts_commits_and_is_reported() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let h1 = open(&w.url);
    let e = crate::codec::bundle();
    let with = |id: u64, host: game::HostId, entities: Vec<game::EntityBundle>| {
        let mut p = part(id, host);
        p.regions[0].1.entities = entities;
        p
    };
    h.prepare_cut(with(1, hid(0), vec![e.clone()]), f0.clone(), 0).recv().unwrap().unwrap();
    h1.prepare_cut(with(1, hid(1), vec![]), f1.clone(), 0).recv().unwrap().unwrap();
    assert!(h.decide_cut(CutId(1), 100, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    settled(&h, &h1, CutId(1));
    until_checked(&h, 1);
    assert_eq!(h.cuts_with_entities_twice(), 0, "a healthy cut reports nothing");
    // Cut 2: host 0's part leaves A out (it holds no regions), host 1's saves e in B.
    let mut empty = part(2, hid(0));
    empty.regions.clear();
    h.prepare_cut(empty, f0.clone(), 0).recv().unwrap().unwrap();
    h1.prepare_cut(with(2, hid(1), vec![e.clone()]), f1.clone(), 0).recv().unwrap().expect("host 1 cannot judge A's row");
    assert!(h.decide_cut(CutId(2), 200, true, vec![hid(0), hid(1)], vec![], c, 0, vec![]).unwrap());
    settled(&h, &h1, CutId(2));
    until_checked(&h, 2);
    assert_eq!(h.cuts_with_entities_twice(), 1, "the decider reports the cut");
    assert_eq!(scalar_i64(&w, ENTITIES_STORED_TWICE), 1);
    assert_eq!(prepared(&w), 0);
    h1.shutdown();
    h.shutdown();
}

/// Serialises the tests that lower the (process-wide) migration lock timeout.
static MIGRATE_TIMEOUT_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The migration lock timeout lowered until dropped (a panicking test too), one test at
/// a time (the guard field holds the lock).
struct MigrateTimeout(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

impl MigrateTimeout {
    fn set(ms: u64) -> MigrateTimeout {
        let guard = MIGRATE_TIMEOUT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        world_store::set_migrate_lock_timeout_ms(ms);
        MigrateTimeout(guard)
    }

    /// Back to the default while still held.
    fn reset(&self) {
        world_store::set_migrate_lock_timeout_ms(world_store::MIGRATE_LOCK_TIMEOUT_MS);
    }
}

impl Drop for MigrateTimeout {
    fn drop(&mut self) {
        self.reset();
    }
}

/// Fix round 1, I2: a prepared cut part a crash left behind holds its table locks; a
/// build whose migration needs `entities` must not wait for ever at open. The open fails
/// within the migration's lock timeout, naming `pg_prepared_xacts`, the gid and the
/// remedy; once the part is rolled back the open migrates.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn migration_behind_an_orphan_prepared_part_fails_with_the_remedy() {
    let Some(w) = pg_world() else { return };
    crate::stud_refusal::old_store(&w.url, 9, "1", &[]);
    let gid = "mmo-cut-1-1-0";
    {
        let url = w.db.url.clone();
        let insert = format!(
            "INSERT INTO entities (birth_rx, birth_rz, serial, rx, rz, saved_tick, kind, bundle) \
             VALUES (0, 0, 1, 0, 0, 1, 1, {})",
            crate::stud_refusal::blob(&w.url, &[1, 2, 3])
        );
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
                for sql in ["BEGIN", insert.as_str(), &format!("PREPARE TRANSACTION '{gid}'")] {
                    sqlx::raw_sql(sql).execute(&mut c).await.unwrap();
                }
                c.close().await.unwrap();
            })
        })
        .join()
        .unwrap();
    }
    assert_eq!(prepared(&w), 1);
    let timeout = MigrateTimeout::set(300);
    let at = std::time::Instant::now();
    let err = StoreHandle::open(&w.url, Dials::BUILT, OpenMode::Writer).err().expect("the open fails");
    assert!(at.elapsed() < Duration::from_secs(10), "within the bound: {:?}", at.elapsed());
    let text = err.to_string();
    assert!(text.contains("pg_prepared_xacts") && text.contains(gid) && text.contains("ROLLBACK PREPARED"), "{text}");
    exec(&w, &format!("ROLLBACK PREPARED '{gid}'"));
    timeout.reset();
    let h = StoreHandle::open(&w.url, Dials::BUILT, OpenMode::Writer).expect("migrates once the part is gone");
    h.shutdown();
    assert_eq!(prepared(&w), 0);
}

/// Fix round 2: a migration that times out behind an ordinary lock (no prepared
/// transaction of the world exists) blames no prepared transaction: it says another
/// process holds the lock and to retry.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn migration_behind_an_ordinary_lock_says_retry() {
    let Some(w) = pg_world() else { return };
    crate::stud_refusal::old_store(&w.url, 9, "1", &[]);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut conn = rt.block_on(sqlx::PgConnection::connect(&w.db.url)).unwrap();
    let mut tx = rt.block_on(conn.begin()).unwrap();
    rt.block_on(sqlx::query("LOCK TABLE entities IN ROW EXCLUSIVE MODE").execute(&mut *tx)).unwrap();
    let timeout = MigrateTimeout::set(300);
    let err = StoreHandle::open(&w.url, Dials::BUILT, OpenMode::Writer).err().expect("the open fails");
    let text = err.to_string();
    assert!(text.contains("another process") && text.contains("retry"), "{text}");
    assert!(!text.contains("ROLLBACK PREPARED"), "no prepared transaction is blamed: {text}");
    rt.block_on(tx.rollback()).unwrap();
    rt.block_on(conn.close()).unwrap();
    timeout.reset();
    StoreHandle::open(&w.url, Dials::BUILT, OpenMode::Writer).expect("migrates once the lock is gone").shutdown();
}

/// R3's 60 s claim bound (`CLAIM_LOCK_TIMEOUT_MS`, lowered by the test hook): a claim behind
/// a staged part nobody decides gives up with `LockConflict` naming the part's gid and the
/// remedy, writing nothing; so does one whose read of the `world` row (the super-region side,
/// `FOR KEY SHARE`) waits behind another transaction's lock on it — the same explanation, not
/// a raw error.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn claim_behind_a_staged_part_times_out_with_the_remedy() {
    const WAIT_MS: u64 = 300;
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, _) = fences(&h);
    h.prepare_cut(part(16, hid(0)), f0.clone(), 0).recv().unwrap().unwrap();
    let gid = cut_gid(scalar_i64(&w, "SELECT oid::bigint FROM pg_database WHERE datname = current_database()") as u32, CutId(16), hid(0));
    let other = open(&w.url);
    other.set_claim_lock_timeout_ms(WAIT_MS);
    let srs: Vec<_> = f0.epochs.keys().copied().collect();
    let started = std::time::Instant::now();
    let err = other.claim_super_regions(hid(2), srs.clone()).unwrap_err();
    let waited = started.elapsed();
    let StoreError::LockConflict { sqlstate, detail } = &err else { panic!("a lock conflict: {err}") };
    assert_eq!(sqlstate, "55P03");
    assert!(detail.contains(&format!("{WAIT_MS} ms")) && detail.contains(&gid) && detail.contains("ROLLBACK PREPARED"), "{detail}");
    assert!(waited >= Duration::from_millis(WAIT_MS) && waited < Duration::from_millis(WAIT_MS * 20), "waited {waited:?}");
    assert_eq!(h.owned_by(hid(0)).unwrap(), f0, "nothing claimed");
    assert!(!h.abort_cut(CutId(16), 1600).unwrap());
    assert_eq!(h.cut_decided(CutId(16)).unwrap(), Some(false));
    // The claim's read of the world row, behind a lock on it (`worldctl super-region`'s
    // `FOR UPDATE`, held here by hand): the same explanation.
    let url = match &w.url {
        StoreUrl::Postgres(u) => u.clone(),
        _ => unreachable!(),
    };
    let (locked_tx, locked_rx) = crossbeam::channel::bounded(1);
    let (release_tx, release_rx) = crossbeam::channel::bounded::<()>(1);
    let holder = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
            let mut tx = c.begin().await.unwrap();
            sqlx::query("SELECT 1 FROM world WHERE id = 1 FOR UPDATE").execute(&mut *tx).await.unwrap();
            locked_tx.send(()).unwrap();
            let _ = release_rx.recv();
            tx.rollback().await.unwrap();
            c.close().await.unwrap();
        })
    });
    locked_rx.recv().unwrap();
    let err = other.claim_super_regions(hid(2), srs).unwrap_err();
    // A registration's read of the world row (`FOR NO KEY UPDATE`) behind it: its own words.
    let reg = other.register_host(crate::hosts::new_host("x", 1), game::CrashPolicy::Succession).unwrap_err();
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    let StoreError::LockConflict { sqlstate, detail } = &err else { panic!("a lock conflict: {err}") };
    assert_eq!(sqlstate, "55P03");
    assert!(detail.contains("claiming super-regions waited longer than") && detail.contains(&format!("{WAIT_MS} ms")), "{detail}");
    let StoreError::LockConflict { detail, .. } = &reg else { panic!("a lock conflict: {reg}") };
    assert!(detail.starts_with("registering this host waited longer than"), "{detail}");
    assert_eq!(h.owned_by(hid(0)).unwrap(), f0, "nothing claimed");
    assert_eq!(prepared(&w), 0);
    other.shutdown();
    h.shutdown();
}
