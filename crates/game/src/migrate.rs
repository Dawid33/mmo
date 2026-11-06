//! What travels with an entity across a region seam, classified
//! exhaustively. `carry_components` destructures [`Ecs`] and
//! [`Client::arrive`] destructures [`Client`] with no `..`, so adding a
//! component or a client field fails to build until someone decides whether
//! it travels.

use crate::Ecs;
use crate::{Camera, Client, ClientId, EntityKey, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Rollback};

/// The ECS components that ride in an [`crate::EntityBundle`] besides the
/// body, kind and id.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Carried {
    /// Authoritative look scalars (yaw, pitch) of a camera-carrying body.
    pub camera: Option<(f32, f32)>,
    pub motion: Option<crate::Motion>,
    pub slime: Option<crate::slime::Slime>,
    pub animator: Option<crate::anim::Animator>,
    /// An adventurer's walk; its path is in world blocks, the receiver walks
    /// on along it (`Rollback::continue_walk_safe`).
    pub walker: Option<crate::Walker>,
    pub wander: Option<crate::people::Wander>,
    /// What the entity leaves behind as item piles if it is destroyed on
    /// its way (the manager's mailbox overflow, `WorldManager::hold_arrival`):
    /// it must never vanish with them. No component holds drops yet, so
    /// [`Rollback::carry_components`] sends none and an arrival must bring
    /// none; voxel bodies (`SettlePolicy::DropItems`) fill it once they
    /// cross seams.
    pub drops: Vec<crate::ItemStack>,
}

impl Client {
    /// The client as it enters a region from elsewhere (a seam arrival, a
    /// restore, a move): everything that only means something in the region
    /// it left is reset, everything else travels.
    pub fn arrive(self) -> Client {
        let Client {
            // Held keys and the first-person flag belong to the session and
            // are re-sent by the client; they travel unchanged.
            input,
            fps_cam_mode,
            mode,
            inventory,
            // An open inventory screen stays open, with its chest.
            inventory_open,
            selected,
            cursor,
            // Mining: a region-local target and a start tick of the old clock.
            mining: _,
            // An absolute tick of the old region's progress; none in the new.
            break_cooldown_until: _,
            // A world cell: the chest stays open across the seam; the new
            // region closes it if it is out of reach or unseen
            // (`Rollback::close_far_containers_safe`).
            open_container,
        } = self;
        Client {
            input,
            fps_cam_mode,
            mode,
            inventory,
            inventory_open,
            selected,
            cursor,
            mining: None,
            break_cooldown_until: 0,
            open_container,
        }
    }
}

impl Rollback {
    /// The components of `key` that travel with it.
    pub(crate) fn carry_components(&self, key: EntityKey) -> Carried {
        let Ecs {
            // The slotmap itself: the receiver allocates its own key.
            entities: _,
            camera,
            // Derived from the body (`BodyState::pose`) on arrival.
            isometry: _,
            // Rebuilt from `BodyState` + `ColliderSpec` on arrival.
            rigidbody: _,
            // Terrain only; crossing entities are never chunks.
            chunk: _,
            // Travels as `EntityBundle::kind`.
            kind: _,
            motion,
            slime,
            animator,
            // Rebuilt from the animator (`attach_rig_safe`).
            bones: _,
            // Voxel bodies never cross seams (despawned at the region edge).
            voxel_body: _,
            walker,
            wander,
            // Travels as `EntityBundle::id`.
            id: _,
        } = &*self.data.ecs;
        Carried {
            camera: camera.try_get(key).as_ref().map(|c| (c.yaw, c.pitch)),
            motion: *motion.try_get(key),
            slime: *slime.try_get(key),
            animator: *animator.try_get(key),
            walker: walker.try_get(key).clone(),
            wander: *wander.try_get(key),
            drops: Vec::new(),
        }
    }

    /// Put `c` on `e` (overwriting what is there, so a replayed arrival
    /// re-applies the whole bundle). `client` is the player attached to `e`;
    /// only a player gets a camera.
    pub(crate) fn restore_components_safe(&mut self, e: EntityKey, c: &Carried, client: Option<ClientId>) {
        debug_assert!(c.drops.is_empty(), "no entity holds drops yet: {:?} would be lost on arrival", c.drops);
        self.data.ecs.motion.set_safe(e, c.motion);
        self.data.ecs.slime.set_safe(e, c.slime);
        if let Some(animator) = c.animator {
            if self.data.ecs.bones.try_get(e).is_none() {
                self.attach_rig_safe(e, animator, animator.foot_y);
            } else {
                self.set_animator_safe(e, animator);
            }
        }
        self.data.ecs.walker.set_safe(e, c.walker.clone());
        self.data.ecs.wander.set_safe(e, c.wander);
        if let (Some(client_id), Some((yaw, pitch))) = (client, c.camera) {
            if let Some(mut cam) = self.data.ecs.camera.try_get(e).clone() {
                cam.yaw = yaw;
                cam.pitch = pitch;
                self.data.ecs.camera.set_safe(e, Some(cam));
            } else {
                let handle = self.data.ecs.rigidbody.try_get(e).expect("camera needs a body");
                let pose = *self.data.physics.bodies.get(handle).unwrap().position();
                let mut cam = Camera::new(handle);
                cam.yaw = yaw;
                cam.pitch = pitch;
                self.data.send(GameDataUpdate::new(
                    GameDataTransactionKind::Do,
                    GameDataUpdateKind::AddCameraComponent(e, client_id, cam.proj_matrix.clone(), pose),
                ));
                self.data.ecs.camera.emit_on_undo(GameDataUpdate::new(
                    GameDataTransactionKind::Undo,
                    GameDataUpdateKind::RemoveCameraComponent(e),
                ));
                self.data.ecs.camera.set_safe(e, Some(cam));
            }
        }
    }
}
