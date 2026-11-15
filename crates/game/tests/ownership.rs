//! Super-region membership (`game::ownership`).

use crate::common::h;
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

use game::{heir_of, ownership_from_values, rendezvous_owner, AllInOne, CrashPolicy, Directory, Owner, Ownership, PgClaim, Static};
use std::collections::BTreeSet;

fn owner_counts(hosts: u32) -> Vec<u32> {
    let mut counts = vec![0; hosts as usize];
    for sx in 0..100 {
        for sz in 0..100 {
            counts[rendezvous_owner((sx, sz), hosts).0 as usize] += 1;
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
                assert_eq!(after, h(4), "a super-region only ever moves to the new host");
                moved += 1;
            }
        }
    }
    assert!(moved <= 2500, "4 -> 5 hosts moved {moved} of 10000 super-regions (want <= 25 %)");
    assert_eq!(rendezvous_owner((3, -4), 1), h(0));
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
        let owners = (0..hosts).filter(|&ordinal| Static { id: h(u64::from(ordinal)), hosts }.owns(rc, s)).count();
        assert_eq!(owners, 1, "{rc:?} has exactly one owner");
        let sr = super_region_of(rc, s);
        for ordinal in 0..hosts {
            let answer = Ownership::owner_of(&Static { id: h(u64::from(ordinal)), hosts }, sr);
            let owner = rendezvous_owner(sr, hosts);
            assert_eq!(answer, if owner == h(u64::from(ordinal)) { Owner::Me } else { Owner::Host(owner) }, "{rc:?} on host {ordinal}");
        }
    }
    let extent = game::MapExtent { origin: RegionCoords::new(-20, -5), width: 40, height: 10 };
    // x -20..20 -> super-regions -2..=1, z -5..5 -> -1..=0: 8 of them.
    assert_eq!(all.claim_set(Some(extent), s).len(), 8);
    assert!(all.claim_set(None, s).is_empty(), "a procedural world claims nothing");
    let shares: usize = (0..hosts).map(|ordinal| Static { id: h(u64::from(ordinal)), hosts }.claim_set(Some(extent), s).len()).sum();
    assert_eq!(shares, 8, "the shares partition the extent");

    let parsed = |o, n, h| ownership_from_values(o, n, h).map(|p| (p.me(), p.hosts(), p.runs_every_host()));
    assert_eq!(parsed(None, None, None), Ok((h(0), 1, true)), "allinone by default");
    assert_eq!(parsed(Some("allinone"), Some(""), None), Ok((h(0), 1, true)));
    assert_eq!(parsed(Some("static"), Some("1"), Some("3")), Ok((h(1), 3, false)));
    let one_of_three = ownership_from_values(Some("static"), Some("1"), Some("3")).unwrap();
    for sx in -8..8 {
        let owner = rendezvous_owner((sx, 2), 3);
        assert_eq!(one_of_three.owner_of((sx, 2)), if owner == h(1) { Owner::Me } else { Owner::Host(owner) });
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

/// A [`Directory`] answers from its rows: an owned row is `Me` or `Host(n)`, a free row
/// `Free`; a super-region with no row is `Unknown` until the first whole read
/// ([`Directory::replace`]), `Free` after it.
#[test]
fn directory_answers_me_host_free_unknown() {
    let mut d = Directory::default();
    assert_eq!(d.get((0, 0), h(1)), Owner::Unknown, "never read: unknown");
    d.set((0, 0), Some(h(1)), 3);
    assert_eq!(d.get((0, 0), h(1)), Owner::Me);
    assert_eq!(d.get((0, 0), h(2)), Owner::Host(h(1)));
    assert_eq!(d.get((5, 5), h(1)), Owner::Unknown, "a row set alone tells nothing of the others");
    d.replace(vec![((1, 0), Some(h(2)), 1), ((2, 0), None, 4)]);
    assert_eq!(d.get((1, 0), h(1)), Owner::Host(h(2)));
    assert_eq!(d.get((2, 0), h(1)), Owner::Free, "a free row");
    assert_eq!(d.get((5, 5), h(1)), Owner::Free, "no row after a read: nobody ever claimed it");
    assert_eq!(d.get((0, 0), h(1)), Owner::Me, "a row the read did not list stays");
    assert_eq!(d.owned_by(h(1)), vec![(0, 0)]);
    assert_eq!(d.owned_by(h(2)), vec![(1, 0)]);
    assert!(d.owned_by(h(3)).is_empty());

    // The policy over it.
    let mut claim = PgClaim::new(h(1), 3);
    assert_eq!(claim.policy(), CrashPolicy::Succession);
    assert_eq!(Ownership::owner_of(&claim, (0, 0)), Owner::Unknown);
    claim.directory_mut().expect("a directory").replace(vec![((0, 0), Some(h(1)), 1), ((0, 1), Some(h(0)), 1)]);
    assert_eq!(
        [(0, 0), (0, 1), (0, 2)].map(|sr| Ownership::owner_of(&claim, sr)),
        [Owner::Me, Owner::Host(h(0)), Owner::Free]
    );
    assert_eq!(claim.claim_set(Some(game::MapExtent { origin: RegionCoords::new(0, 0), width: 1, height: 3 }), 1).len(), 1);
    assert!(AllInOne.directory_mut().is_none() && Static { id: h(0), hosts: 2 }.directory_mut().is_none());
    assert_eq!((AllInOne.policy(), Static { id: h(0), hosts: 2 }.policy()), (CrashPolicy::Rollback, CrashPolicy::Rollback));
    let parsed = ownership_from_values(Some("claim"), Some("2"), Some("3")).expect("claim parses");
    assert_eq!((parsed.me(), parsed.hosts(), parsed.policy()), (h(2), 3, CrashPolicy::Succession));
    assert_eq!(parsed.owner_of((0, 0)), Owner::Unknown, "nothing read yet");
    for bad in [(Some("claim"), None, Some("3")), (Some("claim"), Some("3"), Some("3"))] {
        assert!(ownership_from_values(bad.0, bad.1, bad.2).is_err(), "{bad:?}");
    }
}

/// A row is only ever replaced by a newer epoch: a stale read (or a late `Owns`) never
/// undoes a newer claim.
#[test]
fn directory_ignores_an_older_epoch() {
    let mut d = Directory::default();
    d.set((0, 0), Some(h(1)), 5);
    d.set((0, 0), Some(h(2)), 4);
    assert_eq!((d.get((0, 0), h(9)), d.epoch((0, 0))), (Owner::Host(h(1)), Some(5)), "an older set is ignored");
    d.replace(vec![((0, 0), None, 3)]);
    assert_eq!(d.get((0, 0), h(9)), Owner::Host(h(1)), "an older read is ignored");
    d.set((0, 0), None, 6);
    assert_eq!((d.get((0, 0), h(9)), d.epoch((0, 0))), (Owner::Free, Some(6)), "a newer epoch frees it");
    d.replace(vec![((0, 0), Some(h(3)), 7)]);
    assert_eq!(d.get((0, 0), h(9)), Owner::Host(h(3)), "a newer read takes it");
}

/// With every host of `VOXEL_HOSTS` live, the preferred owner is the R3 rendezvous owner
/// for every super-region (a fresh world under `claim` lays out like `static`).
#[test]
fn preferred_is_the_rendezvous_owner_while_every_host_lives() {
    let mut d = Directory::default();
    assert_eq!(d.preferred((0, 0)), None, "nobody is live");
    for hosts in 2..=6u32 {
        d.set_live((0..u64::from(hosts)).map(h).collect());
        for sx in -16..16 {
            for sz in -16..16 {
                assert_eq!(d.preferred((sx, sz)), Some(rendezvous_owner((sx, sz), hosts)), "({sx}, {sz}) of {hosts}");
            }
        }
    }
}

/// A dead host's share moves; nobody else's does.
#[test]
fn preferred_moves_only_the_dead_hosts_share() {
    let mut d = Directory::default();
    for hosts in 2..=6u32 {
        for dead in (0..u64::from(hosts)).map(h) {
            d.set_live((0..u64::from(hosts)).map(h).filter(|o| *o != dead).collect());
            for sx in -16..16 {
                for sz in -16..16 {
                    let before = rendezvous_owner((sx, sz), hosts);
                    let after = d.preferred((sx, sz)).expect("a live host");
                    assert_ne!(after, dead, "({sx}, {sz}) went to the dead host");
                    if before != dead {
                        assert_eq!(after, before, "({sx}, {sz}) of {hosts} moved although host {before} lives");
                    }
                }
            }
        }
    }
}

/// The heir of a host is a pure function of who is live: the same set gives the same
/// heir, never the predecessor itself, and `None` with nobody else live. Heirs spread
/// over the survivors, and a survivor that dies too moves only its own heirships.
#[test]
fn heir_of_is_stable_and_never_the_predecessor() {
    let live = |v: &[u64]| v.iter().copied().map(h).collect::<BTreeSet<_>>();
    assert_eq!(heir_of(h(3), &live(&[])), None);
    assert_eq!(heir_of(h(3), &live(&[3])), None, "only the predecessor itself");
    assert_eq!(heir_of(h(3), &live(&[1, 3])), Some(h(1)));
    let all = live(&[0, 1, 2, 3, 4, 5]);
    let mut heirs = BTreeSet::new();
    for predecessor in (0..32u64).map(h) {
        let heir = heir_of(predecessor, &all).expect("survivors");
        assert_ne!(heir, predecessor);
        assert!(all.contains(&heir));
        assert_eq!(heir_of(predecessor, &all), Some(heir), "stable");
        heirs.insert(heir);
        // Another host leaving the live set moves this heirship only if it was the heir.
        for gone in all.iter().copied().filter(|o| *o != predecessor) {
            let fewer: BTreeSet<_> = all.iter().copied().filter(|o| *o != gone).collect();
            let again = heir_of(predecessor, &fewer).expect("survivors");
            if gone != heir {
                assert_eq!(again, heir, "host {gone} leaving moved {predecessor}'s heir");
            }
        }
    }
    assert!(heirs.len() >= 3, "heirs spread over the survivors: {heirs:?}");
}
