//! The world's people as bodies in a running region: a [`EntityKind::Person`]
//! entity is a dwarf-rigged kinematic capsule exactly like a player's, but
//! with no camera and no client. `GameData::person_entities` maps each
//! history `PersonId` to its body here; seam crossing keeps it in step
//! (`extract_entity_safe` / `apply_arrival`).

use crate::history::PersonId;
use crate::movement::{PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS};
use crate::{
    ColliderSpec, EntityKey, EntityKind, GameDataTransactionKind, GameDataUpdate,
    GameDataUpdateKind, Pose, Rollback,
};
use rapier3d::math::Vector;

impl Rollback {
    /// Give `person` a body at region-local `xz` (voxel units), standing on
    /// the terrain. `None`, with no change, if they already have one here.
    pub fn spawn_person_safe(&mut self, person: PersonId, xz: (f32, f32)) -> Option<EntityKey> {
        if self.data.person_entities.contains_key(&person) {
            return None;
        }
        let y = self.player_spawn_y(xz);
        let e = self.data.ecs.create_entity_safe();
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
        self.data.ecs.motion.set_safe(e, Some(crate::Motion::default()));
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
        let Some(&e) = self.data.person_entities.get(&person) else { return false };
        self.data.person_entities.remove(&person);
        self.remove_entity_safe(e, None);
        true
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
}
