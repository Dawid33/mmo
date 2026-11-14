//! The structure pass of generation: a placement's ground is levelled, trees keep away, the stored band
//! covers it, and its blocks land after terrain and trees — the same however the region is split into
//! windows, and with no structures exactly the terrain there was before. Procedural terrain through the
//! explicit `*_with` entries: nothing is installed.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use game::prefab::*;
use game::{Block, BlockId, ChiselData, Column, ColumnCoords, RegionCoords, SubChunk, CHUNK_BLOCKS, REGION_CHUNKS};
use worldgen::material::Materials;
use worldgen::pipeline::SEA_TOP_BLOCK;
use worldgen::structures::*;

const CB: i32 = CHUNK_BLOCKS as i32;
const RB: i32 = game::REGION_BLOCKS as i32;
const SEED: u64 = worldgen::WORLD_SEED;
const TREE_REACH: i32 = worldgen::trees::MAX_CROWN_RADIUS_BLOCKS + worldgen::trees::LEAF_REACH_PAST_CROWN_BLOCKS;

fn registry() -> game::BlockRegistry {
    game::BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).expect("valid manifest")
}

fn install_real_block_props() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| game::install_block_props(registry().props()));
}

fn mats() -> Materials {
    let reg = registry();
    Materials::from_names(|n| reg.id_of(n).unwrap_or_else(|| panic!("missing block {n}")))
}

fn default_dials() -> bool {
    std::env::var_os(worldgen::forged::WORLD_FILE_ENV).is_none_or(|v| v.is_empty())
}

/// A forested region near spawn (by world position), the same land at any region size.
fn forest() -> RegionCoords {
    RegionCoords::from_world(-1536.0, 2560.0)
}

fn air() -> Block {
    Block::new(BlockId::AIR)
}

/// Reading generated regions back at world block positions.
mod world {
    use super::*;

    pub type Regions = BTreeMap<(i32, i32), Vec<(ColumnCoords, Column)>>;

    pub fn generate(rcs: &[RegionCoords], src: &dyn StructureSource) -> Regions {
        rcs.iter().map(|&rc| ((rc.x, rc.z), worldgen::generate_region_with(rc, SEED, mats(), src))).collect()
    }

    /// The chunk column holding world column (x, z), and the column's footprint-local x/z.
    pub fn column(r: &Regions, x: i32, z: i32) -> (&Column, usize, usize) {
        let (rx, rz) = (x.div_euclid(RB), z.div_euclid(RB));
        let cols = r.get(&(rx, rz)).unwrap_or_else(|| panic!("region ({rx}, {rz}) not generated for ({x}, {z})"));
        let (lx, lz) = (x - rx * RB, z - rz * RB);
        let cc = ColumnCoords::new((lx / CB) as usize, (lz / CB) as usize);
        let col = &cols.iter().find(|(c, _)| *c == cc).unwrap().1;
        (col, (lx % CB) as usize, (lz % CB) as usize)
    }

    /// The stored block and chisel at world `p`; `None` below the band (implied ground). Above the band: air.
    pub fn stored(r: &Regions, [x, y, z]: [i32; 3]) -> Option<(Block, Option<ChiselData>)> {
        let (col, lx, lz) = column(r, x, z);
        let sub_y = y.div_euclid(CB);
        if sub_y < col.base_sub_y() {
            return None;
        }
        let ly = y.rem_euclid(CB) as usize;
        Some(match col.sub_chunk_at(sub_y) {
            Some(SubChunk::Dense(c)) => (c.block_at(lx, ly, lz), c.chisel.get(&game::BlockIndex::from_xyz(lx, ly, lz)).cloned()),
            _ => (air(), None),
        })
    }

    /// The block at world `p`, implied ground included.
    pub fn read(r: &Regions, p: [i32; 3]) -> (Block, Option<ChiselData>) {
        stored(r, p).unwrap_or_else(|| {
            let (col, lx, lz) = column(r, p[0], p[2]);
            (col.ground().unwrap().block_at(lx, lz, p[1]), None)
        })
    }

    /// The column's ground surface (`ColumnGround::surface`): its top terrain block.
    pub fn ground(r: &Regions, x: i32, z: i32) -> i32 {
        let (col, lx, lz) = column(r, x, z);
        i32::from(col.ground().unwrap().surface[lx * CHUNK_BLOCKS + lz])
    }

    pub fn dry(r: &Regions, x: i32, z: i32) -> bool {
        let (col, lx, lz) = column(r, x, z);
        col.ground().unwrap().recipe_water(lx, lz).is_none() && ground(r, x, z) > SEA_TOP_BLOCK
    }

    /// Every stored block with a tree id: world position → block.
    pub fn tree_blocks(r: &Regions) -> BTreeMap<[i32; 3], Block> {
        let mut out = BTreeMap::new();
        for (&(rx, rz), cols) in r {
            for (cc, col) in cols {
                let (x0, z0) = (rx * RB + cc.x as i32 * CB, rz * RB + cc.z as i32 * CB);
                for (sub_y, sub) in (col.base_sub_y()..).zip(col.sub_chunks()) {
                    let SubChunk::Dense(chunk) = sub else { continue };
                    for (bi, block) in chunk.blocks.iter() {
                        if block.tree_id() != 0 {
                            let (bx, by, bz) = bi.xyz();
                            out.insert([x0 + bx as i32, sub_y * CB + by as i32, z0 + bz as i32], block);
                        }
                    }
                }
            }
        }
        out
    }

    /// The anchor (x, z) of a `w × d` box whose box grown by `skirt` is all dry, the box inside
    /// `[x0, x1) × [z0, z1)`; the first found scanning outward from the rectangle's middle.
    pub fn dry_site(r: &Regions, [x0, z0, x1, z1]: [i32; 4], w: i32, d: i32, skirt: i32) -> Option<(i32, i32)> {
        let (mx, mz) = ((x0 + x1 - w) / 2, (z0 + z1 - d) / 2);
        let mut cands: Vec<(i32, i32)> = (x0..=x1 - w).flat_map(|x| (z0..=z1 - d).map(move |z| (x, z))).collect();
        cands.sort_by_key(|&(x, z)| ((x - mx).abs().max((z - mz).abs()), x, z));
        cands.into_iter().find(|&(ax, az)| {
            (ax - skirt..ax + w + skirt).all(|x| (az - skirt..az + d + skirt).all(|z| dry(r, x, z)))
        })
    }
}

/// Test prefabs, resolved in memory from `PrefabFile`s with real block names.
mod prefabs {
    use super::*;

    pub fn resolve_file(f: PrefabFile) -> Arc<ResolvedPrefab> {
        let reg = registry();
        Arc::new(resolve(&f, &|n| reg.id_of(n), Trust::Builtin).expect("resolves"))
    }

    pub fn file(name: &str, size: [u32; 3], anchor: [i32; 3]) -> PrefabFile {
        PrefabFile {
            format: PREFAB_FORMAT,
            name: name.into(),
            meta: PrefabMeta::default(),
            size,
            anchor,
            palette: vec!["stone_bricks".into(), "oak_planks".into()],
            boxes: vec![],
            shaped: vec![],
            carve: vec![],
            markers: vec![],
            ground: GroundFit::Flat,
            tags: vec![],
        }
    }

    pub const BRICK: u16 = 0;
    pub const PLANK: u16 = 1;

    /// A `w × h × d` hut: a stone-brick floor (cell y 0), plank walls, a hollow carved inside, and a roof
    /// (cell y h - 1) of planks ringed by stairs facing out.
    pub fn hut(w: u32, h: u32, d: u32) -> Arc<ResolvedPrefab> {
        let mut f = file("hut", [w, h, d], [0, 0, 0]);
        let wall = h - 2;
        f.boxes = vec![
            FileBox { min: [0, 0, 0], size: [w, 1, d], mat: BRICK },
            FileBox { min: [0, 1, 0], size: [w, wall, 1], mat: PLANK },
            FileBox { min: [0, 1, d as i32 - 1], size: [w, wall, 1], mat: PLANK },
            FileBox { min: [0, 1, 1], size: [1, wall, d - 2], mat: PLANK },
            FileBox { min: [w as i32 - 1, 1, 1], size: [1, wall, d - 2], mat: PLANK },
            FileBox { min: [1, h as i32 - 1, 1], size: [w - 2, 1, d - 2], mat: PLANK },
        ];
        f.carve = vec![FileCarve { min: [1, 1, 1], size: [w - 2, wall, d - 2] }];
        let top = h as i32 - 1;
        let stairs = |facing| FileShape::Named {
            shape: Shape::Stairs { facing, half: Half::Bottom, corner: StairCorner::Straight },
            mat: PLANK,
        };
        for x in 0..w as i32 {
            f.shaped.push(FileShaped { at: [x, top, 0], shape: stairs(Facing::South) });
            f.shaped.push(FileShaped { at: [x, top, d as i32 - 1], shape: stairs(Facing::North) });
        }
        for z in 1..d as i32 - 1 {
            f.shaped.push(FileShaped { at: [0, top, z], shape: stairs(Facing::East) });
            f.shaped.push(FileShaped { at: [w as i32 - 1, top, z], shape: stairs(Facing::West) });
        }
        resolve_file(f)
    }

    /// A solid box of one material.
    pub fn solid(name: &str, size: [u32; 3], anchor: [i32; 3], mat: u16) -> Arc<ResolvedPrefab> {
        let mut f = file(name, size, anchor);
        f.boxes = vec![FileBox { min: [0, 0, 0], size, mat }];
        resolve_file(f)
    }

    /// Brute force: every non-Keep world cell of the placement → `None` (carve) or its write.
    pub fn expected(p: &ResolvedPrefab, at: &Placement) -> BTreeMap<[i32; 3], Option<(Block, Option<ChiselData>)>> {
        let mut out = BTreeMap::new();
        for ch in chunks_touched(p, at) {
            let Some(stamp) = layout_chunk(p, at, ch) else { continue };
            let world = |bi: game::BlockIndex| {
                let (x, y, z) = bi.xyz();
                [ch[0] * CB + x as i32, ch[1] * CB + y as i32, ch[2] * CB + z as i32]
            };
            for bi in stamp.carve {
                out.insert(world(bi), None);
            }
            for (bi, b, c) in stamp.writes {
                out.insert(world(bi), Some((b, c)));
            }
        }
        out
    }
}

use world::Regions;

fn placed(p: &Arc<ResolvedPrefab>, anchor: [i32; 3], pad: Option<Pad>) -> StructurePlacement {
    StructurePlacement { prefab: p.clone(), at: Placement { anchor, turn: 0, mirror: false }, pad }
}

fn bytes<T: serde::Serialize>(v: &T) -> Vec<u8> {
    bincode::serialize(v).unwrap()
}

/// Every cell of `expected` reads back as its write (a carve as plain air).
fn assert_cells(r: &Regions, sp: &StructurePlacement) {
    let want = prefabs::expected(&sp.prefab, &sp.at);
    assert!(!want.is_empty());
    for (p, w) in want {
        let w = w.unwrap_or((air(), None));
        assert_eq!(world::read(r, p), w, "prefab cell {p:?}");
    }
}

fn box_distance(sp: &StructurePlacement, x: i32, z: i32) -> i32 {
    let (min, max) = sp.bounds();
    let gap = |v: i32, lo: i32, hi: i32| if v < lo { lo - v } else if v >= hi { v - (hi - 1) } else { 0 };
    gap(x, min[0], max[0]).max(gap(z, min[2], max[2]))
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_source_is_byte_identical() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let half = REGION_CHUNKS / 2;
    for rc in [forest(), RegionCoords::new(0, 0), RegionCoords::new(-3, -2)] {
        let a = worldgen::generate_region_with(rc, SEED, mats(), &NoStructures);
        assert!(bytes(&a) == bytes(&worldgen::generate_region(rc, SEED, mats())), "region {rc:?}");
        let far = StructureList::new(
            vec![placed(&prefabs::hut(7, 5, 7), [rc.x * RB - 400, 60, rc.z * RB - 400], Some(Pad { level: 60, skirt: 4 }))],
            1,
        );
        assert!(bytes(&a) == bytes(&worldgen::generate_region_with(rc, SEED, mats(), &far)), "far list {rc:?}");
        let w = worldgen::generate_columns_with(rc, 0..half, 0..REGION_CHUNKS, SEED, mats(), &NoStructures);
        assert!(bytes(&w) == bytes(&worldgen::generate_columns(rc, 0..half, 0..REGION_CHUNKS, SEED, mats())), "window {rc:?}");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_hut_stands_on_its_pad() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    let (w, h, d, skirt) = (7, 5, 7, 4);
    let (r0x, r0z) = (rc.x * RB, rc.z * RB);
    let (ax, az) = world::dry_site(&bare, [r0x + skirt, r0z + skirt, r0x + RB - skirt, r0z + RB - skirt], w, d, skirt)
        .expect("a dry site");
    let level = world::ground(&bare, ax + w / 2, az + d / 2);
    let sp = placed(&prefabs::hut(w as u32, h as u32, d as u32), [ax, level + 1, az], Some(Pad { level, skirt: skirt as u32 }));
    let list = StructureList::new(vec![sp.clone()], 1);
    let r = world::generate(&[rc], &list);
    assert_cells(&r, &sp);
    let (min, max) = sp.bounds();
    let cells = prefabs::expected(&sp.prefab, &sp.at);
    for x in min[0] - skirt..max[0] + skirt {
        for z in min[2] - skirt..max[2] + skirt {
            let natural = world::ground(&bare, x, z);
            let surface = match pad_at(&[&sp], x, z, natural) {
                PadEffect::Level { surface, .. } => surface,
                PadEffect::None => panic!("({x}, {z}) is in the skirt"),
            };
            assert_eq!(world::ground(&r, x, z), surface, "pad surface at ({x}, {z})");
            assert_ne!(world::read(&r, [x, surface, z]).0.id, BlockId::AIR, "top terrain block at ({x}, {z})");
            if box_distance(&sp, x, z) > 0 {
                continue;
            }
            assert_eq!(surface, level);
            for y in surface + 1..max[1] + 8 {
                if !cells.contains_key(&[x, y, z]) {
                    assert_eq!(world::read(&r, [x, y, z]), (air(), None), "above the ground at ({x}, {y}, {z})");
                }
            }
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn window_independence_with_a_structure() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let half = REGION_CHUNKS / 2;
    let (mx, mz) = (rc.x * RB + half as i32 * CB, rc.z * RB + half as i32 * CB);
    let bare = world::generate(&[rc], &NoStructures);
    let level = world::ground(&bare, mx, mz);
    // A hut straddling the middle on both axes, its box and skirt crossing the window cuts.
    let sp = placed(&prefabs::hut(9, 6, 9), [mx - 4, level + 1, mz - 4], Some(Pad { level, skirt: 6 }));
    let list = StructureList::new(vec![sp.clone()], 1);
    let whole = worldgen::generate_region_with(rc, SEED, mats(), &list);
    let by_col: BTreeMap<_, _> = whole.iter().map(|(cc, col)| ((cc.x, cc.z), bytes(col))).collect();
    let (min, _) = sp.bounds();
    let corner = (((min[0] - rc.x * RB) / CB) as usize, ((min[2] - rc.z * RB) / CB) as usize);
    let all = 0..REGION_CHUNKS;
    let windows = [
        (0..half, all.clone()),
        (half..REGION_CHUNKS, all.clone()),
        (all.clone(), 0..half),
        (corner.0..corner.0 + 1, corner.1..corner.1 + 1),
    ];
    for (xs, zs) in windows {
        let part = worldgen::generate_columns_with(rc, xs.clone(), zs.clone(), SEED, mats(), &list);
        assert_eq!(part.len(), xs.len() * zs.len());
        for (cc, col) in &part {
            assert!(by_col[&(cc.x, cc.z)] == bytes(col), "column {cc:?} of window {xs:?} x {zs:?}");
        }
    }
    // The structure is really there.
    let r: Regions = [((rc.x, rc.z), whole)].into_iter().collect();
    assert_cells(&r, &sp);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_structure_crosses_a_region_corner_whole() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let c = forest();
    let rcs: Vec<RegionCoords> =
        [(-1, -1), (0, -1), (-1, 0), (0, 0)].map(|(dx, dz)| RegionCoords::new(c.x + dx, c.z + dz)).to_vec();
    let (cx, cz) = (c.x * RB, c.z * RB);
    let bare = world::generate(&rcs, &NoStructures);
    let level = world::ground(&bare, cx, cz);
    let sp = placed(&prefabs::hut(20, 6, 20), [cx - 10, level + 1, cz - 10], Some(Pad { level, skirt: 4 }));
    let r = world::generate(&rcs, &StructureList::new(vec![sp.clone()], 1));
    let (min, max) = sp.bounds();
    let mut regions = BTreeSet::new();
    for x in min[0]..max[0] {
        for z in min[2]..max[2] {
            regions.insert((x.div_euclid(RB), z.div_euclid(RB)));
        }
    }
    assert_eq!(regions.len(), 4, "the box spans four regions");
    assert_cells(&r, &sp);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn trees_stay_out() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    // The forest region and its east neighbour; the hut near their shared edge's west side, so trees far
    // from it exist on both sides.
    let rcs = [rc, RegionCoords::new(rc.x + 1, rc.z)];
    let bare = world::generate(&rcs, &NoStructures);
    let (ax, az) = (rc.x * RB + RB / 2, rc.z * RB + RB / 2 - 3);
    let level = world::ground(&bare, ax + 3, az + 3);
    let sp = placed(&prefabs::hut(7, 5, 7), [ax, level + 1, az], Some(Pad { level, skirt: 4 }));
    let r = world::generate(&rcs, &StructureList::new(vec![sp.clone()], 1));
    let with = world::tree_blocks(&r);
    let without = world::tree_blocks(&bare);
    // Nothing of a tree in the box, at or above the pad.
    for (p, b) in &with {
        assert!(box_distance(&sp, p[0], p[2]) > 0 || p[1] < level, "tree block {b:?} at {p:?} in the box");
    }
    // No tree stands within the keep-out: a tree's lowest block (its trunk base) is beyond it.
    let mut base: BTreeMap<u32, [i32; 3]> = BTreeMap::new();
    for (p, b) in &with {
        let e = base.entry(b.tree_id()).or_insert(*p);
        if p[1] < e[1] {
            *e = *p;
        }
    }
    for (id, p) in &base {
        assert!(box_distance(&sp, p[0], p[2]) > TREE_REACH, "tree {id} stands at {p:?}");
    }
    // Far from the structure the trees are untouched.
    let bound = TREE_REACH + 2 * worldgen::trees::TREE_LATTICE_BLOCKS + MAX_PAD_SKIRT as i32;
    let far = |m: &BTreeMap<[i32; 3], Block>| -> BTreeMap<[i32; 3], Block> {
        m.iter().filter(|(p, _)| box_distance(&sp, p[0], p[2]) > bound).map(|(p, b)| (*p, *b)).collect()
    };
    let (a, b) = (far(&with), far(&without));
    assert!(!b.is_empty(), "no trees far from the structure to compare");
    assert!(a == b, "trees far from the structure changed");
    assert!(without.len() > with.len(), "the structure removed no tree");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_cellar_has_real_walls() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    // The box and its carve start on a chunk column edge in x and z, so the walls there lie in the
    // neighbouring chunk columns, which the box itself does not reach.
    let (ex, ez) = (rc.x * RB + 2 * CB, rc.z * RB + 2 * CB);
    // The lowest natural ground of the chunk columns around that corner: a pad at or under it leaves
    // their bands' own bottoms at `level - MAX_SOIL_DEPTH` or above.
    let low = (ex - CB..ex + CB).flat_map(|x| (ez - CB..ez + CB).map(move |z| (x, z)));
    let low = low.map(|(x, z)| world::ground(&bare, x, z)).min().unwrap();
    // Two pad heights against the chunk grid: the floor's row (level - 5) just under a chunk edge the
    // neighbours' natural bands stop at, and the floor on a chunk edge with the row under it in the
    // chunk below.
    for phase in [3, 5] {
        let level = low - (low - phase).rem_euclid(CB);
        // 6 × 9 × 6, anchor cell (0, 6, 0): a cellar carved in cells y 0..=6, x/z 0..=3; plank walls
        // above on the far sides.
        let mut f = prefabs::file("cellar", [6, 9, 6], [0, 6, 0]);
        f.carve = vec![FileCarve { min: [0, 0, 0], size: [4, 7, 4] }];
        f.boxes = vec![
            FileBox { min: [0, 7, 5], size: [6, 2, 1], mat: prefabs::PLANK },
            FileBox { min: [5, 7, 0], size: [1, 2, 5], mat: prefabs::PLANK },
        ];
        let sp = placed(&prefabs::resolve_file(f), [ex, level + 1, ez], Some(Pad { level, skirt: 4 }));
        let r = world::generate(&[rc], &StructureList::new(vec![sp.clone()], 1));
        assert_cells(&r, &sp);
        let cells = prefabs::expected(&sp.prefab, &sp.at);
        let carved: BTreeSet<[i32; 3]> = cells.iter().filter(|(_, w)| w.is_none()).map(|(p, _)| *p).collect();
        let (min, max) = sp.bounds();
        assert_eq!(min[1], level - 5, "the cellar floor is 6 below the anchor");
        let mut checked = 0;
        for p in &carved {
            for d in [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]] {
                let n = [p[0] + d[0], p[1] + d[1], p[2] + d[2]];
                if carved.contains(&n) || cells.contains_key(&n) || n[1] > level {
                    continue;
                }
                let got =
                    world::stored(&r, n).unwrap_or_else(|| panic!("wall cell {n:?} beside {p:?} is implied ground"));
                assert_ne!(got.0.id, BlockId::AIR, "wall cell {n:?} beside {p:?}");
                checked += 1;
            }
        }
        assert!(checked > 0);
        // Every chunk column within one block of the box stores down to below the cellar floor.
        for x in min[0] - 1..=max[0] {
            for z in min[2] - 1..=max[2] {
                let (col, _, _) = world::column(&r, x, z);
                assert!(col.base_sub_y() * CB < min[1], "column at ({x}, {z}): band base {}", col.base_sub_y() * CB);
            }
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn later_placements_override() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    let (ax, az) = (rc.x * RB + RB / 2 - 4, rc.z * RB + RB / 2 - 4);
    let level = world::ground(&bare, ax, az);
    let pad = Some(Pad { level, skirt: 3 });
    let a = placed(&prefabs::solid("a", [6, 3, 6], [0, 0, 0], prefabs::BRICK), [ax, level + 1, az], pad);
    let b = placed(&prefabs::solid("b", [6, 3, 6], [0, 0, 0], prefabs::PLANK), [ax + 3, level + 2, az + 2], pad);
    let r = world::generate(&[rc], &StructureList::new(vec![a.clone(), b.clone()], 1));
    let (ea, eb) = (prefabs::expected(&a.prefab, &a.at), prefabs::expected(&b.prefab, &b.at));
    let mut overlap = 0;
    for (p, w) in &ea {
        let want = eb.get(p).unwrap_or(w).clone().unwrap();
        overlap += usize::from(eb.contains_key(p));
        assert_eq!(world::read(&r, *p), want, "cell {p:?}");
    }
    assert!(overlap > 0);
    assert_cells(&r, &b);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn nothing_below_bedrock() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    let (ax, az) = (rc.x * RB + RB / 2, rc.z * RB + RB / 2);
    let level = world::ground(&bare, ax, az);
    // 5 × 85 × 5 of planks, anchored at its top cell: 84 blocks reach below the anchor.
    let sp = placed(&prefabs::solid("deep", [5, 85, 5], [0, 84, 0], prefabs::PLANK), [ax, level, az], Some(Pad { level, skirt: 2 }));
    let r = world::generate(&[rc], &StructureList::new(vec![sp.clone()], 1));
    let plank = registry().id_of("oak_planks").unwrap();
    let bedrock = mats().bedrock;
    let (min, max) = sp.bounds();
    assert!(min[1] < level - game::UNDERGROUND_DEPTH - 10);
    for x in min[0]..max[0] {
        for z in min[2]..max[2] {
            assert_eq!(world::ground(&r, x, z), level);
            for y in min[1]..max[1] {
                let got = world::read(&r, [x, y, z]);
                if y <= level - game::UNDERGROUND_DEPTH {
                    assert_eq!(got.0.id, bedrock, "({x}, {y}, {z}) at or below bedrock");
                } else {
                    // A plain block: a slope's chisel on the ground's top block went with it.
                    assert_eq!(got, (Block::new(plank), None), "({x}, {y}, {z}) above bedrock");
                }
            }
        }
    }
}
