//! Opening a store: connect, take the world lock, migrate, check the world row.

use std::fs::File;
use std::path::Path;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Connection, PgConnection, Row};

use crate::{Dials, STORE_FORMAT_VERSION, STORE_POOL_MAX, StoreError, StoreUrl, WORLD_LOCK_KEY};

/// The connection pool of either engine.
#[derive(Clone)]
pub enum Pool {
    Sqlite(sqlx::SqlitePool),
    Pg(sqlx::PgPool),
}

/// Holds the world lock for as long as it lives: a flock on `<db>.lock` (SQLite)
/// or a session advisory lock on a dedicated connection (Postgres).
#[allow(dead_code)] // held for its Drop
pub enum LockGuard {
    None,
    File(File),
    Pg(PgConnection),
}

fn lock_path(db: &Path) -> std::path::PathBuf {
    let mut name = db.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".lock");
    db.with_file_name(name)
}

/// Open (creating it if missing) the store at `url` for a build with `dials`.
/// The lock is taken BEFORE migrating; the returned guard must outlive the pool's use.
pub async fn open_pool(url: &StoreUrl, dials: Dials) -> Result<(Pool, LockGuard), StoreError> {
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
            let opts = SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal)
                .foreign_keys(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(STORE_POOL_MAX)
                .connect_with(opts)
                .await?;
            (Pool::Sqlite(pool), LockGuard::File(lock_file))
        }
        StoreUrl::Postgres(u) => {
            let opts = PgConnectOptions::from_str(u)?;
            let mut lock_conn = PgConnection::connect_with(&opts).await?;
            let got: bool = sqlx::query("SELECT pg_try_advisory_lock($1)")
                .bind(WORLD_LOCK_KEY)
                .fetch_one(&mut lock_conn)
                .await?
                .get(0);
            if !got {
                return Err(StoreError::Locked("postgres advisory lock".into()));
            }
            let pool = PgPoolOptions::new()
                .max_connections(STORE_POOL_MAX)
                .connect_with(opts)
                .await?;
            (Pool::Pg(pool), LockGuard::Pg(lock_conn))
        }
    };

    match &pool {
        Pool::Sqlite(p) => sqlx::migrate!("migrations/sqlite").run(p).await?,
        Pool::Pg(p) => sqlx::migrate!("migrations/pg").run(p).await?,
    }
    check_world_row(&pool, dials).await?;
    Ok((pool, guard))
}

async fn check_world_row(pool: &Pool, dials: Dials) -> Result<(), StoreError> {
    const SELECT: &str = "SELECT format_version, region_blocks, chunk_blocks FROM world WHERE id = 1";
    const INSERT: &str = "INSERT INTO world (id, format_version, region_blocks, chunk_blocks, content_crc, \
         world_tick, super_region_regions, created_utc) VALUES (1, $1, $2, $3, 0, 0, 16, $4)";
    let created = utc_now();
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
        macro_rules! insert {
            ($p:expr) => {
                sqlx::query(INSERT)
                    .bind(STORE_FORMAT_VERSION as i64)
                    .bind(dials.region_blocks as i64)
                    .bind(dials.chunk_blocks as i64)
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
        return Ok(());
    };
    if format != STORE_FORMAT_VERSION as i64 {
        return Err(StoreError::OtherFormat { found: format.clamp(0, u32::MAX as i64) as u32 });
    }
    let stored = Dials { region_blocks: region as u32, chunk_blocks: chunk as u32 };
    if stored != dials {
        return Err(StoreError::OtherDials { stored, built: dials });
    }
    Ok(())
}

/// `YYYY-MM-DDTHH:MM:SSZ` for now (civil-from-days, no date crate).
fn utc_now() -> String {
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
