//! Island placement (spec §5 of
//! docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md):
//! a hashed quadtree of slots tiles the plane; each leaf slot holds at most
//! one island. Pure and local — the island at a point is found by descending
//! from its root slot (≈7 levels), so no global state and no neighbour
//! queries.
//!
//! Every island's influence (its coast falloff, see `pipeline.rs`) ends
//! inside its own slot, so terrain is continuous across slot edges: between
//! islands everything is deep sea.
//!
//! The root slot (0, 0) is centred on the home region's centre, never
//! splits, and holds the SPAWN island at exactly that centre, sized by the
//! `VOXEL_ISLAND_CELLS` build dial (`just island=<cells>`).

/// The dial's / spec's cell unit (a 32-chunk cell).
pub const ISLAND_CELL_M: f64 = 64.0;
/// Root slot edge in cells (65.5 km).
pub const ISLAND_MAX_CELLS: u32 = 1024;
/// Slots never split below this edge (512 m).
pub const ISLAND_MIN_SLOT_CELLS: u32 = 8;
/// Chance that a root slot splits into four; each level down multiplies it
/// by `ISLAND_SPLIT_DECAY`, so small slots come in scattered groups rather
/// than whole squares packed with specks.
pub const ISLAND_SPLIT_CHANCE: f64 = 0.85;
pub const ISLAND_SPLIT_DECAY: f64 = 0.72;
/// Chance that a leaf slot is open sea.
pub const ISLAND_EMPTY_CHANCE: f64 = 0.35;
/// Island radius as a fraction of its slot edge, (min, max).
pub const ISLAND_RADIUS_FRAC: (f64, f64) = (0.08, 0.18);
/// Coast warp: the effective radius swings by up to ± this share.
pub const ISLAND_COAST_WARP: f64 = 0.25;
/// Coast-noise base wavelength in island radii (smaller = more ragged coast).
pub const ISLAND_COAST_WAVELENGTH: f64 = 0.8;
/// Land relief of an island of radius R: `ISLAND_RELIEF_FRAC * R`, as a share
/// of the full procedural relief `FULL_RELIEF_M`, clamped to
/// `ISLAND_MIN_RELIEF..=1` — a 500 m island gets ~75 m hills, not 190 m peaks.
pub const ISLAND_RELIEF_FRAC: f64 = 0.15;
pub const FULL_RELIEF_M: f64 = 190.0;
pub const ISLAND_MIN_RELIEF: f64 = 0.1;
/// The falloff reaches deep sea at this many (warped) radii.
pub const ISLAND_REACH: f64 = 2.0;
/// Spawn island diameter when `VOXEL_ISLAND_CELLS` is unset or 0 (1 km).
pub const SPAWN_ISLAND_CELLS_DEFAULT: u32 = 16;
/// Spawn island diameter in `ISLAND_CELL_M` cells (build dial).
pub const SPAWN_ISLAND_CELLS: u32 = parse_island_cells(option_env!("VOXEL_ISLAND_CELLS"));

const ROOT_M: f64 = ISLAND_MAX_CELLS as f64 * ISLAND_CELL_M;
const MIN_SLOT_M: f64 = ISLAND_MIN_SLOT_CELLS as f64 * ISLAND_CELL_M;

/// Largest spawn diameter whose falloff still ends inside the root slot.
pub const SPAWN_ISLAND_CELLS_MAX: u32 =
    (ISLAND_MAX_CELLS as f64 / (2.0 * ISLAND_REACH * (1.0 + ISLAND_COAST_WARP))) as u32;

const fn parse_island_cells(var: Option<&str>) -> u32 {
    let Some(text) = var else { return SPAWN_ISLAND_CELLS_DEFAULT };
    let bytes = text.as_bytes();
    let mut value = 0u32;
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i].is_ascii_digit(), "VOXEL_ISLAND_CELLS must be a number");
        value = value * 10 + (bytes[i] - b'0') as u32;
        i += 1;
    }
    if value == 0 {
        return SPAWN_ISLAND_CELLS_DEFAULT;
    }
    assert!(
        value <= SPAWN_ISLAND_CELLS_MAX,
        "VOXEL_ISLAND_CELLS exceeds SPAWN_ISLAND_CELLS_MAX (the coast falloff must fit the root slot)"
    );
    value
}

/// Quadtree address of a slot: `level` 0 is a root slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IslandId {
    pub level: u8,
    pub x: i64,
    pub z: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Island {
    pub id: IslandId,
    /// World meters.
    pub centre: [f64; 2],
    pub radius: f64,
    pub slot_min: [f64; 2],
    pub slot_size: f64,
}

impl Island {
    /// Furthest distance from the centre the island's falloff reaches.
    pub fn reach(&self) -> f64 {
        max_reach(self.radius)
    }
}

pub fn max_reach(radius: f64) -> f64 {
    ISLAND_REACH * (1.0 + ISLAND_COAST_WARP) * radius
}

/// The island layout of one world seed.
#[derive(Debug, Clone)]
pub struct IslandField {
    seed: u64,
    /// World position of root slot (0, 0)'s minimum corner.
    root_origin: [f64; 2],
    spawn_radius: f64,
}

/// Centre of the home region (0, 0) in world meters (1 block = 1 m).
pub fn home_centre_m() -> [f64; 2] {
    let half = (game::REGION_CHUNKS * game::CHUNK_BLOCKS) as f64 / 2.0;
    [half, half]
}

impl IslandField {
    pub fn new(seed: u64) -> Self {
        Self::with_spawn_cells(seed, SPAWN_ISLAND_CELLS)
    }

    pub fn with_spawn_cells(seed: u64, cells: u32) -> Self {
        assert!((1..=SPAWN_ISLAND_CELLS_MAX).contains(&cells), "spawn island of {cells} cells");
        let c = home_centre_m();
        Self {
            seed,
            root_origin: [c[0] - ROOT_M / 2.0, c[1] - ROOT_M / 2.0],
            spawn_radius: cells as f64 * ISLAND_CELL_M / 2.0,
        }
    }

    pub fn spawn_island(&self) -> Island {
        Island {
            id: IslandId { level: 0, x: 0, z: 0 },
            centre: home_centre_m(),
            radius: self.spawn_radius,
            slot_min: self.root_origin,
            slot_size: ROOT_M,
        }
    }

    /// The island whose slot contains world point `(x, z)` (meters), if
    /// that slot holds one.
    pub fn island_at(&self, x: f64, z: f64) -> Option<Island> {
        let (qx, qz) = (x - self.root_origin[0], z - self.root_origin[1]);
        let mut size = ROOT_M;
        let mut level = 0u8;
        let (mut ix, mut iz) = ((qx / size).floor() as i64, (qz / size).floor() as i64);
        if (ix, iz) == (0, 0) {
            return Some(self.spawn_island());
        }
        loop {
            let h = slot_hash(self.seed, level, ix, iz);
            let split = ISLAND_SPLIT_CHANCE * ISLAND_SPLIT_DECAY.powi(level as i32);
            if size > MIN_SLOT_M && unit(h, 0) < split {
                level += 1;
                size /= 2.0;
                ix = (qx / size).floor() as i64;
                iz = (qz / size).floor() as i64;
                continue;
            }
            if unit(h, 1) < ISLAND_EMPTY_CHANCE {
                return None;
            }
            let (lo, hi) = ISLAND_RADIUS_FRAC;
            let radius = (lo + (hi - lo) * unit(h, 2)) * size;
            // Room left for the centre so the falloff stays in the slot.
            let slack = (size / 2.0 - max_reach(radius)).max(0.0);
            let slot_min = [
                self.root_origin[0] + ix as f64 * size,
                self.root_origin[1] + iz as f64 * size,
            ];
            let centre = [
                slot_min[0] + size / 2.0 + (unit(h, 3) * 2.0 - 1.0) * slack,
                slot_min[1] + size / 2.0 + (unit(h, 4) * 2.0 - 1.0) * slack,
            ];
            return Some(Island { id: IslandId { level, x: ix, z: iz }, centre, radius, slot_min, slot_size: size });
        }
    }
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn slot_hash(seed: u64, level: u8, x: i64, z: i64) -> u64 {
    let mut h = splitmix(seed ^ 0x1515_1A4D);
    h = splitmix(h ^ level as u64);
    h = splitmix(h ^ x as u64);
    splitmix(h ^ (z as u64).rotate_left(32))
}

/// Uniform in [0, 1), stream `k` of slot hash `h`.
fn unit(h: u64, k: u64) -> f64 {
    (splitmix(h ^ k.wrapping_mul(0xA24B_AED4_963E_E407)) >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field() -> IslandField {
        IslandField::with_spawn_cells(0x00D5_C0DE_8D78, 16)
    }

    /// Deterministic sample points spread over ~6 root slots.
    fn points(n: usize) -> Vec<[f64; 2]> {
        let mut s = 7u64;
        (0..n)
            .map(|_| {
                s = splitmix(s);
                let x = (s % 400_000) as f64 - 200_000.0;
                s = splitmix(s);
                let z = (s % 400_000) as f64 - 200_000.0;
                [x, z]
            })
            .collect()
    }

    #[test]
    fn placement_is_deterministic() {
        let (a, b) = (field(), field());
        for p in points(500) {
            assert_eq!(a.island_at(p[0], p[1]), b.island_at(p[0], p[1]));
        }
        let other = IslandField::with_spawn_cells(1, 16);
        let differs = points(500).iter().any(|p| other.island_at(p[0], p[1]) != a.island_at(p[0], p[1]));
        assert!(differs, "the seed must matter");
    }

    #[test]
    fn every_point_of_a_slot_sees_the_same_island() {
        let f = field();
        for p in points(300) {
            let Some(i) = f.island_at(p[0], p[1]) else { continue };
            for (fx, fz) in [(0.01, 0.01), (0.99, 0.01), (0.5, 0.5), (0.01, 0.99), (0.99, 0.99)] {
                let q = [i.slot_min[0] + fx * i.slot_size, i.slot_min[1] + fz * i.slot_size];
                assert_eq!(f.island_at(q[0], q[1]), Some(i), "slot {:?}", i.id);
            }
        }
    }

    #[test]
    fn islands_stay_inside_their_slot() {
        let f = field();
        let mut seen = 0;
        for p in points(4000) {
            let Some(i) = f.island_at(p[0], p[1]) else { continue };
            seen += 1;
            for a in 0..2 {
                let lo = i.centre[a] - i.reach();
                let hi = i.centre[a] + i.reach();
                assert!(lo >= i.slot_min[a] - 1e-6 && hi <= i.slot_min[a] + i.slot_size + 1e-6, "{i:?}");
            }
            assert!(i.slot_size >= MIN_SLOT_M && i.slot_size <= ROOT_M);
            let (lo, hi) = ISLAND_RADIUS_FRAC;
            if i.id != f.spawn_island().id {
                assert!(i.radius >= lo * i.slot_size - 1e-9 && i.radius <= hi * i.slot_size + 1e-9);
            }
        }
        assert!(seen > 1000, "most sampled points should be in island slots: {seen}");
    }

    #[test]
    fn sizes_vary() {
        let f = field();
        let levels: std::collections::BTreeSet<u8> =
            points(4000).iter().filter_map(|p| f.island_at(p[0], p[1])).map(|i| i.id.level).collect();
        assert!(levels.len() >= 3, "island sizes should span several quadtree levels: {levels:?}");
    }

    #[test]
    fn the_spawn_island_is_centred_on_the_home_region() {
        let f = IslandField::with_spawn_cells(3, 4);
        let s = f.spawn_island();
        assert_eq!(s.centre, home_centre_m());
        assert_eq!(s.radius, 4.0 * ISLAND_CELL_M / 2.0);
        assert_eq!(f.island_at(s.centre[0] + 10.0, s.centre[1] - 3000.0), Some(s));
        assert!(s.reach() <= ROOT_M / 2.0);
    }

    #[test]
    fn the_largest_spawn_island_still_fits_its_root() {
        let f = IslandField::with_spawn_cells(3, SPAWN_ISLAND_CELLS_MAX);
        assert!(f.spawn_island().reach() <= ROOT_M / 2.0);
    }

    #[test]
    fn dial_parsing() {
        assert_eq!(parse_island_cells(None), SPAWN_ISLAND_CELLS_DEFAULT);
        assert_eq!(parse_island_cells(Some("")), SPAWN_ISLAND_CELLS_DEFAULT);
        assert_eq!(parse_island_cells(Some("0")), SPAWN_ISLAND_CELLS_DEFAULT);
        assert_eq!(parse_island_cells(Some("64")), 64);
    }
}
