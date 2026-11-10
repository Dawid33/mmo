//! The first ground a camera ray actually SEES: what the orbit pivot, the
//! ground pick and the pan speed measure against.
//!
//! With the cut off that is the height grid's surface (the top of every
//! column, trees included). With the cut on, inside the home region, the
//! terrain above the working level is cut away and the top surface is
//! meaningless — underground the camera sits below it, and a pit shows its
//! floor, not the cut plane — so the ray is walked through the home region's
//! blocks ([`super::tiles::TileMirror`]) from where it comes down through the
//! cut plane, taking only blocks at or below the level. Outside the home
//! region nothing is cut, so the height grid still answers there.

use bevy::prelude::*;
use game::rapier3d::math::Vector;
use game::{RegionId, TerrainKind, TilePos, BLOCK_VOXELS};

use super::height::HeightGrid;
use super::level::{cut_plane_world_y, WorkingLevel};
use super::selection::{march_height, PICK_STEP_M};
use super::tiles::TileMirror;
use game::movement::UNITS_PER_METER;

/// The cut, as the ray walk needs it: the working level, the home region
/// (where the cut applies) and the cut plane's world y.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cut {
    pub level: i32,
    pub region: RegionId,
    pub plane_y: f32,
    /// The home region's terrain: its tiles' height is what the walk steps in.
    pub terrain: TerrainKind,
}

impl Cut {
    pub fn of(level: &WorkingLevel, home: Option<RegionId>) -> Option<Self> {
        let (at, region) = (level.level?, home?);
        level.cut.then(|| Cut { level: at, region, plane_y: cut_plane_world_y(level.terrain, at, region), terrain: level.terrain })
    }
}

/// Where a ray (world sim units, `dir` normalised) first enters the
/// axis-aligned box `min..max`, clamped to the ray's start.
fn box_entry(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Vec3 {
    let mut t = 0.0f32;
    for a in 0..3 {
        if dir[a] != 0.0 {
            let (t0, t1) = ((min[a] - origin[a]) / dir[a], (max[a] - origin[a]) / dir[a]);
            t = t.max(t0.min(t1));
        }
    }
    origin + dir * t
}

/// The pure core of [`visible_ground`]. `surface(x, z)` is the top surface
/// (world), `solid(p)` a home-region block (region-local), `in_home(x, z)`
/// whether a world column is inside the home region.
pub fn visible_ground_with(
    origin: Vec3,
    dir: Vec3,
    max: f32,
    cut: Option<Cut>,
    surface: impl Fn(f32, f32) -> Option<f32>,
    solid: impl Fn(TilePos) -> bool,
    in_home: impl Fn(f32, f32) -> bool,
) -> Option<Vec3> {
    let dir = dir.normalize_or_zero();
    let step = PICK_STEP_M * UNITS_PER_METER;
    let Some(cut) = cut else { return march_height(origin, dir, surface, step, max) };
    // Outside the home region nothing is cut: the top surface stands, so
    // the march skips home columns (the block walk answers those).
    let outside = march_height(origin, dir, |x, z| (!in_home(x, z)).then(|| surface(x, z)).flatten(), step, max);
    // Inside: walk the blocks from where the ray comes down through the cut.
    let t0 = if origin.y > cut.plane_y {
        if dir.y >= 0.0 {
            return outside;
        }
        (cut.plane_y - origin.y) / dir.y
    } else {
        0.0
    };
    // Tiles are `size` wide and `cut.terrain`'s tile height tall: the walk runs
    // in tile space (every tile a `size` cube), the hit box is back in units.
    let size = BLOCK_VOXELS as f32;
    let tile = Vec3::new(size, cut.terrain.tile_height_units(), size);
    let offset = Vec3::from_array(cut.region.world_offset());
    let start = origin + dir * t0 - offset;
    let reach = ((max - t0) / size).max(0.0);
    let level = cut.level;
    let (ray_o, ray_d) = super::tiles::tile_ray(
        cut.terrain,
        (Vector::new(start.x, start.y, start.z), Vector::new(dir.x, dir.y, dir.z)),
    );
    let inside = game::edit::raycast_blocks(ray_o, ray_d, reach, |p| p[1] <= level && solid(p)).map(|hit| {
        let min = Vec3::new(hit.block[0] as f32, hit.block[1] as f32, hit.block[2] as f32) * tile;
        box_entry(start, dir, min, min + tile) + offset
    });
    match (inside, outside) {
        (Some(a), Some(b)) => Some(if a.distance(origin) <= b.distance(origin) { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// The first visible ground along a ray, honouring the cut; `None` when the
/// ray meets nothing within `max` sim units.
pub fn visible_ground(
    origin: Vec3,
    dir: Vec3,
    max: f32,
    grid: &HeightGrid,
    level: &WorkingLevel,
    mirror: &TileMirror,
) -> Option<Vec3> {
    let home = mirror.region();
    let rect = home.map(super::cut_entities::home_rect);
    let cut = Cut::of(level, home);
    visible_ground_with(
        origin,
        dir,
        max,
        cut,
        |x, z| grid.surface_at(x, z),
        |p| mirror.pick_solid(p),
        |x, z| rect.is_some_and(|r| r.contains(Vec2::new(x, z))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: f32 = BLOCK_VOXELS as f32;

    fn home() -> RegionId {
        game::RegionCoords::new(0, 0).into()
    }

    fn cut_at(level: i32) -> Cut {
        Cut { level, region: home(), plane_y: cut_plane_world_y(TerrainKind::Voxel, level, home()), terrain: TerrainKind::Voxel }
    }

    /// A column of terrain 30 blocks high with a pit at block column (3, 3)
    /// whose floor is block 5; the rest of the home region is solid to 29.
    fn solid(p: TilePos) -> bool {
        let top = if (p[0], p[2]) == (3, 3) { 5 } else { 29 };
        p[1] <= top
    }

    fn surface(x: f32, z: f32) -> Option<f32> {
        let pit = ((x / S).floor() as i32, (z / S).floor() as i32) == (3, 3);
        Some(if pit { 6.0 * S } else { 30.0 * S })
    }

    fn everywhere(_: f32, _: f32) -> bool {
        true
    }

    /// The top-down view's rays (`top_down`): parallel, straight down, from
    /// an eye `TOP_DOWN_EYE_HEIGHT_M` above the cut.
    #[test]
    fn a_straight_down_ray_from_above_the_cut_finds_the_level_floor() {
        let level = 20;
        let cut = cut_at(level);
        let eye_y = cut.plane_y + super::super::top_down::TOP_DOWN_EYE_HEIGHT_M * UNITS_PER_METER;
        let reach = super::super::selection::PICK_REACH_M * UNITS_PER_METER;
        // Solid ground (to 29, cut at 20): the cut level's block top, not the
        // cut-away rock above it.
        let hit = visible_ground_with(Vec3::new(1.5 * S, eye_y, 1.5 * S), Vec3::NEG_Y, reach, Some(cut), surface, solid, everywhere)
            .unwrap();
        assert!((hit.y - (level + 1) as f32 * S).abs() < 1e-3, "the cut level's top: {hit:?}");
        // Over the pit (floor block 5): the pit's floor, below the level.
        let hit = visible_ground_with(Vec3::new(3.5 * S, eye_y, 3.5 * S), Vec3::NEG_Y, reach, Some(cut), surface, solid, everywhere)
            .unwrap();
        assert!((hit.y - 6.0 * S).abs() < 1e-3, "the pit's floor: {hit:?}");
    }

    #[test]
    fn cut_off_sees_the_top_surface() {
        let eye = Vec3::new(3.5 * S, 60.0 * S, 3.5 * S);
        let hit = visible_ground_with(eye, Vec3::NEG_Y, 1e5, None, surface, solid, everywhere).unwrap();
        assert!((hit.y - 6.0 * S).abs() < 1.0, "{hit:?}");
    }

    #[test]
    fn cut_on_sees_the_level_floor_not_the_cut_away_top() {
        let eye = Vec3::new(1.5 * S, 60.0 * S, 1.5 * S);
        let hit = visible_ground_with(eye, Vec3::NEG_Y, 1e5, Some(cut_at(12)), surface, solid, everywhere).unwrap();
        assert!((hit.y - 13.0 * S).abs() < 1e-3, "the top of level 12's blocks: {hit:?}");
    }

    #[test]
    fn cut_on_sees_down_a_pit() {
        let eye = Vec3::new(3.5 * S, 60.0 * S, 3.5 * S);
        let hit = visible_ground_with(eye, Vec3::NEG_Y, 1e5, Some(cut_at(12)), surface, solid, everywhere).unwrap();
        assert!((hit.y - 6.0 * S).abs() < 1e-3, "the pit floor: {hit:?}");
    }

    #[test]
    fn underground_eye_below_the_top_surface_still_finds_the_floor() {
        // The camera rides level 12's floor, under 17 blocks of cut-away rock.
        let eye = Vec3::new(1.5 * S, 20.0 * S, 1.5 * S);
        let dir = Vec3::new(0.0, -1.0, -1.0).normalize();
        let hit = visible_ground_with(eye, dir, 1e5, Some(cut_at(12)), surface, solid, everywhere).unwrap();
        assert!((hit.y - 13.0 * S).abs() < 1e-3, "{hit:?}");
        assert!(hit.distance(eye) > S, "not the camera itself");
    }

    #[test]
    fn outside_the_home_region_nothing_is_cut() {
        let eye = Vec3::new(1.5 * S, 60.0 * S, 1.5 * S);
        let nowhere = |_: f32, _: f32| false;
        let hit = visible_ground_with(eye, Vec3::NEG_Y, 1e5, Some(cut_at(12)), surface, |_| false, nowhere).unwrap();
        assert!((hit.y - 30.0 * S).abs() < 1.0, "{hit:?}");
    }
}
