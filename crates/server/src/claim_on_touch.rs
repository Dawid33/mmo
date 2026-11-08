//! TRANSITIONAL (R2b Task 2, replaced by Task 3's claims at start): the
//! server's owner-epoch fence, claimed lazily. The world store fences every
//! region load and cut commit (`world_store::Fence`); until the server claims
//! its super-regions at start (Task 3: `AllInOne` = the whole map, `Static` =
//! its share), it claims each super-region the first time it loads a region
//! in it or commits into it, as owner [`ClaimOnTouch::OWNER`], and keeps the
//! claim for its life. One instance is shared (cloned) by everything in the
//! process that loads or commits — two would fence each other.

use std::sync::{Arc, Mutex};

use game::{RegionCoords, WorldCut, super_region_of};
use world_store::{Fence, StoreError, StoreHandle, SuperRegion, touched_super_regions};

/// The world store with the process's lazily claimed fence.
#[derive(Clone)]
pub struct ClaimOnTouch {
    store: StoreHandle,
    fence: Arc<Mutex<Fence>>,
}

impl ClaimOnTouch {
    /// The owner id every claim is made as (the only host).
    pub const OWNER: u32 = 0;

    pub fn new(store: StoreHandle) -> Self {
        ClaimOnTouch { store, fence: Arc::new(Mutex::new(Fence { owner: Self::OWNER, ..Fence::default() })) }
    }

    pub fn store(&self) -> &StoreHandle {
        &self.store
    }

    /// The fence, after claiming whichever of `srs` it does not hold yet.
    pub fn fence_for(&self, srs: impl IntoIterator<Item = SuperRegion>) -> Result<Fence, StoreError> {
        let mut fence = self.fence.lock().unwrap_or_else(|e| e.into_inner());
        let missing: Vec<SuperRegion> = srs.into_iter().filter(|sr| !fence.epochs.contains_key(sr)).collect();
        if !missing.is_empty() {
            let claimed = self.store.claim_super_regions(Self::OWNER, missing)?;
            fence.epochs.extend(claimed.epochs);
        }
        Ok(fence.clone())
    }

    /// The fence for loading `rc`.
    pub fn fence_for_region(&self, rc: RegionCoords) -> Result<Fence, StoreError> {
        self.fence_for([super_region_of(rc, self.store.super_region_regions())])
    }

    /// The fence for committing `cut`.
    pub fn fence_for_cut(&self, cut: &WorldCut) -> Result<Fence, StoreError> {
        self.fence_for(touched_super_regions(cut, self.store.super_region_regions()))
    }

    /// Loads `rc` from the store under the fence.
    pub fn load_region(&self, rc: RegionCoords) -> Result<Option<game::StoredRegion>, StoreError> {
        let fence = self.fence_for_region(rc)?;
        self.store.load_region(rc, &fence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_each_super_region_once_and_clones_share_it() {
        let store = StoreHandle::open(&world_store::StoreUrl::Memory, world_store::Dials::BUILT, world_store::OpenMode::Writer)
            .unwrap();
        let claims = ClaimOnTouch::new(store.clone());
        let first = claims.fence_for_region(RegionCoords::new(0, 0)).unwrap();
        let again = claims.clone().fence_for([(0, 0), (-1, 0)]).unwrap();
        assert_eq!(again.epochs[&(0, 0)], first.epochs[&(0, 0)], "a held super-region is not claimed again");
        assert_eq!(again.epochs.len(), 2);
        assert_eq!(claims.fence_for_region(RegionCoords::new(-1, 3)).unwrap(), again, "the clone's claim is shared");
        store.shutdown();
    }
}
