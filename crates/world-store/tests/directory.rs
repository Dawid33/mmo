//! The ownership directory (R4): the reads `owners` and `owned_by`, and the writes
//! `claim_free` (only absent or free rows) and `release`.

use game::{ChunkCoords, CutId, PortableRegion, RegionCoords, WorldCut};
use world_store::{Fence, NO_OWNER, StoreError, StoreHandle, StoreUrl};

use crate::codec::{chunk, meta, region_ground};
use crate::common;
use crate::handle::{TEST_SUPER_REGIONS, each_backend, open, raw_exec};

#[test]
fn owners_lists_every_row_with_its_epoch() {
    each_backend("dir_owners", true, |url| {
        let h = open(url);
        assert!(h.owners().unwrap().is_empty());
        h.claim_super_regions(0, vec![TEST_SUPER_REGIONS[0]]).unwrap();
        h.claim_super_regions(1, vec![TEST_SUPER_REGIONS[1]]).unwrap();
        // A takeover raises the epoch.
        h.claim_super_regions(2, vec![TEST_SUPER_REGIONS[1]]).unwrap();
        // Ordered by super-region.
        assert_eq!(h.owners().unwrap(), vec![((0, -1), 2, 2), ((0, 0), 0, 1)]);
        // A free row reads as NO_OWNER.
        h.shutdown();
        raw_exec(url, "UPDATE super_regions SET owner = -1 WHERE sx = 0 AND sz = 0");
        let h = open(url);
        let rows = h.owners().unwrap();
        assert!(rows.contains(&((0, 0), NO_OWNER, 1)), "{rows:?}");
        h.shutdown();
    });
}

#[test]
fn owned_by_is_the_claimed_fence() {
    each_backend("dir_owned_by", false, |url| {
        let h = open(url);
        let claimed = h.claim_super_regions(3, vec![TEST_SUPER_REGIONS[0], TEST_SUPER_REGIONS[2]]).unwrap();
        h.claim_super_regions(4, vec![TEST_SUPER_REGIONS[1]]).unwrap();
        assert_eq!(h.owned_by(3).unwrap(), claimed);
        assert!(h.owned_by(9).unwrap().epochs.is_empty());
        h.shutdown();
    });
}

/// Host `owner`'s live boot, registering one (boot `7000 + owner`) if it has no row: a
/// `claim_free` is refused unless its claimer's `hosts` row is that live boot.
pub(crate) fn live_boot(h: &StoreHandle, owner: u32) -> u64 {
    if let Some(r) = h.hosts().unwrap().into_iter().find(|r| r.ordinal == owner && r.state == world_store::HOST_LIVE) {
        return r.boot;
    }
    let row = world_store::HostRow {
        ordinal: owner,
        boot: 7000 + u64::from(owner),
        generation: 0,
        clean: false,
        state: 0,
        seen_ms: 0,
        reg: 0,
        peer_addr: None,
        peer_cert: None,
        quic: None,
        webtransport: None,
        wt_cert_hash: None,
    };
    crate::handle::register(h, row, &Fence::default()).unwrap();
    7000 + u64::from(owner)
}

/// A cut that writes nothing but the world tick: fenced by every super-region of its fence.
fn empty_cut(id: u64) -> WorldCut {
    WorldCut { id: CutId(id), host: 0, tick: 100 * id, regions: vec![], in_flight: vec![], players: vec![], history: None, adopts: vec![] }
}

/// A throwaway Postgres world (`None`, with the skip message, without the test database).
fn pg_db() -> Option<(common::PgTestDb, StoreUrl)> {
    if common::pg_admin_url().is_none() {
        common::skip_msg();
        return None;
    }
    let db = common::PgTestDb::create("mmo_test_dir").expect("pg env var set");
    let url = StoreUrl::Postgres(db.url.clone());
    Some((db, url))
}

#[test]
fn claim_free_takes_absent_and_free_rows_only() {
    each_backend("dir_claim_free", true, |url| {
        let h = open(url);
        let [x, y, z] = TEST_SUPER_REGIONS;
        let a = h.claim_super_regions(0, vec![x, y]).unwrap();
        let b = h.claim_free(1, live_boot(&h, 1), vec![x, z]).unwrap();
        assert_eq!(b.owner, 1);
        assert_eq!(b.epochs, [(z, 1)].into_iter().collect(), "only the absent row is won");
        let rows = h.owners().unwrap();
        assert!(rows.contains(&(x, 0, a.epochs[&x])), "x is still A's at its epoch: {rows:?}");
        assert!(rows.contains(&(z, 1, 1)), "{rows:?}");
        // A's fence still commits; nothing of it was touched.
        h.commit_cut(empty_cut(1), a).recv().unwrap().expect("A is not fenced");
        // A free row is taken, its epoch bumped.
        h.shutdown();
        raw_exec(url, "UPDATE super_regions SET owner = -1 WHERE sx = 0 AND sz = -1");
        let h = open(url);
        let c = h.claim_free(2, live_boot(&h, 2), vec![x, y]).unwrap();
        assert_eq!(c.epochs, [(y, 2)].into_iter().collect(), "only the free row is won");
        assert_eq!(h.claim_free(3, live_boot(&h, 3), vec![]).unwrap().epochs, Default::default(), "nothing asked, nothing won");
        h.shutdown();
    });
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn two_claimers_of_one_free_row_one_wins() {
    let Some((_db, url)) = pg_db() else { return };
    let (a, b) = (open(&url), open(&url));
    let sr = (7, 7);
    // The first round races for an absent row, every later one for the same row freed again.
    let mut last: Option<Fence> = None;
    for round in 0..6u32 {
        if let Some(held) = last.take() {
            a.release(held.owner, &held).unwrap();
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let claim = |h: &StoreHandle, owner: u32| {
            let boot = live_boot(h, owner);
            let (h, barrier) = (h.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                h.claim_free(owner, boot, vec![sr]).unwrap()
            })
        };
        let (ta, tb) = (claim(&a, 2 * round), claim(&b, 2 * round + 1));
        let (fa, fb) = (ta.join().unwrap(), tb.join().unwrap());
        let mut won: Vec<Fence> = [fa, fb].into_iter().filter(|f| !f.epochs.is_empty()).collect();
        assert_eq!(won.len(), 1, "round {round}: exactly one claimer wins: {won:?}");
        let w = won.pop().unwrap();
        let rows = a.owners().unwrap();
        assert_eq!(rows, vec![(sr, i64::from(w.owner), w.epochs[&sr])], "round {round}");
        assert_eq!(w.epochs[&sr], 1 + 2 * u64::from(round), "round {round}: a claim and a release each bump the epoch");
        last = Some(w);
    }
    a.shutdown();
    b.shutdown();
}

#[test]
fn released_rows_are_free_and_the_old_fence_is_refused() {
    each_backend("dir_release", false, |url| {
        let h = open(url);
        let [x, y, _] = TEST_SUPER_REGIONS;
        let old = h.claim_super_regions(0, vec![x, y]).unwrap();
        h.release(0, &old).unwrap();
        let rows = h.owners().unwrap();
        assert!(rows.contains(&(x, NO_OWNER, old.epochs[&x] + 1)), "{rows:?}");
        assert!(rows.contains(&(y, NO_OWNER, old.epochs[&y] + 1)), "{rows:?}");
        let err = h.commit_cut(empty_cut(1), old.clone()).recv().unwrap().unwrap_err();
        assert!(matches!(err, StoreError::Fenced { sr: Some(_) }), "{err}");
        assert_eq!(h.world_info().unwrap().world_tick, 0, "nothing was written");
        // A release is fenced too: the old fence holds nothing any more.
        let err = h.release(0, &old).unwrap_err();
        assert!(matches!(err, StoreError::Fenced { sr: Some(_) }), "{err}");
        let new = h.claim_free(1, live_boot(&h, 1), vec![x]).unwrap();
        assert_eq!(new.epochs, [(x, old.epochs[&x] + 2)].into_iter().collect());
        // A release names its owner: another owner's fence is refused, nothing freed.
        let err = h.release(2, &new).unwrap_err();
        assert!(matches!(err, StoreError::Unsupported(_)), "{err}");
        assert!(h.owners().unwrap().contains(&(x, 1, new.epochs[&x])));
        h.shutdown();
    });
}

#[test]
fn release_waits_for_no_staged_part() {
    let Some((_db, url)) = pg_db() else { return };
    let h = open(&url);
    let rc = RegionCoords::new(0, 0);
    let fence = h.claim_super_regions(0, vec![game::super_region_of(rc, h.super_region_regions())]).unwrap();
    let part = WorldCut {
        regions: vec![(
            rc,
            PortableRegion {
                meta: meta(),
                chunks: [(ChunkCoords::new(0, 0, 0), chunk())].into_iter().collect(),
                ground: Some(region_ground()),
                ..Default::default()
            },
        )],
        ..empty_cut(1)
    };
    h.prepare_cut(part, fence.clone(), 0).recv().unwrap().expect("staged");
    // The handle's other operations' answer for the same state: retry after the decision.
    let err = h.release(0, &fence).unwrap_err();
    assert_eq!(err, StoreError::CutPending(CutId(1)), "{err}");
    assert_eq!(h.owned_by(0).unwrap(), fence, "nothing was released");
    assert!(!h.abort_cut(CutId(1), 100).unwrap());
    assert_eq!(h.cut_decided(CutId(1)).unwrap(), Some(false));
    h.release(0, &fence).expect("no staged part any more");
    assert!(h.owned_by(0).unwrap().epochs.is_empty());
    h.shutdown();
}
