//! Two-phase save cuts (R3): each host prepares its fenced part of a cut
//! (`PREPARE TRANSACTION`), one decision row commits or aborts every part, and a
//! recovery sweep finishes whatever a crash left prepared. Postgres only; SQLite
//! refuses all of it.

use std::time::Duration;

use game::{CutId, InFlightRow, PortableRegion, RegionCoords, StudChunkCoords, WorldCut};
use sqlx::Connection;
use world_store::{Dials, Fence, HostRow, OpenMode, StoreError, StoreHandle, StoreUrl, cut_gid};

use crate::codec::{intent, meta, stud_chunk, stud_ground};
use crate::common;
use crate::handle::{each_backend, open};

/// Host 0's region (super-region (0, 0)).
const A: RegionCoords = RegionCoords { x: 0, z: 0 };
/// Host 1's region (super-region (0, -1)).
const B: RegionCoords = RegionCoords { x: 1, z: -1 };

/// How long a claim stuck behind a prepared part's row lock must stay stuck.
const BLOCKED_FOR: Duration = Duration::from_millis(300);

fn region() -> PortableRegion {
    PortableRegion {
        meta: meta(),
        stud_chunks: [(StudChunkCoords { x: 0, y: 0, z: 0 }, Some(stud_chunk()))].into_iter().collect(),
        stud_ground: Some(stud_ground()),
        ..Default::default()
    }
}

/// Host `host`'s part of cut `id`: its region and one in-flight row.
fn part(id: u64, host: u32) -> WorldCut {
    let rc = if host == 0 { A } else { B };
    WorldCut {
        id: CutId(id),
        host,
        tick: 100 * id,
        regions: vec![(rc, region())],
        in_flight: vec![InFlightRow::Intent { target: rc, msg: intent() }],
        players: vec![],
        history: None,
    }
}

/// A throwaway Postgres world. Its drop rolls back any prepared transaction a
/// failed test left behind (the database could not be dropped otherwise).
struct PgWorld {
    db: common::PgTestDb,
    url: StoreUrl,
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

fn pg_world() -> Option<PgWorld> {
    if common::pg_admin_url().is_none() {
        common::skip_msg();
        return None;
    }
    let db = common::PgTestDb::create("mmo_test_2pc").expect("pg env var set");
    let url = StoreUrl::Postgres(db.url.clone());
    Some(PgWorld { db, url })
}

/// Runs `sql` on its own connection and returns the first column of the first row.
fn scalar_i64(w: &PgWorld, sql: &str) -> i64 {
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

fn exec(w: &PgWorld, sql: &str) {
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
fn prepared(w: &PgWorld) -> i64 {
    scalar_i64(w, "SELECT COUNT(*) FROM pg_prepared_xacts WHERE database = current_database()")
}

/// Host 0 holds super-region (0, 0), host 1 holds (0, -1).
fn fences(h: &StoreHandle) -> (Fence, Fence) {
    let s = h.super_region_regions();
    let f0 = h.claim_super_regions(0, vec![game::super_region_of(A, s)]).unwrap();
    let f1 = h.claim_super_regions(1, vec![game::super_region_of(B, s)]).unwrap();
    (f0, f1)
}

fn host_row(ordinal: u32) -> HostRow {
    HostRow {
        ordinal,
        boot: 1,
        generation: 0,
        clean: false,
        peer_addr: None,
        peer_cert: None,
        quic: None,
        webtransport: None,
        wt_cert_hash: None,
    }
}

/// Prepares host 0's and host 1's parts of cut `id` at generation 0.
fn prepare_both(h: &StoreHandle, id: u64, f0: &Fence, f1: &Fence) {
    h.prepare_cut(part(id, 0), f0.clone(), 0).recv().unwrap().unwrap();
    h.prepare_cut(part(id, 1), f1.clone(), 0).recv().unwrap().unwrap();
}

#[test]
fn gid_names_world_cut_and_host() {
    assert_eq!(cut_gid(16_384, CutId(7), 2), "mmo-cut-16384-7-2");
}

#[test]
fn prepared_part_is_invisible_until_decided() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    h.two_phase_ready().expect("the dev Postgres runs with max_prepared_transactions > 0");
    let (f0, _) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let done = h.prepare_cut(part(1, 0), f0.clone(), 0).recv().unwrap().unwrap();
    assert_eq!(done.id, CutId(1));
    assert_eq!(prepared(&w), 1);
    // Another process sees nothing of it.
    let other = open(&w.url);
    assert_eq!(other.load_region(A, &f0).unwrap(), None);
    assert!(other.stored_regions().unwrap().is_empty());
    assert!(other.load_in_flight(None).unwrap().is_empty());
    assert_eq!(other.world_info().unwrap().world_tick, 0);
    assert_eq!(other.cut_decision(CutId(1)).unwrap(), None);
    assert!(h.decide_cut(CutId(1), 100, true, vec![0], vec![], c, 0).unwrap());
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
    prepare_both(&h, 2, &f0, &f1);
    assert_eq!(prepared(&w), 2);
    assert!(h.decide_cut(CutId(2), 200, true, vec![0, 1], vec![], c, 0).unwrap());
    assert!(h.load_region(A, &f0).unwrap().is_some());
    assert!(h.load_region(B, &f1).unwrap().is_some());
    assert_eq!(h.load_in_flight(None).unwrap().len(), 2, "both hosts' in-flight rows");
    assert_eq!(h.world_info().unwrap().world_tick, 200, "the decision writes the world tick");
    assert_eq!(h.cut_decision(CutId(2)).unwrap(), Some(true));
    assert_eq!(prepared(&w), 0);
    // Deciding again changes nothing and finds nothing left to commit.
    assert!(h.decide_cut(CutId(2), 200, true, vec![0, 1], vec![], c, 0).unwrap());
    h.shutdown();
}

#[test]
fn abort_decision_leaves_nothing_and_no_prepared_xact() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    prepare_both(&h, 3, &f0, &f1);
    assert!(!h.decide_cut(CutId(3), 300, false, vec![0, 1], vec![], c, 0).unwrap());
    assert!(h.stored_regions().unwrap().is_empty());
    assert!(h.load_in_flight(None).unwrap().is_empty());
    assert_eq!(h.world_info().unwrap().world_tick, 0);
    assert_eq!(h.cut_decision(CutId(3)).unwrap(), Some(false));
    assert_eq!(prepared(&w), 0);
    h.shutdown();
}

#[test]
fn first_decision_wins() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    prepare_both(&h, 4, &f0, &f1);
    assert!(!h.abort_cut(CutId(4), 400).unwrap(), "a host's abort is the first decision");
    assert!(!h.abort_cut(CutId(4), 400).unwrap());
    assert!(!h.decide_cut(CutId(4), 400, true, vec![0, 1], vec![], c, 0).unwrap(), "the later commit loses");
    assert!(h.stored_regions().unwrap().is_empty());
    assert_eq!(h.world_info().unwrap().world_tick, 0);
    assert_eq!(prepared(&w), 0, "the losing commit rolled the parts back");
    // And the other way round: a decided commit is not undone by a late abort.
    prepare_both(&h, 5, &f0, &f1);
    assert!(h.decide_cut(CutId(5), 500, true, vec![0, 1], vec![], c, 0).unwrap());
    assert!(h.abort_cut(CutId(5), 500).unwrap());
    h.shutdown();
}

#[test]
fn resolve_finishes_a_decided_cut_stopped_between_commits() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    prepare_both(&h, 6, &f0, &f1);
    h.stop_after_commit_prepared(1);
    let err = h.decide_cut(CutId(6), 600, true, vec![0, 1], vec![], c, 0).unwrap_err();
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
    prepare_both(&h, 7, &f0, &f1);
    assert_eq!(h.resolve_prepared().unwrap(), vec![(CutId(7), false)]);
    assert_eq!(h.cut_decision(CutId(7)).unwrap(), Some(false), "the abort is recorded first");
    assert!(h.stored_regions().unwrap().is_empty());
    assert_eq!(prepared(&w), 0);
    h.shutdown();
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
    h.prepare_cut(part(8, 0), f0, 0).recv().unwrap().unwrap();
    h2.prepare_cut(part(8, 0), g0, 0).recv().unwrap().unwrap();
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
    let newer = h.claim_super_regions(1, stale.epochs.keys().copied().collect()).unwrap();
    let err = h.prepare_cut(part(9, 0), stale, 0).recv().unwrap().unwrap_err();
    assert!(matches!(err, StoreError::Fenced { sr: Some(_) }), "{err}");
    assert_eq!(prepared(&w), 0, "rolled back, not prepared");
    assert!(h.stored_regions().unwrap().is_empty());
    // The connection went back to the pool clean: the next commit works.
    let mut fresh = part(10, 1);
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
    let err = h.prepare_cut(part(11, 0), f0.clone(), 1).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::StaleGeneration { stored: 0, mine: 1 });
    assert_eq!(prepared(&w), 0);
    assert!(h.stored_regions().unwrap().is_empty());
    h.prepare_cut(part(12, 0), f0, 0).recv().unwrap().unwrap();
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
    h.prepare_cut(part(13, 0), f0, 0).recv().unwrap().unwrap();
    exec(&w, "UPDATE world SET generation = 1 WHERE id = 1");
    let err = h.decide_cut(CutId(13), 1300, true, vec![0], vec![], c, 0).unwrap_err();
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
    h.prepare_cut(part(14, 0), f0, 0).recv().unwrap().unwrap();
    let err = h.decide_cut(CutId(14), 1400, true, vec![0], vec![], stale, 0).unwrap_err();
    assert_eq!(err, StoreError::Fenced { sr: None });
    assert_eq!(h.cut_decision(CutId(14)).unwrap(), None, "nothing recorded");
    assert_eq!(prepared(&w), 1, "nothing committed or rolled back");
    let err = h.decide_cut(CutId(14), 1400, false, vec![0], vec![], stale, 0).unwrap_err();
    assert_eq!(err, StoreError::Fenced { sr: None }, "an abort decision is fenced too");
    assert_eq!(prepared(&w), 1);
    assert!(h.decide_cut(CutId(14), 1400, true, vec![0], vec![], newer, 0).unwrap());
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
    h.prepare_cut(part(15, 0), f0.clone(), 0).recv().unwrap().unwrap();
    let other = open(&w.url);
    let (done_tx, done_rx) = crossbeam::channel::bounded(1);
    let claimer = {
        let other = other.clone();
        let srs: Vec<_> = f0.epochs.keys().copied().collect();
        std::thread::spawn(move || {
            let _ = done_tx.send(other.claim_super_regions(2, srs));
        })
    };
    assert!(done_rx.recv_timeout(BLOCKED_FOR).is_err(), "the claim waits for the prepared part");
    assert!(h.decide_cut(CutId(15), 1500, true, vec![0], vec![], c, 0).unwrap());
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
    h.register_host(host_row(0), &f0).unwrap();
    h.register_host(host_row(1), &f1).unwrap();
    let c = h.claim_coordinator().unwrap();
    let clean = |h: &StoreHandle| h.hosts().unwrap().iter().map(|r| (r.ordinal, r.clean)).collect::<Vec<_>>();
    prepare_both(&h, 16, &f0, &f1);
    assert!(!h.decide_cut(CutId(16), 1600, false, vec![0, 1], vec![1], c, 0).unwrap());
    assert_eq!(clean(&h), vec![(0, false), (1, false)], "an aborted cut leaves nobody clean");
    prepare_both(&h, 17, &f0, &f1);
    assert!(h.decide_cut(CutId(17), 1700, true, vec![0, 1], vec![1], c, 0).unwrap());
    assert_eq!(clean(&h), vec![(0, false), (1, true)], "only the leaving host");
    assert_eq!(prepared(&w), 0);
    h.shutdown();
}

#[test]
fn sqlite_is_not_two_phase_ready() {
    let h = open(&StoreUrl::Memory);
    let f = h.claim_super_regions(0, vec![(0, 0)]).unwrap();
    let c = h.claim_coordinator().unwrap();
    fn unsupported<T>(r: Result<T, StoreError>) -> bool {
        matches!(r, Err(StoreError::Unsupported(_)))
    }
    let err = h.two_phase_ready().unwrap_err();
    assert!(err.to_string().contains("max_prepared_transactions"), "{err}");
    assert!(matches!(err, StoreError::Unsupported(_)));
    assert!(unsupported(h.prepare_cut(part(1, 0), f, 0).recv().unwrap()));
    assert!(unsupported(h.decide_cut(CutId(1), 100, true, vec![0], vec![], c, 0)));
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
        assert_eq!(r.prepare_cut(part(1, 0), Fence::default(), 0).recv().unwrap().unwrap_err(), StoreError::ReadOnly);
        assert_eq!(r.decide_cut(CutId(1), 100, true, vec![0], vec![], c, 0).unwrap_err(), StoreError::ReadOnly);
        assert_eq!(r.abort_cut(CutId(1), 100).unwrap_err(), StoreError::ReadOnly);
        assert_eq!(r.resolve_prepared().unwrap_err(), StoreError::ReadOnly);
        assert_eq!(r.cut_decision(CutId(1)).unwrap(), None);
        r.shutdown();
    });
}
