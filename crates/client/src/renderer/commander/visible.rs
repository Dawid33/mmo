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
//!
//! With the cut off and the dug-space x-ray on (`voxel_pull::xray`), the
//! terrain in front of a dug space is not drawn: a ray that passes through
//! dug air in the home region sees the first block after it
//! ([`game::xray_through`]), wherever the top surface lies.

use bevy::prelude::*;
use game::rapier3d::math::Vector;
use game::{BlockId, RegionId, TilePos, TileSource, BLOCK_VOXELS};

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
}

impl Cut {
    pub fn of(level: &WorkingLevel, home: Option<RegionId>) -> Option<Self> {
        let (at, region) = (level.level?, home?);
        level.cut.then(|| Cut { level: at, region, plane_y: cut_plane_world_y(at, region) })
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

/// The dug-space x-ray, as the ray walk needs it: the home region (whose
/// blocks are walked) and which of its tiles are dug air.
pub struct Xray<D> {
    pub region: RegionId,
    pub dug: D,
}

/// The pure core of [`visible_ground`]. `surface(x, z)` is the top surface
/// (world), `solid(p)` a home-region block (region-local), `in_home(x, z)`
/// whether a world column is inside the home region; `xray` (cut off only)
/// looks through dug spaces.
#[allow(clippy::too_many_arguments)] // a pure function over injected lookups
pub fn visible_ground_with(
    origin: Vec3,
    dir: Vec3,
    max: f32,
    cut: Option<Cut>,
    surface: impl Fn(f32, f32) -> Option<f32>,
    solid: impl Fn(TilePos) -> bool,
    in_home: impl Fn(f32, f32) -> bool,
    xray: Option<Xray<impl Fn(TilePos) -> bool>>,
) -> Option<Vec3> {
    let dir = dir.normalize_or_zero();
    let step = PICK_STEP_M * UNITS_PER_METER;
    let Some(cut) = cut else {
        let size = BLOCK_VOXELS as f32;
        let through = xray.and_then(|x| {
            let offset = Vec3::from_array(x.region.world_offset());
            let start = origin - offset;
            let v = |p: Vec3| Vector::new(p.x, p.y, p.z);
            let hit = game::xray_through(v(start), v(dir), max / size, &solid, &x.dug)?;
            let min = Vec3::new(hit.block[0] as f32, hit.block[1] as f32, hit.block[2] as f32) * size;
            Some(box_entry(start, dir, min, min + Vec3::splat(size)) + offset)
        });
        return through.or_else(|| march_height(origin, dir, surface, step, max));
    };
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
    let size = BLOCK_VOXELS as f32;
    let offset = Vec3::from_array(cut.region.world_offset());
    let start = origin + dir * t0 - offset;
    let reach = ((max - t0) / size).max(0.0);
    let level = cut.level;
    let inside = game::edit::raycast_blocks(
        Vector::new(start.x, start.y, start.z),
        Vector::new(dir.x, dir.y, dir.z),
        reach,
        |p| p[1] <= level && solid(p),
    )
    .map(|hit| {
        let min = Vec3::new(hit.block[0] as f32, hit.block[1] as f32, hit.block[2] as f32) * size;
        box_entry(start, dir, min, min + Vec3::splat(size)) + offset
    });
    match (inside, outside) {
        (Some(a), Some(b)) => Some(if a.distance(origin) <= b.distance(origin) { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// The first visible ground along a ray, honouring the cut and (`xray`,
/// `voxel_pull::xray::xray_on`) the dug-space x-ray; `None` when the ray
/// meets nothing within `max` sim units.
pub fn visible_ground(
    origin: Vec3,
    dir: Vec3,
    max: f32,
    grid: &HeightGrid,
    level: &WorkingLevel,
    mirror: &TileMirror,
    xray: bool,
) -> Option<Vec3> {
    let home = mirror.region();
    let rect = home.map(super::cut_entities::home_rect);
    let cut = Cut::of(level, home);
    let xray = home
        .filter(|_| xray && cut.is_none())
        .map(|region| Xray { region, dug: |p: TilePos| mirror.is_dug(p) });
    visible_ground_with(
        origin,
        dir,
        max,
        cut,
        |x, z| grid.surface_at(x, z),
        |p| mirror.block(p) != BlockId::AIR,
        |x, z| rect.is_some_and(|r| r.contains(Vec2::new(x, z))),
        xray,
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
        Cut { level, region: home(), plane_y: cut_plane_world_y(level, home()) }
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

    const NO_XRAY: Option<Xray<fn(TilePos) -> bool>> = None;

    /// A room under the solid-to-29 terrain at block column (1, 1): air
    /// y 10..=12, under a top surface of 30 (so dug).
    fn with_room(p: TilePos) -> bool {
        solid(p) && !((p[0], p[2]) == (1, 1) && (10..=12).contains(&p[1]))
    }

    #[test]
    fn xray_with_the_cut_off_sees_the_floor_of_a_room_under_the_ground() {
        let eye = Vec3::new(1.5 * S, 60.0 * S, 1.5 * S);
        let xray = Some(Xray { region: home(), dug: |p: TilePos| !with_room(p) && game::is_dug_air(p[1], 30) });
        let hit = visible_ground_with(eye, Vec3::NEG_Y, 1e5, None, surface, with_room, everywhere, xray).unwrap();
        assert!((hit.y - 10.0 * S).abs() < 1e-3, "the room's floor (block 9's top): {hit:?}");
        // Without it, the top surface; and elsewhere the x-ray changes nothing.
        let plain = visible_ground_with(eye, Vec3::NEG_Y, 1e5, None, surface, with_room, everywhere, NO_XRAY).unwrap();
        assert!((plain.y - 30.0 * S).abs() < 1.0, "{plain:?}");
        let eye = Vec3::new(5.5 * S, 60.0 * S, 5.5 * S);
        let xray = Some(Xray { region: home(), dug: |p: TilePos| !with_room(p) && game::is_dug_air(p[1], 30) });
        let hit = visible_ground_with(eye, Vec3::NEG_Y, 1e5, None, surface, with_room, everywhere, xray).unwrap();
        assert!((hit.y - 30.0 * S).abs() < 1.0, "no room on this ray: {hit:?}");
    }

    #[test]
    fn cut_off_sees_the_top_surface() {
        let eye = Vec3::new(3.5 * S, 60.0 * S, 3.5 * S);
        let hit = visible_ground_with(eye, Vec3::NEG_Y, 1e5, None, surface, solid, everywhere, NO_XRAY).unwrap();
        assert!((hit.y - 6.0 * S).abs() < 1.0, "{hit:?}");
    }

    #[test]
    fn cut_on_sees_the_level_floor_not_the_cut_away_top() {
        let eye = Vec3::new(1.5 * S, 60.0 * S, 1.5 * S);
        let hit = visible_ground_with(eye, Vec3::NEG_Y, 1e5, Some(cut_at(12)), surface, solid, everywhere, NO_XRAY).unwrap();
        assert!((hit.y - 13.0 * S).abs() < 1e-3, "the top of level 12's blocks: {hit:?}");
    }

    #[test]
    fn cut_on_sees_down_a_pit() {
        let eye = Vec3::new(3.5 * S, 60.0 * S, 3.5 * S);
        let hit = visible_ground_with(eye, Vec3::NEG_Y, 1e5, Some(cut_at(12)), surface, solid, everywhere, NO_XRAY).unwrap();
        assert!((hit.y - 6.0 * S).abs() < 1e-3, "the pit floor: {hit:?}");
    }

    #[test]
    fn underground_eye_below_the_top_surface_still_finds_the_floor() {
        // The camera rides level 12's floor, under 17 blocks of cut-away rock.
        let eye = Vec3::new(1.5 * S, 20.0 * S, 1.5 * S);
        let dir = Vec3::new(0.0, -1.0, -1.0).normalize();
        let hit = visible_ground_with(eye, dir, 1e5, Some(cut_at(12)), surface, solid, everywhere, NO_XRAY).unwrap();
        assert!((hit.y - 13.0 * S).abs() < 1e-3, "{hit:?}");
        assert!(hit.distance(eye) > S, "not the camera itself");
    }

    #[test]
    fn outside_the_home_region_nothing_is_cut() {
        let eye = Vec3::new(1.5 * S, 60.0 * S, 1.5 * S);
        let nowhere = |_: f32, _: f32| false;
        let hit = visible_ground_with(eye, Vec3::NEG_Y, 1e5, Some(cut_at(12)), surface, |_| false, nowhere, NO_XRAY).unwrap();
        assert!((hit.y - 30.0 * S).abs() < 1.0, "{hit:?}");
    }
}
