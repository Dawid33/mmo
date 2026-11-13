//! Old worlds and migration `0011_voxel_terrain_returns`: a store with saved regions
//! that has not had `0011` — a stud world, or a voxel world from before the stud switch,
//! whose chunks `0005_drop_voxel_terrain` would drop — is refused at open, by a writer and
//! by a read-only handle, before any migration runs, and is left exactly as it was. An
//! empty store migrates through `0011` and holds the voxel tables again.
//!
//! The old stores are built here the way the older builds made them: this build's own
//! migrations up to the old schema, then the rows.

use std::borrow::Cow;

use game::ColumnCoords;
use sqlx::Row;
use world_store::{
    Dials, FORMAT_REGION_GROUND, FORMAT_REGION_META, FormatKind, OpenMode, Pool, StoreError, StoreHandle, StoreUrl, decode,
    encode, newest_migration,
};

use crate::codec::meta;
use crate::handle::each_backend;

/// The schema before `world.terrain` (`0003_owner_epochs`): the last one a voxel build wrote.
const BEFORE_TERRAIN_KIND: i64 = 3;
/// The newest schema of the stud builds (`0010_entities_by_id`).
const LAST_STUD_SCHEMA: i64 = 10;
/// The migration that brings the voxel tables back.
const VOXEL_RETURN: i64 = 11;

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

/// Runs this build's migrations up to `schema` on `url`, then `rows` (no world row).
pub(crate) fn migrate_to(url: &StoreUrl, schema: i64, rows: &[String]) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pool = raw_pool(url).await;
        let mut migrator = match url {
            StoreUrl::Postgres(_) => sqlx::migrate!("migrations/pg"),
            _ => sqlx::migrate!("migrations/sqlite"),
        };
        migrator.migrations = Cow::Owned(migrator.iter().filter(|m| m.version <= schema).cloned().collect());
        macro_rules! build {
            ($p:expr) => {{
                migrator.run($p).await.unwrap();
                for sql in rows {
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

/// Makes the store at `url` as a build whose newest migration was `schema` left it:
/// that schema, the world row (`terrain`: the `world.terrain` literal, for a schema
/// that has the column), then `rows`.
pub(crate) fn old_store(url: &StoreUrl, schema: i64, terrain: &str, rows: &[String]) {
    let (region, chunk) = (Dials::BUILT.region_blocks, Dials::BUILT.chunk_blocks);
    let (column, value) = if schema > BEFORE_TERRAIN_KIND { (", terrain", format!(", {terrain}")) } else { ("", String::new()) };
    let world = format!(
        "INSERT INTO world (id, format_version, region_blocks, chunk_blocks, content_crc, world_tick, \
         super_region_regions, created_utc{column}) VALUES (1, 1, {region}, {chunk}, 0, 700, 16, '2026-01-01T00:00:00Z'{value})"
    );
    migrate_to(url, schema, &std::iter::once(world).chain(rows.iter().cloned()).collect::<Vec<_>>());
}

/// The first column of `sql`'s only row on the store, as an integer (`None`: NULL or no row).
fn scalar(url: &StoreUrl, sql: &str) -> Option<i64> {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pool = raw_pool(url).await;
        let v: Option<i64> = match &pool {
            Pool::Sqlite(p) => sqlx::query(sql).fetch_optional(p).await.unwrap().and_then(|r| r.get(0)),
            Pool::Pg(p) => sqlx::query(sql).fetch_optional(p).await.unwrap().and_then(|r| r.get(0)),
        };
        close(pool).await;
        v
    })
}

/// The newest migration applied to the store.
fn applied(url: &StoreUrl) -> Option<i64> {
    scalar(url, "SELECT MAX(version) FROM _sqlx_migrations WHERE success")
}

/// Whether `sql` fails on the store (a table or column that is not there).
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

/// The SQLite store's file, byte for byte (`None` on Postgres).
fn file_bytes(url: &StoreUrl) -> Option<Vec<u8>> {
    match url {
        StoreUrl::Sqlite(path) => Some(std::fs::read(path).unwrap()),
        _ => None,
    }
}

/// Opens the old store at `url` every way there is and checks each open is refused with
/// a message that says to re-forge, and that neither the file nor its migrations changed.
fn assert_refused_untouched(url: &StoreUrl, schema: i64, what: &str) {
    let bytes = file_bytes(url);
    for mode in [OpenMode::Writer, OpenMode::ReadOnly] {
        let err = StoreHandle::open(url, Dials::BUILT, mode).err().unwrap_or_else(|| panic!("{what}: {mode:?} opened it"));
        let text = err.to_string();
        assert!(text.contains("re-forge"), "{what}: {mode:?}: {text}");
        assert_eq!(file_bytes(url), bytes, "{what}: {mode:?} changed the file");
        assert_eq!(applied(url), Some(schema), "{what}: {mode:?} applied a migration");
    }
}

/// A stud world (schema 0010, `world.terrain` = studs, a saved region) is refused before
/// any migration: it stays at 0010 with its stud table.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_stud_world_is_refused_before_any_migration() {
    each_backend("stud_refused", true, |url| {
        let m = blob(url, &encode(FORMAT_REGION_META, &meta()));
        let region = format!(
            "INSERT INTO regions (rx, rz, saved_tick, terrain_version, meta, stud_ground) VALUES (0, 0, 700, 1, {m}, {})",
            blob(url, &[1, 2, 3])
        );
        let chunk = format!("INSERT INTO stud_chunks (rx, rz, cx, cy, cz, data) VALUES (0, 0, 0, 0, 0, {})", blob(url, &[1, 4]));
        old_store(url, LAST_STUD_SCHEMA, "1", &[region, chunk]);
        assert_refused_untouched(url, LAST_STUD_SCHEMA, "a stud world");
        let err = StoreHandle::open(url, Dials::BUILT, OpenMode::Writer).err().unwrap().to_string();
        assert!(err.contains("stud"), "names the world's kind: {err}");
        assert_eq!(applied(url), Some(LAST_STUD_SCHEMA));
        assert_eq!(scalar(url, "SELECT COUNT(*) FROM stud_chunks"), Some(1), "the stud table and its row are still there");
    });
}

/// A voxel world saved before the stud switch (schema 0003, a region and a chunk row) is
/// refused too — `0005` would drop its chunks — and keeps its chunk row.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_voxel_world_from_before_the_stud_switch_is_refused() {
    each_backend("voxel_refused", true, |url| {
        let m = blob(url, &encode(FORMAT_REGION_META, &meta()));
        let region = format!(
            "INSERT INTO regions (rx, rz, saved_tick, terrain_version, meta, ground) VALUES (0, 0, 700, 1, {m}, {})",
            blob(url, &[1, 2, 3])
        );
        let chunk = format!("INSERT INTO chunks (rx, rz, cx, cy, cz, data) VALUES (0, 0, 1, -2, 3, {})", blob(url, &[1, 0xff]));
        old_store(url, BEFORE_TERRAIN_KIND, "", &[region, chunk]);
        assert_refused_untouched(url, BEFORE_TERRAIN_KIND, "a voxel world from before the stud switch");
        let err = StoreHandle::open(url, Dials::BUILT, OpenMode::Writer).err().unwrap().to_string();
        assert!(err.contains("voxel"), "names the world's kind: {err}");
        assert_eq!(scalar(url, "SELECT COUNT(*) FROM chunks"), Some(1), "the chunk row is still there");
    });
}

/// A fresh store migrates through `0011`: the voxel table and column are back, the stud
/// ones gone, the world row is tied to voxels, and `regions.ground`'s default (the value
/// `0011` gives rows it adds the column to) is an encoded empty list.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn an_empty_store_migrates_to_11_and_holds_voxel_tables() {
    each_backend("voxel_fresh", true, |url| {
        StoreHandle::open(url, Dials::BUILT, OpenMode::Writer).unwrap().shutdown();
        assert_eq!(applied(url), Some(VOXEL_RETURN));
        assert_eq!(newest_migration(), VOXEL_RETURN);
        assert!(!fails(url, "SELECT COUNT(*) FROM chunks"), "the voxel chunk table exists");
        assert!(!fails(url, "SELECT ground FROM regions"), "the voxel ground column exists");
        assert!(fails(url, "SELECT COUNT(*) FROM stud_chunks"), "the stud chunk table is gone");
        assert!(fails(url, "SELECT stud_ground FROM regions"), "the stud ground column is gone");
        assert_eq!(scalar(url, "SELECT terrain FROM world WHERE id = 1"), Some(0), "a new world is a voxel world");

        // A row written without `ground` takes the column default: an empty list.
        let m = blob(url, &encode(FORMAT_REGION_META, &meta()));
        migrate_to(url, VOXEL_RETURN, &[format!(
            "INSERT INTO regions (rx, rz, saved_tick, terrain_version, meta) VALUES (5, 5, 1, 0, {m})"
        )]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let ground: Vec<u8> = rt.block_on(async {
            let pool = raw_pool(url).await;
            let sql = "SELECT ground FROM regions WHERE rx = 5 AND rz = 5";
            let g = match &pool {
                Pool::Sqlite(p) => sqlx::query(sql).fetch_one(p).await.unwrap().get(0),
                Pool::Pg(p) => sqlx::query(sql).fetch_one(p).await.unwrap().get(0),
            };
            close(pool).await;
            g
        });
        let decoded: Vec<(ColumnCoords, game::ColumnGround)> = decode(FORMAT_REGION_GROUND, &ground).unwrap();
        assert!(decoded.is_empty());
    });
}

/// A read-only handle never migrates: a store at 0010 (no saved region, so not refused as
/// a stud world) asks for a writer first.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_read_only_handle_needs_migration_11() {
    each_backend("voxel_read_only", true, |url| {
        old_store(url, LAST_STUD_SCHEMA, "NULL", &[]);
        let err = StoreHandle::open(url, Dials::BUILT, OpenMode::ReadOnly).err().expect("a read-only open of schema 0010");
        assert_eq!(err, StoreError::OtherFormat { found: LAST_STUD_SCHEMA as u32, kind: FormatKind::Schema });
        assert_eq!(applied(url), Some(LAST_STUD_SCHEMA), "nothing migrated");
    });
}
