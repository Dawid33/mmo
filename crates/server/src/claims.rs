//! The server's owner-epoch claims: at start a process claims the
//! super-regions it owns ([`HostClaims::claim`]) and keeps that
//! [`Fence`]; a host may later grow it ([`HostClaims::add`], rows won with
//! `claim_free`) or swap it for what it holds now ([`HostClaims::replace`]). The
//! world store checks the current fence on every region load and cut commit; once another process claims one of those super-regions the
//! store answers [`StoreError::Fenced`], and the process is fenced for good
//! ([`Fenced`]): it stops ticking, refuses its clients and exits with
//! [`EXIT_FENCED`] without a last save.
//!
//! Under `claim` (R4) a host holds nothing at boot ([`HostClaims::none`]), registers, then
//! re-claims its own rows ([`HostClaims::reclaim_own`]); the main loop's
//! [`DirectoryKeeper`] claims free super-regions on demand and reads the directory; an heir's
//! [`AdoptionReader`] reads what it inherits (R4, a succession).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, TryRecvError};
use game::{EntityId, InFlightRow, Ownership, RegionCoords, RegionSpawner, StoredRegion, SuccessionId, WorldCut, WorldManager};
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

/// What the client sees when its server was fenced.
pub const FENCED_REFUSAL: &str = "this server was replaced; reconnect";

/// What the client sees when the world restarts from an earlier save.
pub const ROLLBACK_REFUSAL: &str = "the world is restarting; reconnect";

/// What the client sees when its host left the world in a committed leave cut (its player
/// parked and saved first): another host takes its land, the client joins again. The client
/// matches this exact text to decide to rejoin.
pub const LEAVING_REFUSAL: &str = "this server is stopping; reconnect";

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
            .claim_super_regions(ownership.ordinal(), srs)
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
    pub fn claim_srs(store: StoreHandle, owner: u32, srs: Vec<SuperRegion>) -> Result<HostClaims, StoreError> {
        let fence = store.claim_super_regions(owner, srs)?;
        Ok(HostClaims::with_fence(store, fence))
    }

    /// Host `owner` holding nothing yet: what a host claiming on demand (`claim`) registers
    /// under — a boot told to wait must not have touched its predecessor's rows.
    pub fn none(store: StoreHandle, owner: u32) -> HostClaims {
        HostClaims::with_fence(store, Fence { owner, epochs: std::collections::BTreeMap::new(), all_hosts: false })
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
                    let live =
                        self.store.hosts()?.iter().any(|r| r.ordinal == owner && r.boot == boot && r.state == game::HOST_LIVE);
                    if !live {
                        log::error!("[server] re-claiming host {owner}'s rows lost a lock conflict ({e}) to its own declaration: fenced");
                        return Err(StoreError::Fenced { sr: None });
                    }
                    if asked >= 2 {
                        return Err(e);
                    }
                    log::warn!("[server] re-claiming host {owner}'s rows lost a lock conflict ({e}); its row is still live: once more");
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
        // Hard: another owner's rows in this fence would let this host write as that owner.
        assert_eq!(more.owner, current.owner, "a fence grows only by its own owner's rows");
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
        assert_eq!(held.owner, current.owner, "a fence grows only by its own owner's rows");
        let mut grown = (**current).clone();
        for (sr, epoch) in held.epochs {
            let e = grown.epochs.entry(sr).or_insert(epoch);
            *e = (*e).max(epoch);
        }
        *current = Arc::new(grown);
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
        assert_eq!(fence.owner, current.owner, "a fence is replaced only by its own owner's");
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
                log::error!("[server] a write touched super-region {sr:?}, which this host never claimed; refused (not a takeover)");
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
                let forgotten = self.forgotten.read().unwrap_or_else(|e| e.into_inner()).clone();
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

// --- the directory and claiming on demand (R4, `claim`) -----------------------------------

/// The hosts holding their lease by `rows` at the database clock `now_ms`: live, not left
/// clean, and beaten within [`game::HOST_LEASE_MS`] (what a booting host counts as alive).
pub fn live_hosts(rows: &[HostRow], now_ms: u64) -> BTreeSet<u32> {
    rows.iter()
        .filter(|r| r.state == game::HOST_LIVE && !r.clean && now_ms.saturating_sub(r.seen_ms) <= game::HOST_LEASE_MS)
        .map(|r| r.ordinal)
        .collect()
}

/// The directory read whole, blocking (a boot, before the main loop): every `super_regions`
/// row (`(super-region, owner or NO_OWNER, epoch)`) and the live hosts ([`live_hosts`]).
/// Plain reads, no locks.
pub fn read_directory(store: &StoreHandle, me: u32) -> Result<(Vec<(SuperRegion, i64, u64)>, BTreeSet<u32>), StoreError> {
    let rows = store.owners()?;
    let hosts = store.hosts()?;
    let mut live = live_hosts(&hosts, store.db_now_ms()?);
    live.insert(me);
    Ok((rows, live))
}

/// After a failed claim, no new one for this long.
pub const CLAIM_RETRY_MS: u64 = 1_000;
/// At most one warning per this long about failed claims or directory reads.
pub const DIRECTORY_WARN_MS: u64 = 10_000;

/// A directory read in flight: its three plain reads, each answered on its own receiver.
struct DirectoryRead {
    owners: Receiver<Result<Vec<(SuperRegion, i64, u64)>, StoreError>>,
    hosts: Receiver<Result<Vec<HostRow>, StoreError>>,
    now: Receiver<Result<u64, StoreError>>,
    got: (Option<Vec<(SuperRegion, i64, u64)>>, Option<Vec<HostRow>>, Option<u64>),
}

impl DirectoryRead {
    fn start(store: &StoreHandle) -> DirectoryRead {
        DirectoryRead { owners: store.owners_later(), hosts: store.hosts_later(), now: store.db_now_ms_later(), got: (None, None, None) }
    }

    /// The read's rows and live hosts once all three answered; an error as soon as one fails.
    fn poll(&mut self) -> Option<Result<(Vec<(SuperRegion, i64, u64)>, BTreeSet<u32>), StoreError>> {
        fn take<T>(rx: &Receiver<Result<T, StoreError>>, slot: &mut Option<T>) -> Result<(), StoreError> {
            if slot.is_none() {
                match rx.try_recv() {
                    Ok(answer) => *slot = Some(answer?),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => return Err(StoreError::Backend("the world-store thread has stopped".into())),
                }
            }
            Ok(())
        }
        let read = take(&self.owners, &mut self.got.0).and_then(|()| take(&self.hosts, &mut self.got.1)).and_then(|()| take(&self.now, &mut self.got.2));
        if let Err(e) = read {
            return Some(Err(e));
        }
        match &mut self.got {
            (rows @ Some(_), hosts @ Some(_), Some(now)) => {
                let live = live_hosts(hosts.as_ref().expect("matched"), *now);
                Some(Ok((rows.take().expect("matched"), live)))
            }
            _ => None,
        }
    }
}

/// A `claim` host's directory and claims, driven from its main loop (never blocks): the
/// manager says what it wants ([`game::WorldManager::take_claims_wanted`], always rows its
/// directory says are free), this claims it on the store thread (`claim_free_later`, one
/// claim in flight), grows the fence ([`HostClaims::add`]) and answers the manager
/// ([`game::WorldManager::claimed`]). A claim that won less than it asked (another host was
/// faster), or that failed (a lock timeout among them: retried after [`CLAIM_RETRY_MS`],
/// never fatal), triggers a directory read — except a claim refused as `Fenced` (this boot
/// was declared dead or replaced, R4): that sets the [`Fenced`] flag and claims nothing more; so does the manager's
/// [`game::WorldManager::take_directory_wanted`], and every [`world_store::TERRAIN_POLL_SECS`]
/// as a backstop ([`game::WorldManager::directory_read`]).
pub struct DirectoryKeeper {
    claims: HostClaims,
    /// This process's boot: every claim is refused unless its `hosts` row is still that live boot.
    boot: u64,
    claiming: Option<(usize, Receiver<Result<Fence, StoreError>>)>,
    no_claim_before: Option<Instant>,
    /// The directory read in flight, with its number.
    reading: Option<(u64, DirectoryRead)>,
    /// Directory reads started so far (the next one's number is this + 1).
    reads_started: u64,
    /// After a claim that won less than it asked, or failed: no claim at all until a read
    /// numbered ABOVE this one is applied (one started after that answer — an older read may
    /// not show the rows lost). Otherwise the lost rows, still `Free` in the directory, would
    /// be claimed again against another host's rows (finding 1: a claim must never lock
    /// rows it cannot win).
    claims_wait_for_read: Option<u64>,
    read_wanted: bool,
    next_read: Instant,
    warned: Option<Instant>,
}

/// What the [`DirectoryKeeper`] asks of the manager (a trait so a test can play it).
pub trait ClaimingManager {
    fn take_claims_wanted(&mut self) -> Vec<SuperRegion>;
    fn claimed(&mut self, rows: Vec<(SuperRegion, u64)>);
    fn directory_read(&mut self, rows: Vec<(SuperRegion, i64, u64)>, live: BTreeSet<u32>);
    fn take_directory_wanted(&mut self) -> bool;
}

impl<S: RegionSpawner> ClaimingManager for WorldManager<S> {
    fn take_claims_wanted(&mut self) -> Vec<SuperRegion> {
        WorldManager::take_claims_wanted(self)
    }
    fn claimed(&mut self, rows: Vec<(SuperRegion, u64)>) {
        WorldManager::claimed(self, rows)
    }
    fn directory_read(&mut self, rows: Vec<(SuperRegion, i64, u64)>, live: BTreeSet<u32>) {
        WorldManager::directory_read(self, rows, live)
    }
    fn take_directory_wanted(&mut self) -> bool {
        WorldManager::take_directory_wanted(self)
    }
}

impl DirectoryKeeper {
    /// The keeper of `claims` (its owner is this host); its first backstop read comes one
    /// period from now (a boot reads the directory itself, [`read_directory`]).
    pub fn new(claims: HostClaims, boot: u64) -> DirectoryKeeper {
        let next_read = Instant::now() + Duration::from_secs(world_store::TERRAIN_POLL_SECS);
        DirectoryKeeper {
            claims,
            boot,
            claiming: None,
            no_claim_before: None,
            reading: None,
            reads_started: 0,
            claims_wait_for_read: None,
            read_wanted: false,
            next_read,
            warned: None,
        }
    }

    fn warn(&mut self, what: &str) {
        if self.warned.is_none_or(|at| at.elapsed() >= Duration::from_millis(DIRECTORY_WARN_MS)) {
            self.warned = Some(Instant::now());
            log::warn!("[server] {what}");
        }
    }

    /// A claim lost rows or failed: a fresh read is wanted, and nothing is claimed again
    /// until one started from now on is applied.
    fn wait_for_a_fresh_read(&mut self) {
        self.read_wanted = true;
        self.claims_wait_for_read = Some(self.reads_started);
    }

    /// One pass: answers that came are handed to the manager, then the next read and claim
    /// are started if due. At most one claim is in flight, and none while a lost claim waits
    /// for its read.
    pub fn pump(&mut self, manager: &mut impl ClaimingManager) {
        let owner = self.claims.fence().owner;
        if let Some((asked, rx)) = &self.claiming {
            let asked = *asked;
            let answer = match rx.try_recv() {
                Err(TryRecvError::Empty) => None,
                Ok(answer) => Some(answer),
                Err(TryRecvError::Disconnected) => Some(Err(StoreError::Backend("the world-store thread has stopped".into()))),
            };
            if let Some(answer) = answer {
                self.claiming = None;
                match answer {
                    Ok(won) => {
                        let rows: Vec<(SuperRegion, u64)> = won.epochs.iter().map(|(sr, e)| (*sr, *e)).collect();
                        if rows.len() < asked {
                            self.wait_for_a_fresh_read();
                        }
                        if !rows.is_empty() {
                            // The fence first: a load or cut in a won super-region is checked
                            // against it from the moment the manager knows it owns it.
                            self.claims.add(won);
                            manager.claimed(rows);
                        }
                    }
                    Err(e) if self.claims.note_refusal(&e) => {
                        // Declared dead or replaced: the main loop sees the flag and exits fenced.
                        log::error!("[server] claiming super-regions was refused ({e}); this host is fenced");
                        return;
                    }
                    Err(e) => {
                        self.warn(&format!("claiming super-regions failed ({e}); asked again in {CLAIM_RETRY_MS} ms"));
                        self.no_claim_before = Some(Instant::now() + Duration::from_millis(CLAIM_RETRY_MS));
                        self.wait_for_a_fresh_read();
                    }
                }
            }
        }
        if let Some((n, read)) = self.reading.as_mut() {
            let n = *n;
            if let Some(answer) = read.poll() {
                self.reading = None;
                match answer {
                    Ok((rows, live)) => {
                        manager.directory_read(rows, live);
                        if self.claims_wait_for_read.is_some_and(|after| n > after) {
                            self.claims_wait_for_read = None;
                        }
                    }
                    Err(e) => {
                        self.warn(&format!("reading the ownership directory failed ({e}); read again in {CLAIM_RETRY_MS} ms"));
                        // A gated claim waits for a read that succeeds: try again soon.
                        if self.claims_wait_for_read.is_some() {
                            self.next_read = Instant::now() + Duration::from_millis(CLAIM_RETRY_MS);
                        }
                    }
                }
            }
        }
        if manager.take_directory_wanted() {
            self.read_wanted = true;
        }
        let now = Instant::now();
        if self.reading.is_none() && (self.read_wanted || now >= self.next_read) {
            self.read_wanted = false;
            self.next_read = now + Duration::from_secs(world_store::TERRAIN_POLL_SECS);
            self.reads_started += 1;
            self.reading = Some((self.reads_started, DirectoryRead::start(self.claims.store())));
        }
        let may_claim = !self.claims.fenced().is_set()
            && self.claiming.is_none()
            && self.claims_wait_for_read.is_none()
            && self.no_claim_before.is_none_or(|at| now >= at);
        if may_claim {
            let wanted = manager.take_claims_wanted();
            if !wanted.is_empty() {
                log::debug!("[server] claiming free super-region(s) {wanted:?} as host {owner}");
                self.claiming = Some((wanted.len(), self.claims.store().claim_free_later(owner, self.boot, wanted)));
            }
        }
    }
}

// --- an heir's adoption (R4) ----------------------------------------------------------------

/// After a failed adoption read, the next one is not started before this many ms.
pub const ADOPTION_READ_RETRY_MS: u64 = 1_000;

/// What the [`AdoptionReader`] asks of the manager (a trait so a test can play it).
pub trait AdoptingManager {
    fn take_adoption_wanted(&mut self) -> Option<(SuccessionId, u32)>;
    fn adoption_drop_set(&self, id: SuccessionId) -> Vec<EntityId>;
    fn adoption_read_failed(&mut self, id: SuccessionId);
    fn adopted_rows(&mut self, id: SuccessionId, rows: Vec<InFlightRow>, located: Vec<(EntityId, RegionCoords)>);
}

impl<S: RegionSpawner> AdoptingManager for WorldManager<S> {
    fn take_adoption_wanted(&mut self) -> Option<(SuccessionId, u32)> {
        WorldManager::take_adoption_wanted(self)
    }
    fn adoption_drop_set(&self, id: SuccessionId) -> Vec<EntityId> {
        WorldManager::adoption_drop_set(self, id)
    }
    fn adoption_read_failed(&mut self, id: SuccessionId) {
        WorldManager::adoption_read_failed(self, id)
    }
    fn adopted_rows(&mut self, id: SuccessionId, rows: Vec<InFlightRow>, located: Vec<(EntityId, RegionCoords)>) {
        WorldManager::adopted_rows(self, id, rows, located)
    }
}

/// One adoption read in flight.
struct AdoptionRead {
    id: SuccessionId,
    predecessor: u32,
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
        got: (Option<Vec<InFlightRow>>, Option<Vec<(EntityId, RegionCoords)>>),
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
        AdoptionReader { claims, read: None, no_read_before: None, warned: None }
    }

    /// An adoption read is in flight.
    pub fn reading(&self) -> bool {
        self.read.is_some()
    }

    fn warn(&mut self, what: &str) {
        if self.warned.is_none_or(|at| at.elapsed() >= Duration::from_millis(DIRECTORY_WARN_MS)) {
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
                self.read = Some(AdoptionRead { id, predecessor, drop, started: Instant::now(), fence, step });
            }
        }
        let Some(read) = self.read.as_mut() else { return };
        let outcome: Option<Result<(), StoreError>> = match &mut read.step {
            AdoptionStep::Fence(rx) => match rx.try_recv() {
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(StoreError::Backend("the world-store thread has stopped".into()))),
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
                fn take<T>(rx: &Receiver<Result<T, StoreError>>, slot: &mut Option<T>) -> Result<(), StoreError> {
                    if slot.is_none() {
                        match rx.try_recv() {
                            Ok(answer) => *slot = Some(answer?),
                            Err(TryRecvError::Empty) => {}
                            Err(TryRecvError::Disconnected) => {
                                return Err(StoreError::Backend("the world-store thread has stopped".into()))
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
                let AdoptionStep::Rows { got: (Some(rows), Some(located)), .. } = read.step else {
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
                    log::error!("[server] succession {}: the adoption read was refused ({e}); this host is fenced", read.id.0);
                } else {
                    manager.adoption_read_failed(read.id);
                    self.no_read_before = Some(Instant::now() + Duration::from_millis(ADOPTION_READ_RETRY_MS));
                }
            }
            Err(e) => {
                manager.adoption_read_failed(read.id);
                self.no_read_before = Some(Instant::now() + Duration::from_millis(ADOPTION_READ_RETRY_MS));
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

    /// Registers host `ordinal` as boot `boot` (claims on demand need a live `hosts` row).
    fn register(store: &StoreHandle, ordinal: u32, boot: u64) {
        let row = HostRow {
            ordinal,
            boot,
            generation: 0,
            clean: false,
            state: game::HOST_LIVE,
            seen_ms: 0,
            reg: 0,
            peer_addr: None,
            peer_cert: None,
            quic: None,
            webtransport: None,
            wt_cert_hash: None,
        };
        let verdict = store.register_host(row, &Fence::default(), game::CrashPolicy::Rollback).unwrap();
        assert!(matches!(verdict, world_store::BootVerdict::Registered(_)), "{verdict:?}");
    }

    #[test]
    fn claims_the_owned_share_as_the_host_ordinal() {
        let store = memory_store();
        let s = store.super_region_regions();
        let extent = game::MapExtent { origin: RegionCoords::new(-20, -5), width: 40, height: 10 };
        let all = HostClaims::claim(store.clone(), &game::AllInOne, Some(extent), s).unwrap();
        assert_eq!(all.fence().owner, 0);
        assert_eq!(all.fence().epochs.len(), 8, "every super-region of the extent");
        let none = HostClaims::claim(store.clone(), &game::AllInOne, None, s).unwrap();
        assert!(none.fence().epochs.is_empty(), "a world with no map claims nothing");
        let hosts = 3;
        let mine = game::Static { ordinal: 2, hosts };
        let share = HostClaims::claim(store.clone(), &mine, Some(extent), s).unwrap();
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
            adopts: vec![],
        }
    }

    #[test]
    fn added_super_regions_are_checked_by_the_next_commit() {
        let store = memory_store();
        let (x, y) = ((0, 0), (0, -1));
        let claims = HostClaims::claim_srs(store.clone(), 0, vec![x]).unwrap();
        register(&store, 0, 10);
        claims.add(store.claim_free(0, 10, vec![y]).unwrap());
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

    /// Requirement (R4 Task 10): a refusal is classified against the fence the refused write
    /// was made under — a fence grown or replaced since (an heir's adoption, a re-claim) must
    /// not turn a real takeover of a row the write held into a "stray write".
    #[test]
    fn a_refusal_is_classified_against_the_fence_the_write_used() {
        let store = memory_store();
        let (x, y) = ((0, 0), (0, -1));
        let claims = HostClaims::claim_srs(store.clone(), 0, vec![x]).unwrap();
        let used = claims.fence();
        // Since the write: the fence no longer holds `x` (replaced by what holds now).
        claims.replace(Fence { owner: 0, epochs: [(y, 1)].into_iter().collect(), all_hosts: false });
        let refusal = StoreError::Fenced { sr: Some(x) };
        assert!(!claims.clone().note_refusal(&refusal), "against the current fence it reads as a stray write");
        assert!(!claims.fenced().is_set());
        assert!(claims.note_refusal_under(&used, &refusal), "against the fence the write used: a takeover");
        assert!(claims.fenced().is_set());
        // A write that never held the row is still a stray write, not a takeover.
        let other = HostClaims::claim_srs(store.clone(), 1, vec![y]).unwrap();
        let stray = StoreError::Fenced { sr: Some(x) };
        assert!(!other.note_refusal_under(&other.fence(), &stray));
        assert!(!other.fenced().is_set());
        store.shutdown();
    }

    /// An heir grows its fence by what the store says it holds now: every row at the higher
    /// of the two epochs, nothing dropped.
    #[test]
    fn growing_to_what_is_held_keeps_newer_epochs_and_drops_nothing() {
        let store = memory_store();
        let claims = HostClaims::none(store.clone(), 0);
        claims.replace(Fence { owner: 0, epochs: [((0, 0), 3), ((1, 0), 2)].into_iter().collect(), all_hosts: false });
        claims.grow_to(Fence { owner: 0, epochs: [((0, 0), 2), ((2, 0), 5)].into_iter().collect(), all_hosts: false });
        let epochs: Vec<(SuperRegion, u64)> = claims.fence().epochs.iter().map(|(sr, e)| (*sr, *e)).collect();
        assert_eq!(epochs, [((0, 0), 3), ((1, 0), 2), ((2, 0), 5)]);
        store.shutdown();
    }


    /// A manager whose directory says `sr` is free until a read is applied, and that wants
    /// it claimed while it is: the order of the keeper's claims and applied reads.
    struct Wanting {
        sr: SuperRegion,
        read: bool,
        events: Vec<&'static str>,
    }

    impl ClaimingManager for Wanting {
        fn take_claims_wanted(&mut self) -> Vec<SuperRegion> {
            if self.read {
                return Vec::new();
            }
            self.events.push("claim");
            vec![self.sr]
        }
        fn claimed(&mut self, rows: Vec<(SuperRegion, u64)>) {
            panic!("won {rows:?}: the row is another host's");
        }
        fn directory_read(&mut self, rows: Vec<(SuperRegion, i64, u64)>, _live: BTreeSet<u32>) {
            assert!(rows.iter().any(|(sr, owner, _)| *sr == self.sr && *owner == 1), "the read shows host 1's row: {rows:?}");
            self.events.push("read");
            self.read = true;
        }
        fn take_directory_wanted(&mut self) -> bool {
            false
        }
    }

    /// A claim that loses its row (another host holds it; this host's directory said free)
    /// claims nothing more until a directory read started after that answer is applied: the
    /// lost row is never sent to `claim_free` a second time.
    #[test]
    fn a_lost_claim_waits_for_a_fresh_read_before_claiming_again() {
        let store = memory_store();
        let sr = (0, 3);
        store.claim_super_regions(1, vec![sr]).unwrap();
        register(&store, 0, 10);
        let mut keeper = DirectoryKeeper::new(HostClaims::none(store.clone(), 0), 10);
        let mut m = Wanting { sr, read: false, events: Vec::new() };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !m.read {
            assert!(Instant::now() < deadline, "no read applied: {:?}", m.events);
            keeper.pump(&mut m);
            std::thread::sleep(Duration::from_millis(1));
        }
        for _ in 0..20 {
            keeper.pump(&mut m);
        }
        assert_eq!(m.events, ["claim", "read"], "one claim, then the read; no claim of the lost row in between");
        assert!(keeper.claims.fence().epochs.is_empty(), "nothing won");
        store.shutdown();
    }

    /// A claim refused because this boot's `hosts` row is no longer live (declared dead, or
    /// replaced) fences the host: the flag is set and nothing more is claimed.
    #[test]
    fn a_claim_by_a_replaced_boot_fences_the_host() {
        let store = memory_store();
        register(&store, 0, 10);
        register(&store, 0, 11);
        let claims = HostClaims::none(store.clone(), 0);
        let mut keeper = DirectoryKeeper::new(claims.clone(), 10);
        let mut m = Free { sr: (0, 3), asked: 0 };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !claims.fenced().is_set() {
            assert!(Instant::now() < deadline, "the refused claim never fenced the host");
            keeper.pump(&mut m);
            std::thread::sleep(Duration::from_millis(1));
        }
        for _ in 0..20 {
            keeper.pump(&mut m);
        }
        assert_eq!(m.asked, 1, "nothing claimed after the refusal");
        assert!(store.owners().unwrap().is_empty(), "nothing won");
        store.shutdown();
    }

    /// A manager that always wants one free super-region.
    struct Free {
        sr: SuperRegion,
        asked: u32,
    }

    impl ClaimingManager for Free {
        fn take_claims_wanted(&mut self) -> Vec<SuperRegion> {
            self.asked += 1;
            vec![self.sr]
        }
        fn claimed(&mut self, rows: Vec<(SuperRegion, u64)>) {
            panic!("won {rows:?} as a replaced boot");
        }
        fn directory_read(&mut self, _rows: Vec<(SuperRegion, i64, u64)>, _live: BTreeSet<u32>) {}
        fn take_directory_wanted(&mut self) -> bool {
            false
        }
    }

    #[test]
    fn a_different_super_region_side_refuses_the_start() {
        let store = memory_store();
        let stored = store.super_region_regions();
        let err = HostClaims::claim(store.clone(), &game::AllInOne, None, stored + 1).err().expect("refused");
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
