//! Which region host owns which part of the world: regions are grouped into
//! square super-regions of `s`×`s` regions, the unit a host claims and the
//! world store fences (owner epochs).

use std::collections::BTreeSet;

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

/// Environment: `allinone` (default) or `static`.
pub const OWNERSHIP_ENV: &str = "VOXEL_OWNERSHIP";
/// Environment: this host's ordinal (`static` only), `0..hosts`.
pub const HOST_ORDINAL_ENV: &str = "VOXEL_HOST_ORDINAL";
/// Environment: how many hosts share the world (`static` only).
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
    // Each coordinate in its own 32 bits (no sign extension: `(-1, 0)` and `(0, -1)`
    // must not share a key), mixed, then mixed again with the ordinal.
    let (sx, sz) = sr;
    let key = splitmix64(u64::from(sx as u32) | u64::from(sz as u32) << 32);
    let weight = |ordinal: u32| splitmix64(key ^ u64::from(ordinal));
    let mut best = (0, weight(0));
    for ordinal in 1..hosts.max(1) {
        let w = weight(ordinal);
        if w > best.1 {
            best = (ordinal, w);
        }
    }
    best.0
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
    fn hosts(&self) -> u32 {
        self.hosts
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
        "static" => {
            let ordinal =
                number(HOST_ORDINAL_ENV, ordinal)?.ok_or_else(|| format!("{OWNERSHIP_ENV}={kind} needs {HOST_ORDINAL_ENV}"))?;
            let hosts = number(HOSTS_ENV, hosts)?.ok_or_else(|| format!("{OWNERSHIP_ENV}={kind} needs {HOSTS_ENV}"))?;
            if hosts == 0 || ordinal >= hosts {
                return Err(format!("{HOST_ORDINAL_ENV}={ordinal} must be below {HOSTS_ENV}={hosts} (at least 1)"));
            }
            Ok(Box::new(Static { ordinal, hosts }))
        }
        other => Err(format!("{OWNERSHIP_ENV}={other:?} is neither allinone nor static")),
    }
}

/// The policy from `VOXEL_OWNERSHIP`, `VOXEL_HOST_ORDINAL`, `VOXEL_HOSTS`.
pub fn ownership_from_env() -> Result<Box<dyn Ownership>, String> {
    let get = |name: &str| std::env::var(name).ok();
    ownership_from_values(get(OWNERSHIP_ENV).as_deref(), get(HOST_ORDINAL_ENV).as_deref(), get(HOSTS_ENV).as_deref())
}
