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

/// The dial's / spec's cell unit: a 64 m (64-block) cell, independent of the
/// sim chunk size.
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
    let half = game::REGION_BLOCKS as f64 / 2.0;
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

/// Spawn search: rays around the island, walked outward from this share of
/// the radius to `SPAWN_SEARCH_TO`, in `SPAWN_SEARCH_STEP_M` steps.
pub const SPAWN_SEARCH_RAYS: usize = 16;
pub const SPAWN_SEARCH_FROM: f64 = 0.5;
pub const SPAWN_SEARCH_TO: f64 = 1.3;
pub const SPAWN_SEARCH_STEP_M: f64 = 2.0;
/// A spawn column's surface is this many meters above sea level…
pub const SPAWN_HEIGHT_M: (f64, f64) = (1.0, 4.0);
/// …and its neighbours `SPAWN_FLAT_PROBE_M` away differ by less than this.
pub const SPAWN_FLAT_M: f64 = 1.0;
pub const SPAWN_FLAT_PROBE_M: f64 = 4.0;

/// A dry, flat coastal point of `island` (world meters): the first column,
/// radius-major from `SPAWN_SEARCH_FROM` of the radius outward, whose surface
/// is `SPAWN_HEIGHT_M` above the sea and flat to `SPAWN_FLAT_M`. The island
/// centre if none qualifies.
pub fn spawn_point(p: &crate::pipeline::TerrainPipeline, island: &Island) -> [f64; 2] {
    let surface = |x: f64, z: f64| p.column(x, z).surface_y() - crate::pipeline::SEA_LEVEL;
    let [cx, cz] = island.centre;
    let mut r = island.radius * SPAWN_SEARCH_FROM;
    while r <= island.radius * SPAWN_SEARCH_TO {
        for a in 0..SPAWN_SEARCH_RAYS {
            let t = a as f64 * std::f64::consts::TAU / SPAWN_SEARCH_RAYS as f64;
            let (x, z) = (cx + r * t.cos(), cz + r * t.sin());
            let h = surface(x, z);
            if h < SPAWN_HEIGHT_M.0 || h > SPAWN_HEIGHT_M.1 {
                continue;
            }
            let d = SPAWN_FLAT_PROBE_M;
            let flat = [(d, 0.0), (-d, 0.0), (0.0, d), (0.0, -d)]
                .iter()
                .all(|(dx, dz)| (surface(x + dx, z + dz) - h).abs() < SPAWN_FLAT_M);
            if flat {
                return [x, z];
            }
        }
        r += SPAWN_SEARCH_STEP_M;
    }
    island.centre
}

/// What worldgen knows about the island a region hosts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IslandInfo {
    /// Quadtree slot `(level, x, z)`.
    pub id: (u8, i64, i64),
    /// World-meter corner of the island's power-of-two square of 64 m
    /// cells, and its edge in those cells.
    pub origin_m: [f64; 2],
    pub size_cells: u32,
    /// Region-local spawn column, voxel units: a dry, flat coastal point.
    pub spawn: [u32; 2],
}

/// The island whose centre lies in region `rc`, with its spawn converted to
/// region-local voxel units (centred in a block; the island centre when the
/// coast lies outside the region).
pub fn island_info(seed: u64, rc: game::RegionCoords) -> Option<IslandInfo> {
    let blocks = game::REGION_BLOCKS as f64;
    let origin = [rc.x as f64 * blocks, rc.z as f64 * blocks];
    let field = IslandField::new(seed);
    let island = field.island_at(origin[0] + blocks / 2.0, origin[1] + blocks / 2.0)?;
    let inside = |v: f64, o: f64| (o..o + blocks).contains(&v);
    if !inside(island.centre[0], origin[0]) || !inside(island.centre[1], origin[1]) {
        return None;
    }
    let pipeline = crate::pipeline::TerrainPipeline::with_islands(seed, field);
    let mut spawn = spawn_point(&pipeline, &island);
    if !inside(spawn[0], origin[0]) || !inside(spawn[1], origin[1]) {
        // The island outgrows the region (e.g. `region_chunks=128` with the
        // default 1 km island): its coast is out of reach, the centre is not.
        log::warn!("[islands] coastal spawn {spawn:?} lies outside region {rc:?}; spawning at the island centre");
        spawn = island.centre;
    }
    let bv = game::BLOCK_VOXELS as f64;
    let local = |v: f64, o: f64| ((v - o).floor() * bv + bv / 2.0) as u32;
    let size_cells = ((2.0 * island.reach() / ISLAND_CELL_M).ceil() as u32).next_power_of_two();
    let half = size_cells as f64 * ISLAND_CELL_M / 2.0;
    Some(IslandInfo {
        id: (island.id.level, island.id.x, island.id.z),
        origin_m: [island.centre[0] - half, island.centre[1] - half],
        size_cells,
        spawn: [local(spawn[0], origin[0]), local(spawn[1], origin[1])],
    })
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
    fn spawns_are_dry_flat_and_coastal() {
        for cells in [1, 4, 16, 64] {
            let seed = 0x00D5_C0DE_8D78;
            let field = IslandField::with_spawn_cells(seed, cells);
            let island = field.spawn_island();
            let p = crate::pipeline::TerrainPipeline::with_islands(seed, field);
            let s = spawn_point(&p, &island);
            let h = p.column(s[0], s[1]).surface_y() - crate::pipeline::SEA_LEVEL;
            assert!((SPAWN_HEIGHT_M.0..=SPAWN_HEIGHT_M.1).contains(&h), "{cells} cells: spawn {h} m above sea");
            let d = ((s[0] - island.centre[0]).powi(2) + (s[1] - island.centre[1]).powi(2)).sqrt();
            assert!(d >= island.radius * SPAWN_SEARCH_FROM, "{cells} cells: spawn not coastal ({d} m out)");
        }
    }

    #[test]
    fn the_home_region_hosts_the_spawn_island() {
        let info = island_info(0x00D5_C0DE_8D78, game::RegionCoords::new(0, 0)).expect("home island");
        assert_eq!(info.id, (0, 0, 0));
        let size = game::REGION_SIZE as u32;
        assert!(info.spawn[0] < size && info.spawn[1] < size, "spawn is inside the home region");
        assert!(island_info(0x00D5_C0DE_8D78, game::RegionCoords::new(7, -3)).is_none_or(|i| i.id != (0, 0, 0)));
    }

    #[test]
    fn dial_parsing() {
        assert_eq!(parse_island_cells(None), SPAWN_ISLAND_CELLS_DEFAULT);
        assert_eq!(parse_island_cells(Some("")), SPAWN_ISLAND_CELLS_DEFAULT);
        assert_eq!(parse_island_cells(Some("0")), SPAWN_ISLAND_CELLS_DEFAULT);
        assert_eq!(parse_island_cells(Some("64")), 64);
    }
}
