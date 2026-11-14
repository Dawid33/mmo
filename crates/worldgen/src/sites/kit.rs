//! A town kit: the houses a lot can hold, from a prefab set and a style tag, each with the side of its
//! box the entrance is on (its front) and how wide it is along that side.

use std::sync::Arc;

use game::prefab::{Facing, PrefabSet, ResolvedPrefab};

pub const KIT_MIN_SIDE: i32 = 5;
/// A house is a house, not a hall: bigger than this and a lot holds one manor, not a town.
pub const KIT_MAX_SIDE: i32 = 28;
/// A house's box must have at least this percent of its (x, z) columns holding a block. A sparse box is
/// a compound or a diagonal strip, and its front lands on nothing. Houses only, not the centre piece.
pub const KIT_MIN_FILL_PCT: u32 = 60;
/// A centrepiece's x and z extents are at most this (its footprint on the plaza; any height).
pub const KIT_CENTRE_MAX_SIDE: i32 = 12;
/// Fewer houses than this is no kit.
const KIT_MIN_HOUSES: usize = 3;

/// A building a lot can hold, described for turn 0.
#[derive(Debug, Clone)]
pub struct KitHouse {
    pub prefab: Arc<ResolvedPrefab>,
    /// The side of the prefab's box its entrance is on.
    pub front: Facing,
    /// The box's extent ALONG that side (x extent for a north/south front, z for east/west).
    pub frontage: i32,
    /// The extent across it.
    pub depth: i32,
}

#[derive(Debug, Clone)]
pub struct TownKit {
    pub style: String,
    pub houses: Vec<KitHouse>,
    pub centrepieces: Vec<Arc<ResolvedPrefab>>,
}

fn extents(p: &ResolvedPrefab) -> (i32, i32, i32) {
    (p.size[0] as i32, p.size[1] as i32, p.size[2] as i32)
}

/// Whether at least `KIT_MIN_FILL_PCT` percent of the box's (x, z) columns hold a block.
fn filled_enough(p: &ResolvedPrefab) -> bool {
    let (sx, _, sz) = extents(p);
    let filled = (0..sz).flat_map(|z| (0..sx).map(move |x| (x, z))).filter(|&(x, z)| p.column_top(x, z).is_some()).count() as u64;
    filled * 100 >= u64::from(KIT_MIN_FILL_PCT) * (sx as u64 * sz as u64)
}

/// The side of the box the entrance is on: among the `door` markers take those on the lowest marker y;
/// of these the one nearest to a side of the box (ties broken north, east, south, west, then by marker
/// order) decides, and that nearest side is the front. No door marker gives `Facing::South`.
pub fn front_of(prefab: &ResolvedPrefab) -> Facing {
    let (sx, _, sz) = extents(prefab);
    let doors: Vec<_> = prefab.markers.iter().filter(|m| m.kind == "door").collect();
    let Some(low) = doors.iter().map(|m| m.at[1]).min() else { return Facing::South };
    doors
        .iter()
        .enumerate()
        .filter(|(_, m)| m.at[1] == low)
        .flat_map(|(i, m)| {
            let [x, _, z] = m.at;
            // north, east, south, west
            [
                (z, Facing::North),
                (sx - 1 - x, Facing::East),
                (sz - 1 - z, Facing::South),
                (x, Facing::West),
            ]
            .into_iter()
            .map(move |(d, f)| (d.max(0), f.quarter_turns(), i, f))
        })
        .min_by_key(|&(d, side, i, _)| (d, side, i))
        .map(|(_, _, _, f)| f)
        .unwrap_or(Facing::South)
}

/// Houses: prefabs tagged both `style` and `house` whose x and z extents are within
/// `KIT_MIN_SIDE..=KIT_MAX_SIDE` and at least `KIT_MIN_FILL_PCT` percent of whose columns hold a block
/// (others are skipped). Centrepieces: prefabs tagged `statue` or `decor` (any style) whose x and z
/// extents are at most `KIT_CENTRE_MAX_SIDE` (a tall statue is fine: the plaza limits the footprint). Both ascending by name. Fewer than 3 houses is an error
/// naming the style and how many were found.
pub fn town_kit(set: &PrefabSet, style: &str) -> Result<TownKit, String> {
    let has = |p: &ResolvedPrefab, tag: &str| p.tags.iter().any(|t| t == tag);
    let in_range = |v: i32| (KIT_MIN_SIDE..=KIT_MAX_SIDE).contains(&v);
    let mut houses = Vec::new();
    let mut centrepieces = Vec::new();
    for p in set.iter() {
        let (x, _, z) = extents(p);
        if has(p, style) && has(p, "house") && in_range(x) && in_range(z) && filled_enough(p) {
            let front = front_of(p);
            let (frontage, depth) = match front {
                Facing::North | Facing::South => (x, z),
                Facing::East | Facing::West => (z, x),
            };
            houses.push(KitHouse { prefab: p.clone(), front, frontage, depth });
        }
        if (has(p, "statue") || has(p, "decor")) && x.max(z) <= KIT_CENTRE_MAX_SIDE {
            centrepieces.push(p.clone());
        }
    }
    if houses.len() < KIT_MIN_HOUSES {
        return Err(format!(
            "town style \"{style}\": found {} house prefab(s), at least {KIT_MIN_HOUSES} needed",
            houses.len()
        ));
    }
    Ok(TownKit { style: style.to_string(), houses, centrepieces })
}

/// The quarter turns that make a house's front face `toward`.
pub fn turn_to_face(front: Facing, toward: Facing) -> u8 {
    (toward.quarter_turns() + 4 - front.quarter_turns()) % 4
}
