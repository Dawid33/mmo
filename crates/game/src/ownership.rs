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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// The default notice of a drain (`worldctl drain <url> <host id>` without `--in`), in seconds:
/// how long a host marked draining runs on (warning its players) before it leaves.
pub const DRAIN_WARNING_SECS: u64 = 300;

/// `hosts.state`: the host holds its lease.
pub const HOST_LIVE: i64 = 0;
/// `hosts.state`: the host's lease lapsed and a takeover marked it dead.
pub const HOST_DEAD: i64 = 1;

/// `super_regions.owner` of a free row (no host holds it).
pub const NO_OWNER: i64 = -1;

/// A region host's identity: what the store's `hosts` / `super_regions` rows, the peer links
/// and the coordinator name a host by: the number the store issued at its registration
/// (`register_host`, from 1, never reused; [`HostId::SOLO`] for role `all`); it prints as `h7`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct HostId(pub u64);

impl HostId {
    /// The one host of a process that runs the whole world (`AllInOne`).
    pub const SOLO: HostId = HostId(0);
}

impl std::fmt::Display for HostId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "h{}", self.0)
    }
}

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
    pub predecessor: HostId,
    pub predecessor_boot: u64,
    pub heir: HostId,
    pub kind: SuccessionKind,
    /// The last committed world cut at the declaration (`CutId(0)`: none): what the heir
    /// starts the predecessor's regions from.
    pub base_cut: crate::CutId,
    /// The super-regions moved to the heir, at their new epochs, in key order.
    pub moved: Vec<(SuperRegion, u64)>,
    /// Every other live dirty host at the declaration (the heir among them), by ordinal:
    /// the hosts that report their journals to the heir.
    pub survivors: Vec<HostId>,
}

/// What a declaration of successions did (the store's answer). Anything but `Done` wrote
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Declared {
    /// Declared: one succession per predecessor, in the call's order.
    Done(Vec<Succession>),
    /// Crash: these predecessors' rows beat within [`HOST_LEASE_MS`] (and were not
    /// vouched), or left clean meanwhile; leave: these rows are not clean.
    NotLapsed(Vec<HostId>),
    /// Another succession is open: a second failure before the first is settled — the
    /// caller rolls the world back.
    SuccessionOpen,
    /// These rows are gone: no row, another boot, already dead or already succeeded — or,
    /// named as heirs, not live with a fresh lease any more. The list mixes predecessor and
    /// heir ordinals (sorted): the caller does not tell them apart — it forgets the whole
    /// declaration and re-observes (its next lease read gives a new lapsed set and new heirs).
    Gone(Vec<HostId>),
}

/// Environment: `allinone` (default), `static` or `claim`.
pub const OWNERSHIP_ENV: &str = "VOXEL_OWNERSHIP";

/// splitmix64's finaliser (a stateless mix; the rendezvous weights).
fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The one of `ids` (ascending) with the highest rendezvous weight for super-region `sr`,
/// ties to the lower id; `None` for no id. Adding an id moves only the super-regions the new
/// id wins; removing one moves only its own.
pub fn rendezvous_among(sr: SuperRegion, ids: impl IntoIterator<Item = HostId>) -> Option<HostId> {
    // Each coordinate in its own 32 bits (no sign extension: `(-1, 0)` and `(0, -1)`
    // must not share a key), mixed, then mixed again with the id.
    let (sx, sz) = sr;
    let key = splitmix64(u64::from(sx as u32) | u64::from(sz as u32) << 32);
    highest_weight(key, ids)
}

/// The ordinal of `ordinals` (ascending) with the highest `splitmix64(key ^ ordinal)`,
/// ties to the lower ordinal.
fn highest_weight(key: u64, ordinals: impl IntoIterator<Item = HostId>) -> Option<HostId> {
    let mut best: Option<(HostId, u64)> = None;
    for ordinal in ordinals {
        let w = splitmix64(key ^ ordinal.0);
        if best.is_none_or(|(_, b)| w > b) {
            best = Some((ordinal, w));
        }
    }
    best.map(|(ordinal, _)| ordinal)
}

/// Keys [`heir_of`]'s weights apart from the super-regions' ([`rendezvous_among`]).
const HEIR_KEY_SALT: u64 = 0x4845_4952_0000_0000;

/// The one live host that inherits all of `predecessor`'s super-regions (R4 decision 2):
/// a rendezvous pick over `live` without the predecessor, keyed by the predecessor — the
/// same live set always gives the same heir, and another host leaving the set moves the
/// heirship only if it was the heir. `None` with no other live host.
pub fn heir_of(predecessor: HostId, live: &BTreeSet<HostId>) -> Option<HostId> {
    let key = splitmix64(HEIR_KEY_SALT ^ predecessor.0);
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
    Host(HostId),
    /// Nobody holds it (a free directory row, or none at all).
    Free,
    /// This process does not know (its directory was never read).
    Unknown,
}

/// Which regions this process simulates: a policy over the super-regions.
pub trait Ownership: Send + std::fmt::Debug {
    /// The host id claims are made as (`AllInOne` is [`HostId::SOLO`]).
    fn me(&self) -> HostId;
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
    fn me(&self) -> HostId {
        HostId::SOLO
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
    fn runs_every_host(&self) -> bool {
        true
    }
}

/// A fixed table: the hosts of `layout` share the world by rendezvous hashing of its
/// super-regions ([`rendezvous_among`]); this one is `me` (R3's layout: never `Free` /
/// `Unknown`). Outside the seeded tests a `static` host is alone (`layout == [me]`: the store
/// refuses a second `static` registration beside a live row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Static {
    pub me: HostId,
    /// Every host of the table, ascending.
    pub layout: Vec<HostId>,
}

impl Static {
    /// The host of the table that owns super-region `sr` (`me` for an empty table).
    fn owner_at(&self, sr: SuperRegion) -> HostId {
        rendezvous_among(sr, self.layout.iter().copied()).unwrap_or(self.me)
    }

    /// Whether this process owns (simulates) region `rc`, with `s` regions per
    /// super-region side.
    pub fn owns(&self, rc: RegionCoords, s: u32) -> bool {
        self.owner_at(super_region_of(rc, s)) == self.me
    }

    /// The host that owns region `rc`, with `s` regions per super-region side, as a
    /// number (the trait's [`Ownership::owner_of`] answers per super-region).
    pub fn owner_of(&self, rc: RegionCoords, s: u32) -> HostId {
        self.owner_at(super_region_of(rc, s))
    }
}

impl Ownership for Static {
    fn me(&self) -> HostId {
        self.me
    }
    fn owner_of(&self, sr: SuperRegion) -> Owner {
        match self.owner_at(sr) {
            o if o == self.me => Owner::Me,
            o => Owner::Host(o),
        }
    }
    fn policy(&self) -> CrashPolicy {
        CrashPolicy::Rollback
    }
    fn directory_mut(&mut self) -> Option<&mut Directory> {
        None
    }
    /// A `static` host ALONE (its table is itself) runs every region of the world: it adopts
    /// every host's in-flight rows and its (one-phase) cuts replace them all. Since Elastic 1
    /// its restart is a new id, so the rows an earlier process of the world left (under its old
    /// id, e.g. after a clean leave) are its to deliver — none would ever be loaded otherwise.
    fn runs_every_host(&self) -> bool {
        self.layout == [self.me]
    }
}

/// The ownership directory as one host mirrors it (`super_regions.owner`): per super-region
/// its owner (`None` = a free row) and the epoch of that row, and which hosts are live.
/// A super-region with no row is `Unknown` until the first whole read
/// ([`Directory::replace`]), `Free` after it (nobody ever claimed it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Directory {
    rows: BTreeMap<SuperRegion, (Option<HostId>, u64)>,
    /// Read whole at least once.
    read: bool,
    live: BTreeSet<HostId>,
    /// Hosts marked draining (`worldctl drain`, from the `hosts` rows of the latest read): in
    /// no claim preference, this host's own included — a draining host wins no land, so free
    /// land is preferred to the live hosts that are not draining ([`Self::preferred`]).
    draining: BTreeSet<HostId>,
}

impl Directory {
    /// Row `sr` is `owner` (`None`: free) at `epoch`; ignored when `epoch` is lower than
    /// the stored one (a late message never undoes a newer claim).
    pub fn set(&mut self, sr: SuperRegion, owner: Option<HostId>, epoch: u64) {
        if self.rows.get(&sr).is_some_and(|&(_, stored)| epoch < stored) {
            return;
        }
        self.rows.insert(sr, (owner, epoch));
    }

    /// A whole read of the store's rows (`(super-region, owner, epoch)`): each one as
    /// [`Self::set`] (a row read older than what a peer already said is kept as said), and
    /// from now on a super-region with no row is `Free`. A row is never deleted in the
    /// store, so one known here and missing from the read stays.
    pub fn replace(&mut self, rows: impl IntoIterator<Item = (SuperRegion, Option<HostId>, u64)>) {
        self.read = true;
        for (sr, owner, epoch) in rows {
            self.set(sr, owner, epoch);
        }
    }

    /// Who simulates `sr`, as host `me` sees it.
    pub fn get(&self, sr: SuperRegion, me: HostId) -> Owner {
        match self.rows.get(&sr) {
            Some(&(Some(owner), _)) if owner == me => Owner::Me,
            Some(&(Some(owner), _)) => Owner::Host(owner),
            Some(&(None, _)) => Owner::Free,
            None if self.read => Owner::Free,
            None => Owner::Unknown,
        }
    }

    /// Whether the directory was read whole at least once (until then a super-region with
    /// no row is `Unknown`, and an empty [`Self::owned_by`] proves nothing).
    pub fn is_read(&self) -> bool {
        self.read
    }

    /// The epoch of row `sr`, if this directory has it.
    pub fn epoch(&self, sr: SuperRegion) -> Option<u64> {
        self.rows.get(&sr).map(|&(_, epoch)| epoch)
    }

    /// The super-regions host `ordinal` holds, in order.
    pub fn owned_by(&self, ordinal: HostId) -> Vec<SuperRegion> {
        self.rows.iter().filter(|(_, (owner, _))| *owner == Some(ordinal)).map(|(sr, _)| *sr).collect()
    }

    /// The hosts that hold their lease now.
    pub fn set_live(&mut self, live: BTreeSet<HostId>) {
        self.live = live;
    }

    /// Host `ordinal` is live too (its peer link said hello: it registered and runs) until
    /// the next [`Self::set_live`].
    pub fn note_live(&mut self, ordinal: HostId) {
        self.live.insert(ordinal);
    }

    /// Host `ordinal` was declared dead (a succession): not live until the next
    /// [`Self::set_live`] or a hello of another boot of it ([`Self::note_live`]).
    pub fn note_dead(&mut self, ordinal: HostId) {
        self.live.remove(&ordinal);
    }

    /// The hosts marked draining (the `hosts` rows' drain marks, read with the directory): a
    /// mark is never lifted from a host id, so a later hello ([`Self::note_live`]) does not
    /// make a draining host a claimer again.
    pub fn set_draining(&mut self, draining: BTreeSet<HostId>) {
        self.draining = draining;
    }

    /// Host `host` is marked draining ([`Self::set_draining`]).
    pub fn is_draining(&self, host: HostId) -> bool {
        self.draining.contains(&host)
    }

    /// Whether any host is known live. The server's reads leave draining hosts (and clean rows)
    /// out of `live` but always put this host in, draining or not; so with every other live
    /// host draining `live` holds this host alone. With live hosts and no [`Self::preferred`]
    /// claimer, every live host is draining — free land stays free, and a directory read would
    /// change nothing until a host joins (the periodic read finds it).
    pub fn knows_live(&self) -> bool {
        !self.live.is_empty()
    }

    /// The live host that should claim `sr` when it is free: [`rendezvous_among`] the live
    /// hosts that are not draining (a fixed live set lays out like a `Static` table of it; a
    /// dead or draining host's share moves, nobody else's). `None` while no such host is known
    /// (none live, or every live one draining).
    pub fn preferred(&self, sr: SuperRegion) -> Option<HostId> {
        rendezvous_among(sr, self.live.iter().copied().filter(|h| !self.draining.contains(h)))
    }
}

/// Ownership is a claim in Postgres (R4): super-regions are claimed on demand, and this
/// host answers from its mirror of the directory ("wait, never guess": `Free` / `Unknown`
/// are never assumed to be anyone's). A crash is inherited by one heir (`Succession`). Any
/// number of hosts: no count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PgClaim {
    pub id: HostId,
    pub directory: Directory,
}

impl PgClaim {
    /// Host `me` (its id issued at registration), its directory not read yet (everything
    /// `Unknown`).
    pub fn new(me: HostId) -> PgClaim {
        PgClaim { id: me, directory: Directory::default() }
    }
}

impl Ownership for PgClaim {
    fn me(&self) -> HostId {
        self.id
    }
    fn owner_of(&self, sr: SuperRegion) -> Owner {
        self.directory.get(sr, self.id)
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

/// The ownership of host `me` under `policy` (`VOXEL_OWNERSHIP`'s value; empty: `allinone`):
/// `allinone` → [`AllInOne`] (`me` unused: it is [`HostId::SOLO`]), `static` → a table of
/// `me` alone (it claims every super-region), `claim` → [`PgClaim`] over a directory not read
/// yet. A host builds it once its id was issued (`register_host`): nothing is claimed, fenced
/// or sent under a guessed id. Anything else is refused, naming the variable.
pub fn ownership_for(policy: &str, me: HostId) -> Result<Box<dyn Ownership>, String> {
    match policy.trim() {
        "" | "allinone" => Ok(Box::new(AllInOne)),
        "static" => Ok(Box::new(Static { me, layout: vec![me] })),
        "claim" => Ok(Box::new(PgClaim::new(me))),
        other => Err(format!("{OWNERSHIP_ENV}={other:?} is none of allinone, static, claim")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_id_orders_and_prints_by_its_number() {
        assert!(HostId(2) < HostId(10));
        assert_eq!(format!("{}", HostId(7)), "h7");
        let endpoint = crate::HostEndpoint { id: HostId(9), quic: Some("127.0.0.1:1".into()), webtransport: None, cert_hash: None };
        let bytes = bincode::serialize(&endpoint).unwrap();
        let back: crate::HostEndpoint = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back.id, HostId(9));
        assert_eq!(back, endpoint);
    }
}
