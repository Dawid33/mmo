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
    /// A voxel body (`voxel_body.rs`): its model, settle state and policy,
    /// with its `drops` moved out into [`Carried::drops`] (empty here). Its
    /// region-local `origin` is rebased with the body (`EntityBundle::rebase`).
    pub voxel_body: Option<crate::voxel_body::VoxelBody>,
    /// What the entity leaves behind as item piles if it is destroyed on
    /// its way (the manager's mailbox overflow, `WorldManager::hold_arrival`):
    /// it must never vanish with them. A `DropItems` voxel body's
    /// `VoxelBody::drops` (or a stud body's), put back into it on arrival.
    pub drops: Vec<crate::ItemStack>,
    /// A stud body (`stud_body.rs`): its bricks, settle state and policy, with
    /// its `drops` moved out into [`Carried::drops`] (empty here). Nothing in
    /// it is region-local. Appended: [`CarriedV1`] is the layout before it.
    pub stud_body: Option<crate::stud_body::StudBody>,
}

/// [`Carried`] as stored before stud bodies (world-store entity bundle
/// format 1): every field but `stud_body`, in the same order. Decode-only.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CarriedV1 {
    pub camera: Option<(f32, f32)>,
    pub motion: Option<crate::Motion>,
    pub slime: Option<crate::slime::Slime>,
    pub animator: Option<crate::anim::Animator>,
    pub walker: Option<crate::Walker>,
    pub wander: Option<crate::people::Wander>,
    pub voxel_body: Option<crate::voxel_body::VoxelBody>,
    pub drops: Vec<crate::ItemStack>,
}

impl From<CarriedV1> for Carried {
    fn from(c: CarriedV1) -> Self {
        let CarriedV1 { camera, motion, slime, animator, walker, wander, voxel_body, drops } = c;
        Carried { camera, motion, slime, animator, walker, wander, voxel_body, drops, stud_body: None }
    }
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
            voxel_body,
            walker,
            wander,
            // Travels as `EntityBundle::id`.
            id: _,
            // Terrain only (stud world); never travels.
            stud: _,
            stud_body,
        } = &*self.data.ecs;
        let mut voxel_body = voxel_body.try_get(key).clone();
        let mut drops = voxel_body.as_mut().map(|vb| std::mem::take(&mut vb.drops)).unwrap_or_default();
        // An entity is one or the other, so the drops are one body's.
        let mut stud_body = stud_body.try_get(key).clone();
        if let Some(sb) = stud_body.as_mut() {
            drops.append(&mut sb.drops);
        }
        Carried {
            camera: camera.try_get(key).as_ref().map(|c| (c.yaw, c.pitch)),
            motion: *motion.try_get(key),
            slime: *slime.try_get(key),
            animator: *animator.try_get(key),
            walker: walker.try_get(key).clone(),
            wander: *wander.try_get(key),
            voxel_body,
            drops,
            stud_body,
        }
    }

    /// Put `c` on `e` (overwriting what is there, so a replayed arrival
    /// re-applies the whole bundle). `client` is the player attached to `e`;
    /// only a player gets a camera.
    pub(crate) fn restore_components_safe(&mut self, e: EntityKey, c: &Carried, client: Option<ClientId>) {
        self.restore_voxel_body_safe(e, c);
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

    /// The carried voxel body back on `e`, with its drops: its collider's
    /// material set again (not in the `ColliderSpec`), and the renderer told
    /// its model the first time (a replay re-applies the state silently).
    /// Drops that arrive with no voxel body to hold them (no such entity
    /// today) are put down where `e` is rather than lost.
    fn restore_voxel_body_safe(&mut self, e: EntityKey, c: &Carried) {
        let handle = *self.data.ecs.rigidbody.try_get(e);
        let pose = handle.and_then(|h| self.data.physics.bodies.get(h)).map(|b| *b.position());
        if let Some(sb) = &c.stud_body {
            // A stud body: the same steps (its collider's material is the voxel
            // body's, and not in the `ColliderSpec` either).
            if let Some(handle) = handle {
                self.set_voxel_body_material_safe(handle);
            }
            if self.data.ecs.stud_body.try_get(e).is_none() {
                self.data.send(GameDataUpdate::new(
                    GameDataTransactionKind::Do,
                    GameDataUpdateKind::AddStudBody(e, pose.unwrap_or_else(rapier3d::math::Pose::identity), Box::new(sb.model.clone())),
                ));
            }
            let sb = crate::stud_body::StudBody { drops: c.drops.clone(), ..sb.clone() };
            self.data.ecs.stud_body.set_safe(e, Some(sb));
            return;
        }
        let Some(vb) = &c.voxel_body else {
            if !c.drops.is_empty() {
                log::error!("drops {:?} arrived on {e:?} with no voxel body to hold them: put down", c.drops);
                if let Some(pose) = pose {
                    let t = pose.translation;
                    let kind = self.terrain_kind();
                    let (bw, bh) = (kind.tile_width_units(), kind.tile_height_units());
                    let cell = [(t.x / bw).floor() as i32, (t.y / bh).floor() as i32, (t.z / bw).floor() as i32];
                    let tick = *self.data.tick;
                    self.deposit_drops_safe(cell, &c.drops, tick);
                }
            }
            return;
        };
        if let Some(handle) = handle {
            self.set_voxel_body_material_safe(handle);
        }
        if self.data.ecs.voxel_body.try_get(e).is_none() {
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::AddVoxelBody(e, pose.unwrap_or_else(rapier3d::math::Pose::identity), Box::new(vb.model.clone())),
            ));
        }
        let vb = crate::voxel_body::VoxelBody { drops: c.drops.clone(), ..vb.clone() };
        self.data.ecs.voxel_body.set_safe(e, Some(vb));
    }
}
