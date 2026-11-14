//! `worldgen::structures`: placements, the spatial source, the pad. Prefabs are built in memory; no
//! block props, no installs.

use std::sync::Arc;

use game::prefab::*;
use game::BlockId;
use worldgen::structures::*;

use crate::structure_support::box_distance;

fn names(name: &str) -> Option<BlockId> {
    match name {
        "stone" => Some(BlockId(2)),
        "oak_planks" => Some(BlockId(50)),
        "glass" => Some(BlockId(60)),
        _ => None,
    }
}

fn prefab(name: &str, size: [u32; 3]) -> Arc<ResolvedPrefab> {
    let f = PrefabFile {
        format: PREFAB_FORMAT,
        name: name.into(),
        meta: PrefabMeta::default(),
        size,
        anchor: [0, 0, 0],
        palette: vec!["stone".into(), "oak_planks".into(), "glass".into()],
        boxes: vec![FileBox { min: [0, 0, 0], size, mat: 0 }],
        shaped: vec![],
        carve: vec![],
        markers: vec![],
        ground: GroundFit::Flat,
        tags: vec![],
    };
    Arc::new(resolve(&f, &names, Trust::Builtin).expect("resolves"))
}

fn sp(p: &Arc<ResolvedPrefab>, anchor: [i32; 3], turn: u8, pad: Option<Pad>) -> StructurePlacement {
    StructurePlacement { prefab: p.clone(), at: Placement { anchor, turn, mirror: false }, pad }
}

fn boxed(x: i32, z: i32, w: u32, d: u32, pad: Option<Pad>) -> StructurePlacement {
    sp(&prefab("box", [w, 3, d]), [x, 51, z], 0, pad)
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo) as u32) as i32
    }
}

#[test]
fn the_index_finds_what_brute_force_finds() {
    let mut r = Lcg(7);
    let shapes = [prefab("a", [5, 3, 9]), prefab("b", [30, 4, 12]), prefab("c", [90, 4, 70])];
    let mut list = Vec::new();
    for _ in 0..200 {
        let p = &shapes[(r.next() % 3) as usize];
        let pad = (r.next() % 2 == 0)
            .then(|| Pad { level: r.range(40, 60), skirt: r.next() % (MAX_PAD_SKIRT + 1) });
        list.push(sp(p, [r.range(-2000, 2000), 50, r.range(-2000, 2000)], (r.next() % 4) as u8, pad));
    }
    let source = StructureList::new(list.clone(), 1);
    assert_eq!(source.len(), 200);
    for _ in 0..100 {
        let (x0, z0) = (r.range(-2100, 2100), r.range(-2100, 2100));
        let (x1, z1) = (x0 + r.range(1, 200), z0 + r.range(1, 200));
        for margin in [0, 5, 40] {
            let got: Vec<Placement> = source.placements_in(x0, z0, x1, z1, margin).iter().map(|p| p.at).collect();
            let want: Vec<Placement> = list
                .iter()
                .filter(|p| {
                    let pad = p.pad.map(|q| Pad { skirt: q.skirt.min(MAX_PAD_SKIRT), ..q });
                    let p = &StructurePlacement { pad, ..(*p).clone() };
                    let [a, b, c, d] = p.reach_xz();
                    a - margin < x1 && c + margin > x0 && b - margin < z1 && d + margin > z0
                })
                .map(|p| p.at)
                .collect();
            assert_eq!(got, want);
        }
    }
}

#[test]
fn pad_levels_the_box_and_blends_the_skirt() {
    let p = boxed(10, 20, 6, 6, Some(Pad { level: 50, skirt: 3 }));
    let list = [&p];
    let at = |x, z, nat| pad_at(&list, x, z, nat);
    assert_eq!(at(12, 22, 58), PadEffect::Level { surface: 50, in_box: true });
    for (d, want) in [(1, 52), (2, 54), (3, 56)] {
        assert_eq!(at(15 + d, 22, 58), PadEffect::Level { surface: want, in_box: false });
        assert_eq!(at(9 - (d - 1), 22, 58), PadEffect::Level { surface: want, in_box: false });
    }
    assert_eq!(at(20, 22, 58), PadEffect::None);
    for (d, want) in [(1, 48), (2, 46), (3, 44)] {
        assert_eq!(at(15 + d, 22, 42), PadEffect::Level { surface: want, in_box: false });
    }
    for d in 1..=3 {
        assert_eq!(at(15 + d, 22, 50), PadEffect::Level { surface: 50, in_box: false });
    }
}

/// What two placements give under `pad_at`'s rule, from each one alone: the first box holding the
/// column, else the first skirt reaching it.
fn two_pads(a: &StructurePlacement, b: &StructurePlacement, x: i32, z: i32, natural: i32) -> PadEffect {
    let (ea, eb) = (pad_at(&[a], x, z, natural), pad_at(&[b], x, z, natural));
    let in_box = |e: PadEffect| matches!(e, PadEffect::Level { in_box: true, .. });
    if in_box(ea) {
        ea
    } else if in_box(eb) {
        eb
    } else if ea != PadEffect::None {
        ea
    } else {
        eb
    }
}

#[test]
fn the_first_placement_decides() {
    let a = boxed(0, 0, 10, 10, Some(Pad { level: 40, skirt: 4 }));
    let b = boxed(5, 5, 10, 10, Some(Pad { level: 70, skirt: 8 }));
    let both = [&a, &b];
    for x in -10..25 {
        for z in -10..25 {
            assert_eq!(pad_at(&both, x, z, 55), two_pads(&a, &b, x, z, 55), "({x}, {z})");
        }
    }
    // Both boxes hold (7, 7): the first decides.
    assert_eq!(pad_at(&both, 7, 7, 55), PadEffect::Level { surface: 40, in_box: true });
    // Only B's box holds (12, 12), inside A's skirt: B's level.
    assert_eq!(pad_at(&both, 12, 12, 55), PadEffect::Level { surface: 70, in_box: true });
    // A pad-less placement first changes nothing.
    let none = boxed(0, 0, 30, 30, None);
    for x in -10..25 {
        assert_eq!(pad_at(&[&none, &a, &b], x, 3, 55), pad_at(&both, x, 3, 55));
    }
}

#[test]
fn a_box_beats_an_earlier_skirt() {
    // A's skirt (8) reaches six columns into B's box at x 12..22.
    let a = boxed(0, 0, 10, 10, Some(Pad { level: 40, skirt: 8 }));
    let b = boxed(12, 0, 10, 10, Some(Pad { level: 70, skirt: 2 }));
    let both = [&a, &b];
    for x in 12..22 {
        for z in 0..10 {
            assert_eq!(pad_at(&both, x, z, 55), PadEffect::Level { surface: 70, in_box: true }, "B's box at ({x}, {z})");
        }
    }
    // Outside both boxes the earlier skirt still wins where it reaches.
    let mut shared = 0;
    for x in -12..30 {
        for z in -12..20 {
            if box_distance(&a, x, z) == 0 || box_distance(&b, x, z) == 0 {
                continue;
            }
            let (ea, eb) = (pad_at(&[&a], x, z, 55), pad_at(&[&b], x, z, 55));
            let want = if ea != PadEffect::None { ea } else { eb };
            shared += usize::from(ea != PadEffect::None && eb != PadEffect::None);
            assert_eq!(pad_at(&both, x, z, 55), want, "({x}, {z})");
        }
    }
    assert!(shared > 0, "the skirts overlap somewhere outside the boxes");
}

#[test]
fn skirt_is_clamped() {
    let big = boxed(0, 0, 4, 4, Some(Pad { level: 50, skirt: 1000 }));
    let max = boxed(0, 0, 4, 4, Some(Pad { level: 50, skirt: MAX_PAD_SKIRT }));
    let (a, b) = (StructureList::new(vec![big], 0), StructureList::new(vec![max], 0));
    assert_eq!(a.content_hash(), b.content_hash());
    let pa = a.placements_in(-100, -100, 100, 100, 0);
    let pb = b.placements_in(-100, -100, 100, 100, 0);
    assert_eq!(pa[0].pad, Some(Pad { level: 50, skirt: MAX_PAD_SKIRT }));
    let (pa, pb): (Vec<&StructurePlacement>, Vec<&StructurePlacement>) =
        (pa.iter().map(|p| p.as_ref()).collect(), pb.iter().map(|p| p.as_ref()).collect());
    for x in -30..40 {
        assert_eq!(pad_at(&pa, x, 1, 60), pad_at(&pb, x, 1, 60));
    }
    assert_eq!(pad_at(&pa, 3 + MAX_PAD_SKIRT as i32, 1, 60) != PadEffect::None, true);
    assert_eq!(pad_at(&pa, 4 + MAX_PAD_SKIRT as i32, 1, 60), PadEffect::None);
}

#[test]
fn trees_keep_their_distance() {
    let p = boxed(10, 20, 6, 6, None);
    let list = [&p];
    assert!(blocks_trees(&list, 12, 22, 5));
    assert!(blocks_trees(&list, 15 + 5, 22, 5));
    assert!(blocks_trees(&list, 20, 26 + 4, 5));
    assert!(!blocks_trees(&list, 15 + 6, 22, 5));
    assert!(!blocks_trees(&list, 9 - 5, 22, 5));
    assert!(blocks_trees(&list, 10 - 5, 22, 5));
    assert!(!blocks_trees(&list, 12, 26 + 5, 5));
}

#[test]
fn content_hash_tracks_content() {
    let mk = |x, turn, level, set| {
        StructureList::new(vec![boxed(x, 0, 5, 5, Some(Pad { level, skirt: 2 })).with_turn(turn)], set).content_hash()
    };
    let base = mk(3, 0, 50, 9);
    assert_eq!(base, mk(3, 0, 50, 9));
    assert_ne!(base, mk(4, 0, 50, 9));
    assert_ne!(base, mk(3, 1, 50, 9));
    assert_ne!(base, mk(3, 0, 51, 9));
    assert_ne!(base, mk(3, 0, 50, 10));
    assert_ne!(StructureList::new(vec![], 0).content_hash(), 0);
    assert_eq!(NoStructures.content_hash(), 0);
}

trait WithTurn {
    fn with_turn(self, t: u8) -> Self;
}
impl WithTurn for StructurePlacement {
    fn with_turn(mut self, t: u8) -> Self {
        self.at.turn = t;
        self
    }
}

#[test]
fn negative_coordinates() {
    let p = boxed(-130, -7, 4, 4, None);
    let source = StructureList::new(vec![p], 0);
    assert_eq!(source.placements_in(-132, -9, -128, -5, 0).len(), 1);
    // Touches the bucket (-192..-128, -64..0) but not the box (x -130..-126, z -7..-3).
    assert!(source.placements_in(-190, -60, -140, -20, 0).is_empty());
    assert!(source.placements_in(-126, -7, -100, 0, 0).is_empty());
    assert_eq!(source.placements_in(-126, -7, -100, 0, 1).len(), 1);
}
