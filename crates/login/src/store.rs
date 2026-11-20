//! `LoginStore`: the login database on SQLite or Postgres (runtime `sqlx::query()`).

use std::str::FromStr;

use login_api::{EMAIL_MAX_CHARS, WorldStatus};
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use crate::LOGIN_POOL_MAX;

pub type AccountId = i64;

/// Trim and lowercase an email; it must be at most `EMAIL_MAX_CHARS` and have exactly
/// one `@` with a non-empty part on each side.
pub fn normalize_email(email: &str) -> Result<String, &'static str> {
    let e = email.trim().to_lowercase();
    if e.chars().count() > EMAIL_MAX_CHARS {
        return Err("email too long");
    }
    let mut parts = e.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(l), Some(d), None) if !l.is_empty() && !d.is_empty() => Ok(e),
        _ => Err("invalid email address"),
    }
}

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
    /// The identity provider's subject; `None` for an account that never signed in through it.
    pub subject: Option<String>,
    pub display_name: String,
}

/// Longest stored `display_name`, in chars.
pub const DISPLAY_NAME_MAX_CHARS: usize = 64;

/// What [`LoginStore::upsert_oidc_account`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OidcUpsert {
    Created(AccountId),
    /// The subject was known; email and name were refreshed.
    Existing(AccountId),
    /// A password account with this email now carries the subject.
    Adopted(AccountId),
    /// Another subject already holds this email.
    EmailTaken,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The token was live; it is now revoked (rotated).
    Valid {
        account: AccountId,
        family: [u8; 16],
    },
    /// The token was already revoked: the caller revokes its family.
    Reused {
        account: AccountId,
        family: [u8; 16],
    },
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
    /// The world coordinator's HTTPS API (a URL, or a comma list of them; a role-`all` server's
    /// login routes alike): `join` posts `SpawnPlayer` there and waits for its ack among the
    /// answers. `None` = no push (R2a: the host checks tickets by epoch alone).
    pub host_link: Option<String>,
    /// The SHA-256 pin of the coordinator's certificate (64 hex digits).
    pub host_link_cert: Option<String>,
    /// The world map tile route's base URL (migration 0005); `None` = none served.
    pub map_url: Option<String>,
    /// The deployment region whose coordinator serves this world (migration 0006).
    pub region: String,
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
const REVOKE_ACCOUNT: &str =
    "UPDATE refresh_tokens SET revoked_at = $1 WHERE account_id = $2 AND revoked_at IS NULL";
const QUEUE_KICKS: &str = "INSERT INTO pending_kicks (world_id, player_token, session_epoch, reason, created_at) \
     SELECT s.world_id, p.player_token, s.session_epoch, CAST($2 AS TEXT), CAST($3 AS BIGINT) \
     FROM sessions s \
     JOIN world_players p ON p.account_id = s.account_id AND p.world_id = s.world_id \
     JOIN worlds w ON w.world_id = s.world_id \
     WHERE s.account_id = $1 AND s.ended_at IS NULL AND w.host_link IS NOT NULL \
     ON CONFLICT (world_id, player_token) DO UPDATE SET \
     session_epoch = excluded.session_epoch, reason = excluded.reason, created_at = excluded.created_at";

/// The kick reason when an unverified account is adopted by the identity provider's sign-in.
const ADOPTION_KICK_REASON: &str = "signed in again through the identity provider";

/// Inside transaction `$tx`: revoke every refresh token of `$account` and queue a kick for
/// each of its open sessions in a world with a host link; evaluates to how many were queued.
macro_rules! revoke_with_kicks {
    ($tx:ident, $account:expr, $now:expr, $reason:expr) => {{
        sqlx::query(REVOKE_ACCOUNT)
            .bind($now)
            .bind($account)
            .execute(&mut *$tx)
            .await?;
        sqlx::query(QUEUE_KICKS)
            .bind($account)
            .bind($reason)
            .bind($now)
            .execute(&mut *$tx)
            .await?
            .rows_affected()
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
    if i == 0 {
        WorldStatus::Open
    } else {
        WorldStatus::Closed
    }
}

macro_rules! account_row {
    ($r:expr) => {
        AccountRow {
            account_id: $r.get(0),
            email: $r.get(1),
            password_hash: $r.get(2),
            email_verified_at: $r.get(3),
            created_at: $r.get(4),
            subject: $r.get(5),
            display_name: $r.get(6),
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
            map_url: $r.get(9),
            region: $r.get(10),
        }
    };
}

const ACCOUNT_COLS: &str =
    "account_id, email, password_hash, email_verified_at, created_at, subject, display_name";
const WORLD_COLS: &str = "world_id, name, status, quic, webtransport, cert_hash, created_at, host_link, host_link_cert, map_url, region";

impl LoginStore {
    /// Open and migrate: a `postgres://` URL, `sqlite::memory:`, `sqlite:<path>` or a bare path.
    /// No lock is taken: several login replicas may share one Postgres database.
    pub async fn open(url: &str) -> Result<LoginStore, LoginError> {
        let pool = if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            Pool::Pg(
                PgPoolOptions::new()
                    .max_connections(LOGIN_POOL_MAX)
                    .connect(url)
                    .await?,
            )
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
            Pool::Sqlite(
                SqlitePoolOptions::new()
                    .max_connections(LOGIN_POOL_MAX)
                    .connect_with(opts)
                    .await?,
            )
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
        Ok(
            on!(self, p => sqlx::query(SQL).bind(email).bind(password_hash).bind(now).fetch_optional(p).await?
            .map(|r| r.get::<i64, _>(0))),
        )
    }

    pub async fn account_by_email(&self, email: &str) -> Result<Option<AccountRow>, LoginError> {
        let sql = format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE email = $1");
        Ok(
            on!(self, p => sqlx::query(&sql).bind(email).fetch_optional(p).await?.map(|r| account_row!(r))),
        )
    }

    pub async fn account_by_subject(
        &self,
        subject: &str,
    ) -> Result<Option<AccountRow>, LoginError> {
        let sql = format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE subject = $1");
        Ok(
            on!(self, p => sqlx::query(&sql).bind(subject).fetch_optional(p).await?.map(|r| account_row!(r))),
        )
    }

    /// Sign-in through the identity provider, in one transaction: the account of `subject`
    /// (email and name refreshed); else the subject-less account with this email (adopted: its
    /// password is cleared and its email marked verified; if it was not verified yet, its
    /// refresh tokens are revoked and its open sessions kicked in the same transaction); else [`OidcUpsert::EmailTaken`] if another subject holds the
    /// email; else a new password-less account. `email` is already normalised (as for
    /// [`Self::create_account`]); `display_name` is cut to [`DISPLAY_NAME_MAX_CHARS`].
    pub async fn upsert_oidc_account(
        &self,
        subject: &str,
        email: &str,
        display_name: &str,
        now: i64,
    ) -> Result<OidcUpsert, LoginError> {
        let name: String = display_name.chars().take(DISPLAY_NAME_MAX_CHARS).collect();
        macro_rules! tx {
            ($p:expr) => {{
                let mut tx = $p.begin().await?;
                let by_subject = sqlx::query("SELECT account_id FROM accounts WHERE subject = $1")
                    .bind(subject)
                    .fetch_optional(&mut *tx)
                    .await?
                    .map(|r| r.get::<i64, _>(0));
                let out = if let Some(id) = by_subject {
                    let other = sqlx::query("SELECT 1 FROM accounts WHERE email = $1 AND account_id <> $2")
                        .bind(email)
                        .bind(id)
                        .fetch_optional(&mut *tx)
                        .await?;
                    if other.is_some() {
                        OidcUpsert::EmailTaken
                    } else {
                        sqlx::query("UPDATE accounts SET email = $1, display_name = $2 WHERE account_id = $3")
                            .bind(email)
                            .bind(&name)
                            .bind(id)
                            .execute(&mut *tx)
                            .await?;
                        OidcUpsert::Existing(id)
                    }
                } else if let Some(row) =
                    sqlx::query("SELECT account_id, subject, email_verified_at FROM accounts WHERE email = $1")
                        .bind(email)
                        .fetch_optional(&mut *tx)
                        .await?
                {
                    if row.get::<Option<String>, _>(1).is_some() {
                        OidcUpsert::EmailTaken
                    } else {
                        let id: i64 = row.get(0);
                        // Never verified: whoever registered it may not own the email. Their
                        // refresh tokens die and their sessions are kicked with the adoption.
                        if row.get::<Option<i64>, _>(2).is_none() {
                            revoke_with_kicks!(tx, id, now, ADOPTION_KICK_REASON);
                        }
                        sqlx::query(
                            "UPDATE accounts SET subject = $1, display_name = $2, password_hash = '', \
                             email_verified_at = COALESCE(email_verified_at, $3) WHERE account_id = $4",
                        )
                        .bind(subject)
                        .bind(&name)
                        .bind(now)
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                        OidcUpsert::Adopted(id)
                    }
                } else {
                    let id: i64 = sqlx::query(
                        "INSERT INTO accounts (email, password_hash, email_verified_at, created_at, subject, display_name) \
                         VALUES ($1, '', $2, $2, $3, $4) RETURNING account_id",
                    )
                    .bind(email)
                    .bind(now)
                    .bind(subject)
                    .bind(&name)
                    .fetch_one(&mut *tx)
                    .await?
                    .get(0);
                    OidcUpsert::Created(id)
                };
                tx.commit().await?;
                out
            }};
        }
        Ok(match &self.pool {
            Pool::Sqlite(p) => tx!(p),
            Pool::Pg(p) => tx!(p),
        })
    }

    #[tracing::instrument(
        name = "login.store.account",
        skip_all,
        fields(operation = "account_lookup")
    )]
    pub async fn account(&self, id: AccountId) -> Result<Option<AccountRow>, LoginError> {
        let sql = format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE account_id = $1");
        Ok(
            on!(self, p => sqlx::query(&sql).bind(id).fetch_optional(p).await?.map(|r| account_row!(r))),
        )
    }

    /// The account of `email`, made (no password, verified) when there is none: what an operator
    /// needs to hand out a refresh token (`login account issue-refresh`). An account that
    /// already exists is left exactly as it is — its verification included: an operator must
    /// not vouch for an email someone else registered.
    pub async fn ensure_verified_account(
        &self,
        email: &str,
        now: i64,
    ) -> Result<AccountId, LoginError> {
        const CREATE: &str = "INSERT INTO accounts (email, password_hash, email_verified_at, created_at) \
             VALUES ($1, '', $2, $2) ON CONFLICT (email) DO NOTHING RETURNING account_id";
        if let Some(account) = self.account_by_email(email).await? {
            return Ok(account.account_id);
        }
        let made = on!(self, p => sqlx::query(CREATE).bind(email).bind(now).fetch_optional(p).await?
            .map(|r| r.get::<i64, _>(0)));
        match made {
            Some(id) => Ok(id),
            // Lost a race with another creator: the account is there now, left as it is.
            None => self
                .account_by_email(email)
                .await?
                .map(|a| a.account_id)
                .ok_or_else(|| {
                    LoginError::Backend(format!("account {email:?} vanished while being made"))
                }),
        }
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
    pub async fn take_refresh(
        &self,
        hash: [u8; 32],
        now: i64,
    ) -> Result<RefreshOutcome, LoginError> {
        const TAKE: &str = "UPDATE refresh_tokens SET revoked_at = $1 \
             WHERE token_hash = $2 AND revoked_at IS NULL AND expires_at > $1 RETURNING account_id, family";
        let taken: Option<(i64, Vec<u8>)> = on!(self, p => sqlx::query(TAKE).bind(now).bind(&hash[..]).fetch_optional(p).await?
                .map(|r| (r.get(0), r.get(1))));
        if let Some((account, family)) = taken {
            return Ok(RefreshOutcome::Valid {
                account,
                family: family_of(family),
            });
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
    #[tracing::instrument(
        name = "login.store.refresh",
        skip_all,
        fields(operation = "refresh_rotate")
    )]
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
                    .bind(now)
                    .bind(&old[..])
                    .fetch_optional(&mut *tx)
                    .await?
                    .map(|r| (r.get(0), r.get(1)));
                if let Some((account, family)) = &taken {
                    sqlx::query(INS)
                        .bind(&new[..])
                        .bind(account)
                        .bind(&family[..])
                        .bind(new_expires_at)
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit().await?;
                taken
            }
            Pool::Pg(p) => {
                let mut tx = p.begin().await?;
                let taken: Option<(i64, Vec<u8>)> = sqlx::query(TAKE)
                    .bind(now)
                    .bind(&old[..])
                    .fetch_optional(&mut *tx)
                    .await?
                    .map(|r| (r.get(0), r.get(1)));
                if let Some((account, family)) = &taken {
                    sqlx::query(INS)
                        .bind(&new[..])
                        .bind(account)
                        .bind(&family[..])
                        .bind(new_expires_at)
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit().await?;
                taken
            }
        };
        if let Some((account, family)) = taken {
            return Ok(RefreshOutcome::Valid {
                account,
                family: family_of(family),
            });
        }
        self.not_taken(old).await
    }

    /// A refresh token that could not be taken: `Reused` if it was revoked, else `Unknown`.
    async fn not_taken(&self, hash: [u8; 32]) -> Result<RefreshOutcome, LoginError> {
        const LOOK: &str =
            "SELECT account_id, family, revoked_at FROM refresh_tokens WHERE token_hash = $1";
        let seen: Option<(i64, Vec<u8>, Option<i64>)> = on!(self, p => sqlx::query(LOOK).bind(&hash[..]).fetch_optional(p).await?
                .map(|r| (r.get(0), r.get(1), r.get(2))));
        Ok(match seen {
            Some((account, family, Some(_))) => RefreshOutcome::Reused {
                account,
                family: family_of(family),
            },
            // No row, or expired and never used: nothing to revoke.
            _ => RefreshOutcome::Unknown,
        })
    }

    pub async fn revoke_family(&self, family: [u8; 16], now: i64) -> Result<(), LoginError> {
        const SQL: &str =
            "UPDATE refresh_tokens SET revoked_at = $1 WHERE family = $2 AND revoked_at IS NULL";
        exec!(self, p => sqlx::query(SQL).bind(now).bind(&family[..]).execute(p).await?);
        Ok(())
    }

    pub async fn revoke_account_refresh(
        &self,
        account: AccountId,
        now: i64,
    ) -> Result<(), LoginError> {
        const SQL: &str = "UPDATE refresh_tokens SET revoked_at = $1 WHERE account_id = $2 AND revoked_at IS NULL";
        exec!(self, p => sqlx::query(SQL).bind(now).bind(account).execute(p).await?);
        Ok(())
    }

    /// Insert or replace a world of the registry (`created_at` of an existing one is kept).
    pub async fn put_world(&self, w: &WorldRow) -> Result<(), LoginError> {
        const SQL: &str = "INSERT INTO worlds (world_id, name, status, quic, webtransport, cert_hash, created_at, \
             host_link, host_link_cert, map_url, region) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) ON CONFLICT (world_id) DO UPDATE SET \
             name = excluded.name, status = excluded.status, quic = excluded.quic, \
             webtransport = excluded.webtransport, cert_hash = excluded.cert_hash, \
             host_link = excluded.host_link, host_link_cert = excluded.host_link_cert, \
             map_url = excluded.map_url, region = excluded.region";
        exec!(self, p => sqlx::query(SQL)
            .bind(&w.world_id).bind(&w.name).bind(status_int(w.status))
            .bind(&w.quic).bind(&w.webtransport).bind(&w.cert_hash).bind(w.created_at)
            .bind(&w.host_link).bind(&w.host_link_cert).bind(&w.map_url).bind(&w.region)
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
        Ok(
            on!(self, p => sqlx::query(&sql).fetch_all(p).await?.into_iter().map(|r| world_row!(r)).collect()),
        )
    }

    #[tracing::instrument(
        name = "login.store.world",
        skip(self),
        fields(operation = "world_lookup")
    )]
    pub async fn world(&self, id: &str) -> Result<Option<WorldRow>, LoginError> {
        let sql = format!("SELECT {WORLD_COLS} FROM worlds WHERE world_id = $1");
        Ok(
            on!(self, p => sqlx::query(&sql).bind(id).fetch_optional(p).await?.map(|r| world_row!(r))),
        )
    }

    /// The account's player token in a world, created (from `new_token`) on first use.
    #[tracing::instrument(
        name = "login.store.player_token",
        skip_all,
        fields(operation = "player_token_resolve", mmo.world_id = %world)
    )]
    pub async fn player_token_for(
        &self,
        account: AccountId,
        world: &str,
        now: i64,
        new_token: impl FnOnce() -> String,
    ) -> Result<String, LoginError> {
        const SEL: &str =
            "SELECT player_token FROM world_players WHERE account_id = $1 AND world_id = $2";
        const INS: &str = "INSERT INTO world_players (account_id, world_id, player_token, created_at) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (account_id, world_id) DO NOTHING";
        let have: Option<String> = on!(self, p => sqlx::query(SEL).bind(account).bind(world).fetch_optional(p).await?.map(|r| r.get(0)));
        if let Some(t) = have {
            return Ok(t);
        }
        let fresh = new_token();
        exec!(self, p => sqlx::query(INS).bind(account).bind(world).bind(&fresh).bind(now).execute(p).await?);
        // Re-read: a concurrent first join may have stored its token first.
        Ok(on!(self, p => sqlx::query(SEL).bind(account).bind(world).fetch_one(p).await?.get(0)))
    }

    /// Upsert; the new epoch, starting at 1. The session is open again (`ended_at` cleared).
    #[tracing::instrument(
        name = "login.store.session",
        skip_all,
        fields(operation = "session_bump", mmo.world_id = %world)
    )]
    pub async fn bump_session(
        &self,
        account: AccountId,
        world: &str,
        now: i64,
    ) -> Result<u64, LoginError> {
        const SQL: &str = "INSERT INTO sessions (account_id, world_id, session_epoch, started_at) \
             VALUES ($1, $2, 1, $3) ON CONFLICT (account_id, world_id) DO UPDATE SET \
             session_epoch = sessions.session_epoch + 1, started_at = excluded.started_at, ended_at = NULL \
             RETURNING session_epoch";
        let epoch: i64 = on!(self, p => sqlx::query(SQL).bind(account).bind(world).bind(now).fetch_one(p).await?.get(0));
        Ok(epoch as u64)
    }

    /// The account of a live (unrevoked, unexpired) refresh token, without spending it. `join`
    /// reads it before pushing a spawn order, so a world host that is down does not cost the
    /// client its token (the rotation happens after the ack).
    #[tracing::instrument(
        name = "login.store.refresh",
        skip_all,
        fields(operation = "refresh_validate")
    )]
    pub async fn live_refresh_account(
        &self,
        hash: [u8; 32],
        now: i64,
    ) -> Result<Option<AccountId>, LoginError> {
        const SQL: &str = "SELECT account_id FROM refresh_tokens WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > $2";
        Ok(
            on!(self, p => sqlx::query(SQL).bind(&hash[..]).bind(now).fetch_optional(p).await?
            .map(|r| r.get::<i64, _>(0))),
        )
    }

    /// A host reported the session of `player_token` in `world` admitted with `epoch` ended.
    /// Only that epoch's session ends (a late report never ends a newer join). Whether it did.
    pub async fn end_session(
        &self,
        world: &str,
        player_token: &str,
        epoch: u64,
        now: i64,
    ) -> Result<bool, LoginError> {
        const SQL: &str = "UPDATE sessions SET ended_at = $1 \
             WHERE world_id = $2 AND session_epoch = $3 AND ended_at IS NULL AND account_id = \
             (SELECT account_id FROM world_players WHERE world_id = $2 AND player_token = $4)";
        let epoch = i64::try_from(epoch).unwrap_or(i64::MAX);
        const DROP_KICK: &str = "DELETE FROM pending_kicks WHERE world_id = $1 AND player_token = $2 AND session_epoch <= $3";
        // One transaction: an ended session's kick has nothing left to do, so it goes with it.
        macro_rules! tx {
            ($p:expr) => {{
                let mut tx = $p.begin().await?;
                let n = sqlx::query(SQL)
                    .bind(now)
                    .bind(world)
                    .bind(epoch)
                    .bind(player_token)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
                sqlx::query(DROP_KICK)
                    .bind(world)
                    .bind(player_token)
                    .bind(epoch)
                    .execute(&mut *tx)
                    .await?;
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
        Ok(
            on!(self, p => sqlx::query(SQL).bind(account).fetch_all(p).await?.into_iter()
            .map(|r| OpenSession { world_id: r.get(0), player_token: r.get(1), session_epoch: r.get::<i64, _>(2) as u64 })
            .collect()),
        )
    }

    /// Revoke every refresh token of `account` and, in the SAME transaction, queue a kick (with
    /// `reason`) for each of its open sessions in a world with a host link — so a revocation
    /// is never committed without its kicks. A kick already queued for that player is replaced
    /// (the newest epoch and reason). How many kicks were queued.
    pub async fn revoke_account_with_kicks(
        &self,
        account: AccountId,
        now: i64,
        reason: &str,
    ) -> Result<u64, LoginError> {
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
        Ok(
            on!(self, p => sqlx::query(SQL).fetch_all(p).await?.into_iter()
            .map(|r| PendingKick {
                world_id: r.get(0),
                player_token: r.get(1),
                session_epoch: r.get::<i64, _>(2) as u64,
                reason: r.get(3),
            })
            .collect()),
        )
    }

    /// The host applied the kick of `player_token` in `world` up to `epoch`: forgotten, unless
    /// a newer one was queued since. Whether a row went.
    pub async fn kick_acked(
        &self,
        world: &str,
        player_token: &str,
        epoch: u64,
    ) -> Result<bool, LoginError> {
        const SQL: &str = "DELETE FROM pending_kicks WHERE world_id = $1 AND player_token = $2 AND session_epoch <= $3";
        let epoch = i64::try_from(epoch).unwrap_or(i64::MAX);
        let n = on!(self, p => sqlx::query(SQL).bind(world).bind(player_token).bind(epoch).execute(p).await?.rows_affected());
        Ok(n > 0)
    }

    /// Delete and return the kicks queued `ttl_secs` or more before `now_secs`: a host that
    /// never acknowledges must not be retried forever. One statement, so two replicas never
    /// both report the same row.
    pub async fn expire_pending_kicks(
        &self,
        now_secs: u64,
        ttl_secs: u64,
    ) -> Result<Vec<PendingKick>, LoginError> {
        const SQL: &str = "DELETE FROM pending_kicks WHERE created_at <= $1 \
             RETURNING world_id, player_token, session_epoch, reason";
        let cutoff = i64::try_from(now_secs.saturating_sub(ttl_secs)).unwrap_or(i64::MAX);
        let mut kicks: Vec<PendingKick> = on!(self, p => sqlx::query(SQL).bind(cutoff).fetch_all(p).await?
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
