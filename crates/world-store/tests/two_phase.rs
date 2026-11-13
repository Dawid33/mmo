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

/// Prepares host 0's part of cut `id` on `h0` and host 1's on `h1` (each host has
/// its own store handle: a handle stages one undecided part at a time), at generation 0.
fn prepare_both(h0: &StoreHandle, h1: &StoreHandle, id: u64, f0: &Fence, f1: &Fence) {
    h0.prepare_cut(part(id, 0), f0.clone(), 0).recv().unwrap().unwrap();
    h1.prepare_cut(part(id, 1), f1.clone(), 0).recv().unwrap().unwrap();
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
    let h1 = open(&w.url);
    prepare_both(&h, &h1, 2, &f0, &f1);
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
    let h1 = open(&w.url);
    prepare_both(&h, &h1, 3, &f0, &f1);
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
    let h1 = open(&w.url);
    prepare_both(&h, &h1, 4, &f0, &f1);
    assert!(!h.abort_cut(CutId(4), 400).unwrap(), "a host's abort is the first decision");
    assert!(!h.abort_cut(CutId(4), 400).unwrap());
    assert!(!h.decide_cut(CutId(4), 400, true, vec![0, 1], vec![], c, 0).unwrap(), "the later commit loses");
    assert!(h.stored_regions().unwrap().is_empty());
    assert_eq!(h.world_info().unwrap().world_tick, 0);
    assert_eq!(prepared(&w), 0, "the losing commit rolled the parts back");
    // Each host hears the cut is done; its next part is accepted.
    assert_eq!(h.cut_decided(CutId(4)).unwrap(), Some(false));
    assert_eq!(h1.cut_decided(CutId(4)).unwrap(), Some(false));
    // And the other way round: a decided commit is not undone by a late abort.
    prepare_both(&h, &h1, 5, &f0, &f1);
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
    let h1 = open(&w.url);
    prepare_both(&h, &h1, 6, &f0, &f1);
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
    let h1 = open(&w.url);
    prepare_both(&h, &h1, 7, &f0, &f1);
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
    // Every connection of every pool on this database (the store's 4 among them) is
    // outside a transaction block: the refused prepare's ROLLBACK ran on its own
    // connection before it went back to the pool.
    assert_eq!(idle_in_transaction(&w), 0, "no connection was handed back inside the transaction");
    // A refusal that staged nothing settles the part: the next one is accepted.
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
    assert_eq!(idle_in_transaction(&w), 0);
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
    let h1 = open(&w.url);
    prepare_both(&h, &h1, 16, &f0, &f1);
    assert!(!h.decide_cut(CutId(16), 1600, false, vec![0, 1], vec![1], c, 0).unwrap());
    assert_eq!(clean(&h), vec![(0, false), (1, false)], "an aborted cut leaves nobody clean");
    h.cut_decided(CutId(16)).unwrap();
    h1.cut_decided(CutId(16)).unwrap();
    prepare_both(&h, &h1, 17, &f0, &f1);
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

/// A commit decision needs every listed part staged: with host 1's part missing (still
/// queued, mid-PREPARE, never prepared) the decision becomes an abort, and host 0's
/// part is rolled back instead of committing half the cut.
#[test]
fn commit_with_a_missing_part_becomes_an_abort() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, _) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    h.prepare_cut(part(20, 0), f0, 0).recv().unwrap().unwrap();
    assert!(!h.decide_cut(CutId(20), 2000, true, vec![0, 1], vec![], c, 0).unwrap());
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
    assert!(!h.decide_cut(CutId(21), 2100, true, vec![0], vec![], c, 0).unwrap());
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
    assert!(!h.decide_cut(CutId(22), 2200, false, vec![0], vec![], c, 0).unwrap());
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
    assert!(!h.decide_cut(CutId(24), 2400, false, vec![0], vec![], c, 0).unwrap());
    let err = h.prepare_cut(part(24, 0), f0.clone(), 0).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::CutAborted(CutId(24)));
    assert_eq!(prepared(&w), 0, "nothing staged");
    assert!(h.stored_regions().unwrap().is_empty(), "nothing visible");
    // Settled: the next cut is accepted.
    h.prepare_cut(part(25, 0), f0, 0).recv().unwrap().unwrap();
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
    h.prepare_cut(part(30, 0), f0.clone(), 0).recv().unwrap().unwrap();
    let err = h.commit_cut(part(31, 0), f0.clone()).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::CutPending(CutId(30)));
    let err = h.prepare_cut(part(31, 0), f0.clone(), 0).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::CutPending(CutId(30)));
    let clone = h.clone();
    let err = clone.commit_cut(part(31, 0), f0).recv().unwrap().unwrap_err();
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
    h.prepare_cut(part(32, 0), f0.clone(), 0).recv().unwrap().unwrap();
    assert_eq!(h.cut_decided(CutId(32)).unwrap(), None, "undecided: the part stays pending");
    assert!(matches!(h.commit_cut(part(33, 0), f0.clone()).recv().unwrap(), Err(StoreError::CutPending(_))));
    assert!(h.decide_cut(CutId(32), 3200, true, vec![0], vec![], c, 0).unwrap());
    assert_eq!(h.cut_decided(CutId(32)).unwrap(), Some(true));
    h.commit_cut(part(33, 0), f0.clone()).recv().unwrap().expect("accepted once the cut is seen decided");
    // A decision recorded while this handle's part is still staged (the decider died
    // before finishing it): `cut_decided` finishes the part as decided.
    h.prepare_cut(part(34, 0), f0, 0).recv().unwrap().unwrap();
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
        let rx = if prepare { h.prepare_cut(part(40, 0), f0.clone(), 0) } else { h.commit_cut(part(41, 0), f0.clone()) };
        let err = rx.recv_timeout(Duration::from_secs(5)).expect("the cut gives up").unwrap_err();
        assert!(matches!(&err, StoreError::Backend(t) if t.contains("lock_timeout")), "prepare={prepare}: {err}");
    }
    assert_eq!(prepared(&w), 0);
    rt.block_on(tx.rollback()).unwrap();
    h.commit_cut(part(42, 0), f0).recv().unwrap().expect("nothing pending after the refused prepare");
    rt.block_on(conn.close()).unwrap();
    h.shutdown();
}

/// A two-phase part is per host and never carries the history: refused at once, and
/// nothing is left pending.
#[test]
fn prepare_refuses_an_all_hosts_fence_and_the_history() {
    let h = open(&StoreUrl::Memory);
    let f = h.claim_super_regions(0, vec![(0, 0)]).unwrap();
    let all = Fence { all_hosts: true, ..f.clone() };
    let err = h.prepare_cut(part(1, 0), all, 0).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::Unsupported("a two-phase cut needs a per-host fence".into()));
    let with_history = WorldCut { history: Some(vec![1, 2, 3]), ..part(2, 0) };
    let err = h.prepare_cut(with_history, f.clone(), 0).recv().unwrap().unwrap_err();
    assert_eq!(
        err,
        StoreError::Unsupported("the history is the coordinator's; a host's cut part never carries it".into())
    );
    // Not pending: SQLite answers the next one itself.
    let err = h.prepare_cut(part(3, 0), f.clone(), 0).recv().unwrap().unwrap_err();
    assert_eq!(err, StoreError::Unsupported("several hosts need max_prepared_transactions > 0 (Postgres)".into()));
    h.commit_cut(part(4, 0), f).recv().unwrap().expect("nor is a one-phase commit refused");
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
fn entity_crossing_between_two_parts_prepares_without_waiting() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let (f0, f1) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let h1 = open(&w.url);
    let e = crate::codec::bundle();
    let with = |id: u64, host: u32, entities: Vec<game::EntityBundle>| {
        let mut p = part(id, host);
        p.regions[0].1.entities = entities;
        p
    };
    h.prepare_cut(with(1, 0, vec![e.clone()]), f0.clone(), 0).recv().unwrap().unwrap();
    h1.prepare_cut(with(1, 1, vec![]), f1.clone(), 0).recv().unwrap().unwrap();
    assert!(h.decide_cut(CutId(1), 100, true, vec![0, 1], vec![], c, 0).unwrap());
    settled(&h, &h1, CutId(1));
    assert_eq!(h.load_region(A, &f0).unwrap().unwrap().entities, vec![e.clone()], "stored in host 0's region");
    // Cut 2: host 0's part first (it holds its deleted rows' locks while prepared), then
    // host 1's, which must not wait on them.
    h.set_cut_lock_timeout_ms(500);
    h1.set_cut_lock_timeout_ms(500);
    h.prepare_cut(with(2, 0, vec![]), f0.clone(), 0).recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    h1.prepare_cut(with(2, 1, vec![e.clone()]), f1.clone(), 0)
        .recv_timeout(Duration::from_secs(5))
        .expect("host 1's part is answered")
        .expect("host 1's part prepares beside host 0's");
    assert_eq!(prepared(&w), 2);
    assert!(h.decide_cut(CutId(2), 200, true, vec![0, 1], vec![], c, 0).unwrap());
    settled(&h, &h1, CutId(2));
    assert_eq!(prepared(&w), 0);
    assert!(h.load_region(A, &f0).unwrap().unwrap().entities.is_empty(), "gone from host 0's region");
    assert_eq!(h.load_region(B, &f1).unwrap().unwrap().entities, vec![e.clone()], "in host 1's region");
    assert_eq!(scalar_i64(&w, ENTITIES_STORED_TWICE), 0, "no entity stored in two regions");
    // And back the other way, host 1's part prepared first.
    h1.prepare_cut(with(3, 1, vec![]), f1.clone(), 0).recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    h.prepare_cut(with(3, 0, vec![e.clone()]), f0.clone(), 0).recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert!(h.decide_cut(CutId(3), 300, true, vec![0, 1], vec![], c, 0).unwrap());
    settled(&h, &h1, CutId(3));
    assert_eq!(h.load_region(A, &f0).unwrap().unwrap().entities, vec![e]);
    assert!(h.load_region(B, &f1).unwrap().unwrap().entities.is_empty());
    assert_eq!(scalar_i64(&w, "SELECT COUNT(*) FROM entities"), 1);
    assert_eq!(scalar_i64(&w, ENTITIES_STORED_TWICE), 0);
    assert_eq!(prepared(&w), 0);
    h1.shutdown();
    h.shutdown();
}

/// A prepared part holding an entity in two of its regions is refused before anything is
/// staged, and the next part is accepted.
#[test]
fn prepared_part_with_an_entity_twice_is_refused() {
    let Some(w) = pg_world() else { return };
    let h = open(&w.url);
    let f = h.claim_super_regions(0, vec![(0, 0), (0, -1)]).unwrap();
    let mut twice = part(1, 0);
    twice.regions[0].1.entities = vec![crate::codec::bundle()];
    let mut other = region();
    other.entities = vec![crate::codec::bundle()];
    twice.regions.push((B, other));
    let err = h.prepare_cut(twice, f.clone(), 0).recv().unwrap().unwrap_err();
    assert!(matches!(&err, StoreError::Corrupt(m) if m.contains("two regions")), "{err}");
    assert_eq!(prepared(&w), 0, "nothing staged");
    h.prepare_cut(part(2, 0), f, 0).recv().unwrap().expect("the next part is accepted");
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
    let (f0, _) = fences(&h);
    let c = h.claim_coordinator().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut conn = rt.block_on(sqlx::PgConnection::connect(&w.db.url)).unwrap();
    for (id, lock, blocks) in [(50u64, "FOR KEY SHARE", false), (52, "FOR UPDATE", true)] {
        h.prepare_cut(part(id, 0), f0.clone(), 0).recv().unwrap().unwrap();
        let mut tx = rt.block_on(conn.begin()).unwrap();
        rt.block_on(sqlx::query(&format!("SELECT super_region_regions FROM world WHERE id = 1 {lock}")).fetch_all(&mut *tx))
            .unwrap();
        let decided = h.decide_cut_later(CutId(id), 100 * id, true, vec![0], vec![], c, 0);
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
                let (h, f0) = (h.clone(), f0.clone());
                std::thread::spawn(move || {
                    let _ = tx_boot.send(h.register_host(host_row(0), &f0));
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
