//! Shared by the structure tests (`structure_pass`, `structure_coarse`): real materials, test prefabs
//! resolved in memory, and reading generated regions back at world block positions. Procedural terrain
//! through the explicit `*_with` entries: nothing is installed.

use std::collections::BTreeMap;
use std::sync::Arc;

use game::prefab::*;
use game::{Block, BlockId, ChiselData, Column, ColumnCoords, RegionCoords, SubChunk, CHUNK_BLOCKS};
use worldgen::material::Materials;
use worldgen::pipeline::SEA_TOP_BLOCK;
use worldgen::structures::*;

pub const CB: i32 = CHUNK_BLOCKS as i32;
pub const RB: i32 = game::REGION_BLOCKS as i32;
pub const SEED: u64 = worldgen::WORLD_SEED;
pub const TREE_REACH: i32 =
    worldgen::trees::MAX_CROWN_RADIUS_BLOCKS + worldgen::trees::LEAF_REACH_PAST_CROWN_BLOCKS;

pub fn registry() -> game::BlockRegistry {
    game::BlockRegistry::from_ron(include_str!("../../../../assets/blocks/blocks.ron")).expect("valid manifest")
}

pub fn install_real_block_props() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| game::install_block_props(registry().props()));
}

pub fn mats() -> Materials {
    let reg = registry();
    Materials::from_names(|n| reg.id_of(n).unwrap_or_else(|| panic!("missing block {n}")))
}

pub fn default_dials() -> bool {
    std::env::var_os(worldgen::forged::WORLD_FILE_ENV).is_none_or(|v| v.is_empty())
}

/// A forested region near spawn (by world position), the same land at any region size.
pub fn forest() -> RegionCoords {
    RegionCoords::from_world(-1536.0, 2560.0)
}

pub fn air() -> Block {
    Block::new(BlockId::AIR)
}

/// Reading generated regions back at world block positions.
pub mod world {
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

    /// The highest stored block of column (x, z) that is neither air nor part of a tree, as a world y
    /// (the band always holds it): the column's terrain or structure top.
    pub fn top_untreed(r: &Regions, x: i32, z: i32) -> i32 {
        let (col, _, _) = column(r, x, z);
        let top = (col.base_sub_y() + col.sub_chunks().len() as i32) * CB - 1;
        (col.base_sub_y() * CB..=top)
            .rev()
            .find(|&y| stored(r, [x, y, z]).is_some_and(|(b, _)| b.id != BlockId::AIR && b.tree_id() == 0))
            .unwrap_or_else(|| panic!("column ({x}, {z}) stores no solid block"))
    }

    pub fn dry(r: &Regions, x: i32, z: i32) -> bool {
        let (col, lx, lz) = column(r, x, z);
        col.ground().unwrap().recipe_water(lx, lz).is_none() && ground(r, x, z) > SEA_TOP_BLOCK
    }

    /// Panics unless every column the placement's pad reaches (its box grown by the skirt) is dry ground
    /// in `r`: a test meant for land must not silently run on a lake bed or the sea floor.
    pub fn assert_dry(r: &Regions, sp: &StructurePlacement) {
        let [x0, z0, x1, z1] = sp.reach_xz();
        for x in x0..x1 {
            for z in z0..z1 {
                assert!(dry(r, x, z), "the site of {:?} is wet at ({x}, {z})", sp.at.anchor);
            }
        }
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
pub mod prefabs {
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

pub fn placed(p: &Arc<ResolvedPrefab>, anchor: [i32; 3], pad: Option<Pad>) -> StructurePlacement {
    StructurePlacement { prefab: p.clone(), at: Placement { anchor, turn: 0, mirror: false }, pad }
}

pub fn bytes<T: serde::Serialize>(v: &T) -> Vec<u8> {
    bincode::serialize(v).unwrap()
}

/// Chebyshev distance from column (x, z) to the placement's x/z box (0 inside).
pub fn box_distance(sp: &StructurePlacement, x: i32, z: i32) -> i32 {
    let (min, max) = sp.bounds();
    let gap = |v: i32, lo: i32, hi: i32| if v < lo { lo - v } else if v >= hi { v - (hi - 1) } else { 0 };
    gap(x, min[0], max[0]).max(gap(z, min[2], max[2]))
}
