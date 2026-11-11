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
    /// it must never vanish with them. A `DropItems` stud body's
    /// `StudBody::drops`, put back into it on arrival.
    pub drops: Vec<crate::ItemStack>,
    /// A stud body (`stud_body.rs`): its bricks, settle state and policy, with
    /// its `drops` moved out into [`Carried::drops`] (empty here). Nothing in
    /// it is region-local.
    pub stud_body: Option<crate::stud_body::StudBody>,
}

/// The slot a stud body had in the stored layouts ([`CarriedV1`],
/// [`CarriedV2`]): always `None` in a stud world. A stored `Some` (a voxel
/// world's bundle — the store refuses those worlds at open) fails to decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum NoVoxelBody {}

impl<'de> serde::Deserialize<'de> for NoVoxelBody {
    fn deserialize<D: serde::Deserializer<'de>>(_: D) -> Result<Self, D::Error> {
        Err(serde::de::Error::custom("a stud body: voxel terrain is no longer supported"))
    }
}

/// [`Carried`] as stored with the stud body slot (world-store entity bundle
/// format 2): `voxel_body` before `drops`. Decode-only.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CarriedV2 {
    pub camera: Option<(f32, f32)>,
    pub motion: Option<crate::Motion>,
    pub slime: Option<crate::slime::Slime>,
    pub animator: Option<crate::anim::Animator>,
    pub walker: Option<crate::Walker>,
    pub wander: Option<crate::people::Wander>,
    pub voxel_body: Option<NoVoxelBody>,
    pub drops: Vec<crate::ItemStack>,
    pub stud_body: Option<crate::stud_body::StudBody>,
}

impl From<CarriedV2> for Carried {
    fn from(c: CarriedV2) -> Self {
        let CarriedV2 { camera, motion, slime, animator, walker, wander, voxel_body: _, drops, stud_body } = c;
        Carried { camera, motion, slime, animator, walker, wander, drops, stud_body }
    }
}

/// [`Carried`] as stored before stud bodies (world-store entity bundle
/// format 1): [`CarriedV2`] without `stud_body`. Decode-only.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CarriedV1 {
    pub camera: Option<(f32, f32)>,
    pub motion: Option<crate::Motion>,
    pub slime: Option<crate::slime::Slime>,
    pub animator: Option<crate::anim::Animator>,
    pub walker: Option<crate::Walker>,
    pub wander: Option<crate::people::Wander>,
    pub voxel_body: Option<NoVoxelBody>,
    pub drops: Vec<crate::ItemStack>,
}

impl From<CarriedV1> for Carried {
    fn from(c: CarriedV1) -> Self {
        let CarriedV1 { camera, motion, slime, animator, walker, wander, voxel_body: _, drops } = c;
        Carried { camera, motion, slime, animator, walker, wander, drops, stud_body: None }
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
            // Travels as `EntityBundle::kind`.
            kind: _,
            motion,
            slime,
            animator,
            // Rebuilt from the animator (`attach_rig_safe`).
            bones: _,
            walker,
            wander,
            // Travels as `EntityBundle::id`.
            id: _,
            // Terrain only (stud world); never travels.
            stud: _,
            stud_body,
        } = &*self.data.ecs;
        let mut stud_body = stud_body.try_get(key).clone();
        let drops = stud_body.as_mut().map(|sb| std::mem::take(&mut sb.drops)).unwrap_or_default();
        Carried {
            camera: camera.try_get(key).as_ref().map(|c| (c.yaw, c.pitch)),
            motion: *motion.try_get(key),
            slime: *slime.try_get(key),
            animator: *animator.try_get(key),
            walker: walker.try_get(key).clone(),
            wander: *wander.try_get(key),
            drops,
            stud_body,
        }
    }

    /// Put `c` on `e` (overwriting what is there, so a replayed arrival
    /// re-applies the whole bundle). `client` is the player attached to `e`;
    /// only a player gets a camera.
    pub(crate) fn restore_components_safe(&mut self, e: EntityKey, c: &Carried, client: Option<ClientId>) {
        self.restore_stud_body_safe(e, c);
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

    /// The carried stud body back on `e`, with its drops: its collider's
    /// material set again (not in the `ColliderSpec`), and the renderer told
    /// its model the first time (a replay re-applies the state silently).
    /// Drops that arrive with no stud body to hold them (no such entity
    /// today) are put down where `e` is rather than lost.
    fn restore_stud_body_safe(&mut self, e: EntityKey, c: &Carried) {
        let handle = *self.data.ecs.rigidbody.try_get(e);
        let pose = handle.and_then(|h| self.data.physics.bodies.get(h)).map(|b| *b.position());
        let Some(sb) = &c.stud_body else {
            if !c.drops.is_empty() {
                log::error!("drops {:?} arrived on {e:?} with no stud body to hold them: put down", c.drops);
                if let Some(pose) = pose {
                    let t = pose.translation;
                    let (bw, bh) = (crate::tile_width_units(), crate::tile_height_units());
                    let cell = [(t.x / bw).floor() as i32, (t.y / bh).floor() as i32, (t.z / bw).floor() as i32];
                    let tick = *self.data.tick;
                    self.deposit_drops_safe(cell, &c.drops, tick);
                }
            }
            return;
        };
        if let Some(handle) = handle {
            self.set_stud_body_material_safe(handle);
        }
        if self.data.ecs.stud_body.try_get(e).is_none() {
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::AddStudBody(e, pose.unwrap_or_else(rapier3d::math::Pose::identity), Box::new(sb.model.clone())),
            ));
        }
        let sb = crate::stud_body::StudBody { drops: c.drops.clone(), ..sb.clone() };
        self.data.ecs.stud_body.set_safe(e, Some(sb));
    }
}
