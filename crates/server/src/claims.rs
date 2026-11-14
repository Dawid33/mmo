//! The server's owner-epoch claims: at start a process claims the
//! super-regions it owns ([`HostClaims::claim`]) and keeps that
//! [`Fence`]; a host may later grow it ([`HostClaims::add`], rows won with
//! `claim_free`) or swap it for what it holds now ([`HostClaims::replace`]). The
//! world store checks the current fence on every region load and cut commit; once another process claims one of those super-regions the
//! store answers [`StoreError::Fenced`], and the process is fenced for good
//! ([`Fenced`]): it stops ticking, refuses its clients and exits with
//! [`EXIT_FENCED`] without a last save.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use game::{Ownership, RegionCoords, StoredRegion, WorldCut};
use world_store::{Fence, StoreError, StoreHandle, SuperRegion};

/// The process exit status of a fenced host (another process took over its
/// super-regions): non-zero, and distinct from a start-up error (1).
pub const EXIT_FENCED: i32 = 3;

/// The process exit status after the world coordinator thread failed (panicked): the
/// host skips its exit cut — the coordinator's last save may lack parks and crossings
/// the cut would record — and the world restarts from its last committed saves.
/// Non-zero and distinct from a start-up error (1) and a fence ([`EXIT_FENCED`]).
pub const EXIT_COORDINATOR_FAILED: i32 = 4;

/// The process exit status of a host that saw a higher world generation than its own: another
/// host rebooted after a crash, so the whole world restarts from the same save. Nothing is saved
/// on the way out. Distinct from a start-up error (1), a fence and a failed coordinator.
pub const EXIT_WORLD_ROLLBACK: i32 = 5;

/// What the client sees when its server was fenced.
pub const FENCED_REFUSAL: &str = "this server was replaced; reconnect";

/// What the client sees when the world restarts from an earlier save.
pub const ROLLBACK_REFUSAL: &str = "the world is restarting; reconnect";

/// How a server's life ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerExit {
    /// An orderly stop (stop signal, last single-player client gone).
    Clean,
    /// Another process claimed this one's super-regions.
    Fenced,
    /// The world coordinator thread failed (panicked); nothing more was saved.
    CoordinatorFailed,
    /// Another host rebooted dirty (a higher world generation): this one restarts too,
    /// without a park or a save.
    WorldRolledBack,
}

impl ServerExit {
    /// The process exit status.
    pub fn code(self) -> i32 {
        match self {
            ServerExit::Clean => 0,
            ServerExit::Fenced => EXIT_FENCED,
            ServerExit::CoordinatorFailed => EXIT_COORDINATOR_FAILED,
            ServerExit::WorldRolledBack => EXIT_WORLD_ROLLBACK,
        }
    }
}

/// The process-wide "I was fenced" flag: set by the cut writer and the
/// region threads when the store refuses them as fenced, and by the main loop
/// when its lease heartbeat answers that the boot was replaced
/// ([`crate::lease::LeaseState::Replaced`]); polled by the main loop. Clones share it.
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

/// The world generation this host boots at, and the process-wide "the world moved on" flag:
/// [`WorldGeneration::note`] sets it when a peer reports a higher generation, the main loop
/// polls it ([`ServerExit::WorldRolledBack`]). Clones share the flag. A process that never
/// registered as a host (role `all`, no store) runs at generation 0 and is never noted.
#[derive(Clone, Debug, Default)]
pub struct WorldGeneration {
    mine: u64,
    flag: Arc<AtomicBool>,
}

impl WorldGeneration {
    pub fn new(mine: u64) -> Self {
        WorldGeneration { mine, flag: Arc::default() }
    }

    /// The generation this boot runs at.
    pub fn mine(&self) -> u64 {
        self.mine
    }

    /// A generation seen from a peer or the store: a higher one than ours sets the flag.
    pub fn note(&self, seen: u64) {
        if seen > self.mine {
            self.flag.store(true, Ordering::SeqCst);
        }
    }

    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// The world store with the process's claimed fence. Cheap to clone; every
/// clone shares the fence (and its changes) and the [`Fenced`] flag.
#[derive(Clone)]
pub struct HostClaims {
    store: StoreHandle,
    /// The current fence: read whole by every load and commit, swapped whole by
    /// [`HostClaims::add`] / [`HostClaims::replace`].
    fence: Arc<RwLock<Arc<Fence>>>,
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
        let mut fence = store
            .claim_super_regions(ownership.ordinal(), srs)
            .map_err(|e| format!("claiming the super-regions: {e}"))?;
        // All-in-one runs every host's regions: it adopts (and its cuts replace) every
        // host's in-flight rows.
        fence.all_hosts = matches!(ownership, Ownership::AllInOne);
        Ok(HostClaims::with_fence(store, fence))
    }

    /// Claims exactly `srs` as `owner`.
    pub fn claim_srs(store: StoreHandle, owner: u32, srs: Vec<SuperRegion>) -> Result<HostClaims, StoreError> {
        let fence = store.claim_super_regions(owner, srs)?;
        Ok(HostClaims::with_fence(store, fence))
    }

    fn with_fence(store: StoreHandle, fence: Fence) -> HostClaims {
        HostClaims { store, fence: Arc::new(RwLock::new(Arc::new(fence))), fenced: Fenced::default() }
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

    /// The current fence (a snapshot: a later [`Self::add`] / [`Self::replace`] does not
    /// change it).
    pub fn fence(&self) -> Arc<Fence> {
        self.fence.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Grows the fence by `more` (rows this host won since, e.g. with `claim_free`): its
    /// epochs are merged in, a super-region in both taking `more`'s. The next load or
    /// commit is checked against them too (fenced anywhere = fenced).
    pub fn add(&self, more: Fence) {
        let mut current = self.fence.write().unwrap_or_else(|e| e.into_inner());
        debug_assert_eq!(more.owner, current.owner, "a fence grows only by its own owner's rows");
        let mut grown = (**current).clone();
        grown.epochs.extend(more.epochs);
        *current = Arc::new(grown);
    }

    /// Swaps the fence for `fence` (what this host holds now, e.g. `owned_by`). The
    /// process's `all_hosts` is kept: it is how the process runs, not a stored row.
    pub fn replace(&self, mut fence: Fence) {
        let mut current = self.fence.write().unwrap_or_else(|e| e.into_inner());
        debug_assert_eq!(fence.owner, current.owner, "a fence is replaced only by its own owner's");
        fence.all_hosts = current.all_hosts;
        *current = Arc::new(fence);
    }

    pub fn fenced(&self) -> &Fenced {
        &self.fenced
    }

    /// Sets the [`Fenced`] flag if `error` is a fence refusal that concerns
    /// this process's claims (the coordinator row, or a super-region it
    /// holds). A refusal naming a super-region it never claimed is a stray
    /// write of ours, not a takeover: logged, flag untouched. Returns whether
    /// the process is now fenced by `error`.
    pub fn note_refusal(&self, error: &StoreError) -> bool {
        match error {
            StoreError::Fenced { sr: Some(sr) } if !self.fence().epochs.contains_key(sr) => {
                log::error!("[server] a write touched super-region {sr:?}, which this host never claimed; refused (not a takeover)");
                false
            }
            other => self.fenced.note(other),
        }
    }

    /// Loads `rc` from the store under the current fence; a fence refusal also sets
    /// the [`Fenced`] flag.
    pub fn load_region(&self, rc: RegionCoords) -> Result<Option<StoredRegion>, StoreError> {
        let loaded = self.store.load_region(rc, &self.fence());
        if let Err(e) = &loaded {
            self.note_refusal(e);
        }
        loaded
    }

    /// Queues `cut` for commit under the current fence; the answer arrives on the receiver.
    pub fn commit_cut(
        &self,
        cut: WorldCut,
    ) -> crossbeam::channel::Receiver<Result<world_store::CutCommitted, StoreError>> {
        self.store.commit_cut(cut, (*self.fence()).clone())
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

    /// A cut that writes nothing but the world tick: fenced by every super-region of its fence.
    fn empty_cut(id: u64) -> WorldCut {
        WorldCut {
            id: game::CutId(id),
            host: 0,
            tick: 100 * id,
            regions: vec![],
            in_flight: vec![],
            players: vec![],
            history: None,
        }
    }

    #[test]
    fn added_super_regions_are_checked_by_the_next_commit() {
        let store = memory_store();
        let (x, y) = ((0, 0), (0, -1));
        let claims = HostClaims::claim_srs(store.clone(), 0, vec![x]).unwrap();
        claims.add(store.claim_free(0, vec![y]).unwrap());
        assert_eq!(claims.fence().epochs.keys().copied().collect::<Vec<_>>(), vec![y, x]);
        claims.commit_cut(empty_cut(1)).recv().unwrap().expect("both rows are held");
        // Another process takes the added row: the next commit is fenced on it, and that
        // refusal concerns this host (the row is in its current fence).
        store.claim_super_regions(1, vec![y]).unwrap();
        let err = claims.commit_cut(empty_cut(2)).recv().unwrap().unwrap_err();
        assert_eq!(err, StoreError::Fenced { sr: Some(y) });
        assert!(claims.clone().note_refusal(&err), "a takeover of an added row fences the host");
        assert!(claims.fenced().is_set());
        // Replaced by what it holds now, the fence commits again.
        claims.replace(store.owned_by(0).unwrap());
        assert_eq!(claims.fence().epochs.keys().copied().collect::<Vec<_>>(), vec![x]);
        claims.commit_cut(empty_cut(3)).recv().unwrap().expect("x alone is still held");
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
    fn higher_generation_sets_the_flag_lower_does_not() {
        let g = WorldGeneration::new(4);
        assert_eq!(g.mine(), 4);
        g.note(3);
        g.note(4);
        assert!(!g.is_set(), "an equal or lower generation is no news");
        g.note(5);
        assert!(g.clone().is_set(), "clones share the flag");
    }

    #[test]
    fn exit_codes() {
        assert_eq!(ServerExit::Clean.code(), 0);
        assert_eq!(ServerExit::Fenced.code(), 3);
        assert_eq!(EXIT_FENCED, 3);
        assert_eq!(ServerExit::CoordinatorFailed.code(), 4);
        assert_eq!(EXIT_COORDINATOR_FAILED, 4);
        assert_eq!(ServerExit::WorldRolledBack.code(), 5);
        assert_eq!(EXIT_WORLD_ROLLBACK, 5);
    }
}
