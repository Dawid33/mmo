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

use game::{ownership_from_values, rendezvous_owner, AllInOne, Owner, Ownership, Static};

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

/// The two static policies answer through the trait as the enum they replaced did: every
/// region has exactly one owner, the claim sets partition the map, and the environment
/// parses to the same policies (with the same refusals).
#[test]
fn static_and_all_in_one_answer_as_before() {
    let s = DEFAULT_SUPER_REGION_REGIONS;
    let all = AllInOne;
    assert!(all.owns(RegionCoords::new(-40, 99), s));
    assert_eq!(Ownership::owner_of(&all, (-3, 6)), Owner::Me);
    let hosts = 3;
    for x in -40..40 {
        let rc = RegionCoords::new(x, 7);
        let owners = (0..hosts).filter(|&ordinal| Static { ordinal, hosts }.owns(rc, s)).count();
        assert_eq!(owners, 1, "{rc:?} has exactly one owner");
        let sr = super_region_of(rc, s);
        for ordinal in 0..hosts {
            let answer = Ownership::owner_of(&Static { ordinal, hosts }, sr);
            let owner = rendezvous_owner(sr, hosts);
            assert_eq!(answer, if owner == ordinal { Owner::Me } else { Owner::Host(owner) }, "{rc:?} on host {ordinal}");
        }
    }
    let extent = game::MapExtent { origin: RegionCoords::new(-20, -5), width: 40, height: 10 };
    // x -20..20 -> super-regions -2..=1, z -5..5 -> -1..=0: 8 of them.
    assert_eq!(all.claim_set(Some(extent), s).len(), 8);
    assert!(all.claim_set(None, s).is_empty(), "a procedural world claims nothing");
    let shares: usize = (0..hosts).map(|ordinal| Static { ordinal, hosts }.claim_set(Some(extent), s).len()).sum();
    assert_eq!(shares, 8, "the shares partition the extent");

    let parsed = |o, n, h| ownership_from_values(o, n, h).map(|p| (p.ordinal(), p.hosts(), p.runs_every_host()));
    assert_eq!(parsed(None, None, None), Ok((0, 1, true)), "allinone by default");
    assert_eq!(parsed(Some("allinone"), Some(""), None), Ok((0, 1, true)));
    assert_eq!(parsed(Some("static"), Some("1"), Some("3")), Ok((1, 3, false)));
    let one_of_three = ownership_from_values(Some("static"), Some("1"), Some("3")).unwrap();
    for sx in -8..8 {
        let owner = rendezvous_owner((sx, 2), 3);
        assert_eq!(one_of_three.owner_of((sx, 2)), if owner == 1 { Owner::Me } else { Owner::Host(owner) });
    }
    for bad in [
        (Some("static"), None, Some("3")),
        (Some("static"), Some("3"), Some("3")),
        (Some("static"), Some("0"), Some("0")),
        (Some("static"), Some("x"), Some("3")),
        (Some("allinone"), Some("1"), Some("3")),
        (Some("sharded"), None, None),
    ] {
        assert!(ownership_from_values(bad.0, bad.1, bad.2).is_err(), "{bad:?}");
    }
}

/// Mirrored super-regions are distinct keys: `(-1, 0)` and `(0, -1)` (and every pair a
/// sign-extended coordinate would alias) get independent owners across host counts.
#[test]
fn rendezvous_keys_mirrored_super_regions_apart() {
    for (a, b) in [((-1, 0), (0, -1)), ((-1, 3), (3, -1)), ((-7, -2), (-2, -7)), ((-1, -1), (0, 0))] {
        let owners = |sr| (2..=32).map(|hosts| rendezvous_owner(sr, hosts)).collect::<Vec<_>>();
        assert_ne!(owners(a), owners(b), "{a:?} and {b:?} share an owner at every host count");
    }
}
