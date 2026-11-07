use std::path::PathBuf;

use crate::StoreError;

/// Where a world store lives.
#[derive(Clone, Debug, PartialEq, Eq)]
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
