//! The forged world in the store as tiles (forged-world-in-db plan, Task 4):
//! `write_tiled` stores meta + tiles + world map + archive + content crc in one
//! transaction, `load_forged` serves a tiled `ForgedWorld` through
//! `StoreTiles`, bit-equal to the whole world. Every body runs on SQLite and,
//! with `MMO_TEST_DATABASE_URL` set, on a fresh Postgres database.
//!
//! Fixture: `ForgeParams::default()` (16 km / 32 m, 500 x 500 cells), so the
//! 256-cell `TILE_CELLS` cuts it into 2 x 2 tiles (the second row and column
//! partial) with a seam and a corner near the map's centre.

use std::sync::{Arc, OnceLock};

use game::RegionCoords;
use world_store::{FormatKind, StoreError, StoreHandle, StoreTiles, StoreUrl, encode_world_map, load_forged, write_tiled};
use worldgen::forged::ForgedWorld;
use worldgen::forged_tiles::{TILE_CELLS, TileCoords, TileLoader, encode_meta, encode_tile, split};

use crate::common;
use crate::handle::{each_backend, open, raw_exec};

struct Fixture {
    bytes: Vec<u8>,
    whole: ForgedWorld,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let p = worldgen::forge::ForgeParams::default();
        let bytes = worldgen::forge::forge(&p).artifact(&p).to_bytes();
        let whole = ForgedWorld::from_bytes(&bytes).expect("forged bytes load");
        assert_eq!(whole.globals().n.div_ceil(TILE_CELLS), 2, "the fixture is 2 x 2 tiles");
        Fixture { bytes, whole }
    })
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

/// The Task 2 grid subset: a 61 m stride over the map and 200 m past it,
/// every cell's `extended_cell_height` along the tile seams (±3 cells), and
/// the regions around the tile corner and at the map's corner.
fn assert_bit_equal(whole: &ForgedWorld, tiled: &ForgedWorld) {
    let (min, max) = whole.artifact_bounds_engine_m();
    let mut z = min[1] - 200.0;
    while z < max[1] + 200.0 {
        let mut x = min[0] - 200.0;
        while x < max[0] + 200.0 {
            assert_eq!(point_bits(whole, x, z), point_bits(tiled, x, z), "point ({x}, {z})");
            x += 61.0;
        }
        z += 61.0;
    }
    let n = i64::from(whole.globals().n);
    let seam = i64::from(TILE_CELLS);
    for a in -2..n + 2 {
        for b in seam - 3..seam + 3 {
            for (ix, iz) in [(a, b), (b, a)] {
                let (p, q) = (whole.extended_cell_height(ix, iz), tiled.extended_cell_height(ix, iz));
                assert_eq!(p.to_bits(), q.to_bits(), "extended_cell_height({ix}, {iz})");
            }
        }
    }
    let region_m = game::REGION_BLOCKS as f64;
    let rc = |x: f64, z: f64| RegionCoords::new((x / region_m).floor() as i32, (z / region_m).floor() as i32);
    let seam_m = f64::from(TILE_CELLS) * whole.globals().cell_m;
    let centre = rc(min[0] + seam_m, min[1] + seam_m);
    for r in [
        centre,
        RegionCoords::new(centre.x - 1, centre.z),
        RegionCoords::new(centre.x, centre.z - 1),
        RegionCoords::new(centre.x - 1, centre.z - 1),
        rc(min[0], min[1]),
    ] {
        let a = worldgen::stud_gen::generate_stud_region_forged(whole, r);
        let b = worldgen::stud_gen::generate_stud_region_forged(tiled, r);
        assert!(a == b, "generate_stud_region_forged({r:?}): tiled differs from whole");
    }
}

fn loaded(h: &StoreHandle) -> (ForgedWorld, game::WorldMap) {
    load_forged(h).expect("load_forged").expect("a tiled world is stored")
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn write_then_load_bit_equal() {
    let f = fixture();
    each_backend("forged_bit_equal", false, |url| {
        let h = open(url);
        assert!(load_forged(&h).unwrap().is_none(), "no tiled world before a write");
        let crc = write_tiled(&h, f.bytes.clone()).expect("write_tiled");
        let (tiled, _) = loaded(&h);
        assert!(tiled.artifact().is_none(), "the store serves tiles, not the artifact");
        assert_eq!(tiled.content_crc(), crc);
        assert_eq!(h.forged_archive().unwrap().as_deref(), Some(&f.bytes[..]), "the archive is the forge file");
        assert_bit_equal(&f.whole, &tiled);
        let info = h.world_info().unwrap();
        assert_eq!((info.content_crc, info.has_artifact), (Some(crc), true));
        h.shutdown();
    });
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn world_map_is_stored() {
    let f = fixture();
    let expected = worldgen::tile_map::build_world_map(&f.whole);
    each_backend("forged_world_map", false, |url| {
        let h = open(url);
        write_tiled(&h, f.bytes.clone()).unwrap();
        let (_, map) = loaded(&h);
        assert_eq!(map, expected);
        h.shutdown();
    });
}

/// Decision 8: crc32 over the meta row, every tile row in `(tz, tx)` order,
/// then the world map row — the same for the same bytes, in any store.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn content_crc_is_stable() {
    let f = fixture();
    let (meta, tiles) = split(&f.whole, TILE_CELLS).expect("the fixture splits");
    let mut tiles: Vec<_> = tiles.iter().map(|t| (t.origin, encode_tile(t))).collect();
    tiles.sort_by_key(|(c, _)| *c);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&encode_meta(&meta));
    for (_, b) in &tiles {
        hasher.update(b);
    }
    hasher.update(&encode_world_map(&worldgen::tile_map::build_world_map(&f.whole)));
    let expected = hasher.finalize();
    each_backend("forged_crc", false, |url| {
        let h = open(url);
        let first = write_tiled(&h, f.bytes.clone()).unwrap();
        let second = write_tiled(&h, f.bytes.clone()).unwrap();
        assert_eq!((first, second), (expected, expected));
        assert_eq!(h.world_info().unwrap().content_crc, Some(expected), "stored in world.content_crc");
        h.shutdown();
    });
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn old_tile_format_refused() {
    let f = fixture();
    each_backend("forged_old_format", true, |url| {
        let h = open(url);
        write_tiled(&h, f.bytes.clone()).unwrap();
        h.shutdown();
        raw_exec(url, "UPDATE forged_meta SET format_version = 0");
        let h = open(url);
        let err = load_forged(&h).err().expect("an old tile format is refused");
        assert_eq!(err, StoreError::OtherFormat { found: 0, kind: FormatKind::Tiles { archived: true } });
        assert!(err.to_string().contains("just worldctl retile"), "{err}");
        h.shutdown();
        raw_exec(url, "DELETE FROM forged_archive");
        let h = open(url);
        let err = load_forged(&h).err().expect("still refused");
        assert_eq!(err, StoreError::OtherFormat { found: 0, kind: FormatKind::Tiles { archived: false } });
        assert!(err.to_string().contains("re-forge") && !err.to_string().contains("retile"), "{err}");
        h.shutdown();
    });
}

/// A tile the store holds damaged is a load error naming the tile, never
/// terrain (the tile codec's crc).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn damaged_tile_row_fails_its_load() {
    let f = fixture();
    each_backend("forged_damaged", true, |url| {
        let h = open(url);
        write_tiled(&h, f.bytes.clone()).unwrap();
        let good = StoreTiles(h.clone()).load(TileCoords { tx: 1, tz: 0 }).unwrap();
        h.shutdown();
        // Flip one byte in the middle of tile (1, 0), keeping its length.
        let mid = good.len() / 2;
        let hex: String = {
            let mut b = good.clone();
            b[mid] ^= 0x10;
            b.iter().map(|x| format!("{x:02x}")).collect()
        };
        let literal = match url {
            StoreUrl::Postgres(_) => format!("'\\x{hex}'::bytea"),
            _ => format!("X'{hex}'"),
        };
        raw_exec(url, &format!("UPDATE forged_tiles SET bytes = {literal} WHERE tx = 1 AND tz = 0"));
        let h = open(url);
        let (tiled, _) = loaded(&h);
        let (min, _) = tiled.artifact_bounds_engine_m();
        let tile_m = f64::from(TILE_CELLS) * tiled.globals().cell_m;
        let (x, z) = (min[0] + 1.5 * tile_m, min[1] + 0.5 * tile_m);
        let err = tiled.prefetch_rect([x, z], [x + 1.0, z + 1.0]).err().expect("the damaged tile fails");
        assert!(err.contains("(1, 0)"), "{err}");
        h.shutdown();
    });
}

/// A `TileLoader` over its own Postgres pool: a second host's reads of the
/// same database. (Written while Postgres had a world lock; two
/// `StoreHandle`s may open one database now, but a bare pool is still the
/// smallest second reader.)
struct PoolTiles {
    rt: tokio::runtime::Runtime,
    pool: sqlx::PgPool,
}

impl TileLoader for PoolTiles {
    fn load(&self, t: TileCoords) -> Result<Vec<u8>, String> {
        use sqlx::Row;
        self.rt.block_on(async {
            let row = sqlx::query("SELECT bytes FROM forged_tiles WHERE tx = $1 AND tz = $2")
                .bind(i64::from(t.tx))
                .bind(i64::from(t.tz))
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no tile ({}, {})", t.tx, t.tz))?;
            row.try_get::<Vec<u8>, _>(0).map_err(|e| e.to_string())
        })
    }
}

/// Review Focus 5: two readers of one Postgres world load and sample the
/// same tiles concurrently, each bit-equal to the whole world.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn two_handles_read_same_tiles() {
    let Some(admin) = common::pg_admin_url() else { return common::skip_msg() };
    let f = fixture();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(common::PgTestDb::create(&admin));
    let url = StoreUrl::Postgres(db.url.clone());
    let h = open(&url);
    write_tiled(&h, f.bytes.clone()).unwrap();
    let (a, _) = loaded(&h);
    let second_rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let pool = second_rt.block_on(sqlx::PgPool::connect(&db.url)).unwrap();
    // The second host builds its world from the stored meta row.
    let meta_row: Vec<u8> = second_rt.block_on(async {
        use sqlx::Row;
        sqlx::query("SELECT bytes FROM forged_meta WHERE id = 1").fetch_one(&pool).await.unwrap().get(0)
    });
    let meta = worldgen::forged_tiles::decode_meta(&meta_row).expect("stored meta decodes");
    let b = ForgedWorld::from_tiles(meta, Arc::new(PoolTiles { rt: second_rt, pool }), a.content_crc())
        .expect("second host's world");
    std::thread::scope(|s| {
        for w in [&a, &b] {
            s.spawn(|| assert_bit_equal(&f.whole, w));
        }
    });
    drop(b);
    h.shutdown();
    rt.block_on(db.drop_db());
}

/// `worldctl retile` rebuilds the tiles from the archive row after the tile
/// format moved on: the loaded world samples bit-equal to the original.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn retile_from_archive_is_bit_equal() {
    let f = fixture();
    each_backend("forged_retile", true, |url| {
        let h = open(url);
        let crc = write_tiled(&h, f.bytes.clone()).unwrap();
        h.shutdown();
        raw_exec(url, "UPDATE forged_meta SET format_version = 0");
        let h = open(url);
        let err = load_forged(&h).err().expect("the old format is refused before the retile");
        assert!(
            matches!(err, StoreError::OtherFormat { kind: FormatKind::Tiles { archived: true }, .. }),
            "{err:?}"
        );
        h.shutdown();

        assert_eq!(world_store::ctl::retile(url).expect("retile"), crc);

        let h = open(url);
        let (tiled, _) = loaded(&h);
        assert_eq!(tiled.content_crc(), crc);
        assert_bit_equal(&f.whole, &tiled);
        h.shutdown();
    });
}

#[test]
fn retile_without_archive_says_reforge() {
    each_backend("forged_retile_none", true, |url| {
        let err = world_store::ctl::retile(url).unwrap_err();
        assert!(err.contains("re-forge"), "{err}");
    });
}

/// `bytes` as a SQL blob literal for `url`'s engine.
fn blob_literal(url: &StoreUrl, bytes: &[u8]) -> String {
    let hex: String = bytes.iter().map(|x| format!("{x:02x}")).collect();
    match url {
        StoreUrl::Postgres(_) => format!("'\\x{hex}'::bytea"),
        _ => format!("X'{hex}'"),
    }
}

/// A stored world map this build does not serve — the unversioned format-1
/// row, another `WORLD_MAP_VERSION`, a damaged row — is a re-tile-able
/// `OtherFormat` (never "corrupt"), and `retile` rebuilds it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stale_world_map_is_retileable() {
    let f = fixture();
    let expected = worldgen::tile_map::build_world_map(&f.whole);
    let good = encode_world_map(&expected);
    let mut other_version = good.clone();
    other_version[1..5].copy_from_slice(&(worldgen::tile_map::WORLD_MAP_VERSION + 1).to_le_bytes());
    let mut damaged = good.clone();
    let last = damaged.len() - 1;
    damaged[last] ^= 0x40;
    let cases: [(&str, Vec<u8>, u32); 3] = [
        ("format 1", world_store::encode(1, &expected), 0),
        ("other version", other_version, worldgen::tile_map::WORLD_MAP_VERSION + 1),
        ("damaged", damaged, 0),
    ];
    each_backend("forged_stale_map", true, |url| {
        let h = open(url);
        let crc = write_tiled(&h, f.bytes.clone()).unwrap();
        h.shutdown();
        for (what, row, found) in &cases {
            raw_exec(url, &format!("UPDATE forged_world_map SET bytes = {} WHERE id = 1", blob_literal(url, row)));
            let h = open(url);
            let err = load_forged(&h).err().unwrap_or_else(|| panic!("{what}: refused"));
            assert_eq!(
                err,
                StoreError::OtherFormat { found: *found, kind: FormatKind::WorldMap { archived: true } },
                "{what}"
            );
            assert!(err.to_string().contains("just worldctl retile --db"), "{what}: {err}");
            assert!(!err.to_string().contains("corrupt"), "{what}: {err}");
            h.shutdown();
            assert_eq!(world_store::ctl::retile(url).expect("retile"), crc, "{what}");
            let h = open(url);
            let (_, map) = loaded(&h);
            assert_eq!(map, expected, "{what}: rebuilt");
            h.shutdown();
        }
        raw_exec(url, "DELETE FROM forged_archive");
        raw_exec(url, &format!("UPDATE forged_world_map SET bytes = {} WHERE id = 1", blob_literal(url, &cases[0].1)));
        let h = open(url);
        let err = load_forged(&h).err().expect("still refused");
        assert_eq!(err, StoreError::OtherFormat { found: 0, kind: FormatKind::WorldMap { archived: false } });
        assert!(err.to_string().contains("re-forge"), "{err}");
        h.shutdown();
    });
}

/// A forge file whose cell size the tiled store cannot load back (4 m
/// cells: sampling reads past `MAX_READ_RADIUS_CELLS`) is refused BEFORE
/// anything is written: no meta, tiles, map, archive or content crc.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn unsupported_cell_size_refused_before_writing() {
    let p = worldgen::forge::ForgeParams { size_m: 512.0, cell_m: 4.0, seed: 4, ..Default::default() };
    let bytes = worldgen::forge::forge(&p).artifact(&p).to_bytes();
    each_backend("forged_unsupported", false, |url| {
        let h = open(url);
        let err = write_tiled(&h, bytes.clone()).err().expect("4 m cells are refused");
        assert!(
            matches!(&err, StoreError::Unsupported(m) if m.contains("cell size 4 m is not supported by the tiled store")),
            "{err:?}"
        );
        assert!(load_forged(&h).unwrap().is_none(), "no tiled world written");
        let info = h.world_info().unwrap();
        assert!(!info.has_artifact && info.content_crc.is_none(), "no archive, no content crc");
        assert!(h.forged_archive().unwrap().is_none());
        h.shutdown();
    });
}
