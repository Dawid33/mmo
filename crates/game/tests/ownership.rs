//! Super-region membership (`game::ownership`).

use game::{super_region_of, RegionCoords, DEFAULT_SUPER_REGION_REGIONS};

#[test]
fn super_region_floor_div() {
    let s = DEFAULT_SUPER_REGION_REGIONS;
    assert_eq!(s, 16);
    assert_eq!(super_region_of(RegionCoords::new(-1, -1), s), (-1, -1));
    assert_eq!(super_region_of(RegionCoords::new(-16, 0), s), (-1, 0));
    assert_eq!(super_region_of(RegionCoords::new(-17, 0), s), (-2, 0));
    assert_eq!(super_region_of(RegionCoords::new(0, 15), s), (0, 0));
    assert_eq!(super_region_of(RegionCoords::new(16, -16), s), (1, -1));
}

use game::{rendezvous_owner, Ownership};

fn owner_counts(hosts: u32) -> Vec<u32> {
    let mut counts = vec![0; hosts as usize];
    for sx in 0..100 {
        for sz in 0..100 {
            counts[rendezvous_owner((sx, sz), hosts) as usize] += 1;
        }
    }
    counts
}

#[test]
fn rendezvous_is_stable_and_balanced() {
    for (n, count) in owner_counts(4).into_iter().enumerate() {
        assert!((2000..=3000).contains(&count), "host {n} owns {count} of 10000 super-regions (want 20-30 %)");
    }
    let mut moved = 0;
    for sx in -50..50 {
        for sz in -50..50 {
            let (before, after) = (rendezvous_owner((sx, sz), 4), rendezvous_owner((sx, sz), 5));
            assert_eq!(before, rendezvous_owner((sx, sz), 4), "same input, same owner");
            if before != after {
                assert_eq!(after, 4, "a super-region only ever moves to the new host");
                moved += 1;
            }
        }
    }
    assert!(moved <= 2500, "4 -> 5 hosts moved {moved} of 10000 super-regions (want <= 25 %)");
    assert_eq!(rendezvous_owner((3, -4), 1), 0);
}

#[test]
fn ownership_owns_and_claims() {
    let s = DEFAULT_SUPER_REGION_REGIONS;
    let all = Ownership::AllInOne;
    assert!(all.owns(RegionCoords::new(-40, 99), s));
    let hosts = 3;
    for x in -40..40 {
        let rc = RegionCoords::new(x, 7);
        let owners = (0..hosts).filter(|&ordinal| Ownership::Static { ordinal, hosts }.owns(rc, s)).count();
        assert_eq!(owners, 1, "{rc:?} has exactly one owner");
    }
    let extent = game::MapExtent { origin: RegionCoords::new(-20, -5), width: 40, height: 10 };
    // x -20..20 -> super-regions -2..=1, z -5..5 -> -1..=0: 8 of them.
    assert_eq!(all.claim_set(Some(extent), s).len(), 8);
    assert!(all.claim_set(None, s).is_empty(), "a procedural world claims nothing");
    let shares: usize = (0..hosts).map(|ordinal| Ownership::Static { ordinal, hosts }.claim_set(Some(extent), s).len()).sum();
    assert_eq!(shares, 8, "the shares partition the extent");
}

#[test]
fn ownership_from_values() {
    assert_eq!(Ownership::from_values(None, None, None), Ok(Ownership::AllInOne));
    assert_eq!(Ownership::from_values(Some("allinone"), Some(""), None), Ok(Ownership::AllInOne));
    assert_eq!(
        Ownership::from_values(Some("static"), Some("1"), Some("3")),
        Ok(Ownership::Static { ordinal: 1, hosts: 3 })
    );
    for bad in [
        (Some("static"), None, Some("3")),
        (Some("static"), Some("3"), Some("3")),
        (Some("static"), Some("0"), Some("0")),
        (Some("static"), Some("x"), Some("3")),
        (Some("allinone"), Some("1"), Some("3")),
        (Some("sharded"), None, None),
    ] {
        assert!(Ownership::from_values(bad.0, bad.1, bad.2).is_err(), "{bad:?}");
    }
}
