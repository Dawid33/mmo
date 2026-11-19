use std::fmt;
use std::path::PathBuf;

use crate::StoreError;

/// Where a world store lives. `Display` and `Debug` never show a Postgres URL's
/// credentials ([`StoreUrl::redacted`]): print a store with either.
#[derive(Clone, PartialEq, Eq)]
pub enum StoreUrl {
    Sqlite(PathBuf),
    /// In-memory SQLite (tests).
    Memory,
    Postgres(String),
}

impl StoreUrl {
    /// `postgres://` / `postgresql://` -> Postgres, `sqlite::memory:` -> Memory,
    /// anything else is a SQLite file path (an optional `sqlite://` prefix is dropped).
    pub fn parse(s: &str) -> Result<StoreUrl, StoreError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(StoreError::Backend("empty world store URL".into()));
        }
        if s.starts_with("postgres://") || s.starts_with("postgresql://") {
            return Ok(StoreUrl::Postgres(s.to_string()));
        }
        if s == "sqlite::memory:" {
            return Ok(StoreUrl::Memory);
        }
        let path = s.strip_prefix("sqlite://").unwrap_or(s);
        Ok(StoreUrl::Sqlite(PathBuf::from(path)))
    }
}

impl StoreUrl {
    /// A form safe to print: a SQLite file's path, `sqlite::memory:`, or a Postgres
    /// URL's host and database only ([`redact_url`]).
    pub fn redacted(&self) -> String {
        match self {
            StoreUrl::Sqlite(path) => path.display().to_string(),
            StoreUrl::Memory => "sqlite::memory:".into(),
            StoreUrl::Postgres(url) => redact_url(url),
        }
    }
}

/// `url` without credentials: `postgres://<host>/<database>` for a Postgres URL (never its
/// user, password or parameters), the value itself for anything else (a file path).
pub fn redact_url(url: &str) -> String {
    let url = url.trim();
    if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
        return url.to_string();
    }
    match url.parse::<sqlx::postgres::PgConnectOptions>() {
        Ok(o) => format!("postgres://{}/{}", o.get_host(), o.get_database().unwrap_or("?")),
        Err(_) => "postgres://<unparsable URL>".into(),
    }
}

impl fmt::Display for StoreUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted())
    }
}

impl fmt::Debug for StoreUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreUrl::Sqlite(path) => f.debug_tuple("Sqlite").field(path).finish(),
            StoreUrl::Memory => f.write_str("Memory"),
            StoreUrl::Postgres(_) => f.debug_tuple("Postgres").field(&self.redacted()).finish(),
        }
    }
}

