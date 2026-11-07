//! Tiled forged worlds (`worldgen::forged_tiles`, forged-world-in-db plan
//! Task 2): a world split into tiles and sampled through the tile cache
//! returns bit-for-bit what the whole-raster world returns, at every point —
//! on tile edges and corners, along rivers that cross tiles, and past the
//! map's land and sea edges.
//!
//! Fixture: a real forge (4,096 m / 16 m, seed 4 — the Task 1 guard's world)
//! split into 32-cell tiles (512 m, 8 x 8 = 64 tiles) and served from memory.
//! Every assertion compares `to_bits()`. Only explicit worlds — nothing
//! process-global — so this file lives in the shared `it` binary.

use std::sync::{Arc, OnceLock};

use game::RegionCoords;
use worldgen::forged::ForgedWorld;
use worldgen::forged_tiles::{
    decode_meta, decode_tile, encode_meta, encode_tile, split, ForgedMeta, MemTiles, Tile,
};

/// Tile edge of the fixture (cells): 512 m at 16 m cells, a multiple of the
/// 64 m river bucket.
const FIXTURE_TILE_CELLS: u32 = 32;

struct Fixture {
    whole: ForgedWorld,
    tiled: ForgedWorld,
    meta: ForgedMeta,
    tiles: Vec<Tile>,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let p = worldgen::forge::ForgeParams { size_m: 4096.0, cell_m: 16.0, seed: 4, ..Default::default() };
        let whole = ForgedWorld::new(worldgen::forge::forge(&p).artifact(&p));
        let (meta, tiles) = split(&whole, FIXTURE_TILE_CELLS);
        let mem = MemTiles(tiles.iter().map(|t| (t.origin, encode_tile(t))).collect());
        let tiled = ForgedWorld::from_tiles(meta.clone(), Arc::new(mem), whole.content_crc()).expect("tiled world");
        assert!(tiled.artifact().is_none(), "a tiled world holds no artifact");
        Fixture { whole, tiled, meta, tiles }
    })
}

fn materials() -> worldgen::material::Materials {
    let manifest = include_str!("../../../assets/blocks/blocks.ron");
    let reg = game::BlockRegistry::from_ron(manifest).expect("valid manifest");
    worldgen::material::Materials::from_names(|n| reg.id_of(n).unwrap_or_else(|| panic!("missing block {n}")))
}

/// Every point sampler's output at engine point `(x, z)`, as bits.
fn point_bits(w: &ForgedWorld, x: f64, z: f64) -> Vec<u64> {
    let mut v = Vec::with_capacity(24);
    let s = w.sample(x, z);
    v.push(s.ground_y.to_bits());
    v.push(s.water_y.map_or(u64::MAX, f64::to_bits));
    let c = w.column_climate(x, z);
    v.extend([c.biome as u64, c.salt_flat as u64, c.snow_patch as u64]);
    v.extend([c.snow_line_m, c.temperature_c, c.glacier_share].map(|f| u64::from(f.to_bits())));
    v.push(w.biome_at(x, z) as u64);
    v.push(w.biome_cell(x, z) as u64);
    let (id, kind) = w.region_cell(x, z);
    v.push(u64::from(id));
    v.push(kind.map_or(u64::MAX, |k| k as u64));
    let cc = w.cell_climate(x, z);
    v.extend([cc.height_m, cc.temperature_c, cc.precip_mm].map(|f| u64::from(f.to_bits())));
    v.extend([u64::from(cc.drainage), u64::from(cc.salinity), cc.is_lake as u64]);
    v.push(u64::from(w.snow_line_m(x, z).to_bits()));
    v.push(w.geo_offset_blocks(x, z) as u16 as u64);
    v.push(w.surface_rock(x, z).map_or(u64::MAX, u64::from));
    v
}

fn assert_point(f: &Fixture, x: f64, z: f64, what: &str) {
    let (a, b) = (point_bits(&f.whole, x, z), point_bits(&f.tiled, x, z));
    assert_eq!(a, b, "{what}: tiled differs from whole at engine ({x}, {z})");
}

fn assert_cell(f: &Fixture, ix: i64, iz: i64) {
    let (a, b) = (f.whole.extended_cell_height(ix, iz), f.tiled.extended_cell_height(ix, iz));
    assert_eq!(a.to_bits(), b.to_bits(), "extended_cell_height({ix}, {iz})");
}

/// Artifact metres -> engine metres.
fn engine(f: &Fixture, ax: f64, az: f64) -> (f64, f64) {
    let (min, _) = f.whole.artifact_bounds_engine_m();
    (min[0] + ax, min[1] + az)
}

fn assert_region(f: &Fixture, r: RegionCoords, mats: worldgen::material::Materials) {
    let a = bincode::serialize(&worldgen::generate_region_forged(&f.whole, r, mats)).unwrap();
    let b = bincode::serialize(&worldgen::generate_region_forged(&f.tiled, r, mats)).unwrap();
    assert!(a == b, "generate_region_forged({r:?}): tiled differs from whole");
}

fn assert_lod(f: &Fixture, key: &game::lod::LodKey, mats: worldgen::material::Materials) {
    let cells = |w: &ForgedWorld| {
        worldgen::coarse_tile_forged(w, key, mats)
            .into_iter()
            .map(|c| bincode::serialize(&(c.group, &c.grid)).unwrap())
            .collect::<Vec<_>>()
    };
    assert!(cells(&f.whole) == cells(&f.tiled), "coarse_tile_forged({key:?})");
    let water = |w: &ForgedWorld| bincode::serialize(&worldgen::coarse_water_forged(w, key, mats)).unwrap();
    assert!(water(&f.whole) == water(&f.tiled), "coarse_water_forged({key:?})");
}

/// The Task 1 guard's grid and samplers (7 m stride, the map and 200 m past
/// it), every raster cell and 200 m past, every region of the map and its
/// rim, and a factor-16 / factor-64 LOD sweep.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tiled_equals_whole_everywhere() {
    let f = fixture();
    let (min, max) = f.whole.artifact_bounds_engine_m();
    const OUTSIDE_M: f64 = 200.0;
    const STRIDE_M: f64 = 7.0;
    let steps = |lo: f64, hi: f64| ((hi - lo + 2.0 * OUTSIDE_M) / STRIDE_M).ceil() as i64;
    let (nx, nz) = (steps(min[0], max[0]), steps(min[1], max[1]));
    for kz in 0..=nz {
        let z = min[1] - OUTSIDE_M + kz as f64 * STRIDE_M;
        for kx in 0..=nx {
            let x = min[0] - OUTSIDE_M + kx as f64 * STRIDE_M;
            assert_point(f, x, z, "grid");
        }
    }
    let n = i64::from(f.whole.globals().n);
    let pad = (OUTSIDE_M / f.whole.globals().cell_m).ceil() as i64;
    for iz in -pad..n + pad {
        for ix in -pad..n + pad {
            assert_cell(f, ix, iz);
        }
    }
    // Every region touching the map, plus a one-region rim, and LOD over
    // it at two factors (per-region keys and tiles); spread over threads.
    let mats = materials();
    let rb = game::REGION_BLOCKS as f64;
    let (r0x, r0z) = ((min[0] / rb).floor() as i32 - 1, (min[1] / rb).floor() as i32 - 1);
    let (r1x, r1z) = (((max[0] - 1.0) / rb).floor() as i32 + 1, ((max[1] - 1.0) / rb).floor() as i32 + 1);
    let regions: Vec<RegionCoords> =
        (r0z..=r1z).flat_map(|rz| (r0x..=r1x).map(move |rx| RegionCoords::new(rx, rz))).collect();
    let keys: std::collections::BTreeSet<game::lod::LodKey> = [16, 64]
        .into_iter()
        .flat_map(|factor| regions.iter().map(move |&r| game::lod::LodKey::for_region(factor, r)))
        .collect();
    let keys: Vec<_> = keys.into_iter().collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let jobs = regions.len() + keys.len();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let j = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if j >= jobs {
                        break;
                    }
                    match regions.get(j) {
                        Some(&r) => assert_region(f, r, mats),
                        None => assert_lod(f, &keys[j - regions.len()], mats),
                    }
                }
            });
        }
    });
    println!("tiled == whole: {} grid points, {} regions, {} LOD keys", (nx + 1) * (nz + 1), regions.len(), keys.len());
}

/// Points on every tile edge line and corner, exactly and 1e-9 m and half a
/// cell to either side, and the raster cells `k * TILE - 2 ..= k * TILE + 1`
/// across every tile seam.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tile_edges_and_corners_bit_equal() {
    let f = fixture();
    let g = f.whole.globals();
    let tile_m = f64::from(FIXTURE_TILE_CELLS) * g.cell_m;
    let tiles = g.n.div_ceil(FIXTURE_TILE_CELLS);
    let offsets = [-0.5 * g.cell_m, -1e-9, 0.0, 1e-9, 0.5 * g.cell_m];
    const ALONG_STRIDE_M: f64 = 3.0;
    let along = (g.size_m / ALONG_STRIDE_M) as i64;
    for k in 0..=tiles {
        let edge = f64::from(k) * tile_m;
        for &o in &offsets {
            for j in 0..=along {
                let t = j as f64 * ALONG_STRIDE_M;
                let (x, z) = engine(f, edge + o, t);
                assert_point(f, x, z, "vertical tile edge");
                let (x, z) = engine(f, t, edge + o);
                assert_point(f, x, z, "horizontal tile edge");
            }
        }
        for kz in 0..=tiles {
            let edge_z = f64::from(kz) * tile_m;
            for &ox in &offsets {
                for &oz in &offsets {
                    let (x, z) = engine(f, edge + ox, edge_z + oz);
                    assert_point(f, x, z, "tile corner");
                }
            }
        }
    }
    let n = i64::from(g.n);
    let seam = i64::from(FIXTURE_TILE_CELLS);
    for k in 0..=i64::from(tiles) {
        for d in -2..=1 {
            let c = k * seam + d;
            for other in 0..n {
                assert_cell(f, c, other);
                assert_cell(f, other, c);
            }
        }
    }
}

/// Every river segment whose ends lie in different tiles, sampled along it
/// and across it (centreline, channel, levee, bank): the carving and the
/// nearest-segment choice equal the whole world's.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn rivers_across_tiles_bit_equal() {
    let f = fixture();
    let g = f.whole.globals();
    let tile_m = f64::from(FIXTURE_TILE_CELLS) * g.cell_m;
    let tile_of = |x: f32, z: f32| ((f64::from(x) / tile_m).floor() as i64, (f64::from(z) / tile_m).floor() as i64);
    let artifact = f.whole.artifact().expect("whole world");
    let mut crossing = 0;
    for reach in &artifact.rivers {
        let hw = f64::from(reach.width_m) * 0.5;
        let across = [0.0, 0.5 * hw, hw, hw + 1.0, hw + 3.0, hw + 14.0, hw + 27.0];
        for pair in reach.points.windows(2) {
            let (p, q) = (pair[0], pair[1]);
            if tile_of(p.x, p.z) == tile_of(q.x, q.z) {
                continue;
            }
            crossing += 1;
            let (ax, az) = (f64::from(p.x), f64::from(p.z));
            let (vx, vz) = (f64::from(q.x) - ax, f64::from(q.z) - az);
            let len = (vx * vx + vz * vz).sqrt().max(1e-9);
            let (nx, nz) = (-vz / len, vx / len);
            const STEP_M: f64 = 0.5;
            let steps = (len / STEP_M).ceil() as i64;
            for s in 0..=steps {
                let t = (s as f64 * STEP_M / len).min(1.0);
                for &d in &across {
                    for side in [-1.0, 1.0] {
                        let (x, z) = engine(f, ax + vx * t + nx * d * side, az + vz * t + nz * d * side);
                        assert_point(f, x, z, "river across tiles");
                    }
                }
            }
        }
    }
    assert!(crossing > 0, "the fixture must hold a river crossing a tile edge");
    println!("{crossing} river segments cross a tile edge");
}

/// Lines 0..500 m outside each edge, the corners past both, and raster cells
/// far outside: land edges continue the border, sea edges are ocean.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn past_edge_bit_equal() {
    let f = fixture();
    let g = f.whole.globals();
    let size = g.size_m;
    let out = [0.0, 1e-9, 0.5, 8.0, 16.0, 50.0, 100.0, 250.0, 500.0];
    const ALONG_STRIDE_M: f64 = 5.0;
    let along = (size / ALONG_STRIDE_M) as i64;
    for j in 0..=along {
        let t = j as f64 * ALONG_STRIDE_M;
        for &d in &out {
            for (ax, az) in [(t, -d), (size + d, t), (t, size + d), (-d, t)] {
                let (x, z) = engine(f, ax, az);
                assert_point(f, x, z, "past an edge");
            }
        }
    }
    const CORNER_STRIDE_M: f64 = 25.0;
    for i in 0..=20 {
        for j in 0..=20 {
            let (dx, dz) = (i as f64 * CORNER_STRIDE_M, j as f64 * CORNER_STRIDE_M);
            for (ax, az) in [(-dx, -dz), (size + dx, -dz), (-dx, size + dz), (size + dx, size + dz)] {
                let (x, z) = engine(f, ax, az);
                assert_point(f, x, z, "past a corner");
            }
        }
    }
    let n = i64::from(g.n);
    for far in [-100_000, -1000, -40, -1, n, n + 40, n + 1000, n + 100_000] {
        for c in (-50..n + 50).step_by(7) {
            assert_cell(f, far, c);
            assert_cell(f, c, far);
        }
    }
}

/// Meta and every tile survive their codecs; tiles cover the raster once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn meta_and_tile_codecs_roundtrip() {
    let f = fixture();
    assert_eq!(decode_meta(&encode_meta(&f.meta)).expect("meta decodes"), f.meta);
    assert_eq!(f.meta.tile_cells, FIXTURE_TILE_CELLS);
    assert_eq!(f.meta.format_version, worldgen::forged_tiles::TILED_FORMAT_VERSION);
    let per_side = f.whole.globals().n.div_ceil(FIXTURE_TILE_CELLS);
    assert_eq!(f.tiles.len() as u32, per_side * per_side);
    let mut cells = 0u64;
    for t in &f.tiles {
        let bytes = encode_tile(t);
        assert_eq!(&decode_tile(&bytes).expect("tile decodes"), t, "tile {:?}", t.origin);
        cells += u64::from(t.w) * u64::from(t.h);
    }
    assert_eq!(cells, u64::from(f.whole.globals().n).pow(2), "tiles cover every cell exactly once");
    assert!(decode_tile(b"not a tile").is_err());
    assert!(decode_meta(b"not a meta").is_err());
    let bytes = encode_tile(&f.tiles[0]);
    assert!(decode_tile(&bytes[..bytes.len() / 2]).is_err(), "truncated tile");
}

/// What a tiled read costs over a whole one (printed, not asserted): one
/// region and the Task 1 grid's samplers over one tile.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tiled_read_cost() {
    let f = fixture();
    let mats = materials();
    for (name, w) in [("whole", &f.whole), ("tiled", &f.tiled)] {
        let t = std::time::Instant::now();
        for r in 0..4 {
            std::hint::black_box(worldgen::generate_region_forged(w, RegionCoords::new(r, 0), mats));
        }
        let regions = t.elapsed();
        let t = std::time::Instant::now();
        for k in 0..100_000 {
            let (x, z) = engine(f, (k % 317) as f64 * 1.3, (k / 317) as f64 * 1.3);
            std::hint::black_box(point_bits(w, x, z));
        }
        println!("{name}: 4 regions {regions:?}, 100k point samples {:?}", t.elapsed());
    }
}
