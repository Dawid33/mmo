//! Opening a store: connect, (a writer) take SQLite's file lock and migrate,
//! (read-only) check the schema is this build's; then check the world row.

use std::fs::File;
use std::path::Path;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::Row;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use crate::{Dials, FormatKind, STORE_FORMAT_VERSION, STORE_POOL_MAX, StoreError, StoreUrl};

/// The connection pool of either engine.
#[derive(Clone)]
pub enum Pool {
    Sqlite(sqlx::SqlitePool),
    Pg(sqlx::PgPool),
}

/// How a store is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// Migrates and writes. SQLite: holds the file lock (one writer process);
    /// Postgres: no lock — writes are fenced by owner epochs (`fence.rs`).
    Writer,
    /// Takes no lock, never migrates (refused unless the schema is exactly
    /// this build's, [`newest_migration`]) and refuses every write with
    /// [`StoreError::ReadOnly`]: tools reading a store a server is using.
    ReadOnly,
}

/// Holds SQLite's world lock (a flock on `<db>.lock`) for as long as it lives.
#[allow(dead_code)] // held for its Drop
pub enum LockGuard {
    None,
    File(File),
}

fn lock_path(db: &Path) -> std::path::PathBuf {
    let mut name = db.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".lock");
    db.with_file_name(name)
}

/// The newest migration this build knows (both engines carry the same versions).
pub fn newest_migration() -> i64 {
    sqlx::migrate!("migrations/pg").iter().map(|m| m.version).max().unwrap_or(0)
}

/// Open the store at `url` for a build with `dials`. A writer creates a
/// missing store, takes the lock BEFORE migrating (the returned guard must
/// outlive the pool's use) and writes a new world row; a read-only open
/// creates nothing and takes no lock.
pub async fn open_pool(url: &StoreUrl, dials: Dials, mode: OpenMode) -> Result<(Pool, LockGuard), StoreError> {
    let writer = mode == OpenMode::Writer;
    let (pool, guard) = match url {
        StoreUrl::Memory => {
            let opts = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
            // One connection that never expires: each in-memory connection is its own database.
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .idle_timeout(None)
                .max_lifetime(None)
                .connect_with(opts)
                .await?;
            (Pool::Sqlite(pool), LockGuard::None)
        }
        StoreUrl::Sqlite(path) => {
            let guard = if writer {
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir)
                        .map_err(|e| StoreError::Backend(format!("create {}: {e}", dir.display())))?;
                }
                let lock_file = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(lock_path(path))
                    .map_err(|e| StoreError::Backend(format!("open lock file: {e}")))?;
                match lock_file.try_lock() {
                    Ok(()) => {}
                    Err(std::fs::TryLockError::WouldBlock) => {
                        return Err(StoreError::Locked(lock_path(path).display().to_string()));
                    }
                    Err(std::fs::TryLockError::Error(e)) => {
                        return Err(StoreError::Backend(format!("lock file: {e}")));
                    }
                }
                LockGuard::File(lock_file)
            } else {
                LockGuard::None
            };
            let opts = SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(writer)
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal)
                .foreign_keys(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(STORE_POOL_MAX)
                .connect_with(opts)
                .await?;
            (Pool::Sqlite(pool), guard)
        }
        StoreUrl::Postgres(u) => {
            let opts = PgConnectOptions::from_str(u)?;
            let pool = PgPoolOptions::new()
                .max_connections(STORE_POOL_MAX)
                .connect_with(opts)
                .await?;
            (Pool::Pg(pool), LockGuard::None)
        }
    };

    if writer {
        match &pool {
            Pool::Sqlite(p) => sqlx::migrate!("migrations/sqlite").run(p).await?,
            Pool::Pg(p) => sqlx::migrate!("migrations/pg").run(p).await?,
        }
    } else {
        check_schema(&pool).await?;
    }
    check_world_row(&pool, dials, writer).await?;
    Ok((pool, guard))
}

/// A read-only open's schema check: the newest applied migration must be
/// this build's newest (a store never migrated, or migrated by a newer build,
/// is refused — reading it could misread its rows).
async fn check_schema(pool: &Pool) -> Result<(), StoreError> {
    const TABLE_SQLITE: &str = "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'";
    const TABLE_PG: &str = "SELECT COUNT(*) FROM information_schema.tables WHERE table_name = '_sqlx_migrations' \
         AND table_schema = current_schema()";
    const NEWEST: &str = "SELECT MAX(version) FROM _sqlx_migrations WHERE success";
    let found: Option<i64> = match pool {
        Pool::Sqlite(p) => {
            let tables: i64 = sqlx::query(TABLE_SQLITE).fetch_one(p).await?.try_get(0)?;
            if tables == 0 { None } else { sqlx::query(NEWEST).fetch_one(p).await?.try_get(0)? }
        }
        Pool::Pg(p) => {
            let tables: i64 = sqlx::query(TABLE_PG).fetch_one(p).await?.try_get(0)?;
            if tables == 0 { None } else { sqlx::query(NEWEST).fetch_one(p).await?.try_get(0)? }
        }
    };
    let found = found.unwrap_or(0);
    if found != newest_migration() {
        return Err(StoreError::OtherFormat { found: found.clamp(0, u32::MAX as i64) as u32, kind: FormatKind::Schema });
    }
    Ok(())
}

/// Checks the world row's format and dials; a writer first inserts it if
/// missing. The insert is `ON CONFLICT DO NOTHING` and the row is read back
/// after it: two writers opening a fresh database at once (no world lock on
/// Postgres) both succeed, and the one whose dials lost is refused.
async fn check_world_row(pool: &Pool, dials: Dials, writer: bool) -> Result<(), StoreError> {
    const SELECT: &str = "SELECT format_version, region_blocks, chunk_blocks FROM world WHERE id = 1";
    const INSERT: &str = "INSERT INTO world (id, format_version, region_blocks, chunk_blocks, content_crc, \
         world_tick, super_region_regions, created_utc) VALUES (1, $1, $2, $3, 0, 0, $4, $5) \
         ON CONFLICT (id) DO NOTHING";
    if writer {
        let created = utc_now();
        macro_rules! insert {
            ($p:expr) => {
                sqlx::query(INSERT)
                    .bind(STORE_FORMAT_VERSION as i64)
                    .bind(dials.region_blocks as i64)
                    .bind(dials.chunk_blocks as i64)
                    .bind(i64::from(game::DEFAULT_SUPER_REGION_REGIONS))
                    .bind(created)
                    .execute($p)
                    .await
                    .map(|_| ())?
            };
        }
        match pool {
            Pool::Sqlite(p) => insert!(p),
            Pool::Pg(p) => insert!(p),
        }
    }
    let row: Option<(i64, i64, i64)> = match pool {
        Pool::Sqlite(p) => sqlx::query(SELECT)
            .fetch_optional(p)
            .await?
            .map(|r| (r.get(0), r.get(1), r.get(2))),
        Pool::Pg(p) => sqlx::query(SELECT)
            .fetch_optional(p)
            .await?
            .map(|r| (r.get(0), r.get(1), r.get(2))),
    };
    let Some((format, region, chunk)) = row else {
        return Err(StoreError::Corrupt("the world row is missing".into()));
    };
    if format != STORE_FORMAT_VERSION as i64 {
        return Err(StoreError::OtherFormat {
            found: format.clamp(0, u32::MAX as i64) as u32,
            kind: crate::FormatKind::Store,
        });
    }
    let stored = Dials { region_blocks: region as u32, chunk_blocks: chunk as u32 };
    if stored != dials {
        return Err(StoreError::OtherDials { stored, built: dials });
    }
    Ok(())
}

/// `YYYY-MM-DDTHH:MM:SSZ` for now (civil-from-days, no date crate).
pub(crate) fn utc_now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}
