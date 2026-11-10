//! Item animations. Dropped items are cell-keyed piles with no physics
//! (`game::item_piles`): a pile that falls, is pushed up or is thrown jumps
//! to its rest cell in one tick, and a pickup removes it in one tick. The
//! sim tells the renderer about those jumps (`ItemsLanded`, `ItemsPickedUp`,
//! render-only, Do only) and this module plays them as short flights of
//! transient cubes:
//!
//! - a landing flies the cubes from the start cell to the rest cell (a fall
//!   accelerates like gravity, a push up eases out, any sideways move arcs)
//!   while the destination pile is held hidden (`LandingHold`), so the items
//!   are never drawn twice;
//! - a pickup flies the cubes into the picker's body and shrinks them.
//!
//! A rollback replay re-sends the same events with the same tick, so they are
//! deduplicated on (region, tick, cells) ([`RecentFlights`]).

use std::collections::{BTreeSet, VecDeque};

use bevy::prelude::*;
use game::edit::BlockPos;
use game::{EntityKey, ItemStack, RegionId};

use super::bridge::SimEntityMap;
use super::item_piles::{cube_layout, pile_world_position, ItemPileAssets, ItemPileMsg, ItemPileRoots, ITEM_SPIN_RAD_PER_S};
use crate::session::SessionScoped;

/// How long a pickup flight takes, seconds.
pub const PICKUP_FLIGHT_SECS: f32 = 0.15;
/// Scale a picked-up cube has shrunk to when it reaches the body.
pub const PICKUP_END_SCALE: f32 = 0.3;
/// Gravity of a falling pile's flight, m/s² (visual only).
pub const FALL_GRAVITY_M_S2: f32 = 25.0;
/// Speed of a sideways move (a throw), m/s.
pub const THROW_SPEED_M_S: f32 = 8.0;
/// Peak height of a sideways move's arc, metres.
pub const THROW_ARC_M: f32 = 0.6;
/// Shortest and longest landing flight, seconds.
pub const LANDING_MIN_SECS: f32 = 0.12;
pub const LANDING_MAX_SECS: f32 = 1.2;
/// Events remembered for replay dedupe.
const RECENT_MAX: usize = 1024;

/// One transient cube in flight.
#[derive(Component, Debug, Clone)]
pub struct ItemFlight {
    /// World position the cube starts at (its cell floor centre + offset).
    from: Vec3,
    to: FlightTarget,
    /// Offset of this cube within its pile, kept along the way.
    offset: Vec3,
    elapsed: f32,
    duration: f32,
    pickup: bool,
}

#[derive(Debug, Clone, Copy)]
enum FlightTarget {
    Point(Vec3),
    /// A sim body (its render entity); `last` is where it was last seen.
    Body { entity: Entity, last: Vec3 },
}

/// Hides a pile root until its landing flight has arrived (seconds left).
#[derive(Component, Debug, Clone, Copy)]
pub struct LandingHold(pub f32);

/// Events already played (replays re-send them).
#[derive(Resource, Default)]
pub struct RecentFlights {
    seen: BTreeSet<(RegionId, u64, BlockPos, BlockPos, bool)>,
    order: VecDeque<(RegionId, u64, BlockPos, BlockPos, bool)>,
}

impl RecentFlights {
    /// True the first time this event is seen.
    fn first(&mut self, key: (RegionId, u64, BlockPos, BlockPos, bool)) -> bool {
        if !self.seen.insert(key) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > RECENT_MAX {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }
}

/// How long a landing from `from` to `to` (world units) takes.
pub fn landing_secs(from: Vec3, to: Vec3) -> f32 {
    let unit = game::movement::UNITS_PER_METER;
    let drop_m = ((from.y - to.y) / unit).abs();
    let side_m = Vec2::new(to.x - from.x, to.z - from.z).length() / unit;
    let fall = (2.0 * drop_m / FALL_GRAVITY_M_S2).sqrt();
    (fall.max(side_m / THROW_SPEED_M_S)).clamp(LANDING_MIN_SECS, LANDING_MAX_SECS)
}

/// Where a landing flight is at progress `p` (0..=1).
pub fn landing_at(from: Vec3, to: Vec3, p: f32) -> Vec3 {
    let p = p.clamp(0.0, 1.0);
    let unit = game::movement::UNITS_PER_METER;
    let side = Vec2::new(to.x - from.x, to.z - from.z).length();
    // Falls accelerate (gravity), rises ease out.
    let v = if to.y < from.y { p * p } else { 1.0 - (1.0 - p) * (1.0 - p) };
    let arc = if side > 0.5 * unit { THROW_ARC_M * unit * 4.0 * p * (1.0 - p) } else { 0.0 };
    Vec3::new(from.x + (to.x - from.x) * p, from.y + (to.y - from.y) * v + arc, from.z + (to.z - from.z) * p)
}

#[allow(clippy::too_many_arguments)]
fn spawn_flight(
    commands: &mut Commands,
    assets: &mut ItemPileAssets,
    images: &mut Assets<Image>,
    mats: &mut Assets<StandardMaterial>,
    stacks: &[ItemStack],
    from: Vec3,
    to: FlightTarget,
    duration: f32,
    pickup: bool,
) {
    for (_, offset, mesh, material) in cube_layout(assets, images, mats, stacks) {
        commands.spawn((
            ItemFlight { from, to, offset, elapsed: 0.0, duration, pickup },
            SessionScoped,
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::from_translation(from + offset),
            Visibility::default(),
        ));
    }
}

/// Turns pickup / landing events into flights (after `apply_pile_msgs`, so a
/// landing's destination pile already exists and can be held hidden).
#[allow(clippy::too_many_arguments)]
pub fn start_flights(
    mut commands: Commands,
    mut msgs: MessageReader<ItemPileMsg>,
    mut recent: ResMut<RecentFlights>,
    mut assets: ResMut<ItemPileAssets>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    roots: Res<ItemPileRoots>,
    sim_entities: Option<Res<SimEntityMap>>,
    bodies: Query<&GlobalTransform>,
    scale: Option<Res<super::stud::ClientBodyScale>>,
) {
    let kind = super::item_piles::session_terrain(scale.as_deref());
    for msg in msgs.read() {
        match msg {
            ItemPileMsg::Landed { region, flights } => {
                for f in flights {
                    if !recent.first((*region, f.tick, f.from, f.to, false)) {
                        continue;
                    }
                    let (from, to) = (pile_world_position(*region, f.from, kind), pile_world_position(*region, f.to, kind));
                    let secs = landing_secs(from, to);
                    spawn_flight(&mut commands, &mut assets, &mut images, &mut mats, &f.stacks, from, FlightTarget::Point(to), secs, false);
                    if let Some(&root) = roots.0.get(&(*region, f.to)) {
                        commands.entity(root).insert(LandingHold(secs));
                    }
                }
            }
            ItemPileMsg::PickedUp { region, picks } => {
                for p in picks {
                    if !recent.first((*region, p.tick, p.cell, p.cell, true)) {
                        continue;
                    }
                    let from = pile_world_position(*region, p.cell, kind);
                    let body = body_entity(sim_entities.as_deref(), *region, p.entity);
                    let to = match body.and_then(|e| bodies.get(e).ok().map(|g| (e, g.translation()))) {
                        Some((entity, at)) => FlightTarget::Body { entity, last: at },
                        // Not drawn here: the items just vanish where they lay.
                        None => FlightTarget::Point(from),
                    };
                    spawn_flight(&mut commands, &mut assets, &mut images, &mut mats, &p.stacks, from, to, PICKUP_FLIGHT_SECS, true);
                }
            }
            _ => {}
        }
    }
}

fn body_entity(map: Option<&SimEntityMap>, region: RegionId, key: EntityKey) -> Option<Entity> {
    map?.0.get(&(region, key)).copied()
}

/// Moves the flights, ends them on arrival, and counts down landing holds.
pub fn fly(
    mut commands: Commands,
    time: Res<Time>,
    mut flights: Query<(Entity, &mut ItemFlight, &mut Transform)>,
    bodies: Query<&GlobalTransform, Without<ItemFlight>>,
    mut holds: Query<(Entity, &mut LandingHold)>,
) {
    let dt = time.delta_secs();
    for (e, mut f, mut tf) in &mut flights {
        f.elapsed += dt;
        let p = (f.elapsed / f.duration).min(1.0);
        let target = match &mut f.to {
            FlightTarget::Point(at) => *at,
            FlightTarget::Body { entity, last } => {
                if let Ok(g) = bodies.get(*entity) {
                    *last = g.translation();
                }
                *last
            }
        };
        let (from, offset) = (f.from, f.offset);
        if f.pickup {
            // Ease in towards the body, shrinking on the way.
            let e2 = p * p;
            tf.translation = (from + offset).lerp(target, e2);
            tf.scale = Vec3::splat(1.0 + (PICKUP_END_SCALE - 1.0) * p);
        } else {
            tf.translation = landing_at(from, target, p) + offset;
        }
        tf.rotation = Quat::from_rotation_y(f.elapsed * ITEM_SPIN_RAD_PER_S * 4.0);
        if p >= 1.0 {
            commands.entity(e).despawn();
        }
    }
    for (e, mut hold) in &mut holds {
        hold.0 -= dt;
        if hold.0 <= 0.0 {
            commands.entity(e).remove::<LandingHold>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::item_piles::{ItemPileRoot, ItemPilesPlugin};
    use bevy::time::TimeUpdateStrategy;
    use game::{ItemId, PickupView, PileFlight, PileView};
    use std::time::Duration;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin { file_path: "../../assets".into(), ..default() },
            bevy::transform::TransformPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .init_resource::<SimEntityMap>()
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(50)))
        .add_plugins(ItemPilesPlugin);
        app
    }

    fn dirt(n: u32) -> Vec<ItemStack> {
        vec![ItemStack { item: ItemId::block(game::BlockId(1)), count: n }]
    }

    fn flights(app: &mut App) -> Vec<Vec3> {
        let mut q = app.world_mut().query::<(&ItemFlight, &Transform)>();
        q.iter(app.world()).map(|(_, t)| t.translation).collect()
    }

    fn steps(app: &mut App, n: usize) {
        for _ in 0..n {
            app.update();
        }
    }

    #[test]
    fn pickup_flies_into_the_body_and_ends() {
        let mut app = app();
        let region = RegionId::new(0, 0);
        let key = EntityKey::default();
        let body_at = pile_world_position(region, [3, 2, 3], game::TerrainKind::Stud) + Vec3::Y * 16.0;
        let body = app.world_mut().spawn((Transform::from_translation(body_at), GlobalTransform::from_translation(body_at))).id();
        app.world_mut().resource_mut::<SimEntityMap>().0.insert((region, key), body);
        let pick = PickupView { cell: [3, 1, 3], entity: key, stacks: dirt(20), tick: 9 };
        app.world_mut().write_message(ItemPileMsg::PickedUp { region, picks: vec![pick.clone()] });
        steps(&mut app, 2);
        let f = flights(&mut app);
        assert_eq!(f.len(), 2, "20 items = 2 cubes in flight");
        // A replay of the same event (same tick) plays nothing new.
        app.world_mut().write_message(ItemPileMsg::PickedUp { region, picks: vec![pick] });
        steps(&mut app, 1);
        assert!(flights(&mut app).len() <= 2);
        steps(&mut app, 10); // 0.5 s > PICKUP_FLIGHT_SECS
        assert!(flights(&mut app).is_empty(), "flights end at the body");
    }

    #[test]
    fn landing_holds_the_pile_until_the_flight_arrives() {
        let mut app = app();
        let region = RegionId::new(0, 0);
        let (from, to) = ([5, 9, 5], [5, 1, 5]);
        app.world_mut().write_message(ItemPileMsg::Set { region, changes: vec![(to, Some(PileView { stacks: dirt(1) }))] });
        app.world_mut().write_message(ItemPileMsg::Landed { region, flights: vec![PileFlight { from, to, stacks: dirt(1), tick: 3 }] });
        steps(&mut app, 2);
        let start = pile_world_position(region, from, game::TerrainKind::Stud).y;
        let f = flights(&mut app);
        assert_eq!(f.len(), 1);
        assert!(f[0].y < start + 16.0 && f[0].y > pile_world_position(region, to, game::TerrainKind::Stud).y, "in the air between the cells");
        let mut q = app.world_mut().query::<(&ItemPileRoot, &Visibility)>();
        assert!(q.iter(app.world()).all(|(_, v)| *v == Visibility::Hidden), "the pile waits for its flight");
        let secs = landing_secs(pile_world_position(region, from, game::TerrainKind::Stud), pile_world_position(region, to, game::TerrainKind::Stud));
        steps(&mut app, (secs / 0.05) as usize + 3);
        assert!(flights(&mut app).is_empty(), "landed");
        let mut q = app.world_mut().query::<(&ItemPileRoot, &Visibility)>();
        assert!(q.iter(app.world()).all(|(_, v)| *v == Visibility::Inherited), "the pile shows once it has landed");
    }

    #[test]
    fn falls_accelerate_and_rises_ease_out() {
        let (hi, lo) = (Vec3::new(0.0, 160.0, 0.0), Vec3::ZERO);
        // Falling: a quarter of the time covers a sixteenth of the height.
        assert!((landing_at(hi, lo, 0.25).y - 150.0).abs() < 1e-3);
        // Rising: a quarter of the time covers 7/16 of the height.
        assert!((landing_at(lo, hi, 0.25).y - 70.0).abs() < 1e-3);
        assert_eq!(landing_at(hi, lo, 1.0), lo);
        // A sideways move arcs above the straight line.
        let side = Vec3::new(64.0, 0.0, 0.0);
        assert!(landing_at(Vec3::ZERO, side, 0.5).y > 0.0);
    }
}
