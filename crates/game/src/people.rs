//! The world's people as bodies in a running region: a [`EntityKind::Person`]
//! entity is a dwarf-rigged kinematic capsule exactly like a player's, but
//! with no camera and no client. `GameData::person_entities` maps each
//! history `PersonId` to its body here; seam crossing keeps it in step
//! (`extract_entity_safe` / `apply_arrival`).

use crate::history::PersonId;
use crate::movement::{self, MoveKeys, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS};
use crate::{
    ColliderSpec, EntityKey, EntityKind, GameDataTransactionKind, GameDataUpdate,
    GameDataUpdateKind, Goal, PersonSpawn, Pose, REGION_BLOCKS, RegionCoords, Rollback, RouteMap,
    SpawnAt, current_tile, feet_of, nearest_standable, steer,
};
use rapier3d::math::Vector;

/// An idle person waits at least this many ticks between wanders.
pub const WANDER_MIN_TICKS: u64 = 100;
/// ... plus a roll below this many.
pub const WANDER_RANGE_TICKS: u64 = 200;
/// A wander goal lies within this many blocks (per axis) of the person's home column.
pub const WANDER_RADIUS_BLOCKS: i32 = 8;
/// The search budget, in nodes, of a wander's plan. A goal within
/// [`WANDER_RADIUS_BLOCKS`] that a search this size cannot reach is skipped: with the long-walk
/// budget ([`crate::MAX_PATH_NODES`]) an unreachable wander goal (past a cliff, on a ledge) cost a
/// whole-neighbourhood search every wander of every resident.
pub const WANDER_PATH_NODES: u32 = 2_048;
/// A standing person checks its footing every this many ticks (staggered by
/// person, so a crowd never checks on one tick), and re-runs the character
/// controller only when its tile is no longer standable: one whose ground was
/// dug away falls.
pub const IDLE_RESETTLE_TICKS: u64 = 20;
/// A walking person follows its planned tiles directly ([`Rollback::follow_path_safe`],
/// no collision sweep) unless a player body is within this many blocks (horizontally)
/// of it or of its next tile; then it walks through the character controller, so it
/// never walks into a player.
pub const PATH_FOLLOW_PLAYER_CLEARANCE_BLOCKS: f32 = 2.0;
/// ... and within this many blocks vertically.
pub const PATH_FOLLOW_PLAYER_CLEARANCE_Y_BLOCKS: f32 = 3.0;
/// Share of two people's capsule overlap a tick of [`Rollback::separation_pushes`]
/// resolves (split between them): people may overlap a little and drift apart over a
/// few ticks instead of colliding hard.
pub const PERSON_SEPARATION_SHARE: f32 = 0.3;
/// The most a person is pushed aside in one tick, metres (2 m/s at a 50 ms tick).
pub const PERSON_SEPARATION_MAX_STEP_M: f32 = 0.1;
/// A path-following person's feet move toward the next tile's level at up to this
/// many times its horizontal step per tick (a step up or down eases over a few ticks).
pub const PATH_FOLLOW_CLIMB_RATE: f32 = 2.0;

const SALT_WANDER_WAIT: u64 = 0x9E47_0001;
const SALT_WANDER_GOAL: u64 = 0x9E47_0002;

/// When an idle person next picks somewhere to walk. Rollback component
/// (`Ecs::wander`); the choice itself is a stateless roll on the tick.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    state_hash::StateHash,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Wander {
    pub next_tick: u64,
    /// The world block column ([`crate::WorldBlockPos`]'s `[x, z]`) the
    /// person wanders around: where they stood when the component was first
    /// set (so repeated wanders do not drift away). World, not region-local,
    /// so it means the same place on both sides of a seam.
    pub home: [i64; 2],
}

impl Rollback {
    /// Give `person` a body at region-local `xz` (voxel units), standing on
    /// the terrain. `None`, with no change, if they already have one here.
    pub fn spawn_person_safe(&mut self, person: PersonId, xz: (f32, f32)) -> Option<EntityKey> {
        if self.data.person_entities.contains_key(&person) {
            return None;
        }
        let y = self.player_spawn_y(xz);
        let e = self.data.ecs.create_entity_safe();
        self.assign_entity_id_safe(e);
        let pose = Pose::from_translation(Vector::new(xz.0, y, xz.1));
        self.build_kinematic_body_safe(
            e,
            pose,
            &ColliderSpec::CapsuleY {
                half_height: PLAYER_CAPSULE_HALF_HEIGHT,
                radius: PLAYER_CAPSULE_RADIUS,
            },
        );
        let kind = EntityKind::Person { person };
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(e, Some(kind)),
        ));
        self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetEntityKind(e, None),
        ));
        self.data.ecs.kind.set_safe(e, Some(kind));
        self.data
            .ecs
            .motion
            .set_safe(e, Some(crate::Motion::default()));
        self.attach_rig_safe(
            e,
            crate::anim::Animator::idle(crate::anim::RigId::Dwarf),
            -(PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS),
        );
        self.data.person_entities.insert(person, e);
        Some(e)
    }

    /// Remove `person`'s body from this region. `false` if they had none.
    pub fn despawn_person_safe(&mut self, person: PersonId) -> bool {
        let Some(&e) = self.data.person_entities.get(&person) else {
            return false;
        };
        self.data.person_entities.remove(&person);
        self.remove_entity_safe(e, None);
        true
    }

    /// Applies a server-authored [`PersonSpawn`]: the body at the resolved
    /// column, then the optional walk. No change if they already have a body.
    pub fn apply_person_spawn_safe(
        &mut self,
        spawn: PersonSpawn,
        here: RegionCoords,
        map: Option<&RouteMap>,
    ) {
        let n = REGION_BLOCKS as i32;
        let landing = [
            (crate::PLAYER_SPAWN_X as i32 / crate::BLOCK_VOXELS as i32).clamp(0, n - 1),
            (crate::PLAYER_SPAWN_Z as i32 / crate::BLOCK_VOXELS as i32).clamp(0, n - 1),
        ];
        let column = match spawn.at {
            SpawnAt::Column([x, z]) => [(x as i32).clamp(0, n - 1), (z as i32).clamp(0, n - 1)],
            SpawnAt::NearPerson(other) => self.near_person_column(other).unwrap_or(landing),
            SpawnAt::Edge {
                from,
                along_per_mille,
            } => {
                let along = (along_per_mille.min(1000) as i64 * (n as i64 - 1) / 1000) as i32;
                let axis = |d: i32, along: i32| match d.signum() {
                    -1 => 0,
                    1 => n - 1,
                    _ => along,
                };
                // The side `from` lies on (across a wrapped seam the short way).
                let (dx, dz) = crate::region_delta(map.map(RouteMap::extent), here, from);
                let c = [axis(dx, along), axis(dz, along)];
                nearest_standable(self, c, None)
                    .map(|t| [t[0], t[2]])
                    .unwrap_or(c)
            }
        };
        let xz = crate::block_column_xz([column[0] as u32, column[1] as u32]);
        let Some(e) = self.spawn_person_safe(spawn.person, xz) else {
            return;
        };
        if let Some(goal) = spawn.walk_to {
            self.walk_entity_safe(e, goal, here, map);
        }
    }

    /// The nearest standable block column within 2 blocks of `other`'s body,
    /// preferring one that is not their own.
    fn near_person_column(&self, other: PersonId) -> Option<[i32; 2]> {
        let e = self.person_entity(other)?;
        let at = current_tile(self, self.body_feet(e)?)?;
        let n = REGION_BLOCKS as i32;
        let mut offsets: Vec<(i32, i32)> = (-2..=2)
            .flat_map(|dx| (-2..=2).map(move |dz| (dx, dz)))
            .collect();
        offsets.sort_by_key(|&(dx, dz)| (dx == 0 && dz == 0, dx * dx + dz * dz, dx, dz));
        offsets.into_iter().find_map(|(dx, dz)| {
            let c = [at[0] + dx, at[2] + dz];
            if !(0..n).contains(&c[0]) || !(0..n).contains(&c[1]) {
                return None;
            }
            nearest_standable(self, c, Some(at[1])).map(|t| [t[0], t[2]])
        })
    }

    pub fn person_entity(&self, person: PersonId) -> Option<EntityKey> {
        self.data.person_entities.get(&person).copied()
    }

    /// Every embodied person with their region-local body translation, in
    /// id order.
    pub fn people_here(&self) -> Vec<(PersonId, [f32; 3])> {
        self.data
            .person_entities
            .iter()
            .filter_map(|(&p, &e)| {
                let h = (*self.data.ecs.rigidbody.try_get(e))?;
                let t = self.data.physics.bodies.get(h)?.translation();
                Some((p, [t.x, t.y, t.z]))
            })
            .collect()
    }

    /// Walks `person` out of this region toward region `toward` (a
    /// neighbour), the shortest way: to the column of `toward` just across
    /// the facing edge from them, which the walk reaches through the halo
    /// (`path_plan.rs`). If that column cannot be reached, a walk toward
    /// `toward`'s centre is tried. `false` with no change if they have no
    /// body here or no path either way.
    pub fn walk_out_safe(
        &mut self,
        person: PersonId,
        toward: RegionCoords,
        here: RegionCoords,
        map: Option<&RouteMap>,
    ) -> bool {
        let Some(e) = self.person_entity(person) else {
            return false;
        };
        let n = REGION_BLOCKS as i32;
        if let Some(from) = self.body_feet(e).and_then(|feet| current_tile(self, feet)) {
            let across = |d: i32, own: i32| match d.signum() {
                1 => 0,
                -1 => n - 1,
                _ => own,
            };
            // The side `toward` lies on (across a wrapped seam the short way).
            let (dx, dz) = crate::region_delta(map.map(RouteMap::extent), here, toward);
            let column = [across(dx, from[0]), across(dz, from[2])];
            if self.walk_entity_safe(e, Goal::local(toward, column, None), here, map) {
                return true;
            }
        }
        let c = n / 2;
        self.walk_entity_safe(e, Goal::local(toward, [c, c], None), here, map)
    }

    /// Once per region tick, after `tick_walkers_safe` and before the physics
    /// step: idle people pick a wander goal when due, and every embodied
    /// person is steered along its walk (or held to gravity) through the
    /// same planner and character controller as a player's body.
    pub fn tick_people_safe(&mut self, here: RegionCoords, map: Option<&RouteMap>) {
        let tick = *self.data.tick;
        let people: Vec<(PersonId, EntityKey)> = self
            .data
            .person_entities
            .iter()
            .map(|(&p, &e)| (p, e))
            .collect();
        let pushes = self.separation_pushes(&people);
        // People's sweeps pass through each other (separation keeps them apart).
        let bodies: std::collections::BTreeSet<rapier3d::prelude::RigidBodyHandle> = people
            .iter()
            .filter_map(|&(_, e)| *self.data.ecs.rigidbody.try_get(e))
            .collect();
        for ((person, e), push) in people.into_iter().zip(pushes) {
            if self.data.ecs.walker.try_get(e).is_none() {
                self.wander_person_safe(person, e, tick, here, map);
            }
            self.steer_person_safe(person, e, tick, push, &bodies);
        }
    }

    /// Soft separation between people: for each of `people` (in order), the
    /// horizontal push (voxel units) away from every other person whose
    /// capsule overlaps theirs, [`PERSON_SEPARATION_SHARE`] of the overlap,
    /// capped at [`PERSON_SEPARATION_MAX_STEP_M`] a tick. Overlaps are allowed
    /// and sort themselves out over a few ticks. All pushes come from the
    /// positions at the start of the pass, so the order applied in changes
    /// nothing.
    fn separation_pushes(&self, people: &[(PersonId, EntityKey)]) -> Vec<Vector> {
        let feet: Vec<Option<Vector>> = people.iter().map(|&(_, e)| self.body_feet(e)).collect();
        let reach = 2.0 * PLAYER_CAPSULE_RADIUS;
        let tall = 2.0 * (PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS);
        let max_step = PERSON_SEPARATION_MAX_STEP_M * movement::UNITS_PER_METER;
        let mut pushes = vec![Vector::ZERO; people.len()];
        for i in 0..people.len() {
            let Some(a) = feet[i] else { continue };
            for j in i + 1..people.len() {
                let Some(b) = feet[j] else { continue };
                let (dx, dz) = (a.x - b.x, a.z - b.z);
                let d2 = dx * dx + dz * dz;
                if d2 >= reach * reach || (a.y - b.y).abs() >= tall {
                    continue;
                }
                let d = libm::sqrtf(d2);
                // Standing on the same spot: the lower id steps -x, the higher +x.
                let (ux, uz) = if d > 1e-3 {
                    (dx / d, dz / d)
                } else {
                    (-1.0, 0.0)
                };
                let push = Vector::new(ux, 0.0, uz) * ((reach - d) * PERSON_SEPARATION_SHARE * 0.5);
                pushes[i] += push;
                pushes[j] -= push;
            }
        }
        for p in &mut pushes {
            let len = libm::sqrtf(p.x * p.x + p.z * p.z);
            if len > max_step {
                *p *= max_step / len;
            }
        }
        pushes
    }

    fn wander_person_safe(
        &mut self,
        person: PersonId,
        e: EntityKey,
        tick: u64,
        here: RegionCoords,
        map: Option<&RouteMap>,
    ) {
        let wait = |t: u64| {
            WANDER_MIN_TICKS
                + crate::slime::roll(t, person.0 as u64, SALT_WANDER_WAIT) % WANDER_RANGE_TICKS
        };
        let Some(feet) = self.body_feet(e) else {
            return;
        };
        let Some(from) = current_tile(self, feet) else {
            return;
        };
        let home = match *self.data.ecs.wander.try_get(e) {
            Some(w) if tick < w.next_tick => return,
            Some(w) => w.home,
            None => {
                let w = self.data.world_block(from);
                let home = [w.x, w.z];
                self.data.ecs.wander.set_safe(
                    e,
                    Some(Wander {
                        next_tick: tick + wait(tick),
                        home,
                    }),
                );
                return;
            }
        };
        self.data.ecs.wander.set_safe(
            e,
            Some(Wander {
                next_tick: tick + wait(tick),
                home,
            }),
        );
        let r = crate::slime::roll(tick, person.0 as u64, SALT_WANDER_GOAL);
        let span = 2 * WANDER_RADIUS_BLOCKS as u64 + 1;
        let dx = (r % span) as i64 - WANDER_RADIUS_BLOCKS as i64;
        let dz = ((r >> 24) % span) as i64 - WANDER_RADIUS_BLOCKS as i64;
        // Anywhere around home, this side of a seam or the other: the walk
        // plans across seams (`PathPlan`), and finds the goal's standable
        // tile nearest the person's own level or gives up.
        let goal = Goal {
            column: [home[0] + dx, home[1] + dz],
            y: Some(from[1]),
        };
        self.walk_entity_within_safe(e, goal, here, map, WANDER_PATH_NODES);
    }

    fn steer_person_safe(
        &mut self,
        person: PersonId,
        e: EntityKey,
        tick: u64,
        push: Vector,
        people: &std::collections::BTreeSet<rapier3d::prelude::RigidBodyHandle>,
    ) {
        let Some(handle) = *self.data.ecs.rigidbody.try_get(e) else {
            return;
        };
        let Some(body) = self.data.physics.bodies.get(handle) else {
            return;
        };
        let t = body.translation();
        let rot0 = body.position().rotation;
        let motion0 = (*self.data.ecs.motion.try_get(e)).unwrap_or_default();
        let (keys, yaw) = match self.data.ecs.walker.try_get(e) {
            Some(w) => steer(
                w,
                feet_of(t),
                motion0.grounded,
                *self.data.region,
                self.data.map(),
            ),
            None => (MoveKeys::default(), 0.0),
        };
        let walking = self.data.ecs.walker.try_get(e).is_some();
        // A standing person on the ground makes no move at all, skipping the
        // character controller — it checks its footing every
        // IDLE_RESETTLE_TICKS (staggered by person), and only a person whose
        // tile is no longer standable (its ground dug away) falls.
        if !walking && motion0.grounded {
            // Shoved aside by a person standing in it (`separation_pushes`).
            if push != Vector::ZERO && self.can_shove(feet_of(t), push) {
                self.data
                    .physics
                    .bodies
                    .mutate_body_safe(handle, move |b| b.set_next_kinematic_translation(t + push));
                return;
            }
            let due = (tick + person.0 as u64) % IDLE_RESETTLE_TICKS == 0;
            if !due || crate::standable(self, crate::feet_tile(feet_of(t))) {
                return;
            }
        }
        if walking && self.follow_path_safe(e, handle, t, yaw, motion0, push) {
            return;
        }
        let mut motion = motion0;
        let keep = |_: rapier3d::prelude::ColliderHandle, c: &rapier3d::prelude::Collider| {
            c.parent().is_none_or(|b| !people.contains(&b))
        };
        let corrected = movement::walk_body_keeping(
            &mut motion,
            &keys,
            yaw,
            tick,
            &self.data.physics.broad_phase,
            &self.data.physics.narrow_phase,
            &self.data.physics.bodies,
            &self.data.physics.colliders,
            handle,
            &keep,
        );
        if motion != motion0 {
            self.data.ecs.motion.set_safe(e, Some(motion));
        }
        // Face the way the walk heads.
        let rot = walking.then(|| crate::yaw_quat(yaw)).filter(|r| *r != rot0);
        if corrected != Vector::ZERO || rot.is_some() {
            self.data.physics.bodies.mutate_body_safe(handle, move |b| {
                if corrected != Vector::ZERO {
                    b.set_next_kinematic_translation(t + corrected);
                }
                if let Some(r) = rot {
                    b.set_next_kinematic_rotation(r);
                }
            });
        }
    }

    /// Moves walking `e` one tick straight along its planned tiles at walk
    /// speed, with no collision sweep: the plan already keeps it to standable
    /// tiles, and the sweep is most of a walker's cost. Its feet ease to the
    /// next tile's level (where the controller would hold them, a skin above
    /// the floor). `false`, with no change, when the character controller must
    /// walk it instead: a player body near it or its next tile (it must not
    /// walk into a player), that tile no longer standable (dug, built over —
    /// the controller collides, the walker stalls and re-plans), the body
    /// airborne or in water.
    fn follow_path_safe(
        &mut self,
        e: EntityKey,
        handle: rapier3d::prelude::RigidBodyHandle,
        t: Vector,
        yaw: f32,
        motion0: crate::Motion,
        push: Vector,
    ) -> bool {
        if !motion0.grounded || motion0.flying || motion0.immersion != movement::Immersion::Dry {
            return false;
        }
        let Some(wp) = self
            .data
            .ecs
            .walker
            .try_get(e)
            .as_ref()
            .and_then(|w| w.waypoint(w.next, *self.data.region, self.data.map()))
        else {
            return false;
        };
        if !crate::standable(self, wp) {
            return false;
        }
        let bv = crate::BLOCK_VOXELS as f32;
        let feet = feet_of(t);
        let target = Vector::new(
            (wp[0] as f32 + 0.5) * bv,
            wp[1] as f32 * bv + movement::CHARACTER_SKIN,
            (wp[2] as f32 + 0.5) * bv,
        );
        if self.player_near(&[feet, target]) {
            return false;
        }
        let step = movement::WALK_SPEED * movement::UNITS_PER_METER * movement::TICK_DT;
        let (dx, dz) = (target.x - feet.x, target.z - feet.z);
        let d = libm::sqrtf(dx * dx + dz * dz);
        let k = if d > step { step / d } else { 1.0 };
        let climb = PATH_FOLLOW_CLIMB_RATE * step;
        let mut moved = Vector::new(dx * k, (target.y - feet.y).clamp(-climb, climb), dz * k);
        if push != Vector::ZERO && self.can_shove(feet + moved, push) {
            moved += push;
        }
        let motion = crate::Motion {
            vel_y: 0.0,
            ..motion0
        };
        if motion != motion0 {
            self.data.ecs.motion.set_safe(e, Some(motion));
        }
        let rot = crate::yaw_quat(yaw);
        self.data.physics.bodies.mutate_body_safe(handle, move |b| {
            b.set_next_kinematic_translation(t + moved);
            b.set_next_kinematic_rotation(rot);
        });
        true
    }

    /// Whether feet at `feet` may be shoved by `push`: the capsule's centre and
    /// its four horizontal extremes all land over standable tiles (never into a
    /// wall or over a ledge), with no player near (a player is collided with,
    /// never shoved into).
    fn can_shove(&self, feet: Vector, push: Vector) -> bool {
        let to = feet + push;
        let r = PLAYER_CAPSULE_RADIUS;
        [
            Vector::ZERO,
            Vector::X * r,
            Vector::X * -r,
            Vector::Z * r,
            Vector::Z * -r,
        ]
        .iter()
        .all(|o| crate::standable(self, crate::feet_tile(to + *o)))
            && !self.player_near(&[to])
    }

    /// Whether any player body in this region is within
    /// [`PATH_FOLLOW_PLAYER_CLEARANCE_BLOCKS`] (horizontally) and
    /// [`PATH_FOLLOW_PLAYER_CLEARANCE_Y_BLOCKS`] (vertically) of any of `points`
    /// (feet positions, voxel units).
    fn player_near(&self, points: &[Vector]) -> bool {
        let bv = crate::BLOCK_VOXELS as f32;
        let r = PATH_FOLLOW_PLAYER_CLEARANCE_BLOCKS * bv;
        let ry = PATH_FOLLOW_PLAYER_CLEARANCE_Y_BLOCKS * bv;
        self.data.player_entites.values().any(|&p| {
            let Some(pf) = self.body_feet(p) else {
                return false;
            };
            points.iter().any(|q| {
                let (dx, dz) = (pf.x - q.x, pf.z - q.z);
                dx * dx + dz * dz <= r * r && (pf.y - q.y).abs() <= ry
            })
        })
    }
}
