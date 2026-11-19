//! The ownership directory (R4): the reads `owners` and `owned_by`, and the writes
//! `claim_super_regions_live` (a boot's re-claim). Free land is assigned by the coordinator
//! (Elastic 3, `assign.rs`) and released only by a committed cut part (`two_phase.rs`): their
//! tests are there.

use crate::common::hid;
use game::{CutId, WorldCut};
use world_store::{NO_OWNER, StoreHandle};

use crate::handle::{TEST_SUPER_REGIONS, each_backend, open, raw_exec};

#[test]
fn owners_lists_every_row_with_its_epoch() {
    each_backend("dir_owners", true, |url| {
        let h = open(url);
        assert!(h.owners().unwrap().is_empty());
        h.claim_super_regions(hid(0), vec![TEST_SUPER_REGIONS[0]]).unwrap();
        h.claim_super_regions(hid(1), vec![TEST_SUPER_REGIONS[1]]).unwrap();
        // A takeover raises the epoch.
        h.claim_super_regions(hid(2), vec![TEST_SUPER_REGIONS[1]]).unwrap();
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
        let claimed = h.claim_super_regions(hid(3), vec![TEST_SUPER_REGIONS[0], TEST_SUPER_REGIONS[2]]).unwrap();
        h.claim_super_regions(hid(4), vec![TEST_SUPER_REGIONS[1]]).unwrap();
        assert_eq!(h.owned_by(hid(3)).unwrap(), claimed);
        assert!(h.owned_by(hid(9)).unwrap().epochs.is_empty());
        h.shutdown();
    });
}

/// Host `owner`'s live boot, registering hosts (boot `7000 + id`) until `owner` is issued if
/// it has no live row: a `claim_super_regions_live` is refused unless its claimer's `hosts` row
/// is that live boot. `owner` must not have been issued yet (ids are never issued twice).
pub(crate) fn live_boot(h: &StoreHandle, owner: game::HostId) -> u64 {
    if let Some(r) = h.hosts().unwrap().into_iter().find(|r| r.id == owner && r.state == world_store::HOST_LIVE) {
        return r.boot;
    }
    loop {
        let boot = 7000 + h.hosts().unwrap().iter().map(|r| r.id.0 + 1).max().unwrap_or(1);
        let issued = crate::handle::register(h, "", boot).unwrap().id;
        assert!(issued <= owner, "host {owner} can no longer be issued (issued {issued})");
        if issued == owner {
            return boot;
        }
    }
}

/// A cut that writes nothing but the world tick: fenced by every super-region of its fence.
fn empty_cut(id: u64) -> WorldCut {
    WorldCut { id: CutId(id), host: hid(0), tick: 100 * id, regions: vec![], in_flight: vec![], players: vec![], history: None, adopts: vec![], releases: vec![] }
}

/// A draining host keeps the land it owns until it leaves: its re-claim of its own rows still
/// succeeds (the drain guard is only on an assignment, `AssignLand`), and its fence still
/// commits a cut.
#[test]
fn a_draining_host_keeps_its_own_land() {
    each_backend("dir_drain_keeps", false, |url| {
        let h = open(url);
        let [x, _, _] = TEST_SUPER_REGIONS;
        let boot = live_boot(&h, hid(1));
        let first = h.claim_super_regions_live(hid(1), boot, vec![x]).unwrap();
        assert_eq!(first.epochs, [(x, 1)].into_iter().collect());
        h.drain_host(hid(1), 60_000).unwrap();
        let mine = h.claim_super_regions_live(hid(1), boot, vec![x]).expect("its own rows, re-claimed");
        assert_eq!(mine.epochs, [(x, 2)].into_iter().collect());
        h.commit_cut(WorldCut { host: hid(1), ..empty_cut(1) }, mine).recv().unwrap().expect("its fence holds");
        assert_eq!(h.world_info().unwrap().world_tick, 100, "the cut committed");
        h.shutdown();
    });
}

