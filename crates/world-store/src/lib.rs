//! The world store: one world's durable state in SQLite or Postgres through sqlx
//! (native only). This module holds the shared vocabulary; `open` opens a database
//! (a writer: SQLite's file lock, the migrations, the world row; or read-only),
//! `thread` runs the `world-store` thread that every other thread reaches through
//! [`StoreHandle`]; `fence` is the owner-epoch fencing of writes; `rows` is the SQL.

pub mod ctl;
mod codec;
mod counters;
mod fence;
mod forged;
mod handle;
mod listen;
mod open;
mod rows;
mod succession;
mod terrain;
mod thread;
mod two_phase;
mod url;

use std::fmt;

pub use codec::*;
pub use counters::StoreCounters;
pub use fence::{CoordinatorFence, Fence, SuperRegion, touched_super_regions};
pub use forged::{StoreTiles, load_forged, write_tiled};
pub use handle::{
    BootVerdict, CutCommitted, HOST_DEAD, HOST_LIVE, HostBoot, HostRow, NO_OWNER, StoreHandle, WaitFor, WorldInfo,
};
pub use listen::{
    CutNotice, NotifyMode, TERRAIN_LISTENER_APP, TERRAIN_NOTIFY_ENV, TERRAIN_POLL_SECS, WORLD_CUT_CHANNEL,
    WORLD_DB_DIRECT_ENV,
};
pub use open::{LockGuard, MIGRATE_LOCK_TIMEOUT_MS, OpenMode, Pool, newest_migration, open_pool, set_migrate_lock_timeout_ms};
pub use succession::{Declared, Succession, SuccessionId, SuccessionKind};
pub use terrain::TerrainFromStore;
pub use two_phase::cut_gid;
pub use url::{StoreUrl, redact_url};

/// The schema/format generation this build writes and reads.
pub const STORE_FORMAT_VERSION: u32 = 1;
/// Largest connection pool of a store (the writer plus readers).
pub const STORE_POOL_MAX: u32 = 4;
/// How long a super-region claim (Postgres `lock_timeout`) waits for a row lock: longer
/// than a world cut may stay undecided (`game::WORLD_CUT_DEADLINE_MS`, then its decision),
/// so only a part nobody will decide makes it fail.
pub const CLAIM_LOCK_TIMEOUT_MS: u64 = 60_000;

/// How long a save cut's transaction (Postgres `lock_timeout`, both kinds) waits for
/// any one row lock before it gives up: no cut sits on a lock for ever.
pub const CUT_LOCK_TIMEOUT_MS: u64 = 10_000;
/// How long a heartbeat waits for its `hosts` row lock (Postgres): well under the lease's beat
/// period, so a beat blocked behind a registration or a decision fails fast (`Err`, not `Ok(false)`).
pub const HEARTBEAT_LOCK_TIMEOUT_MS: u64 = 1_000;

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

/// The terrain a world's regions are saved in (`world.terrain`, migration `0004`).
/// New worlds are written as [`TerrainKind::Voxels`], the only terrain stored; a stud
/// world is never converted — the kind exists so it can be named when it is refused at
/// open (`open.rs`, `refuse_unreturned_world`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerrainKind {
    /// Voxel terrain (`chunks`, `regions.ground`).
    Voxels,
    /// Stud terrain, saved by the builds between the stud switch and the voxel return
    /// (migration `0011_voxel_terrain_returns` dropped its table and column).
    Studs,
}

impl TerrainKind {
    /// The `world.terrain` value.
    pub fn code(self) -> i64 {
        match self {
            TerrainKind::Voxels => 0,
            TerrainKind::Studs => 1,
        }
    }

    pub fn from_code(code: i64) -> Option<TerrainKind> {
        match code {
            0 => Some(TerrainKind::Voxels),
            1 => Some(TerrainKind::Studs),
            _ => None,
        }
    }

    /// The word for it in messages.
    pub fn name(self) -> &'static str {
        match self {
            TerrainKind::Voxels => "voxels",
            TerrainKind::Studs => "studs",
        }
    }
}

impl fmt::Display for TerrainKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
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
    /// The schema's applied migrations (`_sqlx_migrations`' newest version,
    /// 0 when none): a read-only handle never migrates, so it opens only a
    /// store at exactly this build's newest migration ([`newest_migration`]).
    Schema,
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
    /// A world this build cannot open: saved on stud terrain (`stored` [`TerrainKind::Studs`]),
    /// or on voxel terrain before the stud switch (`stored` [`TerrainKind::Voxels`]; the
    /// stud-era migration `0005` would drop its chunks). Refused at open, by a writer and a
    /// read-only handle alike, before any migration runs: nothing in it is changed.
    OtherTerrain { stored: TerrainKind },
    /// A stored format this build does not read: the store's own
    /// (`world.format_version`) or the forged tiles' (`forged_meta`).
    OtherFormat { found: u32, kind: FormatKind },
    /// A world this store cannot hold, refused before anything is written
    /// (e.g. a forged world whose cell size the tiled store does not
    /// support); the message says why.
    Unsupported(String),
    Corrupt(String),
    /// A fenced write or load whose claim is no longer the stored one:
    /// another process claimed the super-region (`sr`) — or, with `sr` `None`,
    /// the coordinator row — since; also (`sr` `None`) a host's claim whose boot's
    /// `hosts` row is no longer live (declared dead or replaced). Nothing was written.
    Fenced { sr: Option<SuperRegion> },
    /// A cut prepared by a host whose generation (`mine`) is behind the world's
    /// (`stored`): the world restarted from another save since; nothing was written.
    StaleGeneration { stored: u64, mine: u64 },
    /// A write asked of a handle opened [`OpenMode::ReadOnly`].
    ReadOnly,
    /// A host's part of a two-phase cut prepared after the cut was decided aborted:
    /// the part rolled itself back; nothing of it was written.
    CutAborted(game::CutId),
    /// This handle's part of the cut is prepared and not yet seen decided
    /// ([`StoreHandle::cut_decided`]): the next cut is refused at once (it would wait
    /// on that part's row locks). Nothing was written.
    CutPending(game::CutId),
    /// A claim by a handle opened when the world's super-region side was `opened`, after
    /// `worldctl super-region` changed it to `stored`: its super-regions are of the old
    /// side. Nothing was written; restart the process (with `VOXEL_SUPER_REGION_REGIONS`).
    OtherSuperRegionSide { stored: u32, opened: u32 },
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
            StoreError::OtherTerrain { stored: TerrainKind::Studs } => write!(
                f,
                "this world was saved on stud terrain, which this build no longer serves — re-forge it \
                 (the debug world: `just run --reset`; a menu world: make a new one; the dev Postgres worlds: \
                 `just pg-reset`, which deletes all dev Postgres data). Terrain is never \
                 converted; nothing in the world was changed, so a build from before the voxel return \
                 still opens it"
            ),
            StoreError::OtherTerrain { stored: TerrainKind::Voxels } => write!(
                f,
                "this world was saved on voxel terrain by a build from before the stud switch; this build's \
                 schema cannot take it over (an intermediate migration would drop its chunks) — re-forge it \
                 (the debug world: `just run --reset`; a menu world: make a new one). Nothing in the world \
                 was changed, so a build from before the stud switch still opens it"
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
            StoreError::OtherFormat { found, kind: FormatKind::Schema } => write!(
                f,
                "this world's schema is at migration {found} but this build reads migration {}; \
                 a read-only open never migrates — open it once with this build's server, or use a build that matches",
                newest_migration()
            ),
            StoreError::Fenced { sr: Some((sx, sz)) } => write!(
                f,
                "super-region ({sx}, {sz}) was claimed by another process since this one claimed it; nothing was written"
            ),
            StoreError::Fenced { sr: None } => write!(
                f,
                "the world coordinator was claimed by another process since this one claimed it, or (a host's claim) \
                 this boot's hosts row was declared dead or replaced; nothing was written"
            ),
            StoreError::StaleGeneration { stored, mine } => write!(
                f,
                "this host is at world generation {mine} but the world is at generation {stored}: the world restarted \
                 from another save; nothing was written"
            ),
            StoreError::ReadOnly => write!(f, "this world store was opened read-only; it writes nothing"),
            StoreError::CutAborted(cut) => {
                write!(f, "cut {} was decided aborted before this part prepared; the part rolled itself back", cut.0)
            }
            StoreError::CutPending(cut) => write!(
                f,
                "this host's part of cut {} is still undecided; no further cut is written until it is (cut_decided)",
                cut.0
            ),
            StoreError::OtherSuperRegionSide { stored, opened } => write!(
                f,
                "the world's super-region side is now {stored} regions but this process opened it at {opened}; \
                 restart it with VOXEL_SUPER_REGION_REGIONS={stored}"
            ),
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
        match &e {
            // `lock_not_available`: a save cut's `lock_timeout` ran out.
            sqlx::Error::Database(d) if d.code().as_deref() == Some("55P03") => StoreError::Backend(format!(
                "a save cut waited longer than its lock_timeout (CUT_LOCK_TIMEOUT_MS) for a row lock another \
                 transaction or prepared cut part holds; nothing was written ({e})"
            )),
            _ => StoreError::Backend(e.to_string()),
        }
    }
}

impl From<sqlx::migrate::MigrateError> for StoreError {
    fn from(e: sqlx::migrate::MigrateError) -> Self {
        StoreError::Backend(format!("migration: {e}"))
    }
}
