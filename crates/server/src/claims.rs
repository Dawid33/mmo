//! The server's owner-epoch claims: at start a process claims the
//! super-regions it owns ([`HostClaims::claim`]) and keeps that
//! [`Fence`] for its life. The world store checks it on every region load and
//! cut commit; once another process claims one of those super-regions the
//! store answers [`StoreError::Fenced`], and the process is fenced for good
//! ([`Fenced`]): it stops ticking, refuses its clients and exits with
//! [`EXIT_FENCED`] without a last save.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use game::{Ownership, RegionCoords, StoredRegion, WorldCut};
use world_store::{Fence, StoreError, StoreHandle, SuperRegion};

/// The process exit status of a fenced host (another process took over its
/// super-regions): non-zero, and distinct from a start-up error (1).
pub const EXIT_FENCED: i32 = 3;

/// What the client sees when its server was fenced.
pub const FENCED_REFUSAL: &str = "this server was replaced; reconnect";

/// How a server's life ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerExit {
    /// An orderly stop (stop signal, last single-player client gone).
    Clean,
    /// Another process claimed this one's super-regions.
    Fenced,
}

impl ServerExit {
    /// The process exit status.
    pub fn code(self) -> i32 {
        match self {
            ServerExit::Clean => 0,
            ServerExit::Fenced => EXIT_FENCED,
        }
    }
}

/// The process-wide "I was fenced" flag: set by the cut writer and the
/// region threads when the store refuses them as fenced, polled by the main
/// loop. Clones share it.
#[derive(Clone, Debug, Default)]
pub struct Fenced(Arc<AtomicBool>);

impl Fenced {
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Sets the flag if `error` is a fence refusal; returns whether it was.
    pub fn note(&self, error: &StoreError) -> bool {
        let fenced = matches!(error, StoreError::Fenced { .. });
        if fenced {
            self.set();
        }
        fenced
    }
}

/// The world store with the process's claimed fence. Cheap to clone; every
/// clone shares the fence and the [`Fenced`] flag.
#[derive(Clone)]
pub struct HostClaims {
    store: StoreHandle,
    fence: Arc<Fence>,
    fenced: Fenced,
}

impl HostClaims {
    /// Claims, as `ownership`'s host ordinal, the super-regions of the map
    /// `extent` it owns (`AllInOne`: all of them; a world with no map claims
    /// none). A store whose super-region side differs from `super_region_regions`
    /// (`VOXEL_SUPER_REGION_REGIONS`) is refused.
    pub fn claim(
        store: StoreHandle,
        ownership: Ownership,
        extent: Option<game::MapExtent>,
        super_region_regions: u32,
    ) -> Result<HostClaims, String> {
        let stored = store.super_region_regions();
        if stored != super_region_regions {
            return Err(format!(
                "the world store was made with {stored} regions per super-region side but \
                 {SUPER_REGION_REGIONS_ENV}={super_region_regions} (default {}); set it to {stored}",
                game::DEFAULT_SUPER_REGION_REGIONS
            ));
        }
        let srs: Vec<SuperRegion> = ownership.claim_set(extent, stored).into_iter().collect();
        HostClaims::claim_srs(store, ownership.ordinal(), srs).map_err(|e| format!("claiming the super-regions: {e}"))
    }

    /// Claims exactly `srs` as `owner`.
    pub fn claim_srs(store: StoreHandle, owner: u32, srs: Vec<SuperRegion>) -> Result<HostClaims, StoreError> {
        let fence = store.claim_super_regions(owner, srs)?;
        Ok(HostClaims { store, fence: Arc::new(fence), fenced: Fenced::default() })
    }

    /// Claims, as owner 0, the super-regions of every region within `radius`
    /// of `center` — for tests and tools that run regions without a map.
    pub fn claim_around(store: StoreHandle, center: RegionCoords, radius: i32) -> Result<HostClaims, StoreError> {
        let s = store.super_region_regions();
        let mut srs = std::collections::BTreeSet::new();
        for x in center.x - radius..=center.x + radius {
            for z in center.z - radius..=center.z + radius {
                srs.insert(game::super_region_of(RegionCoords::new(x, z), s));
            }
        }
        HostClaims::claim_srs(store, 0, srs.into_iter().collect())
    }

    pub fn store(&self) -> &StoreHandle {
        &self.store
    }

    pub fn fence(&self) -> &Fence {
        &self.fence
    }

    pub fn fenced(&self) -> &Fenced {
        &self.fenced
    }

    /// Loads `rc` from the store under the fence; a fence refusal also sets
    /// the [`Fenced`] flag.
    pub fn load_region(&self, rc: RegionCoords) -> Result<Option<StoredRegion>, StoreError> {
        let loaded = self.store.load_region(rc, &self.fence);
        if let Err(e) = &loaded {
            self.fenced.note(e);
        }
        loaded
    }

    /// Queues `cut` for commit under the fence; the answer arrives on the receiver.
    pub fn commit_cut(
        &self,
        cut: WorldCut,
    ) -> crossbeam::channel::Receiver<Result<world_store::CutCommitted, StoreError>> {
        self.store.commit_cut(cut, (*self.fence).clone())
    }
}

/// Environment: regions per super-region side; must match the world store's.
pub const SUPER_REGION_REGIONS_ENV: &str = "VOXEL_SUPER_REGION_REGIONS";

/// The super-region side from `VOXEL_SUPER_REGION_REGIONS`; unset or empty is
/// [`game::DEFAULT_SUPER_REGION_REGIONS`].
pub fn super_region_regions_from(value: Option<&str>) -> Result<u32, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(game::DEFAULT_SUPER_REGION_REGIONS),
        Some(v) => v
            .parse::<u32>()
            .ok()
            .filter(|s| *s >= 1)
            .ok_or_else(|| format!("{SUPER_REGION_REGIONS_ENV}={v:?} is not a whole number >= 1")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_store() -> StoreHandle {
        StoreHandle::open(&world_store::StoreUrl::Memory, world_store::Dials::BUILT, world_store::OpenMode::Writer).unwrap()
    }

    #[test]
    fn claims_the_owned_share_as_the_host_ordinal() {
        let store = memory_store();
        let s = store.super_region_regions();
        let extent = game::MapExtent { origin: RegionCoords::new(-20, -5), width: 40, height: 10 };
        let all = HostClaims::claim(store.clone(), Ownership::AllInOne, Some(extent), s).unwrap();
        assert_eq!(all.fence().owner, 0);
        assert_eq!(all.fence().epochs.len(), 8, "every super-region of the extent");
        let none = HostClaims::claim(store.clone(), Ownership::AllInOne, None, s).unwrap();
        assert!(none.fence().epochs.is_empty(), "a world with no map claims nothing");
        let hosts = 3;
        let mine = Ownership::Static { ordinal: 2, hosts };
        let share = HostClaims::claim(store.clone(), mine, Some(extent), s).unwrap();
        assert_eq!(share.fence().owner, 2);
        assert!(share.fence().epochs.keys().all(|sr| game::rendezvous_owner(*sr, hosts) == 2));
        store.shutdown();
    }

    #[test]
    fn a_different_super_region_side_refuses_the_start() {
        let store = memory_store();
        let stored = store.super_region_regions();
        let err = HostClaims::claim(store.clone(), Ownership::AllInOne, None, stored + 1).err().expect("refused");
        assert!(err.contains(&stored.to_string()) && err.contains(&(stored + 1).to_string()), "{err}");
        assert!(err.contains(SUPER_REGION_REGIONS_ENV), "{err}");
        store.shutdown();
    }

    #[test]
    fn super_region_side_from_env_value() {
        assert_eq!(super_region_regions_from(None), Ok(game::DEFAULT_SUPER_REGION_REGIONS));
        assert_eq!(super_region_regions_from(Some(" ")), Ok(game::DEFAULT_SUPER_REGION_REGIONS));
        assert_eq!(super_region_regions_from(Some("8")), Ok(8));
        assert!(super_region_regions_from(Some("0")).is_err());
        assert!(super_region_regions_from(Some("big")).is_err());
    }

    #[test]
    fn a_fence_refusal_sets_the_flag_and_other_errors_do_not() {
        let flag = Fenced::default();
        assert!(!flag.note(&StoreError::Backend("x".into())));
        assert!(!flag.is_set());
        assert!(flag.note(&StoreError::Fenced { sr: None }));
        assert!(flag.clone().is_set(), "clones share the flag");
    }

    #[test]
    fn exit_codes() {
        assert_eq!(ServerExit::Clean.code(), 0);
        assert_eq!(ServerExit::Fenced.code(), 3);
        assert_eq!(EXIT_FENCED, 3);
    }
}
