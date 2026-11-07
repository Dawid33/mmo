//! `LoginStore`: the login database on SQLite or Postgres (runtime `sqlx::query()`).

use std::str::FromStr;

use login_api::WorldStatus;
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::Row;

use crate::LOGIN_POOL_MAX;

pub type AccountId = i64;

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("{0}")]
    Backend(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRow {
    pub account_id: AccountId,
    pub email: String,
    pub password_hash: String,
    pub email_verified_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The token was live; it is now revoked (rotated).
    Valid { account: AccountId, family: [u8; 16] },
    /// The token was already revoked: the caller revokes its family.
    Reused { account: AccountId, family: [u8; 16] },
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldRow {
    pub world_id: String,
    pub name: String,
    pub status: WorldStatus,
    pub quic: Option<String>,
    pub webtransport: Option<String>,
    pub cert_hash: Option<String>,
    pub created_at: i64,
}

#[derive(Clone)]
enum Pool {
    Sqlite(sqlx::SqlitePool),
    Pg(sqlx::PgPool),
}

/// Run the same expression against whichever pool is open.
macro_rules! on {
    ($s:expr, $p:ident => $e:expr) => {
        match &$s.pool {
            Pool::Sqlite($p) => $e,
            Pool::Pg($p) => $e,
        }
    };
}

/// Like `on!` for statements whose result is discarded (the row types differ per engine).
macro_rules! exec {
    ($s:expr, $p:ident => $e:expr) => {
        match &$s.pool {
            Pool::Sqlite($p) => {
                $e;
            }
            Pool::Pg($p) => {
                $e;
            }
        }
    };
}

#[derive(Clone)]
pub struct LoginStore {
    pool: Pool,
}

fn family_of(v: Vec<u8>) -> [u8; 16] {
    v.try_into().unwrap_or([0; 16])
}

fn status_int(s: WorldStatus) -> i64 {
    match s {
        WorldStatus::Open => 0,
        WorldStatus::Closed => 1,
    }
}

fn status_of(i: i64) -> WorldStatus {
    if i == 0 { WorldStatus::Open } else { WorldStatus::Closed }
}

macro_rules! account_row {
    ($r:expr) => {
        AccountRow {
            account_id: $r.get(0),
            email: $r.get(1),
            password_hash: $r.get(2),
            email_verified_at: $r.get(3),
            created_at: $r.get(4),
        }
    };
}

macro_rules! world_row {
    ($r:expr) => {
        WorldRow {
            world_id: $r.get(0),
            name: $r.get(1),
            status: status_of($r.get(2)),
            quic: $r.get(3),
            webtransport: $r.get(4),
            cert_hash: $r.get(5),
            created_at: $r.get(6),
        }
    };
}

const ACCOUNT_COLS: &str = "account_id, email, password_hash, email_verified_at, created_at";
const WORLD_COLS: &str = "world_id, name, status, quic, webtransport, cert_hash, created_at";

impl LoginStore {
    /// Open and migrate: a `postgres://` URL, `sqlite::memory:`, `sqlite:<path>` or a bare path.
    /// No lock is taken: several login replicas may share one Postgres database.
    pub async fn open(url: &str) -> Result<LoginStore, LoginError> {
        let pool = if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            Pool::Pg(PgPoolOptions::new().max_connections(LOGIN_POOL_MAX).connect(url).await?)
        } else if url == "sqlite::memory:" {
            let opts = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
            // One connection that never expires: each in-memory connection is its own database.
            Pool::Sqlite(
                SqlitePoolOptions::new()
                    .max_connections(1)
                    .idle_timeout(None)
                    .max_lifetime(None)
                    .connect_with(opts)
                    .await?,
            )
        } else {
            let path = std::path::Path::new(url.strip_prefix("sqlite:").unwrap_or(url));
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)
                    .map_err(|e| LoginError::Backend(format!("create {}: {e}", dir.display())))?;
            }
            let opts = SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal)
                .foreign_keys(true);
            Pool::Sqlite(SqlitePoolOptions::new().max_connections(LOGIN_POOL_MAX).connect_with(opts).await?)
        };
        match &pool {
            Pool::Sqlite(p) => sqlx::migrate!("migrations/sqlite").run(p).await?,
            Pool::Pg(p) => sqlx::migrate!("migrations/pg").run(p).await?,
        }
        Ok(LoginStore { pool })
    }

    /// `None` = the email already has an account.
    pub async fn create_account(
        &self,
        email: &str,
        password_hash: &str,
        now: i64,
    ) -> Result<Option<AccountId>, LoginError> {
        const SQL: &str = "INSERT INTO accounts (email, password_hash, created_at) VALUES ($1, $2, $3) \
             ON CONFLICT (email) DO NOTHING RETURNING account_id";
        Ok(on!(self, p => sqlx::query(SQL).bind(email).bind(password_hash).bind(now).fetch_optional(p).await?
            .map(|r| r.get::<i64, _>(0))))
    }

    pub async fn account_by_email(&self, email: &str) -> Result<Option<AccountRow>, LoginError> {
        let sql = format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE email = $1");
        Ok(on!(self, p => sqlx::query(&sql).bind(email).fetch_optional(p).await?.map(|r| account_row!(r))))
    }

    pub async fn account(&self, id: AccountId) -> Result<Option<AccountRow>, LoginError> {
        let sql = format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE account_id = $1");
        Ok(on!(self, p => sqlx::query(&sql).bind(id).fetch_optional(p).await?.map(|r| account_row!(r))))
    }

    pub async fn set_password(&self, id: AccountId, hash: &str) -> Result<(), LoginError> {
        const SQL: &str = "UPDATE accounts SET password_hash = $1 WHERE account_id = $2";
        exec!(self, p => sqlx::query(SQL).bind(hash).bind(id).execute(p).await?);
        Ok(())
    }

    pub async fn mark_verified(&self, id: AccountId, now: i64) -> Result<(), LoginError> {
        const SQL: &str = "UPDATE accounts SET email_verified_at = $1 WHERE account_id = $2 AND email_verified_at IS NULL";
        exec!(self, p => sqlx::query(SQL).bind(now).bind(id).execute(p).await?);
        Ok(())
    }

    pub async fn insert_refresh(
        &self,
        hash: [u8; 32],
        account: AccountId,
        family: [u8; 16],
        expires_at: i64,
    ) -> Result<(), LoginError> {
        const SQL: &str = "INSERT INTO refresh_tokens (token_hash, account_id, family, expires_at) VALUES ($1, $2, $3, $4)";
        exec!(self, p => sqlx::query(SQL)
            .bind(&hash[..]).bind(account).bind(&family[..]).bind(expires_at)
            .execute(p).await?);
        Ok(())
    }

    /// Atomic: of concurrent takes of one token only one sees `Valid`.
    pub async fn take_refresh(&self, hash: [u8; 32], now: i64) -> Result<RefreshOutcome, LoginError> {
        const TAKE: &str = "UPDATE refresh_tokens SET revoked_at = $1 \
             WHERE token_hash = $2 AND revoked_at IS NULL AND expires_at > $1 RETURNING account_id, family";
        const LOOK: &str = "SELECT account_id, family, revoked_at FROM refresh_tokens WHERE token_hash = $1";
        let taken: Option<(i64, Vec<u8>)> =
            on!(self, p => sqlx::query(TAKE).bind(now).bind(&hash[..]).fetch_optional(p).await?
                .map(|r| (r.get(0), r.get(1))));
        if let Some((account, family)) = taken {
            return Ok(RefreshOutcome::Valid { account, family: family_of(family) });
        }
        let seen: Option<(i64, Vec<u8>, Option<i64>)> =
            on!(self, p => sqlx::query(LOOK).bind(&hash[..]).fetch_optional(p).await?
                .map(|r| (r.get(0), r.get(1), r.get(2))));
        Ok(match seen {
            Some((account, family, Some(_))) => RefreshOutcome::Reused { account, family: family_of(family) },
            // No row, or expired and never used: nothing to revoke.
            _ => RefreshOutcome::Unknown,
        })
    }

    pub async fn revoke_family(&self, family: [u8; 16], now: i64) -> Result<(), LoginError> {
        const SQL: &str = "UPDATE refresh_tokens SET revoked_at = $1 WHERE family = $2 AND revoked_at IS NULL";
        exec!(self, p => sqlx::query(SQL).bind(now).bind(&family[..]).execute(p).await?);
        Ok(())
    }

    pub async fn revoke_account_refresh(&self, account: AccountId, now: i64) -> Result<(), LoginError> {
        const SQL: &str = "UPDATE refresh_tokens SET revoked_at = $1 WHERE account_id = $2 AND revoked_at IS NULL";
        exec!(self, p => sqlx::query(SQL).bind(now).bind(account).execute(p).await?);
        Ok(())
    }

    pub async fn insert_email_token(
        &self,
        hash: [u8; 32],
        account: AccountId,
        kind: i64,
        expires_at: i64,
    ) -> Result<(), LoginError> {
        const SQL: &str = "INSERT INTO email_tokens (token_hash, account_id, kind, expires_at) VALUES ($1, $2, $3, $4)";
        exec!(self, p => sqlx::query(SQL)
            .bind(&hash[..]).bind(account).bind(kind).bind(expires_at)
            .execute(p).await?);
        Ok(())
    }

    /// Single use, atomic: the account the token belongs to, once, if unexpired and of `kind`.
    pub async fn use_email_token(&self, hash: [u8; 32], kind: i64, now: i64) -> Result<Option<AccountId>, LoginError> {
        const SQL: &str = "UPDATE email_tokens SET used_at = $1 \
             WHERE token_hash = $2 AND kind = $3 AND used_at IS NULL AND expires_at > $1 RETURNING account_id";
        Ok(on!(self, p => sqlx::query(SQL).bind(now).bind(&hash[..]).bind(kind).fetch_optional(p).await?
            .map(|r| r.get::<i64, _>(0))))
    }

    /// Insert or replace a world of the registry (`created_at` of an existing one is kept).
    pub async fn put_world(&self, w: &WorldRow) -> Result<(), LoginError> {
        const SQL: &str = "INSERT INTO worlds (world_id, name, status, quic, webtransport, cert_hash, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (world_id) DO UPDATE SET \
             name = excluded.name, status = excluded.status, quic = excluded.quic, \
             webtransport = excluded.webtransport, cert_hash = excluded.cert_hash";
        exec!(self, p => sqlx::query(SQL)
            .bind(&w.world_id).bind(&w.name).bind(status_int(w.status))
            .bind(&w.quic).bind(&w.webtransport).bind(&w.cert_hash).bind(w.created_at)
            .execute(p).await?);
        Ok(())
    }

    /// Whether a world was removed.
    pub async fn remove_world(&self, id: &str) -> Result<bool, LoginError> {
        const SQL: &str = "DELETE FROM worlds WHERE world_id = $1";
        let n = on!(self, p => sqlx::query(SQL).bind(id).execute(p).await?.rows_affected());
        Ok(n > 0)
    }

    pub async fn worlds(&self) -> Result<Vec<WorldRow>, LoginError> {
        let sql = format!("SELECT {WORLD_COLS} FROM worlds ORDER BY world_id");
        Ok(on!(self, p => sqlx::query(&sql).fetch_all(p).await?.into_iter().map(|r| world_row!(r)).collect()))
    }

    pub async fn world(&self, id: &str) -> Result<Option<WorldRow>, LoginError> {
        let sql = format!("SELECT {WORLD_COLS} FROM worlds WHERE world_id = $1");
        Ok(on!(self, p => sqlx::query(&sql).bind(id).fetch_optional(p).await?.map(|r| world_row!(r))))
    }

    /// The account's player token in a world, created (from `new_token`) on first use.
    pub async fn player_token_for(
        &self,
        account: AccountId,
        world: &str,
        now: i64,
        new_token: impl FnOnce() -> String,
    ) -> Result<String, LoginError> {
        const SEL: &str = "SELECT player_token FROM world_players WHERE account_id = $1 AND world_id = $2";
        const INS: &str = "INSERT INTO world_players (account_id, world_id, player_token, created_at) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (account_id, world_id) DO NOTHING";
        let have: Option<String> =
            on!(self, p => sqlx::query(SEL).bind(account).bind(world).fetch_optional(p).await?.map(|r| r.get(0)));
        if let Some(t) = have {
            return Ok(t);
        }
        let fresh = new_token();
        exec!(self, p => sqlx::query(INS).bind(account).bind(world).bind(&fresh).bind(now).execute(p).await?);
        // Re-read: a concurrent first join may have stored its token first.
        Ok(on!(self, p => sqlx::query(SEL).bind(account).bind(world).fetch_one(p).await?.get(0)))
    }

    /// Upsert; the new epoch, starting at 1.
    pub async fn bump_session(&self, account: AccountId, world: &str, now: i64) -> Result<u64, LoginError> {
        const SQL: &str = "INSERT INTO sessions (account_id, world_id, session_epoch, started_at) \
             VALUES ($1, $2, 1, $3) ON CONFLICT (account_id, world_id) DO UPDATE SET \
             session_epoch = sessions.session_epoch + 1, started_at = excluded.started_at \
             RETURNING session_epoch";
        let epoch: i64 =
            on!(self, p => sqlx::query(SQL).bind(account).bind(world).bind(now).fetch_one(p).await?.get(0));
        Ok(epoch as u64)
    }
}
