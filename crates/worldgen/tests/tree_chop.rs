//! Chopping GENERATED trees: `game::fell` over `rasterize(skeleton(..))`
//! blocks, for every species at mature ages. Hand-built trees (game's
//! `tests/chop.rs`) hide what real crowns do — leaves that bridge a cut, wide
//! trunks, branches — so the felling rule is pinned here on the real shapes.

use std::collections::{BTreeMap, BTreeSet};

use game::{fell, install_block_props, Block, BlockId, BlockRegistry, Felling, TilePos, TileSource};
use worldgen::trees::{rasterize, skeleton, Species, TreePart, MAX_AGE, SPECIES};

const MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");
const TREE: u32 = 1;
const SEEDS: std::ops::Range<u64> = 0..6;
const AGES: [u16; 2] = [80, MAX_AGE];

/// One generated tree as a tile source (base block at the origin).
struct Tree {
    blocks: BTreeMap<TilePos, (TreePart, Block)>,
}

impl TileSource for Tree {
    fn block(&self, p: TilePos) -> BlockId {
        self.blocks.get(&p).map_or(BlockId::AIR, |(_, b)| b.id)
    }
    fn is_bedrock(&self, _: TilePos) -> bool {
        false
    }
    fn is_constructed(&self, _: TilePos) -> bool {
        false
    }
    fn tree_id(&self, p: TilePos) -> u32 {
        self.blocks.get(&p).map_or(0, |(_, b)| b.tree_id())
    }
    fn tree_limb(&self, p: TilePos) -> u8 {
        self.blocks.get(&p).map_or(0, |(_, b)| b.limb())
    }
}

impl Tree {
    fn new(sp: &Species, seed: u64, age: u16) -> Tree {
        let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
        install_block_props(reg.props());
        let id = |part: &str| reg.id_of(&format!("{}_{part}", sp.name)).unwrap();
        // A one-block species (cactus) is all its one block, leafless.
        let (log, branch, leaves) = if sp.single_block {
            let b = reg.id_of(sp.name).unwrap();
            (b, b, BlockId::AIR)
        } else {
            (id("log"), id("branch"), id("leaves"))
        };
        let blocks = rasterize(&skeleton(sp, seed, age))
            .into_iter()
            .map(|b| {
                let block = match b.part {
                    TreePart::Log => log,
                    TreePart::Branch => branch,
                    TreePart::Leaves => leaves,
                };
                (b.offset, (b.part, Block::with_tree(block, TREE, b.limb)))
            })
            .collect();
        Tree { blocks }
    }

    fn of(&self, part: TreePart) -> impl Iterator<Item = TilePos> + '_ {
        self.blocks.iter().filter(move |(_, (p, _))| *p == part).map(|(p, _)| *p)
    }

    /// Every wood block on the lowest wood level.
    fn anchors(&self) -> BTreeSet<TilePos> {
        let wood: Vec<_> = self.blocks.iter().filter(|(_, (p, _))| *p != TreePart::Leaves).map(|(p, _)| *p).collect();
        let floor = wood.iter().map(|p| p[1]).min().unwrap();
        wood.into_iter().filter(|p| p[1] == floor).collect()
    }

    /// The trunk log (limb 0) at level `y` nearest the trunk axis.
    fn trunk_at(&self, y: i32) -> Option<TilePos> {
        self.of(TreePart::Log)
            .filter(|p| p[1] == y && self.blocks[p].1.limb() == 0)
            .min_by_key(|p| (p[0] * p[0] + p[2] * p[2], *p))
    }

    fn top_log(&self) -> i32 {
        self.of(TreePart::Log).map(|p| p[1]).max().unwrap()
    }
}

fn felled(f: &Felling) -> BTreeSet<TilePos> {
    f.falling.iter().flatten().copied().collect()
}

fn each_tree(mut check: impl FnMut(&Species, u64, u16, &Tree)) {
    for sp in &SPECIES {
        for seed in SEEDS {
            for age in AGES {
                check(sp, seed, age, &Tree::new(sp, seed, age));
            }
        }
    }
}

impl Tree {
    /// The limb pieces: per limb index ≥ 1, the 26-connected components of
    /// its blocks, each with its root level (lowest wood, or lowest leaf for a
    /// piece with no wood). Indices wrap, so one index may be several pieces.
    fn limb_pieces(&self) -> Vec<(i32, BTreeSet<TilePos>)> {
        let mut by_limb: BTreeMap<u8, BTreeSet<TilePos>> = BTreeMap::new();
        for (p, (_, b)) in &self.blocks {
            if b.limb() != 0 {
                by_limb.entry(b.limb()).or_default().insert(*p);
            }
        }
        let mut out = Vec::new();
        for (_, mut unseen) in by_limb {
            while let Some(start) = unseen.pop_first() {
                let piece = flood(start, |q| unseen.remove(&q));
                let root = piece.iter().map(|p| (self.blocks[p].0 == TreePart::Leaves, p[1])).min().unwrap().1;
                out.push((root, piece));
            }
        }
        out
    }

    /// Root level of the limb piece holding each limb block.
    fn piece_roots(&self) -> BTreeMap<TilePos, i32> {
        self.limb_pieces().into_iter().flat_map(|(root, piece)| piece.into_iter().map(move |p| (p, root))).collect()
    }
}

/// 26-connected flood from `start`; `take(q)` claims a neighbour.
fn flood(start: TilePos, mut take: impl FnMut(TilePos) -> bool) -> BTreeSet<TilePos> {
    let mut seen = BTreeSet::from([start]);
    let mut stack = vec![start];
    while let Some(p) = stack.pop() {
        for dy in -1..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let q = [p[0] + dx, p[1] + dy, p[2] + dz];
                    if !seen.contains(&q) && take(q) {
                        seen.insert(q);
                        stack.push(q);
                    }
                }
            }
        }
    }
    seen
}

/// Blocks still standing after `f` that no longer join (26-connected,
/// through any remaining block of the tree) the stump's lowest wood level.
fn unsupported(tree: &Tree, f: &Felling) -> BTreeSet<TilePos> {
    let down = felled(f);
    let standing: BTreeSet<TilePos> =
        tree.blocks.keys().copied().filter(|p| !down.contains(p) && !f.cut.contains(p)).collect();
    let anchors: Vec<TilePos> = tree.anchors().into_iter().filter(|a| standing.contains(a)).collect();
    let mut reached = BTreeSet::new();
    for a in anchors {
        if !reached.contains(&a) {
            reached.extend(flood(a, |q| standing.contains(&q) && !reached.contains(&q)));
        }
    }
    standing.into_iter().filter(|p| !reached.contains(p)).collect()
}

/// What the limb rule alone fells for a branch cut: the rest of its piece.
fn piece_rest(piece: &BTreeSet<TilePos>, cut: TilePos) -> BTreeSet<TilePos> {
    piece.iter().copied().filter(|&p| p != cut).collect()
}

/// [`unsupported`] blocks above `y`.
fn floating_above(tree: &Tree, f: &Felling, y: i32) -> usize {
    unsupported(tree, f).iter().filter(|p| p[1] > y).count()
}

/// What the limb rule alone fells for a trunk cut at level `y`: limb-0
/// blocks above it, and every limb piece (of the tree without the cut
/// blocks) rooted at or above it.
fn rule_falls(tree: &Tree, f: &Felling, y: i32) -> BTreeSet<TilePos> {
    let left = Tree { blocks: tree.blocks.iter().filter(|(p, _)| !f.cut.contains(p)).map(|(p, v)| (*p, *v)).collect() };
    let roots = left.piece_roots();
    left.blocks
        .iter()
        .filter(|(p, (_, b))| if b.limb() == 0 { p[1] > y } else { roots[*p] >= y })
        .map(|(p, _)| *p)
        .collect()
}

/// Share of the blocks above `y` that `f` fells.
fn share_above(tree: &Tree, f: &Felling, y: i32) -> f64 {
    let down = felled(f);
    let above: Vec<_> = tree.blocks.keys().filter(|p| p[1] > y).collect();
    above.iter().filter(|p| down.contains(**p)).count() as f64 / above.len() as f64
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn trunk_cut_at_a_third_fells_the_trunk_above_and_the_limbs_rooted_there() {
    let mut shares: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    each_tree(|sp, seed, age, tree| {
        let y = tree.top_log() / 3;
        let cut = tree.trunk_at(y).unwrap_or_else(|| panic!("{} {seed}/{age}: no log at {y}", sp.name));
        let f = fell(tree, cut).unwrap();
        let down = felled(&f);
        // The limb rule's falls (pieces taken with the cut removed) all fall;
        // anything more is only what the rule leaves unsupported.
        let rule = rule_falls(tree, &f, y);
        for p in &rule {
            assert!(down.contains(p), "{} seed {seed} age {age}: {p:?} must fall", sp.name);
        }
        let kept = Felling { cut: f.cut.clone(), falling: vec![rule.iter().copied().collect()] };
        let unsupported = unsupported(tree, &kept);
        for p in down.difference(&rule) {
            assert!(unsupported.contains(p), "{} seed {seed} age {age}: {p:?} fell though supported", sp.name);
        }
        for a in tree.anchors() {
            assert!(!down.contains(&a) && !f.cut.contains(&a), "{} seed {seed} age {age}: anchor {a:?} must stand", sp.name);
        }
        let share = share_above(tree, &f, y);
        // A pine's whorls start low: the ones rooted below a 1/3 cut stay on
        // the stump (measured 71-80 % felled), the rest falls.
        let min_share = if sp.name == "pine" { 0.6 } else { 0.9 };
        assert!(share >= min_share, "{} seed {seed} age {age}: cut at y {y} fells {:.1} % above", sp.name, share * 100.0);
        shares.entry(sp.name).or_default().push(share);
    });
    for (name, v) in shares {
        let (min, mean) = (v.iter().copied().fold(1.0, f64::min), v.iter().sum::<f64>() / v.len() as f64);
        println!("{name}: share above a 1/3 trunk cut felled: min {:.1} %, mean {:.1} %", min * 100.0, mean * 100.0);
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn trunk_cut_below_every_limb_fells_what_is_above() {
    let mut worst = f64::INFINITY;
    each_tree(|sp, seed, age, tree| {
        let y = tree.limb_pieces().iter().map(|(root, _)| *root).min().unwrap_or(1).max(1);
        let Some(cut) = tree.trunk_at(y) else { return };
        let f = fell(tree, cut).unwrap();
        let share = share_above(tree, &f, y);
        worst = worst.min(share);
        assert!(share >= 0.9, "{} seed {seed} age {age}: cut at y {y} fells {:.1} % of the blocks above", sp.name, share * 100.0);
    });
    println!("worst share felled above a trunk cut below every limb: {:.1} %", worst * 100.0);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn oak_branch_cut_fells_only_its_limb() {
    let oak = SPECIES.iter().find(|s| s.name == "oak").unwrap();
    let mut cuts = 0;
    for seed in SEEDS {
        for age in AGES {
            let tree = Tree::new(oak, seed, age);
            let anchors = tree.anchors();
            let pieces: Vec<_> = tree.limb_pieces().into_iter().map(|(_, p)| p).collect();
            for cut in tree.of(TreePart::Branch) {
                let limb = tree.blocks[&cut].1.limb();
                assert!(limb > 0, "seed {seed} age {age}: branch {cut:?} is on a limb");
                let f = fell(&tree, cut).unwrap();
                assert_eq!(f.cut, vec![cut], "a branch cut takes one block");
                let down = felled(&f);
                assert!(!down.is_empty(), "seed {seed} age {age}: branch cut {cut:?} fells nothing");
                let piece = pieces.iter().find(|p| p.contains(&cut)).unwrap();
                let rule = piece_rest(piece, cut);
                assert!(rule.iter().all(|p| down.contains(p)), "the rest of the cut's piece falls");
                let orphans = unsupported(&tree, &Felling { cut: f.cut.clone(), falling: vec![rule.iter().copied().collect()] });
                for p in &down {
                    let (part, b) = tree.blocks[p];
                    if rule.contains(p) {
                        assert_eq!(b.limb(), limb, "seed {seed} age {age}: {p:?} is not limb {limb}");
                    } else {
                        // Beyond its piece only what the piece alone held up.
                        assert!(orphans.contains(p), "seed {seed} age {age}: {p:?} fell though supported");
                    }
                    assert!(!(part == TreePart::Log && b.limb() == 0), "trunk log {p:?} fell");
                }
                assert!(anchors.iter().all(|a| !down.contains(a)));
                cuts += 1;
            }
        }
    }
    println!("oak branch cuts, every one felling only its limb: {cuts}");
    assert!(cuts > 0);
}

#[test]
fn neighbouring_tree_is_untouched() {
    // Two mature oaks 6 blocks apart, crowns overlapping (the first keeps a
    // shared block): every cut on the first fells only its own blocks.
    let oak = SPECIES.iter().find(|s| s.name == "oak").unwrap();
    let mut both = Tree::new(oak, 1, MAX_AGE);
    let other = Tree::new(oak, 2, MAX_AGE);
    let mut overlap = 0;
    for (p, (part, b)) in other.blocks {
        let q = [p[0] + 6, p[1], p[2]];
        if both.blocks.contains_key(&q) {
            overlap += 1;
        } else {
            both.blocks.insert(q, (part, Block::with_tree(b.id, TREE + 1, b.limb())));
        }
    }
    let first: Vec<TilePos> = both.blocks.iter().filter(|(_, (_, b))| b.tree_id() == TREE).map(|(p, _)| *p).collect();
    let touching = first.iter().any(|p| {
        (-1..=1).any(|d| both.blocks.get(&[p[0] + d, p[1], p[2]]).is_some_and(|(_, b)| b.tree_id() == TREE + 1))
    });
    assert!(touching, "the crowns touch ({overlap} shared blocks)");
    let cuts = [both.trunk_at(both.top_log() / 3).unwrap(), both.of(TreePart::Branch).find(|p| both.blocks[p].1.tree_id() == TREE).unwrap()];
    for cut in cuts {
        let f = fell(&both, cut).unwrap();
        assert!(felled(&f).iter().all(|p| both.blocks[p].1.tree_id() == TREE), "cut {cut:?} felled the neighbour");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn pine_trunk_cut_at_every_log_level_fells_something() {
    let pine = SPECIES.iter().find(|s| s.name == "pine").unwrap();
    for seed in SEEDS {
        for age in AGES {
            let tree = Tree::new(pine, seed, age);
            // Up to the top log level, which has no wood above it to fell
            // (the leaves over it stay with the standing wood).
            for y in 1..tree.top_log() {
                let Some(cut) = tree.trunk_at(y) else { continue };
                let f = fell(&tree, cut).unwrap();
                assert!(!f.falling.is_empty(), "seed {seed} age {age}: trunk cut at y {y} fells nothing");
            }
        }
    }
}

#[test]
fn anchor_layer_cut_fells_everything() {
    each_tree(|sp, seed, age, tree| {
        let anchors = tree.anchors();
        let cut = tree.trunk_at(anchors.iter().next().unwrap()[1]).unwrap();
        let f = fell(tree, cut).unwrap();
        assert!(anchors.iter().all(|a| f.cut.contains(a)), "{} seed {seed} age {age}: the cut takes every anchor", sp.name);
        let down = felled(&f);
        for (p, (part, _)) in &tree.blocks {
            if *part != TreePart::Leaves && !f.cut.contains(p) {
                assert!(down.contains(p), "{} seed {seed} age {age}: wood {p:?} must fall", sp.name);
            }
        }
    });
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn trunk_cut_leaves_nothing_floating() {
    let mut counts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    each_tree(|sp, seed, age, tree| {
        for y in 1..tree.top_log() {
            let Some(cut) = tree.trunk_at(y) else { continue };
            let f = fell(tree, cut).unwrap();
            let floating = floating_above(tree, &f, y);
            let c = counts.entry(sp.name).or_default();
            c.0 += 1;
            c.1 += floating;
            assert_eq!(floating, 0, "{} seed {seed} age {age}: a cut at y {y} leaves {floating} blocks floating", sp.name);
        }
    });
    println!("floating blocks after trunk cuts (cuts, blocks): {counts:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn pine_limb_cut_stays_within_its_piece() {
    let pine = SPECIES.iter().find(|s| s.name == "pine").unwrap();
    let mut cuts = 0;
    for seed in SEEDS {
        for age in AGES {
            let tree = Tree::new(pine, seed, age);
            let pieces: Vec<_> = tree.limb_pieces().into_iter().map(|(_, p)| p).collect();
            let limb_wood: Vec<TilePos> = tree
                .blocks
                .iter()
                .filter(|(_, (part, b))| *part != TreePart::Leaves && b.limb() != 0)
                .map(|(p, _)| *p)
                .collect();
            for &cut in limb_wood.iter().step_by(3) {
                let piece = pieces.iter().find(|p| p.contains(&cut)).unwrap();
                let lo = |a: usize| piece.iter().map(|p| p[a]).min().unwrap() - 1;
                let hi = |a: usize| piece.iter().map(|p| p[a]).max().unwrap() + 1;
                let f = fell(&tree, cut).unwrap();
                let orphans = unsupported(&tree, &Felling { cut: f.cut.clone(), falling: vec![piece_rest(piece, cut).into_iter().collect()] });
                for p in felled(&f) {
                    // Outside its piece only what the piece alone held up.
                    assert!(
                        (0..3).all(|a| (lo(a)..=hi(a)).contains(&p[a])) || orphans.contains(&p),
                        "seed {seed} age {age}: cut {cut:?} felled {p:?} outside its piece"
                    );
                }
                cuts += 1;
            }
        }
    }
    assert!(cuts > 0);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn branch_cut_leaves_nothing_floating() {
    // (cuts, blocks the piece rule alone left floating, blocks floating now)
    let mut counts: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    each_tree(|sp, seed, age, tree| {
        let pieces: Vec<_> = tree.limb_pieces().into_iter().map(|(_, p)| p).collect();
        let limb_wood: Vec<TilePos> = tree
            .blocks
            .iter()
            .filter(|(_, (part, b))| *part != TreePart::Leaves && b.limb() != 0)
            .map(|(p, _)| *p)
            .collect();
        for &cut in &limb_wood {
            let f = fell(tree, cut).unwrap();
            let piece = pieces.iter().find(|p| p.contains(&cut)).unwrap();
            let before =
                unsupported(tree, &Felling { cut: f.cut.clone(), falling: vec![piece_rest(piece, cut).into_iter().collect()] }).len();
            let after = unsupported(tree, &f).len();
            let c = counts.entry(sp.name).or_default();
            c.0 += 1;
            c.1 += before;
            c.2 += after;
            assert_eq!(after, 0, "{} seed {seed} age {age}: branch cut {cut:?} leaves {after} blocks floating", sp.name);
        }
    });
    println!("floating blocks after branch cuts (cuts, piece rule alone, with support): {counts:?}");
    assert!(counts.values().all(|c| c.0 > 0));
}

/// A player felling a generated tree at its base (plan 2026-11-05,
/// Decision 9) gets every block's drop back — one log per log block, one
/// stick per branch block, nothing for leaves — as piles where the tree
/// came to rest, once its body settles. Every species with logs (a
/// single-block species has none), mature, on a flat floor.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn player_fell_yields_one_log_per_wood_block() {
    use crate::tree_fall::{flat_floor, scene};
    use game::voxel_body::SETTLE_TIMEOUT_TICKS;
    use game::{drop_of, GameEventKind, ItemId};

    for sp in SPECIES.iter().filter(|s| !s.single_block) {
        for seed in 0..2 {
            let mut s = scene(flat_floor(2), sp, seed, MAX_AGE);
            let cut = s.trunk_at(s.base_y()).unwrap_or_else(|| panic!("{} {seed}: no base log", sp.name));
            let mut want: BTreeMap<ItemId, u32> = BTreeMap::new();
            for p in s.tree.keys() {
                let block = s.region.data().block_at(*p).expect("tree block");
                if let Some(stack) = drop_of(block) {
                    *want.entry(stack.item).or_default() += u32::from(stack.count);
                }
            }
            let tick = s.region.current_tick() as u64;
            let destroyed_before = game::item_piles::items_destroyed_on_thread();
            s.region.with_data(|r| assert!(r.player_chop_safe(cut, tick), "{} {seed}: the chop clears", sp.name));
            let mut settled = false;
            for _ in 0..SETTLE_TIMEOUT_TICKS + 5 {
                s.region.handle_event(GameEventKind::Tick).unwrap();
                s.region.forget_last_event();
                if s.region.data().ecs.voxel_body.iter().all(|(_, v)| v.is_none()) {
                    settled = true;
                    break;
                }
            }
            assert!(settled, "{} {seed}: the felled tree settles", sp.name);
            let mut got: BTreeMap<ItemId, u32> = BTreeMap::new();
            for pile in s.region.data().item_piles().values() {
                for st in &pile.stacks {
                    *got.entry(st.item).or_default() += u32::from(st.count);
                }
            }
            assert_eq!(
                game::item_piles::items_destroyed_on_thread(),
                destroyed_before,
                "{} seed {seed}: no item lost to the pile caps",
                sp.name
            );
            assert_eq!(got, want, "{} seed {seed}: one item per wood block ({} piles)", sp.name, s.region.data().item_piles().len());
            println!("{} seed {seed}: {want:?} in {} piles", sp.name, s.region.data().item_piles().len());
        }
    }
}
