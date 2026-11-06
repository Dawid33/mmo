//! Offline water prototype: water bodies + frontier CA versus cell-only CA.
//! Throwaway. Plan: docs/superpowers/plans/2026-09-22-water-prototype.md.
//! Everything authoritative is integer and iterated in ascending cell index.

pub mod body;
pub mod ca;
pub mod edit;
pub mod fixture;
pub mod grid;
pub mod metrics;
pub mod scenario;
pub mod state;

/// Units in one full 1 m³ cell (16³ voxels).
pub const FULL: u16 = 4096;
/// Film depth (1/16 m): a CA cell holding this much or less stops spreading on solid footing.
pub const FILM: u16 = 256;
/// Most units one edge moves in one step (1/4 m³).
pub const MAX_FLOW: u16 = 1024;
/// Steps without a transfer before a CA cell sleeps.
pub const SLEEP_STEPS: u32 = 4;
/// Steps a full CA cell on solid ground must be quiet before it seeds a body.
pub const SEED_QUIET: u32 = 2;
/// Units of level mismatch still counted as "level" for absorb and outlet flow.
pub const ABSORB_TOL: u16 = 1;
/// A new top layer up to this multiple of the current top area is a bowl/wall: always expand.
pub const EXPAND_AREA_FACTOR: u32 = 4;
/// Most cells one expansion flood may visit before it is refused.
pub const EXPAND_BUDGET: u32 = 16384;
/// Steps a refused expansion waits before retrying.
pub const EXPAND_BACKOFF: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The July plan: every wet cell is a CA cell.
    CellOnly,
    /// Settled water pools into bodies; only the frontier is CA.
    Bodies,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::CellOnly => "cell",
            Mode::Bodies => "bodies",
        }
    }
}
