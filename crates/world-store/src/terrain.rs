//! The world store as the game's [`game::StoredTerrain`]: halo
//! seeds of regions that have not run in this process read their saved
//! terrain through it.

use game::{RegionCoords, StoredTerrain, TerrainSnapshot};

use crate::StoreHandle;

/// [`StoredTerrain`] over [`StoreHandle::load_terrain`].
#[derive(Clone)]
pub struct TerrainFromStore(pub StoreHandle);

impl StoredTerrain for TerrainFromStore {
    fn load_terrain(&self, rc: RegionCoords) -> Result<Option<TerrainSnapshot>, String> {
        self.0.load_terrain(rc).map_err(|e| e.to_string())
    }
}
