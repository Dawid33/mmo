//! The world store: one world's durable state in SQLite or Postgres through sqlx
//! (native only). This module holds the shared vocabulary; `open` opens a database,
//! takes the world lock, runs the migrations and checks the world row; `thread` runs
//! the `world-store` thread that every other thread reaches through [`StoreHandle`];
//! `rows` is the SQL.

pub mod ctl;
mod codec;
mod forged;
mod handle;
mod open;
mod rows;
mod terrain;
mod thread;
mod url;

use std::fmt;

pub use codec::*;
pub use forged::{StoreTiles, load_forged, write_tiled};
pub use handle::{CutCommitted, StoreHandle, WorldInfo};
pub use open::{LockGuard, Pool, open_pool};
pub use terrain::TerrainFromStore;
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

/// Which stored format [`StoreError::OtherFormat`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    /// The store's schema/format generation (`STORE_FORMAT_VERSION`).
    Store,
    /// The forged world's tiles (`worldgen::forged_tiles::TILED_FORMAT_VERSION`);
    /// `archived`: the original forge file is stored, so the world can be re-tiled
    /// without re-forging.
    Tiles { archived: bool },
    /// The forged world's stored world map (`forged_world_map`): another row
    /// layout, another `worldgen::tile_map::WORLD_MAP_VERSION` (`found`; 0
    /// when the row's layout predates the version or the row is damaged).
    /// Rebuilt with the tiles by a re-tile when `archived`.
    WorldMap { archived: bool },
}

impl FormatKind {
    /// Whether `worldctl retile` (or the server at start) rebuilds it from
    /// the archived forge file: a tile format or world map of another
    /// version, with the archive row stored.
    pub fn retileable(self) -> bool {
        matches!(self, FormatKind::Tiles { archived: true } | FormatKind::WorldMap { archived: true })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Backend(String),
    Locked(String),
    OtherDials { stored: Dials, built: Dials },
    /// A stored format this build does not read: the store's own
    /// (`world.format_version`) or the forged tiles' (`forged_meta`).
    OtherFormat { found: u32, kind: FormatKind },
    /// A world this store cannot hold, refused before anything is written
    /// (e.g. a forged world whose cell size the tiled store does not
    /// support); the message says why.
    Unsupported(String),
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
            StoreError::OtherFormat { found, kind: FormatKind::Store } => write!(
                f,
                "this world uses store format {found} but this build reads format {STORE_FORMAT_VERSION}; use a build that matches, or re-create the world"
            ),
            StoreError::OtherFormat { found, kind: FormatKind::Tiles { archived } } => {
                write!(
                    f,
                    "this world's forged tiles use format {found} but this build reads format {}; ",
                    worldgen::forged_tiles::TILED_FORMAT_VERSION
                )?;
                retile_hint(f, *archived)
            }
            StoreError::OtherFormat { found, kind: FormatKind::WorldMap { archived } } => {
                if *found == 0 {
                    write!(f, "this world's stored world map has an older row layout or is damaged; ")?;
                } else {
                    write!(
                        f,
                        "this world's stored world map was built by world-map version {found} but this build builds version {}; ",
                        worldgen::tile_map::WORLD_MAP_VERSION
                    )?;
                }
                retile_hint(f, *archived)
            }
            StoreError::Unsupported(e) => write!(f, "{e}"),
            StoreError::Corrupt(e) => write!(f, "world store is corrupt: {e}"),
        }
    }
}

/// The end of a re-tile-able [`StoreError::OtherFormat`] message.
fn retile_hint(f: &mut fmt::Formatter<'_>, archived: bool) -> fmt::Result {
    if archived {
        write!(
            f,
            "re-tile it from its archived forge file: `just worldctl retile --db <db>` (lossless: saved regions, players and history are kept; the server does it by itself at start)"
        )
    } else {
        write!(f, "it keeps no archived forge file to re-tile from, so re-forge the world")
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
