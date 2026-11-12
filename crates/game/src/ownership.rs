//! Which region host owns which part of the world: regions are grouped into
//! square super-regions of `s`×`s` regions, the unit a host claims and the
//! world store fences (owner epochs).

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

/// Which regions this process simulates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ownership {
    /// One process runs the whole world (single-player, the dev server).
    AllInOne,
    /// A fixed set of `hosts` processes share the world by rendezvous
    /// hashing of its super-regions; this one is `ordinal`.
    Static { ordinal: u32, hosts: u32 },
}

impl Ownership {
    /// Whether this process owns (simulates) region `rc`, with `s` regions
    /// per super-region side.
    pub fn owns(&self, rc: RegionCoords, s: u32) -> bool {
        self.owns_super_region(super_region_of(rc, s))
    }

    /// Whether this process owns super-region `sr`.
    pub fn owns_super_region(&self, sr: (i32, i32)) -> bool {
        match *self {
            Ownership::AllInOne => true,
            Ownership::Static { ordinal, hosts } => rendezvous_owner(sr, hosts) == ordinal,
        }
    }

    /// The host that owns region `rc`, with `s` regions per super-region side
    /// (`AllInOne`: host 0, the only one).
    pub fn owner_of(&self, rc: RegionCoords, s: u32) -> u32 {
        match *self {
            Ownership::AllInOne => 0,
            Ownership::Static { hosts, .. } => rendezvous_owner(super_region_of(rc, s), hosts),
        }
    }

    /// The host ordinal claims are made as (`AllInOne` is host 0).
    pub fn ordinal(&self) -> u32 {
        match *self {
            Ownership::AllInOne => 0,
            Ownership::Static { ordinal, .. } => ordinal,
        }
    }

    /// The super-regions this process claims at start: those of the map
    /// `extent` it owns (a procedural world has no extent and claims none).
    pub fn claim_set(&self, extent: Option<crate::MapExtent>, s: u32) -> std::collections::BTreeSet<(i32, i32)> {
        let Some(extent) = extent else { return Default::default() };
        let (w, h) = (extent.width as i32, extent.height as i32);
        let (lo, hi) = (
            super_region_of(extent.origin, s),
            super_region_of(RegionCoords::new(extent.origin.x + w - 1, extent.origin.z + h - 1), s),
        );
        let mut set = std::collections::BTreeSet::new();
        for sx in lo.0..=hi.0 {
            for sz in lo.1..=hi.1 {
                if self.owns_super_region((sx, sz)) {
                    set.insert((sx, sz));
                }
            }
        }
        set
    }

    /// The policy from the three environment values (unset / empty = absent).
    pub fn from_values(ownership: Option<&str>, ordinal: Option<&str>, hosts: Option<&str>) -> Result<Ownership, String> {
        fn blank(v: Option<&str>) -> Option<&str> {
            v.map(str::trim).filter(|v| !v.is_empty())
        }
        let number = |name: &str, v: Option<&str>| -> Result<Option<u32>, String> {
            blank(v)
                .map(|v| v.parse::<u32>().map_err(|_| format!("{name}={v:?} is not a whole number")))
                .transpose()
        };
        match blank(ownership).unwrap_or("allinone") {
            "allinone" => {
                if blank(ordinal).is_some() || blank(hosts).is_some() {
                    return Err(format!(
                        "{HOST_ORDINAL_ENV} / {HOSTS_ENV} are set but {OWNERSHIP_ENV} is allinone; set {OWNERSHIP_ENV}=static"
                    ));
                }
                Ok(Ownership::AllInOne)
            }
            "static" => {
                let ordinal = number(HOST_ORDINAL_ENV, ordinal)?
                    .ok_or_else(|| format!("{OWNERSHIP_ENV}=static needs {HOST_ORDINAL_ENV}"))?;
                let hosts =
                    number(HOSTS_ENV, hosts)?.ok_or_else(|| format!("{OWNERSHIP_ENV}=static needs {HOSTS_ENV}"))?;
                if hosts == 0 || ordinal >= hosts {
                    return Err(format!("{HOST_ORDINAL_ENV}={ordinal} must be below {HOSTS_ENV}={hosts} (at least 1)"));
                }
                Ok(Ownership::Static { ordinal, hosts })
            }
            other => Err(format!("{OWNERSHIP_ENV}={other:?} is neither allinone nor static")),
        }
    }

    /// The policy from `VOXEL_OWNERSHIP`, `VOXEL_HOST_ORDINAL`, `VOXEL_HOSTS`.
    pub fn from_env() -> Result<Ownership, String> {
        let get = |name: &str| std::env::var(name).ok();
        Ownership::from_values(
            get(OWNERSHIP_ENV).as_deref(),
            get(HOST_ORDINAL_ENV).as_deref(),
            get(HOSTS_ENV).as_deref(),
        )
    }
}
