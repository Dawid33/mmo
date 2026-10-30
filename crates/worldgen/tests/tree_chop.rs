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
    fn edge_locked(&self, _: TilePos) -> bool {
        false
    }
    fn is_constructed(&self, _: TilePos) -> bool {
        false
    }
    fn tree_id(&self, p: TilePos) -> u32 {
        self.blocks.get(&p).map_or(0, |(_, b)| b.tree_id())
    }
}

impl Tree {
    fn new(sp: &Species, seed: u64, age: u16) -> Tree {
        let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
        install_block_props(reg.props());
        let id = |part: &str| reg.id_of(&format!("{}_{part}", sp.name)).unwrap();
        let (log, branch, leaves) = (id("log"), id("branch"), id("leaves"));
        let blocks = rasterize(&skeleton(sp, seed, age))
            .into_iter()
            .map(|b| {
                let block = match b.part {
                    TreePart::Log => log,
                    TreePart::Branch => branch,
                    TreePart::Leaves => leaves,
                };
                (b.offset, (b.part, Block::with_tree_id(block, TREE)))
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

    /// The trunk log at level `y` nearest the trunk axis.
    fn trunk_at(&self, y: i32) -> Option<TilePos> {
        self.of(TreePart::Log).filter(|p| p[1] == y).min_by_key(|p| (p[0] * p[0] + p[2] * p[2], *p))
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

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn trunk_cut_at_a_third_fells_what_is_above() {
    let mut worst = f64::INFINITY;
    each_tree(|sp, seed, age, tree| {
        let y = tree.top_log() / 3;
        let cut = tree.trunk_at(y).unwrap_or_else(|| panic!("{} {seed}/{age}: no log at {y}", sp.name));
        let f = fell(tree, cut).unwrap();
        let down = felled(&f);
        let above: Vec<_> = tree.blocks.keys().filter(|p| p[1] > y).collect();
        let share = above.iter().filter(|p| down.contains(**p)).count() as f64 / above.len() as f64;
        worst = worst.min(share);
        assert!(share >= 0.9, "{} seed {seed} age {age}: cut at y {y} fells {:.0} % of the {} blocks above", sp.name, share * 100.0, above.len());
        for a in tree.anchors() {
            assert!(!down.contains(&a) && !f.cut.contains(&a), "{} seed {seed} age {age}: anchor {a:?} must stand", sp.name);
        }
    });
    println!("worst share felled above a 1/3 trunk cut: {:.1} %", worst * 100.0);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn oak_branch_cut_never_fells_trunk_wood() {
    let oak = SPECIES.iter().find(|s| s.name == "oak").unwrap();
    let (mut cuts, mut felling) = (0, 0);
    for seed in SEEDS {
        for age in AGES {
            let tree = Tree::new(oak, seed, age);
            let logs: BTreeSet<_> = tree.of(TreePart::Log).collect();
            let anchors = tree.anchors();
            for cut in tree.of(TreePart::Branch) {
                let f = fell(&tree, cut).unwrap();
                assert_eq!(f.cut, vec![cut], "a branch cut takes one block");
                let down = felled(&f);
                assert!(down.iter().all(|p| !logs.contains(p)), "seed {seed} age {age}: branch cut {cut:?} felled trunk");
                assert!(anchors.iter().all(|a| !down.contains(a)));
                cuts += 1;
                if !down.is_empty() {
                    felling += 1;
                    let leaves = down.iter().filter(|p| tree.blocks[*p].0 == TreePart::Leaves).count();
                    let wood = down.len() - leaves;
                    // A felled limb tip with no wood beyond it may hold no leaves.
                    assert!(wood > 0, "seed {seed} age {age}: {cut:?} felled only leaves");
                }
            }
        }
    }
    // Generated oak twigs are thin 26-connected lines that touch each other
    // and the limbs, so one cut block rarely separates anything (measured
    // 1/586 on these seeds): only the invariants are pinned here.
    println!("oak branch cuts felling something: {felling}/{cuts}");
    assert!(cuts > 0);
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
