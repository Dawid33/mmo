//! Old worlds and migration `0005_drop_voxel_terrain`: a store saved on voxel terrain
//! is refused at open — by a writer and by a read-only handle — before any migration
//! runs, and is left exactly as it was (an older build still opens it); a stud world
//! or an empty one from before the migration is migrated and keeps every stud row.
//!
//! The old stores are built here the way the older builds made them: this build's own
//! migrations up to the old schema, then the rows.

use std::borrow::Cow;

use game::{RegionCoords, StoredRegion, StudChunkCoords, StudChunkRecord};
use sqlx::Row;
use world_store::{
    Dials, FORMAT_ENTITY_BUNDLE, FORMAT_REGION_META, FORMAT_STUD_CHUNK, FORMAT_STUD_GROUND, FormatKind, OpenMode, Pool,
    StoreError, StoreHandle, StoreUrl, TerrainKind, encode, newest_migration,
};

use crate::codec::{bundle, meta, stud_chunk, stud_ground};
use crate::handle::{each_backend, raw_exec};

const A: RegionCoords = RegionCoords { x: 0, z: 0 };

/// The schema before `world.terrain` (`0003_owner_epochs`).
const BEFORE_TERRAIN_KIND: i64 = 3;
/// The last schema with the voxel table and column (`0004_stud_terrain`).
const LAST_VOXEL_SCHEMA: i64 = 4;

const VOXELS: StoreError = StoreError::OtherTerrain { stored: TerrainKind::Voxels, wanted: TerrainKind::Studs };

/// A pool on `url` that checks and migrates nothing (`open_pool` would refuse or migrate).
pub(crate) async fn raw_pool(url: &StoreUrl) -> Pool {
    match url {
        StoreUrl::Sqlite(path) => {
            let opts = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
            Pool::Sqlite(sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect_with(opts).await.unwrap())
        }
        StoreUrl::Postgres(u) => Pool::Pg(sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(u).await.unwrap()),
        StoreUrl::Memory => panic!("an old store needs a database that outlives its pool"),
    }
}

pub(crate) async fn close(pool: Pool) {
    match pool {
        Pool::Sqlite(p) => p.close().await,
        Pool::Pg(p) => p.close().await,
    }
}

/// A blob as an SQL literal of `url`'s engine.
pub(crate) fn blob(url: &StoreUrl, bytes: &[u8]) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    match url {
        StoreUrl::Postgres(_) => format!("'\\x{hex}'::bytea"),
        _ => format!("x'{hex}'"),
    }
}

/// Makes the store at `url` as a build whose newest migration was `schema` left it:
/// that schema, the world row (`terrain`: the `world.terrain` literal, for a schema
/// that has the column), then `rows`.
pub(crate) fn old_store(url: &StoreUrl, schema: i64, terrain: &str, rows: &[String]) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pool = raw_pool(url).await;
        let mut migrator = match url {
            StoreUrl::Postgres(_) => sqlx::migrate!("migrations/pg"),
            _ => sqlx::migrate!("migrations/sqlite"),
        };
        migrator.migrations = Cow::Owned(migrator.iter().filter(|m| m.version <= schema).cloned().collect());
        let (region, chunk) = (Dials::BUILT.region_blocks, Dials::BUILT.chunk_blocks);
        let (column, value) = if schema > BEFORE_TERRAIN_KIND { (", terrain", format!(", {terrain}")) } else { ("", String::new()) };
        let world = format!(
            "INSERT INTO world (id, format_version, region_blocks, chunk_blocks, content_crc, world_tick, \
             super_region_regions, created_utc{column}) VALUES (1, 1, {region}, {chunk}, 0, 700, 16, '2026-01-01T00:00:00Z'{value})"
        );
        macro_rules! build {
            ($p:expr) => {{
                migrator.run($p).await.unwrap();
                for sql in std::iter::once(&world).chain(rows) {
                    sqlx::query(sql).execute($p).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
                }
            }};
        }
        match &pool {
            Pool::Sqlite(p) => build!(p),
            Pool::Pg(p) => build!(p),
        }
        close(pool).await;
    });
}

/// The rows of a saved voxel region as an old build wrote them (the blobs are never
/// decoded: the refusal does not read them).
fn voxel_rows(url: &StoreUrl, schema: i64) -> Vec<String> {
    let (m, g, c) = (blob(url, &encode(FORMAT_REGION_META, &meta())), blob(url, &[1, 2, 3]), blob(url, &[1, 0xff, 0, 0xff]));
    let region = if schema > BEFORE_TERRAIN_KIND {
        format!("INSERT INTO regions (rx, rz, saved_tick, terrain_version, meta, ground, stud_ground) VALUES (0, 0, 700, 1, {m}, {g}, NULL)")
    } else {
        format!("INSERT INTO regions (rx, rz, saved_tick, terrain_version, meta, ground) VALUES (0, 0, 700, 1, {m}, {g})")
    };
    vec![
        region,
        format!("INSERT INTO chunks (rx, rz, cx, cy, cz, data) VALUES (0, 0, 0, 0, 0, {c})"),
        format!("INSERT INTO chunks (rx, rz, cx, cy, cz, data) VALUES (0, 0, 1, -2, 3, {c})"),
    ]
}

/// The stud chunk rows of [`stud_rows`]' region.
fn stud_chunks() -> Vec<(StudChunkCoords, StudChunkRecord)> {
    vec![(StudChunkCoords { x: 0, y: 0, z: 0 }, stud_chunk()), (StudChunkCoords { x: 1, y: -3, z: 0 }, StudChunkRecord::Pristine)]
}

/// The rows of a saved stud region as a schema-0004 build wrote them: the voxel
/// ground column holds an (encoded) empty list, there are no voxel chunk rows.
fn stud_rows(url: &StoreUrl) -> Vec<String> {
    let b = |bytes: Vec<u8>| blob(url, &bytes);
    let empty_voxel_ground = b(encode(1, &Vec::<u8>::new()));
    let mut rows = vec![
        format!(
            "INSERT INTO regions (rx, rz, saved_tick, terrain_version, meta, ground, stud_ground) VALUES (0, 0, 700, 3, {}, {empty_voxel_ground}, {})",
            b(encode(FORMAT_REGION_META, &meta())),
            b(encode(FORMAT_STUD_GROUND, &stud_ground()))
        ),
        format!(
            "INSERT INTO entities (birth_rx, birth_rz, serial, rx, rz, saved_tick, kind, bundle) VALUES (3, -2, 5, 0, 0, 700, 1, {})",
            b(encode(FORMAT_ENTITY_BUNDLE, &bundle()))
        ),
    ];
    for (c, record) in stud_chunks() {
        rows.push(format!(
            "INSERT INTO stud_chunks (rx, rz, cx, cy, cz, data) VALUES (0, 0, {}, {}, {}, {})",
            c.x,
            c.y,
            c.z,
            b(encode(FORMAT_STUD_CHUNK, &record))
        ));
    }
    rows
}

/// What a refused store must keep: its applied migrations and every row of the
/// world, region and voxel chunk tables, as text.
fn contents(url: &StoreUrl, schema: i64) -> Vec<String> {
    let world = if schema > BEFORE_TERRAIN_KIND { "world_tick, terrain" } else { "world_tick, NULL" };
    let queries = [
        "SELECT version, NULL, NULL, NULL, checksum FROM _sqlx_migrations ORDER BY version".to_string(),
        format!("SELECT id, {world}, NULL, NULL FROM world"),
        "SELECT rx, rz, terrain_version, meta, ground FROM regions ORDER BY rx, rz".to_string(),
        "SELECT cx, cy, cz, NULL, data FROM chunks ORDER BY cx, cy, cz".to_string(),
    ];
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pool = raw_pool(url).await;
        let mut out = Vec::new();
        macro_rules! dump {
            ($p:expr) => {
                for sql in &queries {
                    for r in sqlx::query(sql).fetch_all($p).await.unwrap_or_else(|e| panic!("{sql}: {e}")) {
                        let ints: Vec<Option<i64>> = (0..3).map(|i| r.get(i)).collect();
                        let blobs: Vec<Option<Vec<u8>>> = (3..5).map(|i| r.get(i)).collect();
                        out.push(format!("{sql}: {ints:?} {blobs:?}"));
                    }
                }
            };
        }
        match &pool {
            Pool::Sqlite(p) => dump!(p),
            Pool::Pg(p) => dump!(p),
        }
        close(pool).await;
        out
    })
}

/// The SQLite store's file, byte for byte (`None` on Postgres).
fn file_bytes(url: &StoreUrl) -> Option<Vec<u8>> {
    match url {
        StoreUrl::Sqlite(path) => Some(std::fs::read(path).unwrap()),
        _ => None,
    }
}

/// Opens the old store at `url` every way there is, twice, and checks each open is
/// refused as a voxel world and that nothing in the store changed.
fn assert_refused_untouched(url: &StoreUrl, schema: i64, what: &str) {
    let bytes = file_bytes(url);
    let before = contents(url, schema);
    assert_eq!(file_bytes(url), bytes, "{what}: the comparison itself leaves the file alone");
    for round in 0..2 {
        for mode in [OpenMode::Writer, OpenMode::ReadOnly] {
            let err = StoreHandle::open(url, Dials::BUILT, mode).err().unwrap_or_else(|| panic!("{what}: {mode:?} opened it"));
            assert_eq!(err, VOXELS, "{what}: {mode:?}, round {round}");
            assert_eq!(file_bytes(url), bytes, "{what}: {mode:?} changed the file");
        }
    }
    let after = contents(url, schema);
    assert_eq!(after, before, "{what}: rows or applied migrations changed");
    assert!(after[0].starts_with("SELECT version") && after.iter().any(|l| l.contains(&format!("[Some({schema}), None, None]"))));
    assert!(!after.iter().any(|l| l.contains(&format!("[Some({}), None, None]", schema + 1))), "{what}: a migration was applied");
}

/// Whether `sql` fails on the store (a table or column that is gone).
fn fails(url: &StoreUrl, sql: &str) -> bool {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pool = raw_pool(url).await;
        let failed = match &pool {
            Pool::Sqlite(p) => sqlx::query(sql).fetch_all(p).await.is_err(),
            Pool::Pg(p) => sqlx::query(sql).fetch_all(p).await.is_err(),
        };
        close(pool).await;
        failed
    })
}

fn assert_voxel_tables_gone(url: &StoreUrl) {
    assert!(fails(url, "SELECT COUNT(*) FROM chunks"), "the voxel chunk table is dropped");
    assert!(fails(url, "SELECT ground FROM regions"), "the voxel ground column is dropped");
    assert!(!fails(url, "SELECT stud_ground FROM regions") && !fails(url, "SELECT COUNT(*) FROM stud_chunks"));
}

/// The message says what the world is and what to do, and names no terrain switch.
#[test]
fn voxel_world_refusal_says_reforge() {
    let text = VOXELS.to_string();
    assert!(text.contains("saved on voxel terrain") && text.contains("no longer serves") && text.contains("re-forge"), "{text}");
    assert!(text.contains("nothing in the world was changed"), "{text}");
    assert!(!text.contains("VOXEL_TERRAIN"), "{text}");
    let other = StoreError::OtherTerrain { stored: TerrainKind::Studs, wanted: TerrainKind::Voxels }.to_string();
    assert!(other.contains("studs terrain") && !other.contains("VOXEL_TERRAIN"), "{other}");
}

/// A world a server saved on voxel terrain (schema 0004: marked, with region and
/// chunk rows) is refused by a writer and a read-only handle, and nothing changes.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn saved_voxel_world_is_refused_untouched() {
    each_backend("voxel_saved", true, |url| {
        old_store(url, LAST_VOXEL_SCHEMA, "0", &voxel_rows(url, LAST_VOXEL_SCHEMA));
        assert_refused_untouched(url, LAST_VOXEL_SCHEMA, "a saved voxel world");
    });
}

/// Each sign of a voxel world is enough on its own: chunk rows under an unmarked
/// world row, the mark with no rows at all, and — before `world.terrain` existed —
/// saved regions (which `0004` would have marked; it is not applied either).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn every_sign_of_a_voxel_world_refuses_it() {
    each_backend("voxel_rows", true, |url| {
        old_store(url, LAST_VOXEL_SCHEMA, "NULL", &voxel_rows(url, LAST_VOXEL_SCHEMA)[1..]);
        assert_refused_untouched(url, LAST_VOXEL_SCHEMA, "chunk rows, no mark");
    });
    each_backend("voxel_mark", true, |url| {
        old_store(url, LAST_VOXEL_SCHEMA, "0", &[]);
        assert_refused_untouched(url, LAST_VOXEL_SCHEMA, "the mark, no rows");
    });
    each_backend("voxel_old", true, |url| {
        old_store(url, BEFORE_TERRAIN_KIND, "", &voxel_rows(url, BEFORE_TERRAIN_KIND));
        assert_refused_untouched(url, BEFORE_TERRAIN_KIND, "saved regions before world.terrain");
    });
}

/// A stud world from before the migration is migrated by its first writer and keeps
/// every row: the region, its recipes, its stud chunks, its entities.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stud_world_migrates_and_keeps_its_rows() {
    each_backend("voxel_studs", true, |url| {
        old_store(url, LAST_VOXEL_SCHEMA, "1", &stud_rows(url));
        // A read-only handle never migrates: it asks for a writer first.
        let err = StoreHandle::open(url, Dials::BUILT, OpenMode::ReadOnly).err().unwrap();
        assert_eq!(err, StoreError::OtherFormat { found: LAST_VOXEL_SCHEMA as u32, kind: FormatKind::Schema });
        assert!(!fails(url, "SELECT COUNT(*) FROM chunks"), "the read-only open migrated nothing");

        let h = StoreHandle::open(url, Dials::BUILT, OpenMode::Writer).unwrap();
        assert_eq!(h.terrain_kind().unwrap(), Some(TerrainKind::Studs));
        h.bind_terrain(TerrainKind::Studs).unwrap();
        assert_eq!(h.world_info().unwrap().world_tick, 700);
        assert_eq!(h.stored_regions().unwrap().into_iter().collect::<Vec<_>>(), vec![(A, 3)]);
        let f = h.claim_super_regions(0, vec![(0, 0)]).unwrap();
        let want = StoredRegion {
            meta: meta(),
            entities: vec![bundle()],
            stud_chunks: stud_chunks(),
            stud_ground: stud_ground(),
        };
        assert_eq!(h.load_region(A, &f).unwrap(), Some(want));
        assert!(!h.load_terrain(A).unwrap().unwrap().stud.ground.is_empty());
        h.shutdown();
        assert_voxel_tables_gone(url);
        // Now at this build's schema: a read-only handle opens it, a writer again too.
        StoreHandle::open(url, Dials::BUILT, OpenMode::ReadOnly).unwrap().shutdown();
        StoreHandle::open(url, Dials::BUILT, OpenMode::Writer).unwrap().shutdown();
    });
}

/// A world nobody saved a region in — unmarked at schema 0004, or from before
/// `world.terrain` — is migrated and then takes stud terrain.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn unsaved_old_world_migrates() {
    for (tag, schema) in [("voxel_new4", LAST_VOXEL_SCHEMA), ("voxel_new3", BEFORE_TERRAIN_KIND)] {
        each_backend(tag, true, |url| {
            old_store(url, schema, "NULL", &[]);
            let h = StoreHandle::open(url, Dials::BUILT, OpenMode::Writer).unwrap();
            assert_eq!(h.terrain_kind().unwrap(), None);
            h.bind_terrain(TerrainKind::Studs).unwrap();
            assert!(h.stored_regions().unwrap().is_empty());
            assert_eq!(h.world_info().unwrap().world_tick, 700);
            h.shutdown();
            assert_voxel_tables_gone(url);
        });
    }
}

/// A new store is made at this build's schema, without the voxel table and column;
/// and a world row marked voxels in such a store (nothing voxel left to protect) is
/// refused by `bind_terrain`, the mark kept.
#[test]
fn fresh_store_has_no_voxel_tables() {
    each_backend("voxel_fresh", true, |url| {
        let h = StoreHandle::open(url, Dials::BUILT, OpenMode::Writer).unwrap();
        assert_eq!(h.terrain_kind().unwrap(), None);
        h.shutdown();
        assert_voxel_tables_gone(url);
        assert_eq!(newest_migration(), 5, "0005_drop_voxel_terrain is this build's newest migration");
        StoreHandle::open(url, Dials::BUILT, OpenMode::ReadOnly).unwrap().shutdown();

        raw_exec(url, "UPDATE world SET terrain = 0");
        let h = StoreHandle::open(url, Dials::BUILT, OpenMode::Writer).unwrap();
        assert_eq!(h.bind_terrain(TerrainKind::Studs).unwrap_err(), VOXELS);
        assert_eq!(h.terrain_kind().unwrap(), Some(TerrainKind::Voxels));
        h.shutdown();
    });
}
