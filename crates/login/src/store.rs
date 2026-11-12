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
    /// The world host's service link (`host:port`): `join` pushes `SpawnPlayer` there and waits
    /// for its ack. `None` = no push (R2a: the host checks tickets by epoch alone).
    pub host_link: Option<String>,
    /// The SHA-256 pin of the host link's certificate (64 hex digits).
    pub host_link_cert: Option<String>,
}

/// A kick the world host has not acknowledged yet (`pending_kicks`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingKick {
    pub world_id: String,
    pub player_token: String,
    /// Revokes the player's sessions and orders up to this epoch.
    pub session_epoch: u64,
    pub reason: String,
}

/// A session of an account that has not ended: where to send a `Kick`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenSession {
    pub world_id: String,
    pub player_token: String,
    pub session_epoch: u64,
}

#[derive(Clone)]
enum Pool {
    Sqlite(sqlx::SqlitePool),
    Pg(sqlx::PgPool),
}

/// Run the same expression against whichever pool is open.
const SET_PASSWORD: &str = "UPDATE accounts SET password_hash = $1 WHERE account_id = $2";
const REVOKE_ACCOUNT: &str = "UPDATE refresh_tokens SET revoked_at = $1 WHERE account_id = $2 AND revoked_at IS NULL";
const QUEUE_KICKS: &str = "INSERT INTO pending_kicks (world_id, player_token, session_epoch, reason, created_at) \
     SELECT s.world_id, p.player_token, s.session_epoch, CAST($2 AS TEXT), CAST($3 AS BIGINT) \
     FROM sessions s \
     JOIN world_players p ON p.account_id = s.account_id AND p.world_id = s.world_id \
     JOIN worlds w ON w.world_id = s.world_id \
     WHERE s.account_id = $1 AND s.ended_at IS NULL AND w.host_link IS NOT NULL \
     ON CONFLICT (world_id, player_token) DO UPDATE SET \
     session_epoch = excluded.session_epoch, reason = excluded.reason, created_at = excluded.created_at";

/// Inside transaction `$tx`: revoke every refresh token of `$account` and queue a kick for
/// each of its open sessions in a world with a host link; evaluates to how many were queued.
macro_rules! revoke_with_kicks {
    ($tx:ident, $account:expr, $now:expr, $reason:expr) => {{
        sqlx::query(REVOKE_ACCOUNT).bind($now).bind($account).execute(&mut *$tx).await?;
        sqlx::query(QUEUE_KICKS).bind($account).bind($reason).bind($now).execute(&mut *$tx).await?.rows_affected()
    }};
}

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
            host_link: $r.get(7),
            host_link_cert: $r.get(8),
        }
    };
}

const ACCOUNT_COLS: &str = "account_id, email, password_hash, email_verified_at, created_at";
const WORLD_COLS: &str =
    "world_id, name, status, quic, webtransport, cert_hash, created_at, host_link, host_link_cert";

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

    /// The account a live email token of `kind` belongs to, WITHOUT spending it (a reset
    /// checks the link before the slow password hash; [`Self::reset_password`] spends it).
    pub async fn peek_email_token(&self, hash: [u8; 32], kind: i64, now: i64) -> Result<Option<AccountId>, LoginError> {
        const SQL: &str =
            "SELECT account_id FROM email_tokens WHERE token_hash = $1 AND kind = $2 AND used_at IS NULL AND expires_at > $3";
        Ok(on!(self, p => sqlx::query(SQL).bind(&hash[..]).bind(kind).bind(now).fetch_optional(p).await?
            .map(|r| r.get::<i64, _>(0))))
    }

    /// A mailed password reset, all in ONE transaction: the email token of `kind` is spent,
    /// the password set, the account marked verified (the link proved the address), every
    /// refresh token revoked and a kick (with `reason`) queued for each open session — so a
    /// password is never changed with the old sessions surviving, nor a token spent for a
    /// reset that did not happen. `None`: the token is not live (nothing changed). Else the
    /// account and how many kicks were queued.
    pub async fn reset_password(
        &self,
        token: [u8; 32],
        kind: i64,
        password_hash: &str,
        now: i64,
        reason: &str,
    ) -> Result<Option<(AccountId, u64)>, LoginError> {
        const USE: &str = "UPDATE email_tokens SET used_at = $1 \
             WHERE token_hash = $2 AND kind = $3 AND used_at IS NULL AND expires_at > $1 RETURNING account_id";
        const VERIFIED: &str =
            "UPDATE accounts SET email_verified_at = $1 WHERE account_id = $2 AND email_verified_at IS NULL";
        macro_rules! tx {
            ($p:expr) => {{
                let mut tx = $p.begin().await?;
                let used = sqlx::query(USE).bind(now).bind(&token[..]).bind(kind).fetch_optional(&mut *tx).await?;
                match used.map(|r| r.get::<i64, _>(0)) {
                    None => None,
                    Some(account) => {
                        sqlx::query(SET_PASSWORD).bind(password_hash).bind(account).execute(&mut *tx).await?;
                        sqlx::query(VERIFIED).bind(now).bind(account).execute(&mut *tx).await?;
                        let kicks = revoke_with_kicks!(tx, account, now, reason);
                        tx.commit().await?;
                        Some((account, kicks))
                    }
                }
            }};
        }
        Ok(match &self.pool {
            Pool::Sqlite(p) => tx!(p),
            Pool::Pg(p) => tx!(p),
        })
    }

    /// Set `id`'s password and, in the SAME transaction, revoke its refresh tokens and queue a
    /// kick (with `reason`) for each open session (an admin reset): how many kicks were queued.
    pub async fn set_password_with_kicks(
        &self,
        id: AccountId,
        hash: &str,
        now: i64,
        reason: &str,
    ) -> Result<u64, LoginError> {
        macro_rules! tx {
            ($p:expr) => {{
                let mut tx = $p.begin().await?;
                sqlx::query(SET_PASSWORD).bind(hash).bind(id).execute(&mut *tx).await?;
                let kicks = revoke_with_kicks!(tx, id, now, reason);
                tx.commit().await?;
                kicks
            }};
        }
        Ok(match &self.pool {
            Pool::Sqlite(p) => tx!(p),
            Pool::Pg(p) => tx!(p),
        })
    }

    pub async fn set_password(&self, id: AccountId, hash: &str) -> Result<(), LoginError> {
        exec!(self, p => sqlx::query(SET_PASSWORD).bind(hash).bind(id).execute(p).await?);
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
        let taken: Option<(i64, Vec<u8>)> =
            on!(self, p => sqlx::query(TAKE).bind(now).bind(&hash[..]).fetch_optional(p).await?
                .map(|r| (r.get(0), r.get(1))));
        if let Some((account, family)) = taken {
            return Ok(RefreshOutcome::Valid { account, family: family_of(family) });
        }
        self.not_taken(hash).await
    }

    /// Rotate a refresh token: take `old` (as [`take_refresh`](Self::take_refresh)) and, if it was
    /// live, insert `new` in its family — ONE transaction, so a successor is never inserted after a
    /// concurrent reuse revoked the family.
    ///
    /// Why that is enough (the caller revokes the family on `Reused`):
    /// - Postgres (READ COMMITTED): a concurrent take of `old` blocks on the winner's row lock until
    ///   the winner commits — with its successor — then re-checks, sees `old` revoked and answers
    ///   `Reused`; its `revoke_family` runs after that commit, so it revokes the successor too.
    /// - SQLite: writers are serialised, and the take UPDATE is the transaction's first statement,
    ///   so the deferred transaction takes the write lock before reading anything (no stale
    ///   snapshot to upgrade); a loser's reuse lookup runs after the winner committed.
    pub async fn rotate_refresh(
        &self,
        old: [u8; 32],
        new: [u8; 32],
        new_expires_at: i64,
        now: i64,
    ) -> Result<RefreshOutcome, LoginError> {
        const TAKE: &str = "UPDATE refresh_tokens SET revoked_at = $1 \
             WHERE token_hash = $2 AND revoked_at IS NULL AND expires_at > $1 RETURNING account_id, family";
        const INS: &str = "INSERT INTO refresh_tokens (token_hash, account_id, family, expires_at) VALUES ($1, $2, $3, $4)";
        let taken: Option<(i64, Vec<u8>)> = match &self.pool {
            Pool::Sqlite(p) => {
                let mut tx = p.begin().await?;
                let taken: Option<(i64, Vec<u8>)> = sqlx::query(TAKE)
                    .bind(now).bind(&old[..]).fetch_optional(&mut *tx).await?
                    .map(|r| (r.get(0), r.get(1)));
                if let Some((account, family)) = &taken {
                    sqlx::query(INS).bind(&new[..]).bind(account).bind(&family[..]).bind(new_expires_at)
                        .execute(&mut *tx).await?;
                }
                tx.commit().await?;
                taken
            }
            Pool::Pg(p) => {
                let mut tx = p.begin().await?;
                let taken: Option<(i64, Vec<u8>)> = sqlx::query(TAKE)
                    .bind(now).bind(&old[..]).fetch_optional(&mut *tx).await?
                    .map(|r| (r.get(0), r.get(1)));
                if let Some((account, family)) = &taken {
                    sqlx::query(INS).bind(&new[..]).bind(account).bind(&family[..]).bind(new_expires_at)
                        .execute(&mut *tx).await?;
                }
                tx.commit().await?;
                taken
            }
        };
        if let Some((account, family)) = taken {
            return Ok(RefreshOutcome::Valid { account, family: family_of(family) });
        }
        self.not_taken(old).await
    }

    /// A refresh token that could not be taken: `Reused` if it was revoked, else `Unknown`.
    async fn not_taken(&self, hash: [u8; 32]) -> Result<RefreshOutcome, LoginError> {
        const LOOK: &str = "SELECT account_id, family, revoked_at FROM refresh_tokens WHERE token_hash = $1";
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
        const SQL: &str = "INSERT INTO worlds (world_id, name, status, quic, webtransport, cert_hash, created_at, \
             host_link, host_link_cert) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (world_id) DO UPDATE SET \
             name = excluded.name, status = excluded.status, quic = excluded.quic, \
             webtransport = excluded.webtransport, cert_hash = excluded.cert_hash, \
             host_link = excluded.host_link, host_link_cert = excluded.host_link_cert";
        exec!(self, p => sqlx::query(SQL)
            .bind(&w.world_id).bind(&w.name).bind(status_int(w.status))
            .bind(&w.quic).bind(&w.webtransport).bind(&w.cert_hash).bind(w.created_at)
            .bind(&w.host_link).bind(&w.host_link_cert)
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

    /// Upsert; the new epoch, starting at 1. The session is open again (`ended_at` cleared).
    pub async fn bump_session(&self, account: AccountId, world: &str, now: i64) -> Result<u64, LoginError> {
        const SQL: &str = "INSERT INTO sessions (account_id, world_id, session_epoch, started_at) \
             VALUES ($1, $2, 1, $3) ON CONFLICT (account_id, world_id) DO UPDATE SET \
             session_epoch = sessions.session_epoch + 1, started_at = excluded.started_at, ended_at = NULL \
             RETURNING session_epoch";
        let epoch: i64 =
            on!(self, p => sqlx::query(SQL).bind(account).bind(world).bind(now).fetch_one(p).await?.get(0));
        Ok(epoch as u64)
    }

    /// The account of a live (unrevoked, unexpired) refresh token, without spending it. `join`
    /// reads it before pushing a spawn order, so a world host that is down does not cost the
    /// client its token (the rotation happens after the ack).
    pub async fn live_refresh_account(&self, hash: [u8; 32], now: i64) -> Result<Option<AccountId>, LoginError> {
        const SQL: &str =
            "SELECT account_id FROM refresh_tokens WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > $2";
        Ok(on!(self, p => sqlx::query(SQL).bind(&hash[..]).bind(now).fetch_optional(p).await?
            .map(|r| r.get::<i64, _>(0))))
    }

    /// A host reported the session of `player_token` in `world` admitted with `epoch` ended.
    /// Only that epoch's session ends (a late report never ends a newer join). Whether it did.
    pub async fn end_session(&self, world: &str, player_token: &str, epoch: u64, now: i64) -> Result<bool, LoginError> {
        const SQL: &str = "UPDATE sessions SET ended_at = $1 \
             WHERE world_id = $2 AND session_epoch = $3 AND ended_at IS NULL AND account_id = \
             (SELECT account_id FROM world_players WHERE world_id = $2 AND player_token = $4)";
        let epoch = i64::try_from(epoch).unwrap_or(i64::MAX);
        const DROP_KICK: &str = "DELETE FROM pending_kicks WHERE world_id = $1 AND player_token = $2 AND session_epoch <= $3";
        // One transaction: an ended session's kick has nothing left to do, so it goes with it.
        macro_rules! tx {
            ($p:expr) => {{
                let mut tx = $p.begin().await?;
                let n = sqlx::query(SQL).bind(now).bind(world).bind(epoch).bind(player_token)
                    .execute(&mut *tx).await?.rows_affected();
                sqlx::query(DROP_KICK).bind(world).bind(player_token).bind(epoch).execute(&mut *tx).await?;
                tx.commit().await?;
                n
            }};
        }
        let n = match &self.pool {
            Pool::Sqlite(p) => tx!(p),
            Pool::Pg(p) => tx!(p),
        };
        Ok(n > 0)
    }

    /// The account's sessions that have not ended, by world.
    pub async fn open_sessions(&self, account: AccountId) -> Result<Vec<OpenSession>, LoginError> {
        const SQL: &str = "SELECT s.world_id, w.player_token, s.session_epoch FROM sessions s \
             JOIN world_players w ON w.account_id = s.account_id AND w.world_id = s.world_id \
             WHERE s.account_id = $1 AND s.ended_at IS NULL ORDER BY s.world_id";
        Ok(on!(self, p => sqlx::query(SQL).bind(account).fetch_all(p).await?.into_iter()
            .map(|r| OpenSession { world_id: r.get(0), player_token: r.get(1), session_epoch: r.get::<i64, _>(2) as u64 })
            .collect()))
    }

    /// Revoke every refresh token of `account` and, in the SAME transaction, queue a kick (with
    /// `reason`) for each of its open sessions in a world with a host link — so a revocation
    /// is never committed without its kicks. A kick already queued for that player is replaced
    /// (the newest epoch and reason). How many kicks were queued.
    pub async fn revoke_account_with_kicks(&self, account: AccountId, now: i64, reason: &str) -> Result<u64, LoginError> {
        macro_rules! tx {
            ($p:expr) => {{
                let mut tx = $p.begin().await?;
                let n = revoke_with_kicks!(tx, account, now, reason);
                tx.commit().await?;
                n
            }};
        }
        Ok(match &self.pool {
            Pool::Sqlite(p) => tx!(p),
            Pool::Pg(p) => tx!(p),
        })
    }

    /// Every kick not acknowledged yet, by world.
    pub async fn pending_kicks(&self) -> Result<Vec<PendingKick>, LoginError> {
        const SQL: &str = "SELECT world_id, player_token, session_epoch, reason FROM pending_kicks \
             ORDER BY world_id, player_token";
        Ok(on!(self, p => sqlx::query(SQL).fetch_all(p).await?.into_iter()
            .map(|r| PendingKick {
                world_id: r.get(0),
                player_token: r.get(1),
                session_epoch: r.get::<i64, _>(2) as u64,
                reason: r.get(3),
            })
            .collect()))
    }

    /// The host applied the kick of `player_token` in `world` up to `epoch`: forgotten, unless
    /// a newer one was queued since. Whether a row went.
    pub async fn kick_acked(&self, world: &str, player_token: &str, epoch: u64) -> Result<bool, LoginError> {
        const SQL: &str = "DELETE FROM pending_kicks WHERE world_id = $1 AND player_token = $2 AND session_epoch <= $3";
        let epoch = i64::try_from(epoch).unwrap_or(i64::MAX);
        let n = on!(self, p => sqlx::query(SQL).bind(world).bind(player_token).bind(epoch).execute(p).await?.rows_affected());
        Ok(n > 0)
    }

    /// Delete and return the kicks queued `ttl_secs` or more before `now_secs`: a host that
    /// never acknowledges must not be retried forever. One statement, so two replicas never
    /// both report the same row.
    pub async fn expire_pending_kicks(&self, now_secs: u64, ttl_secs: u64) -> Result<Vec<PendingKick>, LoginError> {
        const SQL: &str = "DELETE FROM pending_kicks WHERE created_at + $2 <= $1 \
             RETURNING world_id, player_token, session_epoch, reason";
        let now = i64::try_from(now_secs).unwrap_or(i64::MAX);
        let ttl = i64::try_from(ttl_secs).unwrap_or(i64::MAX);
        let mut kicks: Vec<PendingKick> = on!(self, p => sqlx::query(SQL).bind(now).bind(ttl).fetch_all(p).await?
            .into_iter()
            .map(|r| PendingKick {
                world_id: r.get(0),
                player_token: r.get(1),
                session_epoch: r.get::<i64, _>(2) as u64,
                reason: r.get(3),
            })
            .collect());
        kicks.sort_by(|a, b| (&a.world_id, &a.player_token).cmp(&(&b.world_id, &b.player_token)));
        Ok(kicks)
    }
}
