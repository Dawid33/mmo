//! Generated slopes (spec `docs/superpowers/specs/2026-11-06-terrain-slopes-design.md`):
//! decided from samples only, so both sides of a region seam agree, and never
//! at or under water.
use std::collections::BTreeMap;

use game::{BlockIndex, Column, ColumnCoords, RegionCoords, Side, SlopeShape, SubChunk, CHUNK_BLOCKS, REGION_BLOCKS};
use worldgen::pipeline::SEA_TOP_BLOCK;

fn mats() -> worldgen::material::Materials {
    worldgen::material::Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
        trees: worldgen::material::TreeBlocks::NONE,
        biome: worldgen::material::BiomeBlocks::NONE,
    }
}

const RB: i32 = REGION_BLOCKS as i32;
const CB: i32 = CHUNK_BLOCKS as i32;

/// The generated columns of some regions, read by world block column.
struct Blocks(BTreeMap<(i32, i32, usize, usize), Column>);

impl Blocks {
    fn generate(regions: &[RegionCoords]) -> Self {
        let mut out = BTreeMap::new();
        for &rc in regions {
            for (cc, col) in worldgen::generate_region(rc, worldgen::WORLD_SEED, mats()) {
                out.insert((rc.x, rc.z, cc.x, cc.z), col);
            }
        }
        Blocks(out)
    }

    /// The column holding world block column `(x, z)`, and the block's
    /// footprint-local `(lx, lz)`.
    fn column(&self, x: i32, z: i32) -> (&Column, usize, usize) {
        let (rx, rz) = (x.div_euclid(RB), z.div_euclid(RB));
        let (bx, bz) = (x.rem_euclid(RB), z.rem_euclid(RB));
        let cc = ColumnCoords::new((bx / CB) as usize, (bz / CB) as usize);
        let col = self.0.get(&(rx, rz, cc.x, cc.z)).unwrap_or_else(|| panic!("({x}, {z}) not generated"));
        (col, (bx % CB) as usize, (bz % CB) as usize)
    }

    /// The column's surface block (its ground recipe's top).
    fn top(&self, x: i32, z: i32) -> i32 {
        let (col, lx, lz) = self.column(x, z);
        i32::from(col.ground().expect("grounded").surface[lx * CHUNK_BLOCKS + lz])
    }

    /// The slope emitted on `(x, z)`'s top block, if its chisel is one.
    fn emitted(&self, x: i32, z: i32) -> Option<SlopeShape> {
        let (col, lx, lz) = self.column(x, z);
        let y = self.top(x, z);
        let SubChunk::Dense(chunk) = col.sub_chunk_at(y.div_euclid(CB))? else { return None };
        let chisel = chunk.chisel.get(&BlockIndex::from_xyz(lx, y.rem_euclid(CB) as usize, lz))?;
        let shape = game::slope::classify(chisel);
        assert!(shape.is_some(), "a non-slope chisel on the surface at ({x}, {z})");
        shape
    }
}

/// The spec's placement table over the generated surfaces (procedural world:
/// the water top is the sea's, no glacier).
fn expected(b: &Blocks, x: i32, z: i32) -> Option<SlopeShape> {
    let t = b.top(x, z);
    if t <= SEA_TOP_BLOCK {
        return None;
    }
    let low = |[dx, dz]: [i32; 2]| b.top(x + dx, z + dz) == t - 1;
    let cards: Vec<Side> = [Side::PosX, Side::NegX, Side::PosZ, Side::NegZ].into_iter().filter(|s| low(s.offset())).collect();
    let is_x = |s: Side| matches!(s, Side::PosX | Side::NegX);
    match cards[..] {
        [low] => Some(SlopeShape::Straight { low }),
        [a, b] if is_x(a) != is_x(b) => Some(SlopeShape::Outer { low_x: a, low_z: b }),
        [] => {
            let diags: Vec<(Side, Side)> = [Side::PosX, Side::NegX]
                .into_iter()
                .flat_map(|sx| [Side::PosZ, Side::NegZ].map(|sz| (sx, sz)))
                .filter(|&(sx, sz)| low([sx.offset()[0], sz.offset()[1]]))
                .collect();
            match diags[..] {
                [(low_x, low_z)] => Some(SlopeShape::Inner { low_x, low_z }),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Both columns of every pair across the seam between regions (0,0) and
/// (1,0) carry exactly the slope the table gives from the surfaces on both
/// sides — each region decided its edge column from the other's samples.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn slopes_agree_across_a_region_seam() {
    let b = Blocks::generate(&[RegionCoords::new(0, 0), RegionCoords::new(1, 0)]);
    let mut slopes = 0;
    for z in 1..RB - 1 {
        for x in [RB - 1, RB] {
            let got = b.emitted(x, z);
            assert_eq!(got, expected(&b, x, z), "column ({x}, {z})");
            slopes += got.is_some() as usize;
        }
    }
    // The whole two regions, for a non-vacuous check.
    let mut all = 0;
    for x in 1..2 * RB - 1 {
        for z in 1..RB - 1 {
            let got = b.emitted(x, z);
            assert_eq!(got, expected(&b, x, z), "column ({x}, {z})");
            all += got.is_some() as usize;
        }
    }
    println!("slopes on the seam: {slopes}, in the two regions: {all}");
    assert!(all > 0, "the fixture has slopes");
}

/// Over a coastal region every slope stands above the water: its block is
/// above the sea's top block, and no water sits on it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_slope_at_or_under_water() {
    // Coast north-east of spawn: land, beach and open sea.
    let rc = RegionCoords::from_world(5632.0, -2560.0);
    let b = Blocks::generate(&[rc]);
    let (x0, z0) = (rc.x * RB, rc.z * RB);
    let (mut slopes, mut wet) = (0, 0);
    for x in x0..x0 + RB {
        for z in z0..z0 + RB {
            let t = b.top(x, z);
            wet += (t <= SEA_TOP_BLOCK) as usize;
            if b.emitted(x, z).is_some() {
                slopes += 1;
                assert!(t > SEA_TOP_BLOCK, "slope at block y {t} at ({x}, {z}), sea top {SEA_TOP_BLOCK}");
                let (col, lx, lz) = b.column(x, z);
                let recipe = col.ground().expect("a recipe").recipe_water(lx, lz);
                assert_eq!(recipe, None, "water on the slope at ({x}, {z})");
            }
        }
    }
    assert!(slopes > 0 && wet > 0, "a coast with slopes: {slopes} slopes, {wet} wet columns");
}
