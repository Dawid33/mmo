use std::{
    cell::{Ref, RefCell, RefMut},
    rc::Rc,
};

use borrow::PartialHelper;
use crossbeam::channel::Sender;
#[allow(unused)]
use log::info;
use rapier3d::{
    na::Vector3,
    prelude::{RigidBody, RigidBodyBuilder, RigidBodyHandle},
};
use serde::{Deserialize, Serialize};
use slotmapd::{basic::Iter, Key, SlotMap};

use crate::{
    data::{Camera, EntityKey, GameData, Player, PlayerKey},
    ClientUpdateEvent, GameDataUpdate, RegionId, WinitEvent,
};

#[derive(Copy, Clone, Debug)]
pub enum GameDataTransactionKind {
    Do,
    Undo,
}

#[allow(unused)]
pub struct GameDataTransaction<'a> {
    id: RegionId,
    pub data: &'a mut GameData,
    event_log: Vec<Box<dyn Fn(&mut GameData)>>,
}

macro_rules! undo {
    ($s:ident, $x:expr) => {
        $s.undo_func(Box::new($x));
    };
}
pub(crate) use undo;

impl<'a> GameDataTransaction<'a> {
    pub fn new(data: &'a mut GameData, id: RegionId) -> Self {
        Self {
            data,
            event_log: Vec::new(),
            id,
        }
    }

    pub fn raw(&self) -> &GameData {
        self.data
    }

    pub fn undo_func(&mut self, undo: Box<dyn Fn(&mut GameData)>) {
        self.event_log.push(Box::new(undo));
    }

    #[allow(unused)]
    fn client(&mut self, e: GameDataUpdate, kind: GameDataTransactionKind) {
        // self.client.as_ref().inspect(|c| {
        //     c.send(ClientUpdateEvent::UpdateRegion(self.id, e, kind))
        //         .unwrap()
        // });
    }

    pub fn tick(&mut self) {
        // self.data.tick.set(0.into());
        // self.data.tick += 1;
        // undo!(self, |data| {
        //     data.tick -= 1;
        // });
    }

    pub fn count(&self) -> usize {
        // self.data.ecs.camera.len()
        0
    }

    pub fn add(&mut self) {
        // self.data.ecs.add()
    }

    pub fn set_camera(&mut self, key: EntityKey, camera: Camera) {
        // let old = self.data.ecs.camera.set(key, Some(camera.clone()));
        // undo!(self, move |data| {
        //     data.ecs.camera.set(key, old);
        // });
    }

    pub fn set_player(&mut self, _key: EntityKey, _player: Player) {
        // let old = self.data.ecs.set(key, Some(player.clone()));
        // undo!(self, move |data| {
        //     data.ecs.set(key, old.clone());
        // });
    }

    // pub fn get_body(&self, key: EntityKey) -> &RigidBody {
    //     let handle = self.data.ecs.rigidbody.get(key);
    //     self.data.physics.bodies.get(*handle).unwrap()
    // }

    pub fn set_linvel(&self, _key: EntityKey, _linvel: Vector3<f32>) {
        // let handle = self.data.ecs.get_body(key);
        // let body = self.data.physics.bodies.get_mut(*handle).unwrap()
        // let old = body.linvel().clone();
        // undo!(self, {
        // });
    }

    // pub fn get_body_handle(&self, key: EntityKey) -> &RigidBodyHandle {
    //     self.data.ecs.rigidbody.get(key)
    // }

    pub fn set_body_handle(&mut self, _key: EntityKey, _body: RigidBodyHandle) {
        // let old = self.data.ecs.rigidbody.set(key, Some(body.clone()));
        // undo!(self, move |data| {
        //     data.ecs.set_body(key, old);
        // });
    }

    // pub fn set_body(&mut self, key: RigidBodyHandle, new_body: RigidBody) {
    //     let body = self.data.physics.bodies.get_mut(key).unwrap();
    //     let old = body.clone();
    //     *body = new_body;
    //     undo!(self, move |data| {
    //         *data.physics.bodies.get_mut(key).unwrap() = old.clone();
    //     });
    // }

    pub fn update_player_input(&mut self, player: PlayerKey, _event: &WinitEvent) {
        // let _key = self.data.players.get(player).unwrap();
        // let player = self.data.ecs.get_player_mut(*key);
        // let old = (*player).clone();
        // let key = key.clone();
        // player.input.update(event);
        // undo!(self, move |data| {
        //     data.ecs.set(key, Some(old.clone()));
        // });
    }

    // pub fn get_player(&self, player: PlayerKey) -> &Player {
    //     let key = self.data.players.get(player).unwrap();
    //     self.data.ecs.player.get(*key)
    // }

    // // pub fn builder(&'a mut self) -> {
    // //     let key = self.data.ecs.add();
    //     return EntityBuilder::new(self, key);

    // let body = RigidBodyBuilder::kinematic_velocity_based()
    //     .gravity_scale(0.0)
    //     .can_sleep(true)
    //     .ccd_enabled(false)
    //     .user_data(index.try_into().unwrap())
    //     .build();
    // undo!(self, move |d| {
    //     d.data.undo().remove_entity(index);
    // });
    // }

    // pub fn update_input(&mut self, player: usize, event: WinitEvent) -> WinitInputHelper {
    //     self.data.raw.input.get_mut(&player).unwrap().update(&event)
    // }

    // pub fn set_input(&mut self, player: usize, state: WinitInputHelper) {
    //     *self.data.raw.input.get_mut(&player).unwrap() = state;
    // }

    // pub fn set_entity_type(&mut self, e_id: EntityId, new: EntityType) -> EntityType {
    //     let e = self.data.raw.entities.get_mut(e_id).unwrap();
    //     let old = e.kind.clone();
    //     e.kind = new.clone();
    //     self.send(UpdateGameData::SetEntityType(e_id, new));
    //     return old;
    // }

    // pub fn set_entity_render_isometry(&mut self, cam_id: usize, transform: IsometryReal) {
    //     self.data
    //         .raw
    //         .entities
    //         .get_mut(cam_id)
    //         .unwrap()
    //         .physics_isometry = transform;
    //     self.send(UpdateGameData::SetEntityRenderTransform(cam_id, transform));
    // }

    // pub fn insert_rigid_body(&mut self, b: RigidBody) -> RigidBodyHandle {
    //     let handle = self.data.physics.bodies.insert(b.clone());
    //     handle
    // }

    // pub fn remove_rigid_body(&mut self, b: RigidBodyHandle) {
    //     self.data.physics.bodies.remove(
    //         b,
    //         &mut self.data.physics.islands,
    //         &mut self.data.physics.colliders,
    //         &mut self.data.physics.implules_joint_set,
    //         &mut self.data.physics.multi_body_joint_set,
    //         false,
    //     );
    //     self.change(UpdateGameData::RemoveRigidBody(b));
    // }

    // pub fn create_entity(&mut self, e: Entity) -> EntityId {
    //     let index = self.data.raw.entities.len();
    //     self.data.raw.entities.insert(index, e.clone());
    //     self.send(UpdateGameData::CreateEntity(e));
    //     index
    // }

    // pub fn remove_entity(&mut self, e: EntityId) {
    //     self.data.raw.entities.remove(e);
    //     self.send(UpdateGameData::RemoveEntity(e));
    // }

    // pub fn update_camera_proj_matrix(&mut self, e: EntityId) -> Matrix4<f32> {
    //     let cam = self
    //         .data
    //         .raw
    //         .entities
    //         .get_mut(e)
    //         .unwrap()
    //         .kind
    //         .as_camera_mut();
    //     let old = cam.proj_matrix.clone();
    //     cam.update_camera_proj_matrix();
    //     return old;
    // }

    // pub fn set_velocity(
    //     &mut self,
    //     e: EntityId,
    //     vel: Vector<rapier3d::math::Real>,
    // ) -> Vector<rapier3d::math::Real> {
    //     let entity = self.data.raw.entities.get_mut(e).unwrap();
    //     let body = self
    //         .data
    //         .raw
    //         .physics
    //         .bodies
    //         .get_mut(entity.handle.unwrap())
    //         .unwrap();
    //     let old = body.linvel().clone();
    //     body.set_linvel(vel, true);
    //     return old;
    // }

    // pub fn set_ang_velocity(
    //     &mut self,
    //     e: EntityId,
    //     ang: Vector<f32>,
    // ) -> Vector<rapier3d::math::Real> {
    //     let camera = self.data.raw.entities.get_mut(e).unwrap();
    //     let body = self
    //         .data
    //         .raw
    //         .physics
    //         .bodies
    //         .get_mut(camera.handle.unwrap())
    //         .unwrap();
    //     let old = body.angvel().clone();
    //     body.set_angvel(ang, true);
    //     return old;
    // }

    // pub fn set_isometry(&mut self, e: EntityId, isometry: IsometryReal) {
    //     let entity = self.data.raw.entities.get_mut(e).unwrap();
    //     let body = self
    //         .data
    //         .raw
    //         .physics
    //         .bodies
    //         .get_mut(entity.handle.unwrap())
    //         .unwrap();
    //     body.set_position(isometry, true);
    //     self.send(UpdateGameData::SetEntityPosition(e, isometry));
    // }

    // // No need to update render thread because physics doesn't concern it.
    // pub fn set_rigid_body_handle(&mut self, e: EntityId, r: RigidBody) {
    //     let handle = self.data.raw.physics.bodies.insert(r);
    //     let e = self.data.raw.entities.get_mut(e).unwrap();
    //     e.handle = Some(handle);
    // }
}

// pub struct Transaction<'a> {
//     event: GameEvent,
//     pub event_log: &'a mut VecDeque<(GameEvent, Box<dyn Fn(&mut GameData)>)>,
//     pub r: &'a mut GameData,
// }

// macro_rules! undo {
//     ($s:ident, $x:expr) => {
//         $s.undo(Box::new($x));
//     };
// }

// impl<'a> Transaction<'a> {
//     pub fn new(
//         event: GameEvent,
//         event_log: &'a mut VecDeque<(GameEvent, Box<dyn Fn(&mut GameData)>)>,
//         r: &'a mut GameData,
//     ) -> Self {
//         Self {
//             event,
//             event_log,
//             r,
//         }
//     }

//     fn undo(&mut self, undo: Box<dyn Fn(&mut GameData)>) {
//         self.event_log
//             .push_back((self.event.clone(), Box::new(undo)));
//     }

//     // pub fn get_body(&self, e: EntityId) -> Option<&RigidBody> {
//     //     let entity = self.r.data.raw().entities.get(e)?;
//     //     Some(self.r.data.raw().physics.bodies.get(entity.handle?)?)
//     // }

//     // pub fn _get_isometry(&self, e: EntityId) -> Option<&IsometryReal> {
//     //     self.r.data.get_isometry(e)
//     // }

//     // pub fn get_input(&self, player_id: &usize) -> &WinitInputHelper {
//     //     self.r.data.raw().input.get(player_id).unwrap()
//     // }

//     // pub fn get_camera_id(&self) -> Option<usize> {
//     //     let mut cam = None;
//     //     for (i, item) in self.r.data.raw().entities.iter().enumerate() {
//     //         match &item.kind {
//     //             EntityType::Camera(_) => {
//     //                 cam = Some(i);
//     //                 break;
//     //             }
//     //             _ => (),
//     //         }
//     //     }
//     //     cam
//     // }

//     // pub fn get_camera(&mut self) -> &Camera {
//     //     let id = self.get_camera_id().unwrap();
//     //     match &self.r.data.raw().entities.get(id).unwrap().kind {
//     //         EntityType::Camera(camera) => camera,
//     //         _ => panic!("Tried to get camera but entity was wrong type."),
//     //     }
//     // }

//     // pub fn set_entity_velocity(&mut self, e: EntityId, v: Vector<rapier3d::math::Real>) {
//     //     let old = self.r.data.change().set_velocity(e, v);
//     //     undo!(self, move |r| {
//     //         r.data.change().set_velocity(e, old);
//     //     });
//     // }

//     // pub fn set_entity_angular_velocity(&mut self, e: EntityId, v: Vector<rapier3d::math::Real>) {
//     //     let old = self.r.data.change().set_ang_velocity(e, v);
//     //     undo!(self, move |r| {
//     //         r.data.change().set_ang_velocity(e, old);
//     //     });
//     // }

//     // pub fn tick(&mut self) {
//     //     let tick = self.r.data.raw().tick;
//     //     self.r.data.change().set_tick(tick + 1);
//     //     undo!(self, move |d| {
//     //         d.data.undo().set_tick(tick);
//     //     });
//     // }

//     // pub fn update_physics(&mut self) {
//     //     let state = self
//     //         .r
//     //         .data
//     //         .change()
//     //         .update_physics(&mut self.r.physics_pipeline);
//     //     undo!(self, move |d| {
//     //         d.data.undo().set_physics_state(state.clone());
//     //     });

//     //     let moving: Vec<(usize, IsometryReal)> = self
//     //         .r
//     //         .data
//     //         .raw()
//     //         .physics
//     //         .islands
//     //         .active_kinematic_bodies()
//     //         .iter()
//     //         .filter_map(|k| {
//     //             if self.r.data.raw().physics.bodies[*k].is_moving() {
//     //                 let entity_id = self.r.data.raw().physics.bodies[*k].user_data as usize;
//     //                 let new_iso = self.r.data.raw().physics.bodies[*k].position();
//     //                 Some((entity_id, *new_iso))
//     //             } else {
//     //                 None
//     //             }
//     //         })
//     //         .collect();

//     //     for (id, iso) in moving {
//     //         // match self.r.data.raw().entities.get(id).unwrap().kind.clone() {
//     //         //     EntityType::Camera(camera) => {
//     //         //         self.r
//     //         //             .data
//     //         //             .change()
//     //         //             .set_entity_type(id, EntityType::Camera(camera));
//     //         //     }
//     //         //     _ => {}
//     //         // }
//     //         // self.r.data.change().set_entity_render_isometry(id, iso);
//     //     }
//     // }

//     // pub fn update_input(&mut self, player: usize, event: WinitEvent) {
//     //     let old_state = self.r.data.change().update_input(player, event);
//     //     undo!(self, move |d| {
//     //         d.data.undo().set_input(player, old_state.clone());
//     //     });
//     // }

//     pub fn _create_text_entity(&mut self, text: &str) {
//         // let layout = text_layout(&mut self.r.font_context, text);
//         // let mut e = Entity::default();
//         // e.kind = EntityType::Text {
//         //     content: text.to_string(),
//         // };
//         // let index = self.r.data.change().create_entity(e);
//         // self.r.text_layouts.insert(index, layout);
//         // undo!(self, move |d| {
//         //     d.text_layouts.remove(&index).unwrap();
//         //     d.data.undo().remove_entity(index);
//         // });
//     }

//     // pub fn _add_entity(&mut self, _e: Entity) {
//     // let index = self.r.data.entities.len();
//     // self.r.data.entities.insert(index, e);
//     // self.event_log.push_back((
//     //     self.event,
//     //     Box::new(move |r| {
//     //         r.data.entities.remove(index);
//     //     }),
//     // ));
//     // }
// }
