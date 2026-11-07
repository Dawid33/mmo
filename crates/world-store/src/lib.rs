//! The world store: one world's durable state in SQLite or Postgres through sqlx
//! (native only). This module holds the shared vocabulary; `open` opens a database,
//! takes the world lock, runs the migrations and checks the world row; `thread` runs
//! the `world-store` thread that every other thread reaches through [`StoreHandle`];
//! `rows` is the SQL.

mod codec;
mod handle;
mod open;
mod rows;
mod thread;
mod url;

use std::fmt;

pub use codec::*;
pub use handle::{CutCommitted, StoreHandle, WorldInfo};
pub use open::{LockGuard, Pool, open_pool};
pub use url::StoreUrl;

/// The schema/format generation this build writes and reads.
pub const STORE_FORMAT_VERSION: u32 = 1;
/// Postgres advisory-lock key of "one process owns this world database".
pub const WORLD_LOCK_KEY: i64 = 0x4D4D_4F57_4F52_4C44; // "MMOWORLD"
/// Largest connection pool of a store (the writer plus readers).
pub const STORE_POOL_MAX: u32 = 4;

/// The build-time dials a world is tied to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dials {
    pub region_blocks: u32,
    pub chunk_blocks: u32,
}

impl Dials {
    pub const BUILT: Dials = Dials {
        region_blocks: game::REGION_BLOCKS as u32,
        chunk_blocks: game::CHUNK_BLOCKS as u32,
    };
}

impl fmt::Display for Dials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "region_blocks={} chunk_blocks={}", self.region_blocks, self.chunk_blocks)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Backend(String),
    Locked(String),
    OtherDials { stored: Dials, built: Dials },
    OtherFormat { found: u32 },
    Corrupt(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Backend(e) => write!(f, "world store backend error: {e}"),
            StoreError::Locked(what) => write!(
                f,
                "the world database is already in use by another process ({what}); stop that server first"
            ),
            StoreError::OtherDials { stored, built } => write!(
                f,
                "this world was made with {stored} but this build has {built}; rebuild with the stored dials (just region_blocks=N chunk_blocks=N …) or use another world"
            ),
            StoreError::OtherFormat { found } => write!(
                f,
                "this world uses store format {found} but this build reads format {STORE_FORMAT_VERSION}; use a build that matches, or re-create the world"
            ),
            StoreError::Corrupt(e) => write!(f, "world store is corrupt: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        StoreError::Backend(e.to_string())
    }
}

impl From<sqlx::migrate::MigrateError> for StoreError {
    fn from(e: sqlx::migrate::MigrateError) -> Self {
        StoreError::Backend(format!("migration: {e}"))
    }
}
