//! `DevTownSource`: a town laid out once beside the first region requested, and nowhere else; and
//! `worldgen::is_wet`. In-memory kit, explicit sources, nothing installed.

use game::{BlockId, RegionCoords};
use worldgen::sites::kit::{town_kit, TownKit};
use worldgen::sites::town::TownParams;
use worldgen::structures::{DevTownSource, NoStructures, StructurePlacement, StructureSource};

use crate::structure_support::prefabs::{set_of, town_houses};
use crate::structure_support::*;

const STYLE: &str = "testville";

fn kit(style: &str) -> TownKit {
    town_kit(&set_of(&town_houses(style, true)), style).unwrap()
}

fn id(name: &str) -> BlockId {
    registry().id_of(name).unwrap_or_else(|| panic!("missing block {name}"))
}

fn params() -> TownParams {
    TownParams::small(SEED, id("cobbled_road"), id("smooth_stone"))
}

fn source_at(origin: [i32; 2], level: i32, k: TownKit, p: TownParams) -> DevTownSource {
    DevTownSource::new(
        k,
        p,
        7,
        Box::new(move |_| origin),
        Box::new(move |_, _| level),
        Box::new(|_, _| false),
    )
}

/// A comparable identity of a placement.
fn key(p: &StructurePlacement) -> (String, [i32; 3], u8) {
    (p.prefab.name.clone(), p.at.anchor, p.at.turn)
}

#[test]
fn nothing_until_a_region_is_requested() {
    let s = source_at([500, 500], 70, kit(STYLE), params());
    assert_eq!(s.home(), None);
    assert!(s.layout().is_none());
    assert!(s.placements_in(-100_000, -100_000, 100_000, 100_000, 0).is_empty());
    assert!(s.paints_in(-100_000, -100_000, 100_000, 100_000, 0).is_empty());
}

#[test]
fn queries_return_the_layout_filtered() {
    let s = source_at([500, 500], 70, kit(STYLE), params());
    let a = RegionCoords { x: 2, z: 3 };
    s.region_requested(a);
    assert_eq!(s.home(), Some(a));
    let layout = s.layout().expect("a layout once a home is set");
    assert!(layout.placements.len() >= 4, "{} houses", layout.placements.len());

    // Everything, in the layout's order.
    let all = s.placements_in(-100_000, -100_000, 100_000, 100_000, 0);
    assert_eq!(all.iter().map(|p| key(p)).collect::<Vec<_>>(), layout.placements.iter().map(key).collect::<Vec<_>>());
    let paints = s.paints_in(-100_000, -100_000, 100_000, 100_000, 0);
    assert_eq!(paints, layout.paints);

    // A window and a margin: exactly those whose (grown) reach meets the rectangle.
    for (rect, margin) in [([480, 480, 530, 530], 0), ([560, 440, 600, 470], 0), ([560, 440, 600, 470], 12), ([0, 0, 64, 64], 0)] {
        let [x0, z0, x1, z1] = rect;
        let (gx0, gz0, gx1, gz1) = (x0 - margin, z0 - margin, x1 + margin, z1 + margin);
        let want: Vec<_> = layout
            .placements
            .iter()
            .filter(|p| {
                let [a, c, d, e] = p.reach_xz();
                a < gx1 && d > gx0 && c < gz1 && e > gz0
            })
            .map(key)
            .collect();
        let got: Vec<_> = s.placements_in(x0, z0, x1, z1, margin).iter().map(|p| key(p)).collect();
        assert_eq!(got, want, "placements {rect:?} margin {margin}");
        let want_paints: Vec<_> = layout
            .paints
            .iter()
            .copied()
            .filter(|p| {
                let [a, c, d, e] = p.rect;
                a < gx1 && d > gx0 && c < gz1 && e > gz0
            })
            .collect();
        assert_eq!(s.paints_in(x0, z0, x1, z1, margin), want_paints, "paints {rect:?} margin {margin}");
    }
    assert!(s.placements_in(0, 0, 64, 64, 0).is_empty());
    assert!(s.paints_in(0, 0, 64, 64, 0).is_empty());
    assert!(s.placements_in(480, 480, 480, 530, 0).is_empty(), "an empty rectangle");

    // A second request changes nothing.
    s.region_requested(RegionCoords { x: 9, z: 9 });
    assert_eq!(s.home(), Some(a));
    let again = s.placements_in(-100_000, -100_000, 100_000, 100_000, 0);
    assert_eq!(again.iter().map(|p| key(p)).collect::<Vec<_>>(), all.iter().map(|p| key(p)).collect::<Vec<_>>());
}

#[test]
fn the_hash_follows_the_style_the_params_and_the_set() {
    let h = |k: TownKit, p: TownParams, set: u32| {
        DevTownSource::new(k, p, set, Box::new(|_| [0, 0]), Box::new(|_, _| 0), Box::new(|_, _| false)).content_hash()
    };
    let base = h(kit(STYLE), params(), 7);
    assert_eq!(base, h(kit(STYLE), params(), 7));
    assert_ne!(base, h(kit("other"), params(), 7), "style");
    assert_ne!(base, h(kit(STYLE), params(), 8), "set hash");
    assert_ne!(base, h(kit(STYLE), TownParams { arms: 3, ..params() }, 7), "arms");
    assert_ne!(base, h(kit(STYLE), TownParams { arm_len: 64, ..params() }, 7), "arm_len");
    assert_ne!(base, h(kit(STYLE), TownParams { seed: SEED + 1, ..params() }, 7), "seed");
    assert_ne!(base, h(kit(STYLE), TownParams { road: id("stone_bricks"), ..params() }, 7), "road");
}

/// A dry column of a generated region, scanning outward from its middle.
fn dry_origin(r: &world::Regions, rc: RegionCoords) -> [i32; 2] {
    let (mx, mz) = (rc.x * RB + RB / 2, rc.z * RB + RB / 2);
    for d in 0..RB / 2 - 8 {
        for (x, z) in [(mx + d, mz), (mx - d, mz), (mx, mz + d), (mx, mz - d)] {
            if world::dry(r, x, z) {
                return [x, z];
            }
        }
    }
    panic!("no dry column in {rc:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn generation_stamps_the_plaza_and_the_first_lot() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    let origin = dry_origin(&bare, rc);
    let level = world::ground(&bare, origin[0], origin[1]);
    let s = source_at(origin, level, kit(STYLE), params());
    // Before the request: byte-identical to no source.
    assert!(bytes(&worldgen::generate_region_with(rc, SEED, mats(), &s)) == bytes(&bare[&(rc.x, rc.z)]));
    s.region_requested(rc);
    let r = world::generate(&[rc], &s);

    assert_eq!(world::read(&r, [origin[0], level, origin[1]]).0.id, id("smooth_stone"), "plaza at the origin column");
    let layout = s.layout().unwrap();
    let first = &layout.placements[0];
    let (min, _) = first.bounds();
    let floor = world::read(&r, [min[0], min[1], min[2]]).0.id;
    assert_eq!(floor, id("stone_bricks"), "the first lot's floor at {min:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn is_wet_matches_the_generated_recipe_water() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let m = mats();
    let (mut wet, mut dry) = (Vec::new(), Vec::new());
    // The open sea far east of the spawn island, and a forest near spawn: every column is compared.
    for rc in [RegionCoords::new(4096 / RB, 0), forest()] {
        let r = world::generate(&[rc], &NoStructures);
        for lx in (0..RB).step_by(3) {
            for lz in (0..RB).step_by(3) {
                let (x, z) = (rc.x * RB + lx, rc.z * RB + lz);
                let (col, cx, cz) = world::column(&r, x, z);
                if col.ground().unwrap().recipe_water(cx, cz).is_some() {
                    wet.push((x, z));
                } else {
                    dry.push((x, z));
                }
            }
        }
    }
    assert!(wet.len() >= 100 && dry.len() >= 100, "scanned {} wet, {} dry columns", wet.len(), dry.len());
    for &(x, z) in &wet {
        assert!(worldgen::is_wet(x, z, SEED, m), "({x}, {z}) is under recipe water");
    }
    for &(x, z) in &dry {
        assert!(!worldgen::is_wet(x, z, SEED, m), "({x}, {z}) has no recipe water");
    }
}
