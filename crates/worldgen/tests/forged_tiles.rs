//! Tiled forged worlds (`worldgen::forged_tiles`, forged-world-in-db plan
//! Task 2): a world split into tiles and sampled through the tile cache
//! returns bit-for-bit what the whole-raster world returns, at every point —
//! on tile edges and corners, along rivers that cross tiles, and past the
//! map's land and sea edges.
//!
//! Task 3 adds the cache's behaviour: eviction under a small budget stays
//! bit-equal, the budget holds beyond pinned tiles, prefetch pins every tile a
//! region or LOD job reads (a counting loader sees no load during
//! generation), and a failing or corrupt tile is an error, never terrain.
//!
//! Fixture: a real forge (4,096 m / 16 m, seed 4 — the Task 1 guard's world)
//! split into 32-cell tiles (512 m, 8 x 8 = 64 tiles) and served from memory.
//! Every assertion compares `to_bits()`. Only explicit worlds — nothing
//! process-global — so this file lives in the shared `it` binary.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use game::RegionCoords;
use worldgen::forged::ForgedWorld;
use worldgen::forged_tiles::{
    decode_meta, decode_tile, encode_meta, encode_tile, split, ForgedMeta, MemTiles, Tile, TileCoords, TileLoader,
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

/// A stored bit flip is a decode error, never a tile with wrong values: every
/// single-byte flip past the 4-byte magic of an encoded tile (and of the
/// meta) is refused — the header's crc32 covers the compressed body.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn flipped_byte_is_a_decode_error() {
    let f = fixture();
    let bytes = encode_tile(&f.tiles[9]);
    assert!(decode_tile(&bytes).is_ok());
    let mid = bytes.len() / 2;
    let mut one = bytes.clone();
    one[mid] ^= 0x01;
    assert!(decode_tile(&one).is_err(), "a flip in the middle of a tile must fail its decode");
    for i in 4..bytes.len() {
        let mut bad = bytes.clone();
        bad[i] ^= 0x10;
        assert!(decode_tile(&bad).is_err(), "tile byte {i} of {} flipped still decodes", bytes.len());
    }
    let meta = encode_meta(&f.meta);
    for i in 4..meta.len() {
        let mut bad = meta.clone();
        bad[i] ^= 0x10;
        assert!(decode_meta(&bad).is_err(), "meta byte {i} of {} flipped still decodes", meta.len());
    }
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

/// The fixture's tiles behind a loader that counts its loads and fails the
/// tiles in `failing`.
struct CountingLoader {
    tiles: MemTiles,
    loads: AtomicUsize,
    failing: Vec<TileCoords>,
}

impl TileLoader for CountingLoader {
    fn load(&self, t: TileCoords) -> Result<Vec<u8>, String> {
        self.loads.fetch_add(1, Ordering::Relaxed);
        if self.failing.contains(&t) {
            return Err("injected failure".into());
        }
        self.tiles.load(t)
    }
}

fn counting(f: &Fixture, failing: Vec<TileCoords>) -> Arc<CountingLoader> {
    let tiles = MemTiles(f.tiles.iter().map(|t| (t.origin, encode_tile(t))).collect());
    Arc::new(CountingLoader { tiles, loads: AtomicUsize::new(0), failing })
}

/// A fresh tiled world over `loader` with a cache of `budget` bytes.
fn tiled_with(f: &Fixture, loader: Arc<dyn TileLoader>, budget: usize) -> ForgedWorld {
    ForgedWorld::from_tiles_with_budget(f.meta.clone(), loader, f.whole.content_crc(), budget).expect("tiled world")
}

/// The decoded size of the fixture's largest tile.
fn max_tile_bytes(f: &Fixture) -> usize {
    f.tiles.iter().map(Tile::decoded_bytes).max().expect("tiles")
}

/// Windows of an LOD-style sweep: 512 m steps over the fixture and one step
/// past each edge, row by row (engine metres, min corner).
fn sweep_windows(f: &Fixture) -> Vec<(f64, f64)> {
    const STRIDE_M: f64 = 512.0;
    let (min, max) = f.whole.artifact_bounds_engine_m();
    let steps = ((max[0] - min[0]) / STRIDE_M) as i64;
    (-1..=steps)
        .flat_map(|kz| (-1..=steps).map(move |kx| (kx, kz)))
        .map(|(kx, kz)| (min[0] + kx as f64 * STRIDE_M, min[1] + kz as f64 * STRIDE_M))
        .collect()
}

/// One sweep window: prefetch it (pins held, as an LOD job would), compare
/// a 32 m grid of point samples, and (with `lod`) the factor-16 key there.
fn sweep_window(f: &Fixture, tiled: &ForgedWorld, (x0, z0): (f64, f64), lod: bool, mut check: impl FnMut()) {
    const WINDOW_M: f64 = 512.0;
    const GRID_M: f64 = 32.0;
    let pins = tiled.prefetch_rect([x0, z0], [x0 + WINDOW_M, z0 + WINDOW_M]).expect("prefetch");
    let steps = (WINDOW_M / GRID_M) as i64;
    for j in 0..steps {
        for i in 0..steps {
            let (x, z) = (x0 + i as f64 * GRID_M + 3.0, z0 + j as f64 * GRID_M + 5.0);
            assert_eq!(point_bits(&f.whole, x, z), point_bits(tiled, x, z), "sweep point ({x}, {z})");
        }
    }
    if lod {
        let rb = game::REGION_BLOCKS as f64;
        let rc = RegionCoords::new((x0 / rb).floor() as i32, (z0 / rb).floor() as i32);
        let key = game::lod::LodKey::for_region(16, rc);
        let mats = materials();
        let cells = |w: &ForgedWorld| {
            worldgen::coarse_tile_forged(w, &key, mats)
                .into_iter()
                .map(|c| bincode::serialize(&(c.group, &c.grid)).unwrap())
                .collect::<Vec<_>>()
        };
        assert!(cells(&f.whole) == cells(tiled), "coarse_tile_forged({key:?}) under eviction");
    }
    check();
    drop(pins);
}

/// Review Focus 4: with the budget at 4 tiles, an LOD-style sweep over all
/// 64 tiles (on 4 threads, so eviction races reads) evicts and reloads, and
/// every answer stays bit-equal to the whole world's.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn eviction_is_transparent() {
    let f = fixture();
    let loader = counting(f, Vec::new());
    let budget = 4 * max_tile_bytes(f);
    let tiled = tiled_with(f, loader.clone(), budget);
    let windows = sweep_windows(f);
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&w) = windows.get(k) else { break };
                    sweep_window(f, &tiled, w, true, || {});
                }
            });
        }
    });
    let loads = loader.loads.load(Ordering::Relaxed);
    assert!(loads > f.tiles.len(), "the sweep must evict and reload ({loads} loads of {} tiles)", f.tiles.len());
    println!("{} windows, {loads} tile loads of {} tiles", windows.len(), f.tiles.len());
}

/// After every window (pins held) the cache holds no more than the budget
/// plus the pinned tiles, and with no pins left, no more than the budget.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cache_respects_budget() {
    let f = fixture();
    let budget = 4 * max_tile_bytes(f);
    let tiled = tiled_with(f, counting(f, Vec::new()), budget);
    let cache = tiled.tile_cache().expect("a tiled world has a tile cache");
    let mut peak_pinned = 0;
    for w in sweep_windows(f) {
        sweep_window(f, &tiled, w, false, || {
            let (resident, pinned) = (cache.resident_bytes(), cache.pinned_bytes());
            peak_pinned = peak_pinned.max(pinned);
            assert!(resident <= budget + pinned, "resident {resident} > budget {budget} + pinned {pinned}");
        });
        assert_eq!(cache.pinned_bytes(), 0, "dropping the pins unpins");
        assert!(cache.resident_bytes() <= budget, "resident {} > budget {budget}", cache.resident_bytes());
    }
    assert!(peak_pinned > 0, "the sweep pinned tiles");
    // A job pinning more than the budget: the cache goes over (it logs once)
    // but never evicts a pinned tile.
    let (min, max) = f.whole.artifact_bounds_engine_m();
    let all = tiled.prefetch_rect(min, max).expect("prefetch the whole map");
    assert_eq!(all.len(), f.tiles.len());
    assert_eq!(cache.pinned_bytes(), cache.resident_bytes(), "every tile pinned and resident");
    drop(all);
    assert!(cache.resident_bytes() <= budget, "unpinning trims the cache back to the budget");
}

/// A tile that cannot be loaded makes `prefetch_rect` over it an `Err` naming
/// the tile — no panic, no terrain — and holds no pins.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn prefetch_error_is_reported_not_panicked() {
    let f = fixture();
    let bad = TileCoords { tx: 1, tz: 1 };
    let tiled = tiled_with(f, counting(f, vec![bad]), usize::MAX);
    let tile_m = f64::from(FIXTURE_TILE_CELLS) * f.whole.globals().cell_m;
    let (x, z) = engine(f, 1.5 * tile_m, 1.5 * tile_m);
    let err = match tiled.prefetch_rect([x, z], [x + 1.0, z + 1.0]) {
        Ok(_) => panic!("prefetch over a failing tile must fail"),
        Err(e) => e,
    };
    assert!(err.contains("(1, 1)"), "the error names the tile: {err}");
    assert_eq!(tiled.tile_cache().unwrap().pinned_bytes(), 0, "a failed prefetch holds no pins");
    // Tiles away from it still prefetch.
    let (x, z) = engine(f, 5.5 * tile_m, 5.5 * tile_m);
    assert!(tiled.prefetch_rect([x, z], [x + 1.0, z + 1.0]).is_ok());
}

/// Regions on tile seams and corners, at the map's edges and past them:
/// after `prefetch_region_forged`, generating the region loads nothing — even
/// with a one-tile budget, which only the pins hold back.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn pinned_generation_needs_no_loads() {
    let f = fixture();
    let mats = materials();
    let per_tile = (f64::from(FIXTURE_TILE_CELLS) * f.whole.globals().cell_m) as i32 / game::REGION_BLOCKS as i32;
    let (min, max) = f.whole.artifact_bounds_engine_m();
    let rb = game::REGION_BLOCKS as f64;
    let (r0x, r0z) = ((min[0] / rb).floor() as i32, (min[1] / rb).floor() as i32);
    let (r1x, r1z) = (((max[0] - 1.0) / rb).floor() as i32, ((max[1] - 1.0) / rb).floor() as i32);
    let mut regions = Vec::new();
    // On seams and corners, and one region short of a seam on either side
    // (the patch margin stops short of it; only the read reach crosses).
    let spots = [
        (-1, -1),
        (0, 0),
        (per_tile - 1, per_tile - 1),
        (per_tile, per_tile),
        (per_tile, 2 * per_tile - 1),
        (per_tile - 2, per_tile + 3),
        (per_tile + 1, 2 * per_tile - 2),
        (2 * per_tile + 3, per_tile + 1),
    ];
    for (dx, dz) in spots {
        regions.push(RegionCoords::new(r0x + dx, r0z + dz));
    }
    regions.extend([RegionCoords::new(r1x, r1z), RegionCoords::new(r1x + 1, r0z + 3), RegionCoords::new(r1x + 2, r1z + 2)]);
    for rc in regions {
        let loader = counting(f, Vec::new());
        let tiled = tiled_with(f, loader.clone(), max_tile_bytes(f));
        let pins = worldgen::prefetch_region_forged(&tiled, rc).expect("prefetch region");
        let before = loader.loads.load(Ordering::Relaxed);
        assert_eq!(before, pins.len(), "prefetch loads each pinned tile once");
        let a = bincode::serialize(&worldgen::generate_region_forged(&f.whole, rc, mats)).unwrap();
        let b = bincode::serialize(&worldgen::generate_region_forged(&tiled, rc, mats)).unwrap();
        assert!(a == b, "generate_region_forged({rc:?}) after prefetch");
        assert_eq!(loader.loads.load(Ordering::Relaxed), before, "region {rc:?} loaded a tile prefetch missed");
    }
}

/// The same for LOD keys: a per-region factor (16) and tile factors (64,
/// 256), on tile seams, at an edge and past it — `coarse_tile_forged` and
/// `coarse_water_forged` after `prefetch_lod_forged` load nothing.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn pinned_lod_needs_no_loads() {
    let f = fixture();
    let mats = materials();
    let per_tile = (f64::from(FIXTURE_TILE_CELLS) * f.whole.globals().cell_m) as i32 / game::REGION_BLOCKS as i32;
    let (min, _) = f.whole.artifact_bounds_engine_m();
    let rb = game::REGION_BLOCKS as f64;
    let (r0x, r0z) = ((min[0] / rb).floor() as i32, (min[1] / rb).floor() as i32);
    let n_regions = (f.whole.globals().size_m / rb) as i32;
    let spots = [
        (-1, -1),
        (per_tile - 1, per_tile),
        (per_tile, per_tile - 1),
        (per_tile - 2, per_tile + 3),
        (per_tile + 1, 2 * per_tile - 2),
        (n_regions - 1, 2),
        (n_regions, n_regions),
    ];
    for factor in [16, 64, 256] {
        for (dx, dz) in spots {
            let key = game::lod::LodKey::for_region(factor, RegionCoords::new(r0x + dx, r0z + dz));
            let loader = counting(f, Vec::new());
            let tiled = tiled_with(f, loader.clone(), max_tile_bytes(f));
            let pins = worldgen::prefetch_lod_forged(&tiled, &key).expect("prefetch lod");
            let before = loader.loads.load(Ordering::Relaxed);
            assert_eq!(before, pins.len());
            let cells = |w: &ForgedWorld| {
                worldgen::coarse_tile_forged(w, &key, mats)
                    .into_iter()
                    .map(|c| bincode::serialize(&(c.group, &c.grid)).unwrap())
                    .collect::<Vec<_>>()
            };
            assert!(cells(&f.whole) == cells(&tiled), "coarse_tile_forged({key:?}) after prefetch");
            let water = |w: &ForgedWorld| bincode::serialize(&worldgen::coarse_water_forged(w, &key, mats)).unwrap();
            assert!(water(&f.whole) == water(&tiled), "coarse_water_forged({key:?}) after prefetch");
            assert_eq!(loader.loads.load(Ordering::Relaxed), before, "LOD {key:?} loaded a tile prefetch missed");
        }
    }
}

/// A tile whose lake runs, coast or segments are corrupt fails its load:
/// prefetch reports it, nothing samples wrong answers.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn corrupt_tile_fails_the_load() {
    let f = fixture();
    let t = TileCoords { tx: 2, tz: 3 };
    let good = f.tiles.iter().find(|x| x.origin == t).expect("tile").clone();
    let n = f.whole.globals().n;
    let first = t.tz * FIXTURE_TILE_CELLS * n + t.tx * FIXTURE_TILE_CELLS;
    let corruptions: Vec<(&str, Box<dyn Fn(&mut Tile)>)> = vec![
        ("lake run outside the tile", Box::new(|x: &mut Tile| x.lake_runs.push((0, 1, 5.0)))),
        ("lake run across rows", Box::new(move |x: &mut Tile| x.lake_runs = vec![(first, first + n, 5.0)])),
        ("NaN lake level", Box::new(move |x: &mut Tile| x.lake_runs = vec![(first, first + 1, f32::NAN)])),
        ("empty lake run", Box::new(move |x: &mut Tile| x.lake_runs = vec![(first + 1, first + 1, 5.0)])),
        (
            "unsorted lake runs",
            Box::new(move |x: &mut Tile| x.lake_runs = vec![(first + 3, first + 4, 5.0), (first, first + 1, 5.0)]),
        ),
        ("NaN coast", Box::new(|x: &mut Tile| x.coast[7] = f32::NAN)),
    ];
    for (what, corrupt) in corruptions {
        let mut bad = good.clone();
        corrupt(&mut bad);
        let mut tiles: std::collections::BTreeMap<_, _> = f.tiles.iter().map(|x| (x.origin, encode_tile(x))).collect();
        tiles.insert(t, encode_tile(&bad));
        let tiled = tiled_with(f, Arc::new(MemTiles(tiles)), usize::MAX);
        let tile_m = f64::from(FIXTURE_TILE_CELLS) * f.whole.globals().cell_m;
        let (x, z) = engine(f, (f64::from(t.tx) + 0.5) * tile_m, (f64::from(t.tz) + 0.5) * tile_m);
        let err = tiled.prefetch_rect([x, z], [x + 1.0, z + 1.0]).err();
        assert!(err.is_some_and(|e| e.contains("(2, 3)")), "{what}: the load must fail naming the tile");
    }
}

/// The meta is refused when its size is not `n * cell_m`, or its cells are
/// so small that sampling reads past `MAX_READ_RADIUS_CELLS`.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn meta_validate_checks_geometry() {
    let f = fixture();
    assert!(f.meta.validate().is_ok());
    let mut m = f.meta.clone();
    m.globals.size_m += 16.0;
    assert!(m.validate().is_err(), "size_m != n * cell_m");
    let mut m = f.meta.clone();
    m.globals.cell_m = 4.0;
    m.globals.size_m = f64::from(m.globals.n) * 4.0;
    assert!(worldgen::forged_tiles::read_radius_cells(4.0) > worldgen::forged_tiles::MAX_READ_RADIUS_CELLS);
    assert!(m.validate().is_err(), "4 m cells read past MAX_READ_RADIUS_CELLS");
    for cell_m in [8.0, 16.0, 32.0, 64.0, 128.0] {
        assert!(worldgen::forged_tiles::read_radius_cells(cell_m) <= worldgen::forged_tiles::MAX_READ_RADIUS_CELLS);
    }
    assert_eq!(worldgen::forged_tiles::read_radius_cells(8.0), 4, "Decision 1: 4 cells at 8 m");
    assert_eq!(worldgen::forged_tiles::read_radius_cells(16.0), 3, "Decision 1: 3 cells at 16 m");
}

/// A loader may sample another tiled world on the same thread (the memo is
/// not borrowed across a load).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn loader_may_sample_on_the_same_thread() {
    struct Reentrant(MemTiles);
    impl TileLoader for Reentrant {
        fn load(&self, t: TileCoords) -> Result<Vec<u8>, String> {
            let f = fixture();
            let (x, z) = engine(f, 100.0 + f64::from(t.tx) * 600.0, 100.0 + f64::from(t.tz) * 600.0);
            std::hint::black_box(f.tiled.sample(x, z));
            self.0.load(t)
        }
    }
    let f = fixture();
    let tiles = MemTiles(f.tiles.iter().map(|t| (t.origin, encode_tile(t))).collect());
    let tiled = tiled_with(f, Arc::new(Reentrant(tiles)), 2 * max_tile_bytes(f));
    for k in 0..40 {
        let (x, z) = engine(f, 37.0 + k as f64 * 97.0, 11.0 + k as f64 * 83.0);
        assert_eq!(point_bits(&f.whole, x, z), point_bits(&tiled, x, z));
    }
}
