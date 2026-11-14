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
    fn placements_in(&self, x0: i32, z0: i32, x1: i32, z1: i32, margin: i32) -> Vec<&StructurePlacement>;
    /// Identity of the whole source, for the worldgen cache key.
    fn content_hash(&self) -> u32;
}

/// No placements. `content_hash()` is 0.
pub struct NoStructures;

impl StructureSource for NoStructures {
    fn placements_in(&self, _: i32, _: i32, _: i32, _: i32, _: i32) -> Vec<&StructurePlacement> {
        Vec::new()
    }
    fn content_hash(&self) -> u32 {
        0
    }
}

/// An explicit list with a spatial index (x/z buckets of `STRUCTURE_BUCKET_BLOCKS`).
pub struct StructureList {
    placements: Vec<StructurePlacement>,
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
        Self { placements, buckets, hash: h.finalize() }
    }

    pub fn len(&self) -> usize {
        self.placements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }
}

impl StructureSource for StructureList {
    fn placements_in(&self, x0: i32, z0: i32, x1: i32, z1: i32, margin: i32) -> Vec<&StructurePlacement> {
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
