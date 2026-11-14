//! `DevRegionSource`: dev placements beside EVERY region's origin column, computed lazily per region.
//! Procedural terrain through the explicit `*_with` entries: nothing is installed.

use game::prefab::*;
use game::RegionCoords;
use worldgen::structures::*;

use crate::structure_support::*;

fn entry(prefab: &str, offset: Option<[i32; 2]>) -> DevEntry {
    DevEntry { prefab: prefab.into(), offset, turn: 0 }
}

/// A set of solid stone-brick boxes `w × 3 × 5` named after their width.
fn set_of(widths: &[u32]) -> PrefabSet {
    let reg = registry();
    let texts: Vec<(String, String)> = widths
        .iter()
        .map(|&w| {
            let name = format!("box{w}");
            let mut f = prefabs::file(&name, [w, 3, 5], [0, 0, 0]);
            f.boxes = vec![FileBox { min: [0, 0, 0], size: [w, 3, 5], mat: prefabs::BRICK }];
            (format!("{name}.prefab.ron"), prefab_to_ron(&f))
        })
        .collect();
    PrefabSet::from_sources(texts.iter().map(|(n, t)| (n.as_str(), t.as_bytes())), &|n| reg.id_of(n)).unwrap()
}

fn centre(rc: RegionCoords) -> [i32; 2] {
    [rc.x * RB + RB / 2, rc.z * RB + RB / 2]
}

fn source(entries: Vec<DevEntry>, set: &PrefabSet) -> DevRegionSource {
    DevRegionSource::new(entries, set, Box::new(centre_of), Box::new(|_, _| 70)).unwrap()
}

fn centre_of(rc: RegionCoords) -> [i32; 2] {
    centre(rc)
}

fn key(sp: &std::sync::Arc<StructurePlacement>) -> ([i32; 3], u32) {
    (sp.at.anchor, sp.prefab.name_key)
}

#[test]
fn unknown_prefab_is_an_error_at_construction() {
    let set = set_of(&[7]);
    let e = DevRegionSource::new(vec![entry("nope", None)], &set, Box::new(centre_of), Box::new(|_, _| 70));
    assert!(e.err().unwrap().contains("nope"));
}

#[test]
fn the_answer_does_not_depend_on_what_was_cached() {
    let set = set_of(&[7, 12]);
    let entries = vec![entry("box7", None), entry("box12", None)];
    let (fresh, used) = (source(entries.clone(), &set), source(entries, &set));
    let near = [0, 0, RB, RB];
    // `used` answers far rectangles first.
    for far in [[5 * RB, -3 * RB, 6 * RB, -2 * RB], [-9 * RB, 9 * RB, -8 * RB, 10 * RB], near] {
        used.placements_in(far[0], far[1], far[2], far[3], 3);
    }
    for margin in [0, 10, 100] {
        let (a, b) = (
            fresh.placements_in(0, 0, RB, RB, margin).iter().map(key).collect::<Vec<_>>(),
            used.placements_in(0, 0, RB, RB, margin).iter().map(key).collect::<Vec<_>>(),
        );
        assert!(!a.is_empty());
        assert_eq!(a, b, "margin {margin}");
    }
    assert_eq!(fresh.content_hash(), used.content_hash());
    // Not a StructureList's hash, and it follows the entries.
    let other = source(vec![entry("box7", None)], &set);
    assert_ne!(other.content_hash(), fresh.content_hash());
    assert_ne!(fresh.content_hash(), 0);
}

#[test]
fn a_neighbours_placement_reaching_in_is_returned() {
    let set = set_of(&[30]);
    // The row is anchored 20 blocks west of the origin, which is one block into its region.
    let origin_of = |rc: RegionCoords| [rc.x * RB + 1, rc.z * RB + RB / 2];
    let src = DevRegionSource::new(vec![entry("box30", Some([-20, 0]))], &set, Box::new(origin_of), Box::new(|_, _| 70)).unwrap();
    let in_zero = src.placements_in(0, 0, RB, RB, 0);
    let xs: Vec<i32> = in_zero.iter().map(|p| p.at.anchor[0]).collect();
    // Region (0, 0)'s own, and region (1, 0)'s, whose box starts RB - 19 and so is outside; (0,0)'s reaches
    // west of the region instead, into (-1, 0), which a query inside (-1, 0) must return.
    assert!(xs.contains(&(1 - 20)), "{xs:?}");
    let west = src.placements_in(-RB, 0, 0, RB, 0);
    assert!(west.iter().any(|p| p.at.anchor[0] == 1 - 20), "region (0, 0)'s placement reaches into (-1, 0)");
}

#[test]
fn every_region_gets_its_row_and_a_row_across_the_seam_is_whole() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    // Wide enough that the row, laid out beside the centre, crosses the seam at x = RB.
    let w = (RB / 2 + 8) as u32;
    let set = set_of(&[w]);
    let src = source(vec![entry(&format!("box{w}"), None)], &set);
    let rcs = [RegionCoords::new(0, 0), RegionCoords::new(1, 0)];
    let regions = world::generate(&rcs, &src);
    let all = src.placements_in(0, 0, 2 * RB, RB, 0);
    for rc in rcs {
        let [ox, oz] = centre(rc);
        let sp = all
            .iter()
            .find(|p| p.at.anchor[0] == ox + DEV_GAP_BLOCKS && p.at.anchor[2] == oz)
            .unwrap_or_else(|| panic!("no placement beside the centre of {rc:?}"));
        assert_eq!(sp.pad, Some(Pad { level: 70, skirt: DEV_PAD_SKIRT }));
    }
    let seam = all.iter().find(|p| p.bounds().0[0] < RB && p.bounds().1[0] > RB).expect("a row crosses the seam");
    let _ = seam;
    for sp in &all {
        for (p, w) in prefabs::expected(&sp.prefab, &sp.at) {
            if p[0] < 0 || p[0] >= 2 * RB || p[2] < 0 || p[2] >= RB {
                continue;
            }
            let w = w.unwrap_or((air(), None));
            assert_eq!(world::read(&regions, p), w, "prefab cell {p:?}");
        }
    }
}

fn home_source(entries: Vec<DevEntry>, set: &PrefabSet) -> DevHomeSource {
    DevHomeSource::new(entries, set, Box::new(centre_of), Box::new(|_, _| 70)).unwrap()
}

#[test]
fn home_source_is_empty_until_a_home_is_set() {
    let set = set_of(&[7]);
    let src = home_source(vec![entry("box7", None)], &set);
    assert_eq!(src.home(), None);
    for rc in [RegionCoords::new(0, 0), RegionCoords::new(3, -2)] {
        assert!(src.placements_in(rc.x * RB, rc.z * RB, (rc.x + 1) * RB, (rc.z + 1) * RB, 50).is_empty());
    }
    if default_dials() {
        install_real_block_props();
        let rc = RegionCoords::new(0, 0);
        let with = worldgen::generate_region_with(rc, SEED, mats(), &src);
        let without = worldgen::generate_region_with(rc, SEED, mats(), &NoStructures);
        assert_eq!(bincode::serialize(&with).unwrap(), bincode::serialize(&without).unwrap());
    }
}

#[test]
fn home_source_stands_beside_the_first_requested_region_only() {
    let set = set_of(&[7, 12]);
    let entries = vec![entry("box7", None), entry("box12", None)];
    let src = home_source(entries.clone(), &set);
    let (a, b) = (RegionCoords::new(2, 1), RegionCoords::new(-4, 5));
    src.region_requested(a);
    src.region_requested(b);
    assert_eq!(src.home(), Some(a));
    let region_of = |rc: RegionCoords| src.placements_in(rc.x * RB, rc.z * RB, (rc.x + 1) * RB, (rc.z + 1) * RB, 0);
    let expected = dev_placements(&entries, centre(a), &set, &|_, _| 70).unwrap();
    let got = region_of(a);
    assert_eq!(got.len(), 2);
    assert_eq!(got.iter().map(key).collect::<Vec<_>>(), expected.iter().map(|p| (p.at.anchor, p.prefab.name_key)).collect::<Vec<_>>());
    assert!(region_of(b).is_empty());
    assert!(region_of(RegionCoords::new(20, 20)).is_empty());
    // Not the hash of a DevRegionSource for the same spec.
    assert_ne!(src.content_hash(), source(entries, &set).content_hash());
    assert_ne!(src.content_hash(), 0);
}

#[test]
fn home_row_across_a_seam_is_whole() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let w = (RB / 2 + 8) as u32;
    let set = set_of(&[w]);
    let src = home_source(vec![entry(&format!("box{w}"), None)], &set);
    let home = RegionCoords::new(0, 0);
    src.region_requested(home);
    let rcs = [home, RegionCoords::new(1, 0)];
    let regions = world::generate(&rcs, &src);
    let all = src.placements_in(0, 0, 2 * RB, RB, 0);
    assert!(all.iter().any(|p| p.bounds().0[0] < RB && p.bounds().1[0] > RB), "the row crosses the seam");
    for sp in &all {
        for (p, w) in prefabs::expected(&sp.prefab, &sp.at) {
            if p[0] < 0 || p[0] >= 2 * RB || p[2] < 0 || p[2] >= RB {
                continue;
            }
            assert_eq!(world::read(&regions, p), w.unwrap_or((air(), None)), "prefab cell {p:?}");
        }
    }
}
