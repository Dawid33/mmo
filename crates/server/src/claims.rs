//! The server's owner-epoch claims: at start a process claims the
//! super-regions it owns ([`HostClaims::claim`]) and keeps that
//! [`Fence`]; a host may later grow it ([`HostClaims::add`], rows the coordinator assigned
//! it) or swap it for what it holds now ([`HostClaims::replace`]). The
//! world store checks the current fence on every region load and cut commit; once another process claims one of those super-regions the
//! store answers [`StoreError::Fenced`], and the process is fenced for good
//! ([`Fenced`]): it stops ticking, refuses its clients and exits with
//! [`EXIT_FENCED`] without a last save.
//!
//! Under `claim` (R4) a host holds nothing at boot ([`HostClaims::none`]), registers, then
//! re-claims its own rows ([`HostClaims::reclaim_own`]); the main loop's
//! [`DirectoryKeeper`] fences the land the coordinator assigns it and reads the directory; an heir's
//! [`AdoptionReader`] reads what it inherits (R4, a succession).

use game::HostId;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, TryRecvError};
use game::{
    EntityId, InFlightRow, Ownership, RegionCoords, RegionSpawner, StoredRegion, SuccessionId,
    WorldCut, WorldManager,
};
use world_store::{Fence, HostRow, StoreError, StoreHandle, SuperRegion};

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

/// What the client sees when its server was fenced, the world rolled back, or its host left
/// (defined in `game` so the client can match them: it rejoins on each).
pub use game::{FENCED_REFUSAL, LEAVING_REFUSAL, ROLLBACK_REFUSAL};

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
    /// The flag was set by [`Self::stop_for_rollback`] (this host's own stop), not by a higher
    /// generation seen: [`Self::why`] says which.
    own: Arc<AtomicBool>,
}

impl WorldGeneration {
    pub fn new(mine: u64) -> Self {
        WorldGeneration {
            mine,
            flag: Arc::default(),
            own: Arc::default(),
        }
    }

    /// Why the flag is set, for the stop's log lines: this host's own stop for a rollback, or
    /// another host's restart from a crash (a higher world generation seen).
    pub fn why(&self) -> String {
        if self.own.load(Ordering::SeqCst) {
            "this host traded state with a dead host that no succession accounts for (it stops for a rollback)".into()
        } else {
            format!(
                "another host restarted dirty (a world generation higher than {})",
                self.mine
            )
        }
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

    /// This host must stop as for a rollback — no save, its `hosts` row left dirty — because it
    /// holds state nobody can account for (Elastic 2a, `WorldManager::take_unaccounted_dead`):
    /// its lapse rolls the world back to the last committed cut.
    pub fn stop_for_rollback(&self) {
        self.own.store(true, Ordering::SeqCst);
        self.flag.store(true, Ordering::SeqCst);
    }
}

/// The world store with the process's claimed fence. Cheap to clone; every
/// clone shares the fence (and its changes) and the [`Fenced`] flag.
#[derive(Clone)]
pub struct HostClaims {
    store: StoreHandle,
    /// The current fence: read whole by every load and commit, swapped whole by
    /// [`HostClaims::add`] / [`HostClaims::replace`] / [`HostClaims::shrink`].
    fence: Arc<RwLock<Arc<Fence>>>,
    fenced: Fenced,
    /// Entities a region load leaves out ([`HostClaims::forget_entities`]: an heir's drop
    /// set, R4), read whole by every load.
    forgotten: Arc<RwLock<Arc<BTreeSet<EntityId>>>>,
}

impl HostClaims {
    /// Claims, as `ownership`'s host ordinal, the super-regions of the map
    /// `extent` it owns (`AllInOne`: all of them; a world with no map claims
    /// none). A store whose super-region side differs from `super_region_regions`
    /// (`VOXEL_SUPER_REGION_REGIONS`) is refused.
    pub fn claim(
        store: StoreHandle,
        ownership: &dyn Ownership,
        extent: Option<game::MapExtent>,
        super_region_regions: u32,
    ) -> Result<HostClaims, String> {
        HostClaims::check_side(&store, super_region_regions)?;
        let stored = store.super_region_regions();
        let srs: Vec<SuperRegion> = ownership.claim_set(extent, stored).into_iter().collect();
        let mut fence = store
            .claim_super_regions(ownership.me(), srs)
            .map_err(|e| format!("claiming the super-regions: {e}"))?;
        // All-in-one runs every host's regions: it adopts (and its cuts replace) every
        // host's in-flight rows.
        fence.all_hosts = ownership.runs_every_host();
        Ok(HostClaims::with_fence(store, fence))
    }

    /// Refuses a store whose super-region side differs from `super_region_regions`
    /// (`VOXEL_SUPER_REGION_REGIONS`).
    pub fn check_side(store: &StoreHandle, super_region_regions: u32) -> Result<(), String> {
        let stored = store.super_region_regions();
        if stored != super_region_regions {
            return Err(format!(
                "the world store was made with {stored} regions per super-region side but \
                 {SUPER_REGION_REGIONS_ENV}={super_region_regions} (default {}); set it to {stored}",
                game::DEFAULT_SUPER_REGION_REGIONS
            ));
        }
        Ok(())
    }

    /// Claims exactly `srs` as `owner`.
    pub fn claim_srs(
        store: StoreHandle,
        owner: HostId,
        srs: Vec<SuperRegion>,
    ) -> Result<HostClaims, StoreError> {
        let fence = store.claim_super_regions(owner, srs)?;
        Ok(HostClaims::with_fence(store, fence))
    }

    /// Host `owner` holding nothing yet: what a host claiming on demand (`claim`) registers
    /// under — a boot told to wait must not have touched its predecessor's rows.
    pub fn none(store: StoreHandle, owner: HostId) -> HostClaims {
        HostClaims::with_fence(
            store,
            Fence {
                owner,
                epochs: std::collections::BTreeMap::new(),
                all_hosts: false,
            },
        )
    }

    /// Re-claims what the store says this host's ordinal holds (its previous boot's rows,
    /// `owned_by`) with `claim_super_regions_live` as boot `boot` — every epoch bumped, so
    /// that boot is fenced from now on — and makes it the fence. Called once, right after this
    /// boot registered (a registered live row is this ordinal's: no other process may hold its
    /// rows then). Refused with `Fenced` if this boot was declared dead meanwhile (its rows
    /// went to an heir between `owned_by` and the claim). How many super-regions it re-claimed.
    ///
    /// A claim that loses a lock conflict (Postgres picks it in a deadlock with a declaration
    /// of this very boot — the declaration holds the `hosts` row and waits for the rows this
    /// claim re-took, which waits for that row, `world_store::succession`'s lock notes — or
    /// its lock timeout runs out) re-reads this boot's `hosts` row: no longer this live boot
    /// (the declaration won) → `Fenced`, the fenced exit (3), never a start-up error; still
    /// live → asked once more. A second lost conflict re-reads the row again: not live →
    /// `Fenced`; still live → that conflict is the error.
    pub fn reclaim_own(&self, boot: u64) -> Result<usize, StoreError> {
        let owner = self.fence().owner;
        let mut asked = 0;
        loop {
            asked += 1;
            match self.reclaim_own_once(boot) {
                Err(e) if e.is_lock_conflict() => {
                    let live = self
                        .store
                        .hosts()?
                        .iter()
                        .any(|r| r.id == owner && r.boot == boot && r.state == game::HOST_LIVE);
                    if !live {
                        log::error!(
                            "[server] re-claiming host {owner}'s rows lost a lock conflict ({e}) to its own declaration: fenced"
                        );
                        return Err(StoreError::Fenced { sr: None });
                    }
                    if asked >= 2 {
                        return Err(e);
                    }
                    log::warn!(
                        "[server] re-claiming host {owner}'s rows lost a lock conflict ({e}); its row is still live: once more"
                    );
                }
                other => return other,
            }
        }
    }

    fn reclaim_own_once(&self, boot: u64) -> Result<usize, StoreError> {
        let owner = self.fence().owner;
        let srs: Vec<SuperRegion> = self.store.owned_by(owner)?.epochs.into_keys().collect();
        if srs.is_empty() {
            return Ok(0);
        }
        let fence = self.store.claim_super_regions_live(owner, boot, srs)?;
        let n = fence.epochs.len();
        self.replace(fence);
        Ok(n)
    }

    fn with_fence(store: StoreHandle, fence: Fence) -> HostClaims {
        HostClaims {
            store,
            fence: Arc::new(RwLock::new(Arc::new(fence))),
            fenced: Fenced::default(),
            forgotten: Arc::default(),
        }
    }

    /// Claims, as owner 0, the super-regions of every region within `radius`
    /// of `center` — for tests and tools that run regions without a map.
    pub fn claim_around(
        store: StoreHandle,
        center: RegionCoords,
        radius: i32,
    ) -> Result<HostClaims, StoreError> {
        let s = store.super_region_regions();
        let mut srs = std::collections::BTreeSet::new();
        for x in center.x - radius..=center.x + radius {
            for z in center.z - radius..=center.z + radius {
                srs.insert(game::super_region_of(RegionCoords::new(x, z), s));
            }
        }
        HostClaims::claim_srs(store, HostId::SOLO, srs.into_iter().collect())
    }

    pub fn store(&self) -> &StoreHandle {
        &self.store
    }

    /// The current fence (a snapshot: a later [`Self::add`] / [`Self::replace`] does not
    /// change it).
    pub fn fence(&self) -> Arc<Fence> {
        self.fence.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Grows the fence by `more` (rows this host won since, e.g. assigned by the coordinator): its
    /// epochs are merged in, a super-region in both taking `more`'s. The next load or
    /// commit is checked against them too (fenced anywhere = fenced).
    pub fn add(&self, more: Fence) {
        let mut current = self.fence.write().unwrap_or_else(|e| e.into_inner());
        // Hard: another owner's rows in this fence would let this host write as that owner.
        assert_eq!(
            more.owner, current.owner,
            "a fence grows only by its own owner's rows"
        );
        let mut grown = (**current).clone();
        grown.epochs.extend(more.epochs);
        *current = Arc::new(grown);
    }

    /// Grows the fence to what the store says this host holds now (`owned_by`, an heir's
    /// inherited rows among them): every row of `held` is in it at `held`'s epoch, or at the
    /// current one if that is higher (won since `held` was read). Nothing is dropped: a row
    /// the current fence holds and `held` lacks was won after the read (a claim answered
    /// meanwhile) or was taken from this host — then the next write under it is refused
    /// `Fenced` and the host is fenced, as it must be.
    pub fn grow_to(&self, held: Fence) {
        let mut current = self.fence.write().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            held.owner, current.owner,
            "a fence grows only by its own owner's rows"
        );
        let mut grown = (**current).clone();
        for (sr, epoch) in held.epochs {
            let e = grown.epochs.entry(sr).or_insert(epoch);
            *e = (*e).max(epoch);
        }
        *current = Arc::new(grown);
    }

    /// Shrinks the fence by `released` (Elastic 3: the super-regions a committed cut part gave
    /// back, each with the epoch it was RELEASED at — the part's fence held it — now free at
    /// that epoch + 1): a key leaves the fence only while the fence still holds that epoch for
    /// it; nothing else changes. A key the fence holds at another epoch was assigned to this
    /// host again after the release committed and before this host settled the cut (its
    /// `Assigned` came first, [`Self::add`]): it is this host's in the store and stays fenced.
    /// Runs before the manager learns of the commit, so no later load or part is checked against
    /// a row this host no longer holds (it would be refused `Fenced`).
    pub fn shrink(&self, released: &[(SuperRegion, u64)]) {
        if released.is_empty() {
            return;
        }
        let mut current = self.fence.write().unwrap_or_else(|e| e.into_inner());
        let mut shrunk = (**current).clone();
        for (sr, epoch) in released {
            match shrunk.epochs.get(sr) {
                Some(held) if held == epoch => {
                    shrunk.epochs.remove(sr);
                }
                Some(held) => {
                    log::info!(
                        "[server] super-region {sr:?} was released at epoch {epoch} and is this host's again at epoch {held}: it stays in the fence"
                    );
                }
                None => {}
            }
        }
        *current = Arc::new(shrunk);
    }

    /// From now on a region load leaves `ids` out of the stored state it returns (an heir's
    /// drop set, R4: each lives elsewhere or comes from a survivor's journal); replaces the
    /// previous set, an empty one ends the filtering. Shared by every clone (the region
    /// threads load through one).
    pub fn forget_entities(&self, ids: &BTreeSet<EntityId>) {
        *self.forgotten.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(ids.clone());
    }

    /// Swaps the fence for `fence` (what this host holds now, e.g. `owned_by`). The
    /// process's `all_hosts` is kept: it is how the process runs, not a stored row.
    pub fn replace(&self, mut fence: Fence) {
        let mut current = self.fence.write().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            fence.owner, current.owner,
            "a fence is replaced only by its own owner's"
        );
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
    ///
    /// Classified against the CURRENT fence: only for a refusal of no write's fence (a
    /// claim's, `sr` `None`). A load's or commit's refusal is classified against the fence it
    /// was made under ([`Self::note_refusal_under`]): the fence may have changed since.
    pub fn note_refusal(&self, error: &StoreError) -> bool {
        self.note_refusal_under(&self.fence(), error)
    }

    /// [`Self::note_refusal`] for a load or commit made under `fence` (the snapshot it was
    /// checked against): a refusal naming a super-region of `fence` concerns this host — it
    /// held the row when it wrote — even if a later [`Self::grow_to`] / [`Self::replace`]
    /// changed the fence meanwhile.
    pub fn note_refusal_under(&self, fence: &Fence, error: &StoreError) -> bool {
        match error {
            StoreError::Fenced { sr: Some(sr) } if !fence.epochs.contains_key(sr) => {
                log::error!(
                    "[server] a write touched super-region {sr:?}, which this host never claimed; refused (not a takeover)"
                );
                false
            }
            other => self.fenced.note(other),
        }
    }

    /// Loads `rc` from the store under the current fence, leaving out the entities of
    /// [`Self::forget_entities`]; a fence refusal also sets the [`Fenced`] flag.
    pub fn load_region(&self, rc: RegionCoords) -> Result<Option<StoredRegion>, StoreError> {
        let fence = self.fence();
        let loaded = self.store.load_region(rc, &fence);
        match loaded {
            Err(e) => {
                self.note_refusal_under(&fence, &e);
                Err(e)
            }
            Ok(Some(mut region)) => {
                let forgotten = self
                    .forgotten
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if !forgotten.is_empty() {
                    region.entities.retain(|b| !forgotten.contains(&b.id));
                }
                Ok(Some(region))
            }
            Ok(None) => Ok(None),
        }
    }

    /// Queues `cut` for commit under the current fence; the answer arrives on the receiver.
    pub fn commit_cut(
        &self,
        cut: WorldCut,
    ) -> crossbeam::channel::Receiver<Result<world_store::CutCommitted, StoreError>> {
        self.store.commit_cut(cut, (*self.fence()).clone())
    }
}

// --- the directory and assigned land (R4 `claim`; assigned by the coordinator, Elastic 3) ---

/// The hosts a directory read counts live by `rows` at the database clock `now_ms`: live, not
/// left clean, beaten within [`game::HOST_LEASE_MS`] (what a booting host counts as alive),
/// and NOT marked draining (`worldctl drain`: a draining host is assigned no land, Elastic 1).
pub fn live_hosts(rows: &[HostRow], now_ms: u64) -> BTreeSet<HostId> {
    rows.iter()
        .filter(|r| {
            r.state == game::HOST_LIVE
                && !r.clean
                && now_ms.saturating_sub(r.seen_ms) <= game::HOST_LEASE_MS
        })
        .filter(|r| r.drain_in_ms.is_none())
        .map(|r| r.id)
        .collect()
}

/// The hosts whose `hosts` row carries a drain mark ([`game::WorldManager::set_draining`]).
pub fn draining_hosts(rows: &[HostRow]) -> BTreeSet<HostId> {
    rows.iter()
        .filter(|r| r.drain_in_ms.is_some())
        .map(|r| r.id)
        .collect()
}

/// The directory read whole, blocking (a boot, before the main loop): every `super_regions`
/// row (`(super-region, owner or NO_OWNER, epoch)`), the live hosts ([`live_hosts`], `me`
/// among them) and the draining ones ([`draining_hosts`]). Plain reads, no locks. The caller
/// hands them to the manager: `set_draining`, then `directory_read`.
#[allow(clippy::type_complexity)]
pub fn read_directory(
    store: &StoreHandle,
    me: HostId,
) -> Result<
    (
        Vec<(SuperRegion, i64, u64)>,
        BTreeSet<HostId>,
        BTreeSet<HostId>,
    ),
    StoreError,
> {
    let rows = store.owners()?;
    let hosts = store.hosts()?;
    let mut live = live_hosts(&hosts, store.db_now_ms()?);
    live.insert(me);
    Ok((rows, live, draining_hosts(&hosts)))
}

/// After a failed directory read, the next one is not started before this many ms (a read
/// the manager wants is not put off to the next backstop period). Also the least spacing of
/// two reads the manager wants ([`DirectoryKeeper`]).
pub const DIRECTORY_READ_RETRY_MS: u64 = 1_000;
/// At most one warning per this long about failed directory reads.
pub const DIRECTORY_WARN_MS: u64 = 10_000;

/// A directory read in flight: the owners read first, then — only once it answered — the
/// `hosts` read and the database clock.
///
/// **The order is the keeper's open-run check** ([`DirectoryKeeper`]): each read is its own
/// statement on its own pooled connection and snapshot, so two reads issued together may be
/// answered in either order. Issued one after the other, the `hosts` snapshot is never older
/// than the owners one: a succession's declaration whose moved rows the owners read shows
/// committed before the `hosts` read began, and that read shows its predecessor `succeeding`
/// (or the run settled meanwhile, and the heir's fence holds the rows since its adoption).
struct DirectoryRead {
    owners: Receiver<Result<Vec<(SuperRegion, i64, u64)>, StoreError>>,
    /// Asked once the owners read answered.
    hosts: Option<Receiver<Result<world_store::HostsRead, StoreError>>>,
    now: Option<Receiver<Result<u64, StoreError>>>,
    got: (
        Option<Vec<(SuperRegion, i64, u64)>>,
        Option<world_store::HostsRead>,
        Option<u64>,
    ),
}

impl DirectoryRead {
    fn start(store: &StoreHandle) -> DirectoryRead {
        DirectoryRead {
            owners: store.owners_later(),
            hosts: None,
            now: None,
            got: (None, None, None),
        }
    }

    /// The read's rows, live hosts, `hosts` rows and `world.next_host_id` (read before them)
    /// once all three answered; an error as soon as one fails. Asks `store` for the `hosts`
    /// rows and the clock when the owners answered.
    #[allow(clippy::type_complexity)]
    fn poll(
        &mut self,
        store: &StoreHandle,
    ) -> Option<
        Result<
            (
                Vec<(SuperRegion, i64, u64)>,
                BTreeSet<HostId>,
                Vec<HostRow>,
                u64,
            ),
            StoreError,
        >,
    > {
        fn take<T>(
            rx: &Receiver<Result<T, StoreError>>,
            slot: &mut Option<T>,
        ) -> Result<(), StoreError> {
            if slot.is_none() {
                match rx.try_recv() {
                    Ok(answer) => *slot = Some(answer?),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        return Err(StoreError::Backend(
                            "the world-store thread has stopped".into(),
                        ));
                    }
                }
            }
            Ok(())
        }
        if let Err(e) = take(&self.owners, &mut self.got.0) {
            return Some(Err(e));
        }
        if self.got.0.is_none() {
            return None;
        }
        let hosts = self.hosts.get_or_insert_with(|| store.hosts_later());
        let now = self.now.get_or_insert_with(|| store.db_now_ms_later());
        if let Err(e) = take(hosts, &mut self.got.1).and_then(|()| take(now, &mut self.got.2)) {
            return Some(Err(e));
        }
        match &mut self.got {
            (rows @ Some(_), hosts @ Some(_), Some(now)) => {
                let live = live_hosts(&hosts.as_ref().expect("matched").rows, *now);
                let hosts = hosts.take().expect("matched");
                Some(Ok((
                    rows.take().expect("matched"),
                    live,
                    hosts.rows,
                    hosts.next_host_id,
                )))
            }
            _ => None,
        }
    }
}

/// Whether directory row `row` gives host `me` (the fence's owner) a super-region its fence
/// does not hold AT THAT EPOCH: no entry, or an older one (the row was released by this host
/// and assigned to it again — the fence still holds the released epoch until the releasing cut
/// is settled here). A fence entry at the row's epoch or newer covers it (newer: a stale read).
fn unfenced(fence: &Fence, me: i64, row: &(SuperRegion, i64, u64)) -> bool {
    let (sr, owner, epoch) = row;
    *owner == me && fence.epochs.get(sr).is_none_or(|held| held < epoch)
}

/// A `claim` host's directory and assigned land, driven from its main loop (never blocks).
/// The land the coordinator assigned this host ([`game::WorldManager::take_assigned`]) goes
/// into the fence ([`HostClaims::add`]) BEFORE the manager learns it
/// ([`game::WorldManager::claimed`]): a load or a cut in it is checked against the fence from
/// the moment the manager treats it as its own. The directory is read whole on the store thread
/// when the manager wants it ([`game::WorldManager::take_directory_wanted`]) and every
/// [`world_store::TERRAIN_POLL_SECS`] as a backstop ([`game::WorldManager::directory_read`]).
///
/// A read the manager wants is started at most once per [`DIRECTORY_READ_RETRY_MS`] (a want
/// raised on every tick — a peer's stale directory sending per-tick intents — would otherwise
/// read back to back); the want is kept meanwhile.
///
/// **A lost `Assigned` is healed by a read** (Elastic 3, the plan's departure 7): a read row
/// this host owns that its fence lacks at that epoch ([`unfenced`]: no entry, or the older
/// epoch of a row it released and was assigned again), at an epoch above the one the manager
/// knows, is an assignment whose message never came. It is fenced and handed to the manager as
/// assigned FIRST, before the read itself. A row at or below the epoch the manager knows is
/// not: a read issued before this host gave the row up must not put it back.
///
/// **Never while a succession is open** (final review, A-F1): a declaration moves the
/// predecessor's rows to its heir in the store (`owner = heir`, `epoch + 1`) — to a read they
/// look exactly like a lost assignment, but they are INHERITED: the heir may use them only
/// behind its `inheriting` mask, from its order on, and after its adoption read grew the fence
/// ([`AdoptionReader`]). Fenced and told here they would be plain land of this host: a region
/// of the dead host's land could start from the store with no drop set, beside the copies the
/// survivors' journals hold. So while the read's `hosts` rows show any open run (a row
/// `succeeding`; the `hosts` read is ISSUED only after the owners read answered
/// ([`DirectoryRead`]), so a declaration the owners show is in the hosts rows too) nothing is healed — a truly lost `Assigned` is
/// then healed by the coordinator (this host asks, the owners lookup shows it the owner) or by
/// the first read after the settle.
///
/// **The manager never sees an unfenced row as its own**: a read row this host owns in the
/// store that its fence does not hold at that epoch after the heal (inherited and not adopted
/// yet, or a stale read of a row it released) is WITHHELD from the rows handed to
/// [`game::WorldManager::directory_read`] — the directory keeps what it knew of it (the
/// predecessor's row until the order; `Free`, asked for, if it knew none).
pub struct DirectoryKeeper {
    claims: HostClaims,
    /// The directory read in flight.
    reading: Option<DirectoryRead>,
    read_wanted: bool,
    next_read: Instant,
    /// When the latest read was started (the spacing of wanted reads).
    last_started: Option<Instant>,
    warned: Option<Instant>,
    /// The `hosts` rows of the latest directory read, until taken ([`Self::take_hosts_read`]:
    /// the peer links' discovery, Elastic 1 Task 4).
    hosts_read: Option<Vec<HostRow>>,
}

/// What the [`DirectoryKeeper`] asks of the manager (a trait so a test can play it).
pub trait ClaimingManager {
    /// The rows the coordinator assigned since the last call ([`WorldManager::take_assigned`]).
    fn take_assigned(&mut self) -> Vec<(SuperRegion, u64)>;
    /// `rows` are this host's, already in its fence ([`WorldManager::claimed`]).
    fn claimed(&mut self, rows: Vec<(SuperRegion, u64)>);
    /// The epoch the manager's directory knows for `sr` ([`WorldManager::known_epoch`]).
    fn known_epoch(&self, sr: SuperRegion) -> Option<u64>;
    fn directory_read(&mut self, rows: Vec<(SuperRegion, i64, u64)>, live: BTreeSet<HostId>);
    fn set_draining(&mut self, draining: BTreeSet<HostId>);
    fn take_directory_wanted(&mut self) -> bool;
    /// The clean `hosts` rows of a read, `(id, boot)` ([`WorldManager::note_clean_rows`]).
    /// The tests' fakes keep no peers: nothing to note.
    fn note_clean_rows(&mut self, _clean: Vec<(HostId, u64)>) {}
    /// The dead `hosts` rows no open succession names ([`WorldManager::forget_dead_hosts`]).
    /// The tests' fakes keep no peers: nothing to forget.
    fn forget_dead_hosts(&mut self, _dead: Vec<HostId>) {}
    /// The ids of a read's `hosts` rows and its `next_host_id`: an id below it absent from the
    /// rows was pruned ([`WorldManager::forget_absent_hosts`]). The tests' fakes keep no peers.
    fn forget_absent_hosts(&mut self, _present: BTreeSet<HostId>, _next_host_id: u64) {}
    /// Every dead `hosts` row of a read ([`WorldManager::note_dead_rows`]).
    fn note_dead_rows(&mut self, _dead: Vec<HostId>) {}
    /// Elastic 5: land a committed move brings here and not claimed yet
    /// ([`WorldManager::expected_committed`]): fenced from the first read that shows it as this
    /// host's, even while a succession is open (it came by its cut's `CutDone`, in commit order —
    /// no declaration's moved row). The tests' fakes expect none.
    fn expected_committed(&self) -> Vec<SuperRegion> {
        Vec::new()
    }
}

impl<S: RegionSpawner> ClaimingManager for WorldManager<S> {
    fn note_clean_rows(&mut self, clean: Vec<(HostId, u64)>) {
        WorldManager::note_clean_rows(self, clean)
    }
    fn forget_dead_hosts(&mut self, dead: Vec<HostId>) {
        WorldManager::forget_dead_hosts(self, dead)
    }
    fn forget_absent_hosts(&mut self, present: BTreeSet<HostId>, next_host_id: u64) {
        WorldManager::forget_absent_hosts(self, &present, next_host_id)
    }
    fn note_dead_rows(&mut self, dead: Vec<HostId>) {
        WorldManager::note_dead_rows(self, dead)
    }
    fn take_assigned(&mut self) -> Vec<(SuperRegion, u64)> {
        WorldManager::take_assigned(self)
    }
    fn claimed(&mut self, rows: Vec<(SuperRegion, u64)>) {
        WorldManager::claimed(self, rows)
    }
    fn known_epoch(&self, sr: SuperRegion) -> Option<u64> {
        WorldManager::known_epoch(self, sr)
    }
    fn directory_read(&mut self, rows: Vec<(SuperRegion, i64, u64)>, live: BTreeSet<HostId>) {
        WorldManager::directory_read(self, rows, live)
    }
    fn set_draining(&mut self, draining: BTreeSet<HostId>) {
        WorldManager::set_draining(self, draining)
    }
    fn take_directory_wanted(&mut self) -> bool {
        WorldManager::take_directory_wanted(self)
    }
    fn expected_committed(&self) -> Vec<SuperRegion> {
        WorldManager::expected_committed(self)
    }
}

impl DirectoryKeeper {
    /// The keeper of `claims` (its owner is this host); its first backstop read comes one
    /// period from now (a boot reads the directory itself, [`read_directory`]).
    pub fn new(claims: HostClaims) -> DirectoryKeeper {
        let next_read = Instant::now() + Duration::from_secs(world_store::TERRAIN_POLL_SECS);
        DirectoryKeeper {
            claims,
            reading: None,
            read_wanted: false,
            next_read,
            last_started: None,
            warned: None,
            hosts_read: None,
        }
    }

    /// The `hosts` rows the latest directory read brought (each read once): what the peer links
    /// discover hosts from ([`crate::peer_link::PeerLinks::refresh`]) — the same read, every
    /// [`world_store::TERRAIN_POLL_SECS`] or when the manager wants one, never a read of its own.
    pub fn take_hosts_read(&mut self) -> Option<Vec<HostRow>> {
        self.hosts_read.take()
    }

    fn warn(&mut self, what: &str) {
        if self
            .warned
            .is_none_or(|at| at.elapsed() >= Duration::from_millis(DIRECTORY_WARN_MS))
        {
            self.warned = Some(Instant::now());
            log::warn!("[server] {what}");
        }
    }

    /// `rows` (already this host's in the store) into the fence, then to the manager.
    fn fence_then_tell(&self, rows: Vec<(SuperRegion, u64)>, manager: &mut impl ClaimingManager) {
        let owner = self.claims.fence().owner;
        self.claims.add(Fence {
            owner,
            epochs: rows.iter().copied().collect(),
            all_hosts: false,
        });
        manager.claimed(rows);
    }

    /// One pass, in this order: a directory read that answered is handed to the manager (the
    /// heal of a lost assignment first, unfenced rows of this host withheld: the type's docs);
    /// a read is started when due, or wanted and spaced; the rows the coordinator assigned are
    /// fenced, then handed to the manager. Nothing is taken once the host is fenced (it is
    /// stopping).
    pub fn pump(&mut self, manager: &mut impl ClaimingManager) {
        if let Some(read) = self.reading.as_mut() {
            if let Some(answer) = read.poll(self.claims.store()) {
                self.reading = None;
                match answer {
                    Ok((rows, live, hosts, next_host_id)) => {
                        // A lost `Assigned`: fenced and told BEFORE the read reaches the manager
                        // — never while a succession is open (the rows may be inherited: the
                        // type's docs).
                        let fence = self.claims.fence();
                        let me = i64::try_from(fence.owner.0).unwrap_or(i64::MAX);
                        let open_run = hosts.iter().any(|r| r.succeeding);
                        let expected: BTreeSet<SuperRegion> =
                            manager.expected_committed().into_iter().collect();
                        let lost: Vec<(SuperRegion, u64)> = rows
                            .iter()
                            .filter(|row| {
                                unfenced(&fence, me, row)
                                    && row.2 > manager.known_epoch(row.0).unwrap_or(0)
                            })
                            .map(|(sr, _, epoch)| (*sr, *epoch))
                            .collect();
                        // Elastic 5: a committed move's land, once the read shows it as this host's
                        // (`COMMIT PREPARED` ran): fenced and taken even while a run is open.
                        let (moved, lost): (Vec<_>, Vec<_>) =
                            lost.into_iter().partition(|(sr, _)| expected.contains(sr));
                        if !moved.is_empty() && !self.claims.fenced().is_set() {
                            log::info!(
                                "[server] the directory shows super-region(s) {moved:?} moved to this host: fenced and taken"
                            );
                            self.fence_then_tell(moved, manager);
                        }
                        if !lost.is_empty() && !open_run && !self.claims.fenced().is_set() {
                            log::info!(
                                "[server] the directory shows super-region(s) {lost:?} assigned to this host: fenced and taken"
                            );
                            self.fence_then_tell(lost, manager);
                        }
                        // What the store gives this host and its fence still lacks is withheld:
                        // the manager never treats an unfenced row as its own.
                        let fence = self.claims.fence();
                        let (withheld, rows): (Vec<_>, Vec<_>) =
                            rows.into_iter().partition(|row| unfenced(&fence, me, row));
                        if !withheld.is_empty() {
                            let why = if open_run {
                                "a succession is open: inherited land comes by its order"
                            } else {
                                "a read older than what this host knows"
                            };
                            log::info!(
                                "[server] the directory shows {} super-region(s) as this host's that its fence lacks ({why}): withheld from the read",
                                withheld.len()
                            );
                            log::debug!("[server] withheld directory rows: {withheld:?}");
                        }
                        manager.set_draining(draining_hosts(&hosts));
                        manager.directory_read(rows, live);
                        // Every dead row is noted (`note_dead_rows`); then those no open
                        // succession names — gone for good (an ended landless host, Elastic 2a)
                        // — are forgotten, before the clean rows below, so none is noted as left.
                        manager.note_dead_rows(
                            hosts
                                .iter()
                                .filter(|r| r.state != world_store::HOST_LIVE)
                                .map(|r| r.id)
                                .collect(),
                        );
                        manager.forget_dead_hosts(
                            hosts
                                .iter()
                                .filter(|r| r.state != world_store::HOST_LIVE && !r.succeeding)
                                .map(|r| r.id)
                                .collect(),
                        );
                        // Rows that are gone (pruned: clean or dead, owning nothing, named by no
                        // open succession — Elastic 2b Task 10b): an id below the read's
                        // `next_host_id` absent from it is forgotten like a dead row, else a
                        // peer whose dead row was pruned before its forget succeeded is awaited
                        // for ever (the gate refuses every login).
                        manager.forget_absent_hosts(
                            hosts.iter().map(|r| r.id).collect(),
                            next_host_id,
                        );
                        // Hosts that left in a committed leave cut: one this host missed (it
                        // registered while that cut ran, outside it; or its boot's read of the
                        // clean rows failed) is noted now — the manager skips the known ones.
                        manager.note_clean_rows(
                            hosts
                                .iter()
                                .filter(|r| r.clean)
                                .map(|r| (r.id, r.boot))
                                .collect(),
                        );
                        self.hosts_read = Some(hosts);
                    }
                    Err(e) => {
                        self.warn(&format!("reading the ownership directory failed ({e}); read again in {DIRECTORY_READ_RETRY_MS} ms"));
                        self.next_read =
                            Instant::now() + Duration::from_millis(DIRECTORY_READ_RETRY_MS);
                    }
                }
            }
        }
        if manager.take_directory_wanted() {
            self.read_wanted = true;
        }
        // Elastic 5: a committed move's land not shown as this host's yet (its part's `COMMIT
        // PREPARED` not run yet): read again, spaced.
        if self.reading.is_none() && !manager.expected_committed().is_empty() {
            self.read_wanted = true;
        }
        let now = Instant::now();
        // A wanted read waits out the spacing (the want is kept); a due one (the backstop, a
        // failed read's retry) does not.
        let spaced = self.last_started.is_none_or(|at| {
            now.duration_since(at) >= Duration::from_millis(DIRECTORY_READ_RETRY_MS)
        });
        if self.reading.is_none() && ((self.read_wanted && spaced) || now >= self.next_read) {
            self.read_wanted = false;
            self.next_read = now + Duration::from_secs(world_store::TERRAIN_POLL_SECS);
            self.last_started = Some(now);
            self.reading = Some(DirectoryRead::start(self.claims.store()));
        }
        if self.claims.fenced().is_set() {
            return;
        }
        let assigned = manager.take_assigned();
        if !assigned.is_empty() {
            log::debug!(
                "[server] super-region(s) {assigned:?} assigned to this host: fenced, then taken"
            );
            self.fence_then_tell(assigned, manager);
        }
    }
}

// --- an heir's adoption (R4) ----------------------------------------------------------------

/// After a failed adoption read, the next one is not started before this many ms.
pub const ADOPTION_READ_RETRY_MS: u64 = 1_000;

/// What the [`AdoptionReader`] asks of the manager (a trait so a test can play it).
pub trait AdoptingManager {
    fn take_adoption_wanted(&mut self) -> Option<(SuccessionId, HostId)>;
    fn adoption_drop_set(&self, id: SuccessionId) -> Vec<EntityId>;
    fn adoption_read_failed(&mut self, id: SuccessionId);
    fn adopted_rows(
        &mut self,
        id: SuccessionId,
        rows: Vec<InFlightRow>,
        located: Vec<(EntityId, RegionCoords)>,
    );
}

impl<S: RegionSpawner> AdoptingManager for WorldManager<S> {
    fn take_adoption_wanted(&mut self) -> Option<(SuccessionId, HostId)> {
        WorldManager::take_adoption_wanted(self)
    }
    fn adoption_drop_set(&self, id: SuccessionId) -> Vec<EntityId> {
        WorldManager::adoption_drop_set(self, id)
    }
    fn adoption_read_failed(&mut self, id: SuccessionId) {
        WorldManager::adoption_read_failed(self, id)
    }
    fn adopted_rows(
        &mut self,
        id: SuccessionId,
        rows: Vec<InFlightRow>,
        located: Vec<(EntityId, RegionCoords)>,
    ) {
        WorldManager::adopted_rows(self, id, rows, located)
    }
}

/// One adoption read in flight.
struct AdoptionRead {
    id: SuccessionId,
    predecessor: HostId,
    drop: Vec<EntityId>,
    started: Instant,
    /// The fence the reads are made under (grown once `owned_by` answered).
    fence: Arc<Fence>,
    step: AdoptionStep,
}

enum AdoptionStep {
    /// What this host holds now (`owned_by`): the inherited rows are in it.
    Fence(Receiver<Result<Fence, StoreError>>),
    /// The predecessor's in-flight rows and where the store holds each dropped entity.
    Rows {
        rows: Receiver<Result<Vec<InFlightRow>, StoreError>>,
        located: Receiver<Result<Vec<(EntityId, RegionCoords)>, StoreError>>,
        got: (
            Option<Vec<InFlightRow>>,
            Option<Vec<(EntityId, RegionCoords)>>,
        ),
    },
}

/// An heir's adoption reads, driven from its main loop (never blocks — the heartbeat beats on
/// meanwhile). When the manager has every survivor's report resolved it wants the store
/// ([`game::WorldManager::take_adoption_wanted`]); this reads, one adoption at a time, on the
/// store thread: what this host holds now (`owned_by_later`), which grows the fence by the
/// inherited rows ([`HostClaims::grow_to`]) BEFORE anything of them loads; then, under that
/// fence, the predecessor's in-flight rows of the base cut (`load_in_flight_of_later`) and the
/// stored regions of the drop set (`regions_of_entities_later`); and hands them to the manager
/// ([`game::WorldManager::adopted_rows`]). A read refused `Fenced` (this boot was replaced)
/// sets the [`Fenced`] flag — the main loop exits fenced (3); any other failure (a lock
/// timeout, the store down, a succession no longer open) gives the adoption back
/// ([`game::WorldManager::adoption_read_failed`]) and asks again after
/// [`ADOPTION_READ_RETRY_MS`] — "wait, never guess": the coordinator's deadline rolls the
/// world back if it never succeeds.
pub struct AdoptionReader {
    claims: HostClaims,
    read: Option<AdoptionRead>,
    no_read_before: Option<Instant>,
    warned: Option<Instant>,
}

impl AdoptionReader {
    pub fn new(claims: HostClaims) -> AdoptionReader {
        AdoptionReader {
            claims,
            read: None,
            no_read_before: None,
            warned: None,
        }
    }

    /// An adoption read is in flight.
    pub fn reading(&self) -> bool {
        self.read.is_some()
    }

    fn warn(&mut self, what: &str) {
        if self
            .warned
            .is_none_or(|at| at.elapsed() >= Duration::from_millis(DIRECTORY_WARN_MS))
        {
            self.warned = Some(Instant::now());
            log::warn!("[server] {what}");
        }
    }

    /// One pass: an answer that came moves the read on (or ends it); a new read starts when
    /// the manager wants one and none is in flight.
    pub fn pump(&mut self, manager: &mut impl AdoptingManager) {
        if self.claims.fenced().is_set() {
            return;
        }
        if self.read.is_none() && self.no_read_before.is_none_or(|at| Instant::now() >= at) {
            if let Some((id, predecessor)) = manager.take_adoption_wanted() {
                let drop = manager.adoption_drop_set(id);
                let owner = self.claims.fence().owner;
                log::info!(
                    "[server] succession {}: reading host {predecessor}'s land for the adoption ({} dropped entities)",
                    id.0,
                    drop.len()
                );
                let step = AdoptionStep::Fence(self.claims.store().owned_by_later(owner));
                let fence = self.claims.fence();
                self.read = Some(AdoptionRead {
                    id,
                    predecessor,
                    drop,
                    started: Instant::now(),
                    fence,
                    step,
                });
            }
        }
        let Some(read) = self.read.as_mut() else {
            return;
        };
        let outcome: Option<Result<(), StoreError>> = match &mut read.step {
            AdoptionStep::Fence(rx) => match rx.try_recv() {
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(StoreError::Backend(
                    "the world-store thread has stopped".into(),
                ))),
                Ok(Err(e)) => Some(Err(e)),
                Ok(Ok(held)) => {
                    // The fence first: an inherited region loads (and a part is checked)
                    // against it from the moment the manager owns the land.
                    self.claims.grow_to(held);
                    let fence = self.claims.fence();
                    read.fence = fence.clone();
                    let store = self.claims.store();
                    read.step = AdoptionStep::Rows {
                        rows: store.load_in_flight_of_later(read.predecessor, &fence),
                        located: store.regions_of_entities_later(read.drop.clone()),
                        got: (None, None),
                    };
                    None
                }
            },
            AdoptionStep::Rows { rows, located, got } => {
                fn take<T>(
                    rx: &Receiver<Result<T, StoreError>>,
                    slot: &mut Option<T>,
                ) -> Result<(), StoreError> {
                    if slot.is_none() {
                        match rx.try_recv() {
                            Ok(answer) => *slot = Some(answer?),
                            Err(TryRecvError::Empty) => {}
                            Err(TryRecvError::Disconnected) => {
                                return Err(StoreError::Backend(
                                    "the world-store thread has stopped".into(),
                                ));
                            }
                        }
                    }
                    Ok(())
                }
                match take(rows, &mut got.0).and_then(|()| take(located, &mut got.1)) {
                    Err(e) => Some(Err(e)),
                    Ok(()) if got.0.is_some() && got.1.is_some() => Some(Ok(())),
                    Ok(()) => None,
                }
            }
        };
        let Some(outcome) = outcome else { return };
        let read = self.read.take().expect("a read");
        match outcome {
            Ok(()) => {
                let AdoptionStep::Rows {
                    got: (Some(rows), Some(located)),
                    ..
                } = read.step
                else {
                    unreachable!("an adoption read ends with both answers")
                };
                log::info!(
                    "[server] succession {}: host {}'s land read in {} ms ({} in-flight rows, {} stored rows of dropped entities)",
                    read.id.0,
                    read.predecessor,
                    read.started.elapsed().as_millis(),
                    rows.len(),
                    located.len()
                );
                manager.adopted_rows(read.id, rows, located);
            }
            Err(e @ StoreError::Fenced { .. }) => {
                // Classified like every refusal, against the fence the reads were made under.
                if self.claims.note_refusal_under(&read.fence, &e) {
                    log::error!(
                        "[server] succession {}: the adoption read was refused ({e}); this host is fenced",
                        read.id.0
                    );
                } else {
                    manager.adoption_read_failed(read.id);
                    self.no_read_before =
                        Some(Instant::now() + Duration::from_millis(ADOPTION_READ_RETRY_MS));
                }
            }
            Err(e) => {
                manager.adoption_read_failed(read.id);
                self.no_read_before =
                    Some(Instant::now() + Duration::from_millis(ADOPTION_READ_RETRY_MS));
                self.warn(&format!(
                    "succession {}: reading host {}'s land failed ({e}); asked again in {ADOPTION_READ_RETRY_MS} ms",
                    read.id.0, read.predecessor
                ));
            }
        }
    }
}

/// Environment: regions per super-region side; must match the world store's.
pub const SUPER_REGION_REGIONS_ENV: &str = "VOXEL_SUPER_REGION_REGIONS";

/// The super-region side from `VOXEL_SUPER_REGION_REGIONS`; unset or empty is
/// [`game::DEFAULT_SUPER_REGION_REGIONS`].
pub fn super_region_regions_from(value: Option<&str>) -> Result<u32, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(game::DEFAULT_SUPER_REGION_REGIONS),
        Some(v) => {
            v.parse::<u32>().ok().filter(|s| *s >= 1).ok_or_else(|| {
                format!("{SUPER_REGION_REGIONS_ENV}={v:?} is not a whole number >= 1")
            })
        }
    }
}
