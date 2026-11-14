//! Which region host owns which part of the world: regions are grouped into
//! square super-regions of `s`×`s` regions, the unit a host claims and the
//! world store fences (owner epochs).

use std::collections::{BTreeMap, BTreeSet};

use crate::RegionCoords;

/// Regions per super-region side unless the world row says otherwise
/// (`world.super_region_regions`).
pub const DEFAULT_SUPER_REGION_REGIONS: u32 = 16;

/// The super-region holding `rc` with `s` regions per side (floor division,
/// so `(-1, -1)` is in `(-1, -1)`, never `(0, 0)`).
pub fn super_region_of(rc: RegionCoords, s: u32) -> (i32, i32) {
    let s = s.max(1) as i32;
    (rc.x.div_euclid(s), rc.z.div_euclid(s))
}

/// What a host's crash costs the world (R4). `Rollback`: the R3 rule — a host that
/// boots after a boot of its ordinal that never left clean raises the world generation and
/// every host restarts from the last cut. `Succession`: the dead host's land is inherited by
/// a live host while the others run on; a boot that finds its predecessor's dirty row waits
/// for that (or, if no host is alive, raises the generation as before).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashPolicy {
    Rollback,
    Succession,
}

/// How often a host stamps its lease (`hosts.seen_ms`), from its MAIN loop: a hung loop must
/// stop beating although its sockets live (a timer thread would keep a hung host alive).
pub const HOST_HEARTBEAT_MS: u64 = 2_000;
/// How long a host's lease lasts without a beat: the coordinator declares a host dead once
/// it has read the same `seen_ms` over this long of its own successful reads; a booting host
/// counts another host alive while `db_now - seen_ms` is within it.
pub const HOST_LEASE_MS: u64 = 10_000;

/// `hosts.state`: the host holds its lease.
pub const HOST_LIVE: i64 = 0;
/// `hosts.state`: the host's lease lapsed and a takeover marked it dead.
pub const HOST_DEAD: i64 = 1;

/// `super_regions.owner` of a free row (no host holds it).
pub const NO_OWNER: i64 = -1;

/// A succession's id (R4, the store's `successions.id`): one host's land given to one heir.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct SuccessionId(pub u64);

/// Why a host's land moved to an heir (R4): it crashed (its lease lapsed, or its next boot
/// vouched it dead) or it left clean. Stored as `successions.kind`: 0 crash, 1 leave.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub enum SuccessionKind {
    Crash,
    Leave,
}

impl SuccessionKind {
    /// The stored `successions.kind` value.
    pub fn code(self) -> i64 {
        match self {
            SuccessionKind::Crash => 0,
            SuccessionKind::Leave => 1,
        }
    }

    pub fn from_code(code: i64) -> Option<SuccessionKind> {
        match code {
            0 => Some(SuccessionKind::Crash),
            1 => Some(SuccessionKind::Leave),
            _ => None,
        }
    }
}

/// How long (the coordinator's wall ms, from the declaration or its restart) a succession
/// may wait for every survivor's `SuccessionApplied` and the heir's `SuccessionAdopted`:
/// past it the world rolls back to the last committed cut (a survivor that can never report
/// — it lost its journal, or cannot reach the base cut — must not stop saving for ever).
pub const SUCCESSION_DEADLINE_MS: u64 = 30_000;

/// One host's land given to one heir (the store's `successions` row as declared; R4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Succession {
    pub id: SuccessionId,
    pub predecessor: u32,
    pub predecessor_boot: u64,
    pub heir: u32,
    pub kind: SuccessionKind,
    /// The last committed world cut at the declaration (`CutId(0)`: none): what the heir
    /// starts the predecessor's regions from.
    pub base_cut: crate::CutId,
    /// The super-regions moved to the heir, at their new epochs, in key order.
    pub moved: Vec<(SuperRegion, u64)>,
    /// Every other live dirty host at the declaration (the heir among them), by ordinal:
    /// the hosts that report their journals to the heir.
    pub survivors: Vec<u32>,
}

/// What a declaration of successions did (the store's answer). Anything but `Done` wrote
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Declared {
    /// Declared: one succession per predecessor, in the call's order.
    Done(Vec<Succession>),
    /// Crash: these predecessors' rows beat within [`HOST_LEASE_MS`] (and were not
    /// vouched), or left clean meanwhile; leave: these rows are not clean.
    NotLapsed(Vec<u32>),
    /// Another succession is open: a second failure before the first is settled — the
    /// caller rolls the world back.
    SuccessionOpen,
    /// These rows are gone: no row, another boot, already dead or already succeeded — or,
    /// named as heirs, not live with a fresh lease any more. The list mixes predecessor and
    /// heir ordinals (sorted): the caller does not tell them apart — it forgets the whole
    /// declaration and re-observes (its next lease read gives a new lapsed set and new heirs).
    Gone(Vec<u32>),
}

/// Environment: `allinone` (default), `static` or `claim`.
pub const OWNERSHIP_ENV: &str = "VOXEL_OWNERSHIP";
/// Environment: this host's ordinal (`static` / `claim` only), `0..hosts`.
pub const HOST_ORDINAL_ENV: &str = "VOXEL_HOST_ORDINAL";
/// Environment: how many hosts share the world (`static` / `claim` only).
pub const HOSTS_ENV: &str = "VOXEL_HOSTS";

/// splitmix64's finaliser (a stateless mix; the rendezvous weights).
fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The host (`0..hosts`) that owns super-region `sr`: rendezvous hashing — the
/// host with the highest weight for it, ties to the lower ordinal. Growing
/// the host count moves only the super-regions the new host wins.
pub fn rendezvous_owner(sr: (i32, i32), hosts: u32) -> u32 {
    rendezvous_among(sr, 0..hosts.max(1)).unwrap_or(0)
}

/// [`rendezvous_owner`] over `ordinals` (ascending): the one with the highest weight for
/// `sr`, ties to the lower ordinal; `None` for no ordinal.
fn rendezvous_among(sr: SuperRegion, ordinals: impl IntoIterator<Item = u32>) -> Option<u32> {
    // Each coordinate in its own 32 bits (no sign extension: `(-1, 0)` and `(0, -1)`
    // must not share a key), mixed, then mixed again with the ordinal.
    let (sx, sz) = sr;
    let key = splitmix64(u64::from(sx as u32) | u64::from(sz as u32) << 32);
    highest_weight(key, ordinals)
}

/// The ordinal of `ordinals` (ascending) with the highest `splitmix64(key ^ ordinal)`,
/// ties to the lower ordinal.
fn highest_weight(key: u64, ordinals: impl IntoIterator<Item = u32>) -> Option<u32> {
    let mut best: Option<(u32, u64)> = None;
    for ordinal in ordinals {
        let w = splitmix64(key ^ u64::from(ordinal));
        if best.is_none_or(|(_, b)| w > b) {
            best = Some((ordinal, w));
        }
    }
    best.map(|(ordinal, _)| ordinal)
}

/// Keys [`heir_of`]'s weights apart from the super-regions' ([`rendezvous_owner`]).
const HEIR_KEY_SALT: u64 = 0x4845_4952_0000_0000;

/// The one live host that inherits all of `predecessor`'s super-regions (R4 decision 2):
/// a rendezvous pick over `live` without the predecessor, keyed by the predecessor — the
/// same live set always gives the same heir, and another host leaving the set moves the
/// heirship only if it was the heir. `None` with no other live host.
pub fn heir_of(predecessor: u32, live: &BTreeSet<u32>) -> Option<u32> {
    let key = splitmix64(HEIR_KEY_SALT ^ u64::from(predecessor));
    highest_weight(key, live.iter().copied().filter(|&o| o != predecessor))
}

/// A super-region's coordinates ([`super_region_of`]).
pub type SuperRegion = (i32, i32);

/// Who simulates a super-region, as this process knows it ("wait, never guess": a
/// `Free` or `Unknown` super-region is treated like one whose owner is unreachable —
/// a player is clamped at the seam, an intent is refused back, a transfer waits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// This process.
    Me,
    /// Another host, by ordinal.
    Host(u32),
    /// Nobody holds it (a free directory row, or none at all).
    Free,
    /// This process does not know (its directory was never read).
    Unknown,
}

/// Which regions this process simulates: a policy over the super-regions.
pub trait Ownership: Send + std::fmt::Debug {
    /// The host ordinal claims are made as (`AllInOne` is host 0).
    fn ordinal(&self) -> u32;
    /// Who simulates super-region `sr`.
    fn owner_of(&self, sr: SuperRegion) -> Owner;
    /// What a crash of this host costs the world.
    fn policy(&self) -> CrashPolicy;
    /// The directory this policy answers from (`None` for `AllInOne` / `Static`).
    fn directory_mut(&mut self) -> Option<&mut Directory>;
    /// [`Self::directory_mut`], to read.
    fn directory(&self) -> Option<&Directory> {
        None
    }
    /// How many hosts share the world (`VOXEL_HOSTS`; `AllInOne`: 1).
    fn hosts(&self) -> u32;
    /// Whether this process runs every host's regions (`AllInOne`): it adopts every
    /// host's in-flight rows and its cuts replace them all.
    fn runs_every_host(&self) -> bool {
        false
    }
    /// The super-regions this process claims at start: those of the map `extent` it owns
    /// now (a procedural world has no extent and claims none).
    fn claim_set(&self, extent: Option<crate::MapExtent>, s: u32) -> BTreeSet<SuperRegion> {
        let Some(extent) = extent else { return BTreeSet::new() };
        let (w, h) = (extent.width as i32, extent.height as i32);
        let (lo, hi) = (
            super_region_of(extent.origin, s),
            super_region_of(RegionCoords::new(extent.origin.x + w - 1, extent.origin.z + h - 1), s),
        );
        let mut set = BTreeSet::new();
        for sx in lo.0..=hi.0 {
            for sz in lo.1..=hi.1 {
                if self.owner_of((sx, sz)) == Owner::Me {
                    set.insert((sx, sz));
                }
            }
        }
        set
    }
}

/// One process runs the whole world (single-player, the dev server).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllInOne;

impl AllInOne {
    /// Whether this process owns region `rc`: always.
    pub fn owns(&self, _rc: RegionCoords, _s: u32) -> bool {
        true
    }
}

impl Ownership for AllInOne {
    fn ordinal(&self) -> u32 {
        0
    }
    fn owner_of(&self, _sr: SuperRegion) -> Owner {
        Owner::Me
    }
    fn policy(&self) -> CrashPolicy {
        CrashPolicy::Rollback
    }
    fn directory_mut(&mut self) -> Option<&mut Directory> {
        None
    }
    fn hosts(&self) -> u32 {
        1
    }
    fn runs_every_host(&self) -> bool {
        true
    }
}

/// A fixed set of `hosts` processes share the world by rendezvous hashing of its
/// super-regions; this one is `ordinal` (R3 as built: never `Free` / `Unknown`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Static {
    pub ordinal: u32,
    pub hosts: u32,
}

impl Static {
    /// Whether this process owns (simulates) region `rc`, with `s` regions per
    /// super-region side.
    pub fn owns(&self, rc: RegionCoords, s: u32) -> bool {
        rendezvous_owner(super_region_of(rc, s), self.hosts) == self.ordinal
    }

    /// The host that owns region `rc`, with `s` regions per super-region side, as a
    /// number (the trait's [`Ownership::owner_of`] answers per super-region).
    pub fn owner_of(&self, rc: RegionCoords, s: u32) -> u32 {
        rendezvous_owner(super_region_of(rc, s), self.hosts)
    }
}

impl Ownership for Static {
    fn ordinal(&self) -> u32 {
        self.ordinal
    }
    fn owner_of(&self, sr: SuperRegion) -> Owner {
        match rendezvous_owner(sr, self.hosts) {
            o if o == self.ordinal => Owner::Me,
            o => Owner::Host(o),
        }
    }
    fn policy(&self) -> CrashPolicy {
        CrashPolicy::Rollback
    }
    fn directory_mut(&mut self) -> Option<&mut Directory> {
        None
    }
    fn hosts(&self) -> u32 {
        self.hosts
    }
}

/// The ownership directory as one host mirrors it (`super_regions.owner`): per super-region
/// its owner (`None` = a free row) and the epoch of that row, and which hosts are live.
/// A super-region with no row is `Unknown` until the first whole read
/// ([`Directory::replace`]), `Free` after it (nobody ever claimed it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Directory {
    rows: BTreeMap<SuperRegion, (Option<u32>, u64)>,
    /// Read whole at least once.
    read: bool,
    live: BTreeSet<u32>,
}

impl Directory {
    /// Row `sr` is `owner` (`None`: free) at `epoch`; ignored when `epoch` is lower than
    /// the stored one (a late message never undoes a newer claim).
    pub fn set(&mut self, sr: SuperRegion, owner: Option<u32>, epoch: u64) {
        if self.rows.get(&sr).is_some_and(|&(_, stored)| epoch < stored) {
            return;
        }
        self.rows.insert(sr, (owner, epoch));
    }

    /// A whole read of the store's rows (`(super-region, owner, epoch)`): each one as
    /// [`Self::set`] (a row read older than what a peer already said is kept as said), and
    /// from now on a super-region with no row is `Free`. A row is never deleted in the
    /// store, so one known here and missing from the read stays.
    pub fn replace(&mut self, rows: impl IntoIterator<Item = (SuperRegion, Option<u32>, u64)>) {
        self.read = true;
        for (sr, owner, epoch) in rows {
            self.set(sr, owner, epoch);
        }
    }

    /// Who simulates `sr`, as host `me` sees it.
    pub fn get(&self, sr: SuperRegion, me: u32) -> Owner {
        match self.rows.get(&sr) {
            Some(&(Some(owner), _)) if owner == me => Owner::Me,
            Some(&(Some(owner), _)) => Owner::Host(owner),
            Some(&(None, _)) => Owner::Free,
            None if self.read => Owner::Free,
            None => Owner::Unknown,
        }
    }

    /// The epoch of row `sr`, if this directory has it.
    pub fn epoch(&self, sr: SuperRegion) -> Option<u64> {
        self.rows.get(&sr).map(|&(_, epoch)| epoch)
    }

    /// The super-regions host `ordinal` holds, in order.
    pub fn owned_by(&self, ordinal: u32) -> Vec<SuperRegion> {
        self.rows.iter().filter(|(_, (owner, _))| *owner == Some(ordinal)).map(|(sr, _)| *sr).collect()
    }

    /// The hosts that hold their lease now.
    pub fn set_live(&mut self, live: BTreeSet<u32>) {
        self.live = live;
    }

    /// Host `ordinal` is live too (its peer link said hello: it registered and runs) until
    /// the next [`Self::set_live`].
    pub fn note_live(&mut self, ordinal: u32) {
        self.live.insert(ordinal);
    }

    /// Host `ordinal` was declared dead (a succession): not live until the next
    /// [`Self::set_live`] or a hello of another boot of it ([`Self::note_live`]).
    pub fn note_dead(&mut self, ordinal: u32) {
        self.live.remove(&ordinal);
    }

    /// The live host that should claim `sr` when it is free: [`rendezvous_owner`]
    /// restricted to the live hosts (with every host of `VOXEL_HOSTS` live, exactly
    /// `rendezvous_owner(sr, hosts)`; a dead host's share moves, nobody else's). `None`
    /// while no host is known live.
    pub fn preferred(&self, sr: SuperRegion) -> Option<u32> {
        rendezvous_among(sr, self.live.iter().copied())
    }
}

/// Ownership is a claim in Postgres (R4): super-regions are claimed on demand, and this
/// host answers from its mirror of the directory ("wait, never guess": `Free` / `Unknown`
/// are never assumed to be anyone's). A crash is inherited by one heir (`Succession`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PgClaim {
    pub ordinal: u32,
    pub hosts: u32,
    pub directory: Directory,
}

impl PgClaim {
    /// Host `ordinal` of `hosts`, its directory not read yet (everything `Unknown`).
    pub fn new(ordinal: u32, hosts: u32) -> PgClaim {
        PgClaim { ordinal, hosts, directory: Directory::default() }
    }
}

impl Ownership for PgClaim {
    fn ordinal(&self) -> u32 {
        self.ordinal
    }
    fn owner_of(&self, sr: SuperRegion) -> Owner {
        self.directory.get(sr, self.ordinal)
    }
    fn policy(&self) -> CrashPolicy {
        CrashPolicy::Succession
    }
    fn directory_mut(&mut self) -> Option<&mut Directory> {
        Some(&mut self.directory)
    }
    fn directory(&self) -> Option<&Directory> {
        Some(&self.directory)
    }
    fn hosts(&self) -> u32 {
        self.hosts
    }
}

/// What a host crash costs a world run with `VOXEL_OWNERSHIP` = `ownership` (unset / empty:
/// `allinone`): `claim` → `Succession`, `allinone` / `static` → `Rollback`. For a process
/// that has no ordinal of its own (the coordinator); hosts ask their [`Ownership::policy`].
pub fn crash_policy_from_value(ownership: Option<&str>) -> Result<CrashPolicy, String> {
    match ownership.map(str::trim).filter(|v| !v.is_empty()).unwrap_or("allinone") {
        "allinone" | "static" => Ok(CrashPolicy::Rollback),
        "claim" => Ok(CrashPolicy::Succession),
        other => Err(format!("{OWNERSHIP_ENV}={other:?} is none of allinone, static, claim")),
    }
}

/// The policy from the three environment values (unset / empty = absent).
pub fn ownership_from_values(
    ownership: Option<&str>,
    ordinal: Option<&str>,
    hosts: Option<&str>,
) -> Result<Box<dyn Ownership>, String> {
    fn blank(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|v| !v.is_empty())
    }
    let number = |name: &str, v: Option<&str>| -> Result<Option<u32>, String> {
        blank(v).map(|v| v.parse::<u32>().map_err(|_| format!("{name}={v:?} is not a whole number"))).transpose()
    };
    let kind = blank(ownership).unwrap_or("allinone");
    match kind {
        "allinone" => {
            if blank(ordinal).is_some() || blank(hosts).is_some() {
                return Err(format!(
                    "{HOST_ORDINAL_ENV} / {HOSTS_ENV} are set but {OWNERSHIP_ENV} is allinone; set {OWNERSHIP_ENV}=static"
                ));
            }
            Ok(Box::new(AllInOne))
        }
        "static" | "claim" => {
            let ordinal =
                number(HOST_ORDINAL_ENV, ordinal)?.ok_or_else(|| format!("{OWNERSHIP_ENV}={kind} needs {HOST_ORDINAL_ENV}"))?;
            let hosts = number(HOSTS_ENV, hosts)?.ok_or_else(|| format!("{OWNERSHIP_ENV}={kind} needs {HOSTS_ENV}"))?;
            if hosts == 0 || ordinal >= hosts {
                return Err(format!("{HOST_ORDINAL_ENV}={ordinal} must be below {HOSTS_ENV}={hosts} (at least 1)"));
            }
            if kind == "claim" {
                return Ok(Box::new(PgClaim::new(ordinal, hosts)));
            }
            Ok(Box::new(Static { ordinal, hosts }))
        }
        other => Err(format!("{OWNERSHIP_ENV}={other:?} is none of allinone, static, claim")),
    }
}

/// The policy from `VOXEL_OWNERSHIP`, `VOXEL_HOST_ORDINAL`, `VOXEL_HOSTS`.
pub fn ownership_from_env() -> Result<Box<dyn Ownership>, String> {
    let get = |name: &str| std::env::var(name).ok();
    ownership_from_values(get(OWNERSHIP_ENV).as_deref(), get(HOST_ORDINAL_ENV).as_deref(), get(HOSTS_ENV).as_deref())
}
