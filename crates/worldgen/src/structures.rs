//! Structure placements for world generation: what a placement is, a source of them with a spatial
//! index, the pad arithmetic that levels ground under a building, and the tree-keep-out predicate.
//! Data only: no terrain is sampled here. All coordinate math is integer.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use game::prefab::{placement_bounds, Placement, ResolvedPrefab};

/// Level the ground under a placement. `level` is the world y of the top ground block inside the
/// placement's x/z box (the prefab's lowest layer normally sits at level + 1); over `skirt` further
/// columns the ground blends back to the natural surface.
///
/// The pad moves ground only, never water: where `level` (or a skirt column's blended surface) lies
/// below the column's water top, that column is flooded — its recipe water fills the prefab's air — so
/// a placer must not choose such a level. Every padded column, box and skirt alike, loses its glacier
/// ice (and any ice on its water).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad {
    pub level: i32,
    pub skirt: u32,
}

pub const MAX_PAD_SKIRT: u32 = 16;
/// Side of one spatial-index bucket, in world blocks.
pub const STRUCTURE_BUCKET_BLOCKS: i32 = 64;

#[derive(Debug, Clone)]
pub struct StructurePlacement {
    pub prefab: Arc<ResolvedPrefab>,
    pub at: Placement,
    pub pad: Option<Pad>,
}

impl StructurePlacement {
    /// World-block box of the prefab: (min inclusive, max exclusive).
    pub fn bounds(&self) -> ([i32; 3], [i32; 3]) {
        placement_bounds(&self.prefab, &self.at)
    }

    /// The x/z box grown by the pad's skirt (0 without a pad): [x0, z0, x1, z1], max exclusive.
    pub fn reach_xz(&self) -> [i32; 4] {
        let (min, max) = self.bounds();
        let s = self.pad.map_or(0, |p| p.skirt as i32);
        [min[0] - s, min[2] - s, max[0] + s, max[2] + s]
    }
}

/// What a pad does to one ground column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadEffect {
    None,
    Level { surface: i32, in_box: bool },
}

pub trait StructureSource: Send + Sync {
    /// Placements whose `reach_xz` grown by `margin` intersects the half-open x/z rectangle
    /// `[x0, x1) x [z0, z1)`, in the source's own fixed order, each once. Must not depend on anything
    /// but its arguments.
    fn placements_in(&self, x0: i32, z0: i32, x1: i32, z1: i32, margin: i32) -> Vec<Arc<StructurePlacement>>;
    /// Identity of the whole source, for the worldgen cache key.
    fn content_hash(&self) -> u32;
}

/// No placements. `content_hash()` is 0.
pub struct NoStructures;

impl StructureSource for NoStructures {
    fn placements_in(&self, _: i32, _: i32, _: i32, _: i32, _: i32) -> Vec<Arc<StructurePlacement>> {
        Vec::new()
    }
    fn content_hash(&self) -> u32 {
        0
    }
}

/// An explicit list with a spatial index (x/z buckets of `STRUCTURE_BUCKET_BLOCKS`).
pub struct StructureList {
    placements: Vec<Arc<StructurePlacement>>,
    buckets: BTreeMap<[i32; 2], Vec<u32>>,
    hash: u32,
}

impl StructureList {
    /// `set_hash` = the prefab set's `content_hash()` the placements were drawn from.
    pub fn new(mut placements: Vec<StructurePlacement>, set_hash: u32) -> Self {
        for p in &mut placements {
            if let Some(pad) = &mut p.pad {
                pad.skirt = pad.skirt.min(MAX_PAD_SKIRT);
            }
        }
        let b = STRUCTURE_BUCKET_BLOCKS;
        let mut buckets: BTreeMap<[i32; 2], Vec<u32>> = BTreeMap::new();
        let mut h = crc32fast::Hasher::new();
        h.update(&set_hash.to_le_bytes());
        for (i, p) in placements.iter().enumerate() {
            let [x0, z0, x1, z1] = p.reach_xz();
            for bx in x0.div_euclid(b)..=(x1 - 1).div_euclid(b) {
                for bz in z0.div_euclid(b)..=(z1 - 1).div_euclid(b) {
                    buckets.entry([bx, bz]).or_default().push(i as u32);
                }
            }
            h.update(&p.prefab.name_key.to_le_bytes());
            for v in p.at.anchor {
                h.update(&v.to_le_bytes());
            }
            h.update(&[p.at.turn, p.at.mirror as u8]);
            match p.pad {
                Some(pad) => {
                    h.update(&[1]);
                    h.update(&pad.level.to_le_bytes());
                    h.update(&pad.skirt.to_le_bytes());
                }
                None => h.update(&[0]),
            }
        }
        Self { placements: placements.into_iter().map(Arc::new).collect(), buckets, hash: h.finalize() }
    }

    pub fn len(&self) -> usize {
        self.placements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }
}

impl StructureSource for StructureList {
    fn placements_in(&self, x0: i32, z0: i32, x1: i32, z1: i32, margin: i32) -> Vec<Arc<StructurePlacement>> {
        if x0 >= x1 || z0 >= z1 {
            return Vec::new();
        }
        let b = STRUCTURE_BUCKET_BLOCKS;
        // A placement reaches the rectangle when its box grown by `margin` meets it; equivalently the
        // rectangle grown by `margin` meets its box. Query the grown rectangle's buckets.
        let (gx0, gz0, gx1, gz1) = (x0 - margin, z0 - margin, x1 + margin, z1 + margin);
        let mut ids: Vec<u32> = Vec::new();
        for bx in gx0.div_euclid(b)..=(gx1 - 1).div_euclid(b) {
            for bz in gz0.div_euclid(b)..=(gz1 - 1).div_euclid(b) {
                if let Some(v) = self.buckets.get(&[bx, bz]) {
                    ids.extend_from_slice(v);
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .map(|i| &self.placements[i as usize])
            .filter(|p| {
                let [a, c, d, e] = p.reach_xz();
                a < gx1 && d > gx0 && c < gz1 && e > gz0
            })
            .cloned()
            .collect()
    }

    fn content_hash(&self) -> u32 {
        self.hash
    }
}

/// Chebyshev distance from column (x, z) to the half-open x/z box (0 inside).
fn box_distance(min: [i32; 3], max: [i32; 3], x: i32, z: i32) -> i64 {
    let gap = |v: i32, lo: i32, hi: i32| -> i64 {
        if v < lo {
            (lo - v) as i64
        } else if v >= hi {
            (v - (hi - 1)) as i64
        } else {
            0
        }
    };
    gap(x, min[0], max[0]).max(gap(z, min[2], max[2]))
}

/// The surface of ground column (x, z) under these placements: the FIRST placement (in list order)
/// whose pad reaches the column decides. Inside its x/z box: `Level { surface: level, in_box: true }`.
/// At Chebyshev distance d in 1..=skirt from the box: `surface = level + (natural - level) * d /
/// (skirt + 1)` (i64 arithmetic, truncating division), `in_box: false`. Otherwise `PadEffect::None`.
pub fn pad_at(placements: &[&StructurePlacement], x: i32, z: i32, natural: i32) -> PadEffect {
    for p in placements {
        let Some(pad) = p.pad else { continue };
        let skirt = pad.skirt.min(MAX_PAD_SKIRT) as i64;
        let (min, max) = p.bounds();
        let d = box_distance(min, max, x, z);
        if d == 0 {
            return PadEffect::Level { surface: pad.level, in_box: true };
        }
        if d <= skirt {
            let level = pad.level as i64;
            let surface = level + (natural as i64 - level) * d / (skirt + 1);
            return PadEffect::Level { surface: surface as i32, in_box: false };
        }
    }
    PadEffect::None
}

/// Whether trees must stay off column (x, z): inside any placement's x/z box grown by `tree_reach`.
pub fn blocks_trees(placements: &[&StructurePlacement], x: i32, z: i32, tree_reach: i32) -> bool {
    placements.iter().any(|p| {
        let (min, max) = p.bounds();
        box_distance(min, max, x, z) <= tree_reach as i64
    })
}

static ACTIVE: OnceLock<Arc<dyn StructureSource>> = OnceLock::new();
static NONE: NoStructures = NoStructures;

/// Panics if a source was already installed.
pub fn install(source: Arc<dyn StructureSource>) {
    if ACTIVE.set(source).is_err() {
        panic!("worldgen::structures::install: a structure source is already installed");
    }
}

/// The installed source, or `&NoStructures`.
pub fn active() -> &'static dyn StructureSource {
    match ACTIVE.get() {
        Some(s) => s.as_ref(),
        None => &NONE,
    }
}

/// One entry of a dev structure spec: `name`, `name:dx,dz`, or `name:dx,dz:turn` (turn 0..=3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevEntry {
    pub prefab: String,
    pub offset: Option<[i32; 2]>,
    pub turn: u8,
}

/// Skirt of the pad under a dev structure, in blocks.
pub const DEV_PAD_SKIRT: u32 = 4;
/// Gap between dev structures in a row, and between the origin and the first, in blocks.
pub const DEV_GAP_BLOCKS: i32 = 6;

/// Parses `"dev-hut;statuen-06:40,0:1"`: entries separated by `;`, each `name[:dx,dz[:turn]]`. Name,
/// dx, dz and turn are trimmed of whitespace. A name is non-empty and has no `:`, `;` or whitespace
/// inside; an empty entry (a trailing `;` too) is an error. Errors name the entry.
pub fn parse_dev_spec(spec: &str) -> Result<Vec<DevEntry>, String> {
    spec.split(';').enumerate().map(|(i, e)| parse_dev_entry(i + 1, e)).collect()
}

fn parse_dev_entry(position: usize, entry: &str) -> Result<DevEntry, String> {
    let bad = |why: &str| format!("structure entry {position} \"{entry}\": {why}");
    if entry.trim().is_empty() {
        return Err(bad("empty entry"));
    }
    let mut parts = entry.split(':');
    let name = parts.next().unwrap_or("").trim();
    if name.is_empty() {
        return Err(bad("missing prefab name"));
    }
    if name.contains(char::is_whitespace) {
        return Err(bad("a prefab name has no whitespace"));
    }
    let offset = match parts.next() {
        None => None,
        Some(o) => {
            let (dx, dz) = o.split_once(',').ok_or_else(|| bad("offset must be dx,dz"))?;
            let num = |s: &str| s.trim().parse::<i32>().map_err(|_| bad(&format!("\"{}\" is not a number", s.trim())));
            Some([num(dx)?, num(dz)?])
        }
    };
    let turn = match parts.next() {
        None => 0,
        Some(t) => match t.trim().parse::<u8>() {
            Ok(t) if t <= 3 => t,
            _ => return Err(bad("turn must be 0..=3")),
        },
    };
    if parts.next().is_some() {
        return Err(bad("too many ':' parts"));
    }
    Ok(DevEntry { prefab: name.to_string(), offset, turn })
}

/// Lays the entries out beside `origin` (a world x/z, e.g. a landing column): an entry with an offset
/// is anchored at origin + offset; entries without one are put in a row along +x starting at
/// origin.x + DEV_GAP_BLOCKS, each after the previous one's x extent plus DEV_GAP_BLOCKS, all at
/// origin.z. Each prefab's anchor lands at (x, ground(x, z) + 1, z) with `Pad { level: ground(x, z),
/// skirt: DEV_PAD_SKIRT }`, where `ground` is called for the ANCHOR column only. Unknown prefab → Err.
pub fn dev_placements(
    entries: &[DevEntry],
    origin: [i32; 2],
    prefabs: &game::prefab::PrefabSet,
    ground: &dyn Fn(i32, i32) -> i32,
) -> Result<Vec<StructurePlacement>, String> {
    let mut next_min_x = origin[0] + DEV_GAP_BLOCKS;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let prefab = prefabs
            .named(&e.prefab)
            .ok_or_else(|| format!("unknown prefab \"{}\" in the dev structure spec", e.prefab))?
            .clone();
        let at = |x: i32, y: i32, z: i32| Placement { anchor: [x, y, z], turn: e.turn, mirror: false };
        let [x, z] = match e.offset {
            Some([dx, dz]) => [origin[0] + dx, origin[1] + dz],
            None => {
                // Where the box starts relative to the anchor, for this turn.
                let (min, max) = placement_bounds(&prefab, &at(0, 0, 0));
                let x = next_min_x - min[0];
                next_min_x = x + max[0] + DEV_GAP_BLOCKS;
                [x, origin[1]]
            }
        };
        let level = ground(x, z);
        out.push(StructurePlacement {
            prefab,
            at: at(x, level + 1, z),
            pad: Some(Pad { level, skirt: DEV_PAD_SKIRT }),
        });
    }
    Ok(out)
}

type OriginOf = Box<dyn Fn(game::RegionCoords) -> [i32; 2] + Send + Sync>;
type GroundOf = Box<dyn Fn(i32, i32) -> i32 + Send + Sync>;

/// Dev placements beside EVERY region's origin column (`origin_of`), computed on first use per region
/// and cached. A region's placements are `dev_placements(entries, origin_of(rc), prefabs, ground)`: a
/// pure function of the region, so answers never depend on what was cached before.
///
/// `origin_of(rc)` must lie inside region `rc`: then every placement of `rc` lies within the region's
/// block box grown by `reach`.
pub struct DevRegionSource {
    entries: Vec<DevEntry>,
    prefabs: game::prefab::PrefabSet,
    origin_of: OriginOf,
    ground: GroundOf,
    /// How far (Chebyshev, x/z) a region's placements, pads included, reach from its origin.
    reach: i32,
    hash: u32,
    cache: std::sync::Mutex<BTreeMap<[i32; 2], Arc<Vec<Arc<StructurePlacement>>>>>,
}

impl DevRegionSource {
    /// Lays the row out once at a dummy origin over flat ground: an unknown prefab is an error here,
    /// and the layout gives `reach`.
    pub fn new(
        entries: Vec<DevEntry>,
        prefabs: &game::prefab::PrefabSet,
        origin_of: OriginOf,
        ground: GroundOf,
    ) -> Result<Self, String> {
        let dummy = dev_placements(&entries, [0, 0], prefabs, &|_, _| 0)?;
        let mut reach = 0i32;
        for p in &dummy {
            let [x0, z0, x1, z1] = p.reach_xz();
            reach = reach.max(x0.abs()).max(z0.abs()).max(x1.abs()).max(z1.abs());
        }
        let max_offset = entries
            .iter()
            .filter_map(|e| e.offset)
            .map(|[dx, dz]| dx.abs().max(dz.abs()))
            .max()
            .unwrap_or(0);
        let mut h = crc32fast::Hasher::new();
        h.update(&prefabs.content_hash().to_le_bytes());
        h.update(&[2]); // not a StructureList
        for e in &entries {
            h.update(&(e.prefab.len() as u32).to_le_bytes());
            h.update(e.prefab.as_bytes());
            match e.offset {
                Some([dx, dz]) => {
                    h.update(&[1]);
                    h.update(&dx.to_le_bytes());
                    h.update(&dz.to_le_bytes());
                }
                None => h.update(&[0]),
            }
            h.update(&[e.turn]);
        }
        Ok(Self {
            entries,
            prefabs: prefabs.clone(),
            origin_of,
            ground,
            reach: reach + max_offset,
            hash: h.finalize(),
            cache: std::sync::Mutex::new(BTreeMap::new()),
        })
    }

    fn region(&self, rx: i32, rz: i32) -> Arc<Vec<Arc<StructurePlacement>>> {
        let cached = self.cache.lock().unwrap_or_else(|e| e.into_inner()).get(&[rx, rz]).cloned();
        if let Some(c) = cached {
            return c;
        }
        // Outside the lock: pure, so a race computes the same value twice.
        let origin = (self.origin_of)(game::RegionCoords { x: rx, z: rz });
        let placements = dev_placements(&self.entries, origin, &self.prefabs, &*self.ground)
            .expect("checked in DevRegionSource::new");
        let value = Arc::new(placements.into_iter().map(Arc::new).collect::<Vec<_>>());
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).entry([rx, rz]).or_insert(value).clone()
    }
}

impl StructureSource for DevRegionSource {
    fn placements_in(&self, x0: i32, z0: i32, x1: i32, z1: i32, margin: i32) -> Vec<Arc<StructurePlacement>> {
        if x0 >= x1 || z0 >= z1 {
            return Vec::new();
        }
        let rb = game::REGION_BLOCKS as i32;
        let (gx0, gz0, gx1, gz1) = (x0 - margin, z0 - margin, x1 + margin, z1 + margin);
        // Regions whose block box grown by `reach` meets the grown rectangle.
        let r = self.reach;
        let lo = |g: i32| (g - r).div_euclid(rb) - 1;
        let hi = |g: i32| (g + r).div_euclid(rb) + 1;
        let mut out = Vec::new();
        for rx in lo(gx0)..=hi(gx1) {
            if rx * rb - r >= gx1 || (rx + 1) * rb + r <= gx0 {
                continue;
            }
            for rz in lo(gz0)..=hi(gz1) {
                if rz * rb - r >= gz1 || (rz + 1) * rb + r <= gz0 {
                    continue;
                }
                for p in self.region(rx, rz).iter() {
                    let [a, c, d, e] = p.reach_xz();
                    if a < gx1 && d > gx0 && c < gz1 && e > gz0 {
                        out.push(p.clone());
                    }
                }
            }
        }
        out
    }

    fn content_hash(&self) -> u32 {
        self.hash
    }
}
