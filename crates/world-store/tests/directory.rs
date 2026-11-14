//! The ownership directory reads (R4): `owners` and `owned_by`.

use world_store::NO_OWNER;

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
