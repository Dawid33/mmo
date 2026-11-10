//! Trees on stud terrain (`stud_trees.rs`): placed trees as feature
//! bricks of `generate_stud_region`.
use std::collections::{BTreeMap, BTreeSet};

use game::{
    RegionCoords, StudBrick, StudChunk, StudChunkCoords, StudRegionSeed, MAT_LEAVES, MAT_WOOD, REGION_CHUNKS,
    STUD_CHUNK_PLATES, STUD_CHUNK_STUDS,
};
use worldgen::stud_gen::*;
use worldgen::stud_trees::*;
use worldgen::trees;

const SEED: u64 = 7;
/// Regions of the procedural world scanned for trees.
const REGIONS: [(i32, i32); 3] = [(0, 0), (1, 0), (3, -2)];

fn seeds() -> Vec<(RegionCoords, StudRegionSeed)> {
    REGIONS.iter().map(|&(x, z)| (RegionCoords::new(x, z), generate_stud_region(RegionCoords::new(x, z), SEED))).collect()
}

/// A brick's cell box in region stud cells: `(lo, hi)`, `hi` exclusive.
fn brick_box(c: StudChunkCoords, b: &StudBrick) -> ([i32; 3], [i32; 3]) {
    let base = [(c.x * STUD_CHUNK_STUDS) as i32, c.y * STUD_CHUNK_PLATES as i32, (c.z * STUD_CHUNK_STUDS) as i32];
    let s = b.size();
    let lo = [0, 1, 2].map(|a| base[a] + b.min[a] as i32);
    (lo, [0, 1, 2].map(|a| lo[a] + s[a] as i32))
}

#[test]
fn stud_tree_ids_are_non_zero_and_distinct_within_reach() {
    use trees::placement::tree_id;
    let mut seen = BTreeMap::new();
    // Every lattice cell of a 3 x 3 block of the widest regions (512 m = 64 cells).
    for lx in -64..128 {
        for lz in -64..128 {
            let id = stud_tree_id(tree_id(lx, lz));
            assert_ne!(id, 0, "cell ({lx}, {lz})");
            assert!(seen.insert(id, (lx, lz)).is_none(), "cells ({lx}, {lz}) and {:?} share id {id:#x}", seen[&id]);
        }
    }
    // The cells whose low bits are zero share the antipodal id, 128 cells away.
    assert_eq!(stud_tree_id(tree_id(0, 0)), 0x8080);
    assert_eq!(stud_tree_id(tree_id(128, 128)), 0x8080);
    assert_eq!(stud_tree_id(tree_id(256, 256)), 0x8080);
}

#[test]
fn every_species_rasterises_to_a_standing_tree() {
    for (i, species) in trees::SPECIES.iter().enumerate() {
        for age in [5, 40, trees::MAX_AGE] {
            let sk = trees::skeleton(species, 0xABCD + i as u64, age);
            let bare = species.shape.leafless || species.single_block;
            let cells = stud_tree_cells(&sk, if bare { STUD_TREE_BARE_WOOD_LEVEL } else { STUD_TREE_WOOD_LEVEL });
            assert_eq!(cells, stud_tree_cells(&sk, if bare { STUD_TREE_BARE_WOOD_LEVEL } else { STUD_TREE_WOOD_LEVEL }));
            let name = species.name;
            assert!(cells.wood.iter().any(|c| c[1] == 0), "{name} age {age}: a trunk on the base plate");
            assert!(cells.wood.is_disjoint(&cells.leaves), "{name}: wood and leaves share a cell");
            if age == trees::MAX_AGE {
                assert_eq!(cells.leaves.is_empty(), bare, "{name}: leaves");
            }
            // Inside the species' box: the crown radius (+ the leaf reach) around the
            // base block, the height above it. Studs are half a block, plates a fifth.
            let reach = (species.crown_radius_blocks + trees::LEAF_REACH_PAST_CROWN_BLOCKS) * STUDS_PER_M + LEAF_CELL_STUDS;
            let top = species.max_height_blocks * PLATES_PER_M + LEAF_COURSE_PLATES.max(WOOD_COURSE_PLATES);
            for c in cells.wood.iter().chain(&cells.leaves) {
                assert!(c[1] >= 0 && c[1] < top, "{name} age {age}: cell {c:?} over {top} plates");
                assert!(
                    (-reach..STUDS_PER_M + reach).contains(&c[0]) && (-reach..STUDS_PER_M + reach).contains(&c[2]),
                    "{name} age {age}: cell {c:?} past the crown reach {reach}"
                );
            }
            // The trunk is one piece standing on the base (6-connected wood from y 0).
            let mut seen: BTreeSet<[i32; 3]> = cells.wood.iter().copied().filter(|c| c[1] == 0).collect();
            let mut stack: Vec<[i32; 3]> = seen.iter().copied().collect();
            while let Some(p) = stack.pop() {
                for d in [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]] {
                    let q = [p[0] + d[0], p[1] + d[1], p[2] + d[2]];
                    if cells.wood.contains(&q) && seen.insert(q) {
                        stack.push(q);
                    }
                }
            }
            let trunk_top = sk.nodes.iter().filter(|n| n.level == 0).map(|n| n.pos[1]).max().unwrap();
            let reached = seen.iter().map(|c| c[1]).max().unwrap();
            assert!(reached >= trunk_top * 5 / 16, "{name} age {age}: trunk broken at plate {reached}");
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_region_is_its_columns_plus_trees() {
    let mut trees_total = 0;
    for (rc, seed) in seeds() {
        let columns = generate_stud_columns(rc, 0..REGION_CHUNKS, 0..REGION_CHUNKS, SEED);
        assert!(seed.ground == columns, "{rc:?}: the wider tree patch changed a recipe");
        assert!(generate_stud_region(rc, SEED) == seed, "{rc:?}: deterministic");
        let ids: BTreeSet<u16> = seed.features.iter().flat_map(|(_, b)| b.iter().map(|b| b.data)).collect();
        trees_total += ids.len();
    }
    assert!(trees_total > 0, "no tree in {REGIONS:?}: pick regions with trees");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tree_bricks_are_valid_tagged_and_clear_of_the_ground() {
    for (rc, seed) in seeds() {
        let mut chunks = BTreeSet::new();
        let mut placed = 0;
        for (c, bricks) in &seed.features {
            assert!(chunks.insert(*c), "{rc:?}: chunk {c:?} listed twice");
            assert!(c.x < REGION_CHUNKS && c.z < REGION_CHUNKS, "{rc:?}: a tree left its region");
            // Valid, inside the chunk (never across an edge) and disjoint.
            StudChunk::new(bricks.iter().copied()).unwrap_or_else(|e| panic!("{rc:?} {c:?}: {e:?}"));
            for b in bricks {
                assert_ne!(b.data, 0);
                assert!(b.material == MAT_WOOD || b.material == MAT_LEAVES);
            }
            placed += bricks.len();
        }
        // None overlaps the pristine ground: every feature brick is in its seeded chunk.
        let seeded: usize =
            seed.feature_chunks().values().map(|c| c.bricks().filter(|b| b.data != 0).count()).sum();
        assert_eq!(seeded, placed, "{rc:?}: feature bricks dropped for overlapping the ground");
    }
}

#[test]
fn each_tree_stands_on_the_ground_within_its_crown() {
    for (rc, seed) in seeds() {
        let ground: BTreeMap<(usize, usize), _> = seed.ground.iter().map(|(c, g)| ((c.x, c.z), g)).collect();
        // Per tree id: its bricks' bounds, and its wood cells' lowest plate per column.
        let mut bounds: BTreeMap<u16, ([i32; 3], [i32; 3])> = BTreeMap::new();
        let mut feet: BTreeMap<u16, BTreeMap<(i32, i32), i32>> = BTreeMap::new();
        for (c, bricks) in &seed.features {
            for b in bricks {
                let (lo, hi) = brick_box(*c, b);
                let e = bounds.entry(b.data).or_insert((lo, hi));
                *e = ([0, 1, 2].map(|a| e.0[a].min(lo[a])), [0, 1, 2].map(|a| e.1[a].max(hi[a])));
                if b.material == MAT_WOOD {
                    for x in lo[0]..hi[0] {
                        for z in lo[2]..hi[2] {
                            let f = feet.entry(b.data).or_default().entry((x, z)).or_insert(lo[1]);
                            *f = (*f).min(lo[1]);
                        }
                    }
                }
            }
        }
        let widest = (2 * (trees::MAX_CROWN_RADIUS_BLOCKS + trees::LEAF_REACH_PAST_CROWN_BLOCKS) + 1) * STUDS_PER_M;
        for (id, (lo, hi)) in &bounds {
            assert!(hi[0] - lo[0] <= widest && hi[2] - lo[2] <= widest, "{rc:?}: id {id:#x} spans {lo:?}..{hi:?}: two trees?");
            assert!(hi[1] - lo[1] <= trees::MAX_TREE_HEIGHT_BLOCKS * PLATES_PER_M + 2 * PLATES_PER_M + LEAF_COURSE_PLATES);
            // Some wood column starts exactly on the ground's top plate.
            let n = STUD_CHUNK_STUDS as i32;
            let stands = feet.get(id).is_some_and(|f| {
                f.iter().any(|(&(x, z), &plate)| {
                    ground[&((x / n) as usize, (z / n) as usize)].surface((x % n) as usize, (z % n) as usize) == plate
                })
            });
            assert!(stands, "{rc:?}: tree {id:#x} does not stand on the ground");
        }
    }
}

#[test]
fn the_toy_world_has_no_trees() {
    for rc in [RegionCoords::new(0, 0), RegionCoords::new(1, 1)] {
        let seed = worldgen::stud_world::region_seed(rc);
        let tree = |b: &StudBrick| b.data != 0 || b.material == MAT_WOOD || b.material == MAT_LEAVES;
        assert!(!seed.features.iter().any(|(_, bricks)| bricks.iter().any(tree)), "{rc:?}");
    }
}

#[test]
#[ignore = "diagnostic: prints the tree brick counts of generated regions"]
fn tree_brick_census() {
    for (rc, seed) in seeds() {
        let t = std::time::Instant::now();
        let again = generate_stud_region(rc, SEED);
        let took = t.elapsed();
        let bricks: usize = again.features.iter().map(|(_, b)| b.len()).sum();
        let ids: BTreeSet<u16> = seed.features.iter().flat_map(|(_, b)| b.iter().map(|b| b.data)).collect();
        let ground: usize =
            seed.ground.iter().flat_map(|(_, g)| g.surface_layers().filter_map(|y| g.pristine_chunk(y))).map(|c| c.len()).sum();
        eprintln!(
            "{rc:?}: {} trees, {bricks} tree bricks in {} chunks ({} per tree); {ground} ground bricks; region in {took:?}",
            ids.len(),
            seed.features.len(),
            bricks / ids.len().max(1)
        );
    }
}

/// The sim's felling scan (`game::STUD_TREE_SCAN_TILES_*`) reaches every brick of
/// the widest and tallest species from any tile of it, and its brick bound holds the
/// biggest tree.
#[test]
fn trees_fit_the_felling_scan() {
    let widest = 2 * (trees::MAX_CROWN_RADIUS_BLOCKS + trees::LEAF_REACH_PAST_CROWN_BLOCKS) + 1;
    // Tiles are a block wide; a leaf cell may poke one stud past the crown's box.
    assert!(game::STUD_TREE_SCAN_TILES_H >= widest + 1, "{widest} blocks wide");
    let tallest_plates = trees::MAX_TREE_HEIGHT_BLOCKS * PLATES_PER_M + LEAF_COURSE_PLATES + 2 * PLATES_PER_M;
    assert!(game::STUD_TREE_SCAN_TILES_V * game::TILE_PLATES as i32 >= tallest_plates, "{tallest_plates} plates tall");
    for (i, species) in trees::SPECIES.iter().enumerate() {
        let sk = trees::skeleton(species, 77 + i as u64, trees::MAX_AGE);
        let bare = species.shape.leafless || species.single_block;
        let cells = stud_tree_cells(&sk, if bare { STUD_TREE_BARE_WOOD_LEVEL } else { STUD_TREE_WOOD_LEVEL });
        // Every cell its own brick is the worst case.
        let cells = cells.wood.len() / 3 + cells.leaves.len() / (LEAF_COURSE_PLATES * LEAF_CELL_STUDS * LEAF_CELL_STUDS) as usize;
        assert!(cells < game::MAX_STUD_TREE_BRICKS, "{}: {cells} bricks at worst", species.name);
    }
}

/// A generated tree in a seeded sim: its trunk tile takes a chop, and felling it at
/// the foot removes every brick of it and nothing else.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_generated_tree_is_felled_whole_at_its_foot() {
    let (rc, seed) = seeds().into_iter().find(|(_, s)| !s.features.is_empty()).expect("a region with trees");
    let ground: BTreeMap<(usize, usize), game::StudColumnGround> = seed.ground.iter().map(|(c, g)| ((c.x, c.z), g.clone())).collect();
    // The first tree's lowest wood brick standing on the ground: its foot.
    let n = STUD_CHUNK_STUDS as i32;
    let foot = seed
        .features
        .iter()
        .flat_map(|(c, bricks)| bricks.iter().map(move |b| (*c, b)))
        .filter(|(_, b)| b.material == MAT_WOOD)
        .map(|(c, b)| (b.data, brick_box(c, b).0))
        .find(|(_, lo)| ground[&((lo[0] / n) as usize, (lo[2] / n) as usize)].surface((lo[0] % n) as usize, (lo[2] % n) as usize) == lo[1])
        .expect("a tree stands on the ground");
    let (id, lo) = foot;
    let tile = [lo[0].div_euclid(game::TILE_STUDS as i32), lo[1].div_euclid(game::TILE_PLATES as i32), lo[2].div_euclid(game::TILE_STUDS as i32)];
    let count = |d: &game::Rollback, id: u16| {
        d.stud_index.keys().flat_map(|c| d.stud_chunk_at(*c).unwrap().bricks()).filter(|b| b.data == id).count()
    };
    let total_tagged = |d: &game::Rollback| {
        d.stud_index.keys().flat_map(|c| d.stud_chunk_at(*c).unwrap().bricks()).filter(|b| b.data != 0).count()
    };
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = game::Region::new(game::Rollback::new(Some(send.clone())), Some(send), rc, None);
    let bricks_of_tree: usize = seed.features.iter().flat_map(|(_, b)| b).filter(|b| b.data == id).count();
    region.seed_stud_world(seed);
    assert_eq!(count(region.data(), id), bricks_of_tree, "every brick of the tree was seeded");
    let others = total_tagged(region.data()) - bricks_of_tree;
    assert_eq!(region.data().stud_tree_at(tile), Some(id));
    assert_eq!(game::validate(region.data(), game::DesignationKind::Chop, tile), Ok(()));
    let before = game::state_hash(region.data());
    let mut felled = None;
    region.with_data(|d| felled = d.fell_stud_safe(tile, game::SettlePolicy::Despawn));
    let (cut, body) = felled.expect("the tree is felled");
    assert!(cut.iter().all(|(m, cells)| *m == MAT_WOOD && *cells > 0));
    assert_ne!(game::state_hash(region.data()), before);
    assert_eq!(count(region.data(), id), 0, "a chop at the foot leaves nothing of the tree");
    assert_eq!(total_tagged(region.data()), others, "the other trees are untouched");
    let body = body.expect("the rest of the tree fell as a body");
    let model = region.data().ecs.stud_body.try_get(body).clone().unwrap().model;
    assert!(model.bricks.len() < bricks_of_tree && model.bricks.len() + 16 >= bricks_of_tree, "all but the cut is in the body");
}

/// What a stud region's join snapshot (`ServerPacket::Region`) is made of, on the
/// wire and before compression (P10a: "6-10 MB a region" at 256 m).
#[test]
#[ignore = "diagnostic: prints the wire size breakdown of a stud region snapshot"]
fn stud_snapshot_census() {
    let mib = |n: u64| n as f64 / (1024.0 * 1024.0);
    macro_rules! size {
        ($v:expr) => {
            bincode::serialized_size($v).unwrap_or(0)
        };
    }
    for (rc, seed) in seeds() {
        let (send, _recv) = crossbeam::channel::unbounded();
        let mut region = game::Region::new(game::Rollback::new(Some(send.clone())), Some(send), rc, None);
        region.seed_stud_world(seed);
        let rollback = region.data();
        let data = &rollback.data;
        let chunks: Vec<&StudChunk> = data.ecs.stud.iter().filter_map(|(_, c)| c.as_ref()).collect();
        let bricks: usize = chunks.iter().map(|c| c.len()).sum();
        let lz4 = |v: &[u8]| game::encode_packet(&v.to_vec()).map_or(0, |w| w.len() as u64);
        let part = |name: &str, bytes: Vec<u8>| {
            eprintln!("    {:>8.3} MiB raw {:>8.3} MiB packed  {name}", mib(bytes.len() as u64), mib(lz4(&bytes)));
        };
        eprintln!("{rc:?}: {} stud chunks, {bricks} bricks; whole GameData {:.3} MiB raw", chunks.len(), mib(size!(&**data)));
        part("ecs.stud (the bricks)", bincode::serialize(&data.ecs.stud).unwrap());
        part("ecs (all components)", bincode::serialize(&data.ecs).unwrap());
        part("stud_ground (recipes)", bincode::serialize(&*data.stud_ground).unwrap());
        part("physics.colliders", bincode::serialize(&data.physics.colliders).unwrap());
        part("physics.bodies", bincode::serialize(&data.physics.bodies).unwrap());
        part("physics.broad_phase", bincode::serialize(&data.physics.broad_phase).unwrap());
        part("physics.narrow_phase", bincode::serialize(&data.physics.narrow_phase).unwrap());
        part("physics (all)", bincode::serialize(&data.physics).unwrap());
        // What "pristine chunks are never sent" (D1) would leave of the bricks: only the
        // chunks that differ from their recipe's pristine chunk (edited, or holding trees).
        let mut kept = Vec::new();
        let (mut pristine, mut differing, mut kept_bricks) = (0, 0, 0);
        for (c, _) in rollback.stud_index.iter() {
            let chunk = rollback.stud_chunk_at(*c).unwrap();
            let derived = data.stud_ground.get(&game::ColumnCoords::new(c.x, c.z)).and_then(|g| g.pristine_chunk(c.y));
            if derived.as_ref() == Some(chunk) {
                pristine += 1;
            } else {
                differing += 1;
                kept_bricks += chunk.len();
                kept.push(chunk.clone());
            }
        }
        eprintln!("    {pristine} chunks equal their pristine chunk, {differing} differ ({kept_bricks} bricks)");
        part("ecs.stud without pristine chunks", bincode::serialize(&kept).unwrap());
        let wire = game::encode_packet(&game::ServerPacket::Region(rc, rollback.clone())).expect("the packet encodes");
        eprintln!("    {:>8.3} MiB on the wire (ServerPacket::Region)", mib(wire.len() as u64));
    }
}
