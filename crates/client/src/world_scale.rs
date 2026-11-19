//! World-size knobs: how much world the client subscribes to. The server is
//! demand-driven (it only generates/simulates what clients request), so
//! shrinking these shrinks both sides. Chunk and region dimensions are baked
//! into packed formats and shaders — the *window* is the dial.
//!
//! Resolved once from the environment by the binaries; `GameInstanceManager`
//! itself stays env-free so tests and harnesses keep explicit control.

use std::fmt;

/// Production near-window radius: 2 = the 5x5 block of simulated regions at
/// 64 m regions; 0 = one region from 256 m regions up (the play recipes build
/// 256 m regions), which is already 16x (64x at 512) the area.
/// The LOD ring table is NOT widened with it: `desired_lod` subtracts the
/// near window, so the factor-2 ring (bound 2) is simply absorbed into
/// simulated terrain and the rings from factor 4 out — and the horizon —
/// stay where they were.
pub const DEFAULT_NEAR_RADIUS: i32 = if game::REGION_BLOCKS >= 256 { 0 } else { 2 };
/// Upper bound on `VOXEL_NEAR_RADIUS`. Every near region is a full sim +
/// collider genesis on the server and a predicted copy on the client, so a
/// typo like `10` (441 regions) should fail loudly instead of thrashing.
pub const MAX_NEAR_RADIUS: i32 = 3;
/// `small` preset ring table: one ring at the production table's finest
/// factor (2 at the default region size), so the far path still has
/// something to draw (24 keys around a radius-0 near window).
pub const SMALL_LOD_RING_TABLE: &[(i32, u32)] = &[(2, game::lod::LOD_RING_TABLE[0].1)];
/// Upper bound on `VOXEL_VIEW_DISTANCE_M` (`?view=` in the browser), in
/// metres. Every ring region is a `RequestLod` key the server builds and the
/// client meshes (624 at the default 3 km and 256 m regions, ~4,560 at 15 km); past 16 km
/// the coarsest factor (512) runs over the pixel budget's spacing and the key
/// count climbs fast, so a typo should fail loudly instead of flooding both sides.
pub const MAX_VIEW_DISTANCE_M: f32 = 16_384.0;

/// The resolved near window + LOD ring table.
#[derive(bevy::prelude::Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldScale {
    /// Radius of the simulated near window (0 = home region alone).
    pub near_radius: i32,
    /// `(chebyshev_bound, lod_factor)` rings; empty = no streamed LOD.
    pub lod_table: &'static [(i32, u32)],
}

impl WorldScale {
    /// Production: 5x5 near window + the full ring table.
    pub const FULL: Self = Self {
        near_radius: DEFAULT_NEAR_RADIUS,
        lod_table: game::lod::LOD_RING_TABLE,
    };
    /// One simulated region plus a single factor-2 LOD ring.
    pub const SMALL: Self = Self {
        near_radius: 0,
        lod_table: SMALL_LOD_RING_TABLE,
    };
    /// One simulated region, no LOD at all — the cheapest world that loads.
    pub const TINY: Self = Self {
        near_radius: 0,
        lod_table: &[],
    };

    /// Regions in the near window when the viewer stands in their home region.
    pub const fn near_region_count(&self) -> usize {
        let edge = (2 * self.near_radius + 1) as usize;
        edge * edge
    }

    /// Resolve optional environment values. `preset` (`VOXEL_WORLD`) picks a
    /// named pairing; `near_radius` (`VOXEL_NEAR_RADIUS`) and `lod_rings`
    /// (`VOXEL_LOD_RINGS`) override its parts individually.
    pub fn resolve(
        preset: Option<&str>,
        near_radius: Option<&str>,
        lod_rings: Option<&str>,
    ) -> Result<Self, WorldScaleError> {
        let mut scale = match preset {
            None | Some("full") => Self::FULL,
            Some("small") => Self::SMALL,
            Some("tiny") => Self::TINY,
            Some(value) => return Err(WorldScaleError::InvalidPreset(value.to_owned())),
        };
        if let Some(value) = near_radius {
            scale.near_radius = value
                .parse::<i32>()
                .ok()
                .filter(|radius| (0..=MAX_NEAR_RADIUS).contains(radius))
                .ok_or_else(|| WorldScaleError::InvalidNearRadius(value.to_owned()))?;
        }
        if let Some(value) = lod_rings {
            scale.lod_table = parse_lod_rings(value)?;
        }
        Ok(scale)
    }

    /// The ring table reaching `value` metres (`VOXEL_VIEW_DISTANCE_M`),
    /// rounded up to whole regions, built by the pixel-budget rule
    /// (`game::lod::ring_table_for`). The table is leaked, like an explicit
    /// `VOXEL_LOD_RINGS` one: this runs once per process.
    pub fn with_view_distance_m(mut self, value: &str) -> Result<Self, WorldScaleError> {
        let invalid = || WorldScaleError::InvalidViewDistance(value.to_owned());
        let metres = value.trim().parse::<f32>().map_err(|_| invalid())?;
        if !(metres > 0.0 && metres <= MAX_VIEW_DISTANCE_M) {
            return Err(invalid());
        }
        let regions = (metres * game::movement::UNITS_PER_METER / game::REGION_SIZE).ceil() as i32;
        self.lod_table = Box::leak(game::lod::ring_table_for(regions).into_boxed_slice());
        Ok(self)
    }

    /// How far the LOD rings reach, in world units (0 without rings).
    pub fn lod_reach_units(&self) -> f32 {
        game::lod::ring_table_reach_units(self.lod_table)
    }

    /// Resolve the process environment on native; browser builds read the
    /// page URL's `?world=` preset and `?view=` distance instead (default full).
    pub fn from_env() -> Result<Self, WorldScaleError> {
        #[cfg(target_arch = "wasm32")]
        {
            // No environment in the browser: `?world=tiny|small|full` in the
            // page URL picks the preset (default full).
            let search = web_sys::window().and_then(|w| w.location().search().ok());
            let preset = search.as_deref().and_then(|s| query_param(s, "world"));
            let scale = Self::resolve(preset, None, None)?;
            match search.as_deref().and_then(|s| query_param(s, "view")) {
                Some(view) => scale.with_view_distance_m(view),
                None => Ok(scale),
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Empty counts as unset: the justfile exports `VOXEL_WORLD=""`
            // whenever its `world` variable is left at the default.
            let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
            let preset = var("VOXEL_WORLD");
            let near_radius = var("VOXEL_NEAR_RADIUS");
            let lod_rings = var("VOXEL_LOD_RINGS");
            let scale = Self::resolve(preset.as_deref(), near_radius.as_deref(), lod_rings.as_deref())?;
            // An explicit `VOXEL_LOD_RINGS` table wins over the distance.
            match var("VOXEL_VIEW_DISTANCE_M") {
                Some(view) if lod_rings.is_none() => scale.with_view_distance_m(&view),
                _ => Ok(scale),
            }
        }
    }
}

impl Default for WorldScale {
    fn default() -> Self {
        Self::FULL
    }
}

impl fmt::Display for WorldScale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "near_radius={} ({} regions), lod_rings={:?}",
            self.near_radius,
            self.near_region_count(),
            self.lod_table
        )
    }
}

/// One parameter of a page URL's query string (`?world=small`,
/// `?port=1&world=tiny&view=3000`), for the browser build's [`WorldScale`].
#[cfg(target_arch = "wasm32")]
pub(crate) fn query_param<'a>(search: &'a str, name: &str) -> Option<&'a str> {
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
        .filter(|value| !value.is_empty())
}

/// `off`, `small`, `full`, or an explicit `bound:factor,...` table. An explicit
/// table is leaked: the ring table is `&'static` everywhere downstream and
/// this runs once per process.
fn parse_lod_rings(value: &str) -> Result<&'static [(i32, u32)], WorldScaleError> {
    match value {
        "off" => return Ok(&[]),
        "small" => return Ok(SMALL_LOD_RING_TABLE),
        "full" => return Ok(game::lod::LOD_RING_TABLE),
        _ => {}
    }
    let invalid = || WorldScaleError::InvalidLodRings(value.to_owned());
    let mut table: Vec<(i32, u32)> = Vec::new();
    for ring in value.split(',') {
        let (bound, factor) = ring.split_once(':').ok_or_else(invalid)?;
        let bound = bound.trim().parse::<i32>().map_err(|_| invalid())?;
        let factor = factor.trim().parse::<u32>().map_err(|_| invalid())?;
        let ascending = table.last().map_or(true, |&(prev, _)| bound > prev);
        if bound < 1 || !ascending || !game::lod::LOD_FACTORS.contains(&factor) {
            return Err(invalid());
        }
        table.push((bound, factor));
    }
    Ok(Box::leak(table.into_boxed_slice()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldScaleError {
    InvalidPreset(String),
    InvalidNearRadius(String),
    InvalidLodRings(String),
    InvalidViewDistance(String),
}

impl fmt::Display for WorldScaleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPreset(value) => write!(
                f,
                "invalid VOXEL_WORLD value `{value}` (expected tiny, small, or full)"
            ),
            Self::InvalidNearRadius(value) => write!(
                f,
                "invalid VOXEL_NEAR_RADIUS value `{value}` (expected integer 0..={MAX_NEAR_RADIUS})"
            ),
            Self::InvalidLodRings(value) => write!(
                f,
                "invalid VOXEL_LOD_RINGS value `{value}` (expected off, small, full, or \
                 ascending `bound:factor,...` with factors from {:?})",
                game::lod::LOD_FACTORS
            ),
            Self::InvalidViewDistance(value) => write!(
                f,
                "invalid VOXEL_VIEW_DISTANCE_M value `{value}` (expected metres, 0 < m <= {MAX_VIEW_DISTANCE_M})"
            ),
        }
    }
}

impl std::error::Error for WorldScaleError {}

