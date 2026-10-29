//! Entities above the working level are hidden while the z-level cut is on
//! (spec §6.2). Units and models are not voxel geometry, so the terrain
//! clip does not cut them: a unit standing ON the working level is drawn
//! whole (its head may poke above the cut), one whose feet are at or above
//! the cut — standing on level + 1 or higher — is hidden. Only inside the
//! home region, like the terrain cut (ruling R10).
//!
//! Feet: a sim entity's `Transform` is its body centre, region-local under
//! the region root at `RegionId::world_offset()`. Players are a capsule
//! `PLAYER_HEIGHT` tall, slimes a cube `slime_edge_units(size)` tall, both
//! centred on the pose, so feet = world y − half the body height
//! ([`feet_offset`]).
//!
//! Composing with other `Visibility` writers (ghost dedupe, the hidden local
//! body): the cut owns the entity's `Visibility` only while it hides it. On
//! hiding, [`CutHidden`] remembers what was there; a write by any other
//! system meanwhile (seen through change detection — this system's own
//! writes are not "changed" to it) replaces that remembered value, and the
//! cut re-hides. Releasing writes the remembered value back, so the cut can
//! never un-hide something another system hid.

use bevy::prelude::*;
use game::{EntityKind, RegionId, REGION_SIZE};

use super::level::{camera_cut, WorkingLevel};
use super::tiles::TileMirror;
use crate::renderer::bridge::{SimEntity, SimKind};

/// Feet this far below the cut still count as on it (world units, 1/16 m):
/// float error and the character controller's skin, not a standing height.
pub const FEET_EPSILON: f32 = 1.0;

/// Hidden by the cut; holds the visibility other systems want underneath.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutHidden(pub Visibility);

/// Body centre → feet, in world units.
pub fn feet_offset(kind: EntityKind) -> f32 {
    match kind {
        EntityKind::Player => game::movement::PLAYER_CAPSULE_HALF_HEIGHT + game::movement::PLAYER_CAPSULE_RADIUS,
        EntityKind::Slime { size } => game::slime::slime_edge_units(size) / 2.0,
    }
}

/// The home region's world-space xz rectangle, `[offset, offset + REGION_SIZE)`.
pub fn home_rect(home: RegionId) -> Rect {
    let o = home.world_offset();
    Rect::new(o[0], o[2], o[0] + REGION_SIZE, o[2] + REGION_SIZE)
}

/// Whether an entity with world-space `feet` is hidden by the cut at `cut_y`
/// (`None` = cut off) inside `home` (half-open on the far edges).
pub fn hidden_by_cut(feet: Vec3, cut_y: Option<f32>, home: Option<Rect>) -> bool {
    let (Some(cut), Some(home)) = (cut_y, home) else { return false };
    let inside = (home.min.x..home.max.x).contains(&feet.x) && (home.min.y..home.max.y).contains(&feet.z);
    inside && feet.y >= cut - FEET_EPSILON
}

/// Hide / restore kind-bearing sim entities against the current cut.
pub fn hide_cut_entities(
    mut commands: Commands,
    level: Res<WorkingLevel>,
    tiles: Res<TileMirror>,
    mut entities: Query<(Entity, &SimEntity, &SimKind, &Transform, &mut Visibility, Option<&mut CutHidden>)>,
) {
    let cut = camera_cut(&level, &tiles);
    let home = tiles.region().map(home_rect);
    for (e, sim, kind, tf, mut vis, held) in &mut entities {
        let o = sim.region.world_offset();
        let feet = tf.translation + Vec3::new(o[0], o[1] - feet_offset(kind.0), o[2]);
        let hide = hidden_by_cut(feet, cut, home);
        match (hide, held) {
            (true, None) => {
                commands.entity(e).insert(CutHidden(*vis));
                vis.set_if_neq(Visibility::Hidden);
            }
            (true, Some(mut held)) => {
                // Someone else wrote meanwhile: that is the value to restore.
                if vis.is_changed() {
                    held.0 = *vis;
                }
                vis.set_if_neq(Visibility::Hidden);
            }
            (false, Some(held)) => {
                let underneath = if vis.is_changed() { *vis } else { held.0 };
                vis.set_if_neq(underneath);
                commands.entity(e).remove::<CutHidden>();
            }
            (false, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{EntityKey, RegionCoords};

    const CUT: f32 = 640.0;

    fn home() -> RegionId {
        RegionCoords::new(1, -2)
    }

    fn inside(y: f32) -> Vec3 {
        let o = home().world_offset();
        Vec3::new(o[0] + 100.0, y, o[2] + 100.0)
    }

    #[test]
    fn feet_on_the_working_level_stay_visible() {
        assert!(!hidden_by_cut(inside(CUT - 16.0), Some(CUT), Some(home_rect(home()))));
    }

    #[test]
    fn feet_on_the_cut_are_hidden() {
        assert!(hidden_by_cut(inside(CUT), Some(CUT), Some(home_rect(home()))));
        assert!(hidden_by_cut(inside(CUT - FEET_EPSILON / 2.0), Some(CUT), Some(home_rect(home()))));
        assert!(hidden_by_cut(inside(CUT + 200.0), Some(CUT), Some(home_rect(home()))));
    }

    #[test]
    fn outside_home_region_never_hidden() {
        let rect = home_rect(home());
        let o = home().world_offset();
        for (x, z) in [
            (o[0] - 1.0, o[2] + 10.0),
            (o[0] + REGION_SIZE, o[2] + 10.0),
            (o[0] + 10.0, o[2] + REGION_SIZE),
            (o[0] + 10.0, o[2] - 0.5),
        ] {
            assert!(!hidden_by_cut(Vec3::new(x, CUT + 50.0, z), Some(CUT), Some(rect)));
        }
        assert!(hidden_by_cut(Vec3::new(o[0], CUT, o[2]), Some(CUT), Some(rect)), "near edges are inside");
        assert!(!hidden_by_cut(inside(CUT + 50.0), Some(CUT), None), "no home region");
    }

    #[test]
    fn cut_off_never_hidden() {
        assert!(!hidden_by_cut(inside(CUT + 50.0), None, Some(home_rect(home()))));
    }

    #[test]
    fn feet_are_below_the_body_centre() {
        let h = game::movement::PLAYER_HEIGHT * game::movement::UNITS_PER_METER;
        assert!((feet_offset(EntityKind::Player) - h / 2.0).abs() < 1e-4);
        assert_eq!(feet_offset(EntityKind::Slime { size: 2 }), game::slime::slime_edge_units(2) / 2.0);
    }

    fn app(level: i32) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        let mut tiles = TileMirror::default();
        tiles.set_home(Some(home()));
        app.insert_resource(tiles)
            .insert_resource(WorkingLevel { level: Some(level), cut: true })
            .add_systems(Update, hide_cut_entities);
        app
    }

    /// A slime of size 1 whose feet sit at world `feet_y` inside home.
    fn spawn(app: &mut App, feet_y: f32, vis: Visibility) -> Entity {
        let kind = EntityKind::Slime { size: 1 };
        let o = home().world_offset();
        let local = Vec3::new(100.0, feet_y - o[1] + feet_offset(kind), 100.0);
        app.world_mut()
            .spawn((
                SimEntity { region: home(), key: EntityKey::default() },
                SimKind(kind),
                Transform::from_translation(local),
                vis,
            ))
            .id()
    }

    fn vis(app: &App, e: Entity) -> Visibility {
        *app.world().entity(e).get::<Visibility>().unwrap()
    }

    fn set_level(app: &mut App, level: Option<i32>, cut: bool) {
        *app.world_mut().resource_mut::<WorkingLevel>() = WorkingLevel { level, cut };
    }

    #[test]
    fn visibility_follows_the_cut_and_restores() {
        let mut app = app(10);
        let cut = super::super::level::cut_plane_world_y(10, home());
        let e = spawn(&mut app, cut, Visibility::Inherited);
        app.update();
        assert_eq!(vis(&app, e), Visibility::Hidden);
        assert!(app.world().entity(e).contains::<CutHidden>());

        // Level up past the entity: it now stands on the working level.
        set_level(&mut app, Some(11), true);
        app.update();
        assert_eq!(vis(&app, e), Visibility::Inherited);
        assert!(!app.world().entity(e).contains::<CutHidden>());

        // Back down, then the cut toggled off.
        set_level(&mut app, Some(10), true);
        app.update();
        assert_eq!(vis(&app, e), Visibility::Hidden);
        set_level(&mut app, Some(10), false);
        app.update();
        assert_eq!(vis(&app, e), Visibility::Inherited);
    }

    #[test]
    fn hidden_by_another_system_stays_hidden() {
        let mut app = app(10);
        let cut = super::super::level::cut_plane_world_y(10, home());
        // Hidden before the cut touched it.
        let before = spawn(&mut app, cut, Visibility::Hidden);
        // Hidden by another system while the cut held it.
        let during = spawn(&mut app, cut, Visibility::Inherited);
        app.update();
        app.world_mut().entity_mut(during).insert(Visibility::Hidden);
        app.update();
        set_level(&mut app, None, true);
        app.update();
        assert_eq!(vis(&app, before), Visibility::Hidden);
        assert_eq!(vis(&app, during), Visibility::Hidden);

        // And one another system showed while the cut held it is shown after.
        set_level(&mut app, Some(10), true);
        app.update();
        app.world_mut().entity_mut(before).insert(Visibility::Inherited);
        app.update();
        assert_eq!(vis(&app, before), Visibility::Hidden, "still under the cut");
        set_level(&mut app, Some(10), false);
        app.update();
        assert_eq!(vis(&app, before), Visibility::Inherited);
    }
}
