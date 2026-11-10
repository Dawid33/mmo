//! The toy-brick test world: a value-noise height field as stud ground recipes.
//!
//! `region_seed` is a pure function of the region coordinates. Regions with
//! `|x| <= STUD_WORLD_RADIUS && |z| <= STUD_WORLD_RADIUS` get a ground recipe per chunk column
//! (the sim derives the bricks, `game::StudColumnGround::pristine_chunk`); every other region is
//! empty. The height field is f64 value noise (server-only, never run inside the sim), so all
//! that matters is that it is identical across calls.

use game::{
    box_part, fill_box, ColumnCoords, RegionCoords, StudBrick, StudChunkCoords, StudColumnGround, StudProfile,
    StudRegionSeed, REGION_CHUNKS, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS, STUD_NO_WATER,
};

/// Regions within this many regions of the origin (Chebyshev) are generated.
pub const STUD_WORLD_RADIUS: i32 = 1;
/// Lowest surface, in plates (2 m).
pub const MIN_SURFACE_PLATES: u8 = 10;
/// Highest surface, in plates (13 m).
pub const MAX_SURFACE_PLATES: u8 = 65;
pub const GRASS: u16 = game::MAT_GRASS;
pub const DIRT: u16 = game::MAT_DIRT;
pub const STONE: u16 = game::MAT_STONE;
/// The toy world's one material stack: a course of grass, two of dirt, stone below.
pub const PROFILE: StudProfile = StudProfile { cover: GRASS, cover_plates: 3, soil: DIRT, soil_plates: 6 };
/// Bedrock plate: 100 layers down.
pub const BEDROCK_PLATE: i32 = -100 * STUD_CHUNK_PLATES as i32;
const WALL_COLOR: u16 = 4;
const TOWER_COLOR: u16 = 1;
const COURSE: usize = 3;
const N: usize = STUD_CHUNK_STUDS;

/// Lattice spacing (studs) and weight of each noise octave.
const OCTAVES: [(i64, f64); 2] = [(48, 0.75), (16, 0.25)];

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn lattice(ix: i64, iz: i64, octave: u64) -> f64 {
    let h = splitmix64(splitmix64(ix as u64) ^ splitmix64((iz as u64).wrapping_add(0x1234_5678)) ^ splitmix64(octave + 77));
    (h >> 11) as f64 / (1u64 << 53) as f64
}

fn smooth(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

fn noise(wx: i64, wz: i64, octave: usize) -> f64 {
    let (spacing, _) = OCTAVES[octave];
    let (ix, iz) = (wx.div_euclid(spacing), wz.div_euclid(spacing));
    let fx = smooth(wx.rem_euclid(spacing) as f64 / spacing as f64);
    let fz = smooth(wz.rem_euclid(spacing) as f64 / spacing as f64);
    let o = octave as u64;
    let (a, b) = (lattice(ix, iz, o), lattice(ix + 1, iz, o));
    let (c, d) = (lattice(ix, iz + 1, o), lattice(ix + 1, iz + 1, o));
    let top = a + (b - a) * fx;
    let bot = c + (d - c) * fx;
    top + (bot - top) * fz
}

/// Surface height in whole plates at world stud column `(wx, wz)`.
pub fn surface_plates(wx: i64, wz: i64) -> u8 {
    let n: f64 = OCTAVES.iter().enumerate().map(|(i, &(_, w))| noise(wx, wz, i) * w).sum();
    let span = (MAX_SURFACE_PLATES - MIN_SURFACE_PLATES) as f64;
    let h = MIN_SURFACE_PLATES as f64 + n.clamp(0.0, 1.0) * span;
    (h.round() as u8).clamp(MIN_SURFACE_PLATES, MAX_SURFACE_PLATES)
}

/// A box part of the given footprint (studs x, plates, studs z) at `min`.
fn part(min: [usize; 3], size: [u8; 3], material: u16) -> StudBrick {
    let (part, rot) = box_part(size).unwrap_or_else(|| panic!("no box part of size {size:?} in the catalog"));
    StudBrick::part_at([min[0] as u8, min[1] as u8, min[2] as u8], part, rot, material)
}

/// The spawn-side wall and tower of chunk (0, 0) of region (0, 0), standing on the highest
/// surface under each footprint; lower footprint columns get a stone foundation up to it.
fn spawn_feature(heights: &[[usize; N]; N]) -> Vec<StudBrick> {
    let mut out = Vec::new();
    let footing = |out: &mut Vec<StudBrick>, xs: std::ops::Range<usize>, zs: std::ops::Range<usize>| {
        let base = xs.clone().flat_map(|x| zs.clone().map(move |z| heights[x][z])).max().unwrap();
        for x in xs {
            for z in zs.clone() {
                let h = heights[x][z];
                if h < base {
                    fill_box([x as u8, h as u8, z as u8], [1, (base - h) as u8, 1], STONE, 0, out);
                }
            }
        }
        base
    };
    let fit = |base: usize, courses: usize| courses.min((STUD_CHUNK_PLATES - base) / COURSE);

    // Wall: 1 stud thick along x at z = 24, x = 4..28, running bond.
    let base = footing(&mut out, 4..28, 24..25);
    for c in 0..fit(base, 6) {
        let y = base + c * COURSE;
        let mut x = 4;
        if c % 2 == 1 {
            out.push(part([x, y, 24], [2, 3, 1], WALL_COLOR));
            x += 2;
        }
        while x + 4 <= 28 {
            out.push(part([x, y, 24], [4, 3, 1], WALL_COLOR));
            x += 4;
        }
        if x < 28 {
            out.push(part([x, y, 24], [2, 3, 1], WALL_COLOR));
        }
    }

    // Tower: 4x4 studs at x = 26..30, z = 26..30, 8 courses, alternating 2x4 orientation.
    let base = footing(&mut out, 26..30, 26..30);
    for c in 0..fit(base, 8) {
        let y = base + c * COURSE;
        for off in [0, 2] {
            if c % 2 == 0 {
                out.push(part([26, y, 26 + off], [4, 3, 2], TOWER_COLOR));
            } else {
                out.push(part([26 + off, y, 26], [2, 3, 4], TOWER_COLOR));
            }
        }
    }
    out
}

/// What region `rc` starts from: a ground recipe per chunk column plus the spawn feature, or
/// nothing outside the test world.
pub fn region_seed(rc: RegionCoords) -> StudRegionSeed {
    let mut seed = StudRegionSeed::default();
    if rc.x.abs() > STUD_WORLD_RADIUS || rc.z.abs() > STUD_WORLD_RADIUS {
        return seed;
    }
    let region_studs = (REGION_CHUNKS * N) as i64;
    for cx in 0..REGION_CHUNKS {
        for cz in 0..REGION_CHUNKS {
            let (ox, oz) = (rc.x as i64 * region_studs + (cx * N) as i64, rc.z as i64 * region_studs + (cz * N) as i64);
            let mut heights = [[0usize; N]; N];
            let mut surface = vec![0i32; N * N];
            for x in 0..N {
                for z in 0..N {
                    let h = surface_plates(ox + x as i64, oz + z as i64);
                    heights[x][z] = h as usize;
                    surface[z * N + x] = h as i32;
                }
            }
            let ground = StudColumnGround::new(
                surface.into(),
                vec![STUD_NO_WATER; N * N].into(),
                vec![0; N * N].into(),
                vec![PROFILE],
                vec![0; N * N].into(),
                vec![(i32::MAX, STONE)],
                BEDROCK_PLATE,
            )
            .expect("stud_world recipes are valid by construction");
            seed.ground.push((ColumnCoords::new(cx, cz), ground));
            if (rc.x, rc.z, cx, cz) == (0, 0, 0, 0) {
                seed.features.push((StudChunkCoords { x: 0, y: 0, z: 0 }, spawn_feature(&heights)));
            }
        }
    }
    seed
}
