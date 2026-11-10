//! Terrain of regions the world store holds, read by halo seeds of regions
//! that have not run in this process (a saved region's neighbours' halos show
//! its saved edits, not pristine worldgen).
//!
//! `game` stays IO-free: this is only the seam. The server's implementation
//! (`world_store::TerrainFromStore`) reads the store; tests, wasm and the
//! client have none.

use crate::{RegionCoords, TerrainSnapshot};

/// A source of stored region terrain (chunks + ground recipes).
pub trait StoredTerrain: Send + Sync {
    /// The region's stored terrain; `Ok(None)` if the store does not hold it.
    /// A caller treats an error like `None` (halos show terrain; they never
    /// stop a region) after logging it.
    fn load_terrain(&self, rc: RegionCoords) -> Result<Option<TerrainSnapshot>, String>;
}

/// `stored.load_terrain(rc)`, with a missing region or a read error logged as
/// a warning and turned into `None` (the caller seeds from worldgen). `what`
/// names the reader in the log line.
pub fn load_stored_or_warn(stored: &dyn StoredTerrain, rc: RegionCoords, what: &str) -> Option<TerrainSnapshot> {
    match stored.load_terrain(rc) {
        Ok(Some(snap)) => Some(snap),
        Ok(None) => {
            log::warn!("{what}: region {},{} is listed as stored but the store has no terrain; pristine", rc.x, rc.z);
            None
        }
        Err(e) => {
            log::warn!("{what}: cannot read region {},{} from the world store ({e}); pristine", rc.x, rc.z);
            None
        }
    }
}
