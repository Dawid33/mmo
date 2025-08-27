use std::collections::BTreeMap;

use game::{EntityId, GameData, UpdateGameData};
use rapier3d::na::Matrix4;

use crate::state::RenderWorld;

#[allow(unused)]
pub enum RenderEntityType {
    Camera {
        proj_matrix: Matrix4<f32>,
        view_matrix: Matrix4<f32>,
    },
    Default,
}

#[allow(unused)]
pub struct RenderEntity {
    kind: RenderEntityType,
}

#[allow(unused)]
pub struct TrueRenderWorld {
    entities: BTreeMap<EntityId, RenderEntity>,
}

impl TrueRenderWorld {
    #[allow(unused)]
    pub fn new(data: &GameData, state: &mut RenderWorld) -> Self {
        let entities = BTreeMap::new();
        // for (id, e) in data.raw().entities.iter().enumerate() {
        //     match &e.kind {
        //         game::EntityType::Camera(camera) => {

        //         }
        //         _ => (),
        //     }
        // }
        Self { entities }
    }
    #[allow(unused)]
    pub fn update(event: UpdateGameData, state: &mut RenderWorld) {
        //     match event {
        //         // UpdateGameData::CreateEntity(e) => {
        //             // let index = data.change().create_entity(e.clone());
        //             // match &e.kind {
        //             //     game::EntityType::Camera(c) => {
        //             //         self.render_info.create_camera(
        //             //             index,
        //             //             c.build_view_projection_matrix(&IsometryReal::identity()),
        //             //         );
        //             //     }
        //             //     _ => (),
        //             // }
        //         }
        //         UpdateGameData::RemoveEntity(i) => {
        //             // let e = data.raw().entities.get(i).unwrap();
        //             // match e.kind {
        //             //     EntityType::Camera(_) => {
        //             //         self.render_info.cameras.remove(&i);
        //             //     }
        //             //     _ => (),
        //             // }
        //             // data.change().remove_entity(i);
        //         }
        //         UpdateGameData::SetEntityRenderTransform(id, uniform) => {
        //             // data.change().set_entity_render_isometry(id, uniform);
        //         }
        //         UpdateGameData::SetEntityType(id, entity) => {
        //             // data.change().set_entity_type(id, entity);
        //             // self.render_info.lerp_set.insert(id);
        //         }
        //         UpdateGameData::AddRigidBody(body) => {
        //             // data.change().insert_rigid_body(body);
        //         }
        //         UpdateGameData::SetCameraUniform(id, uniform) => {
        //             // data.change().update_camera_uniform(id, uniform);
        //             // self.render_info.lerp_set.insert(id);
        //         }
        //         UpdateGameData::RemoveRigidBody(body) => {
        //             // data.change().remove_rigid_body(body);
        //         }
        //         UpdateGameData::SetTick(tick) => {
        //             // data.change().set_tick(tick);
        //         }
        //         UpdateGameData::SetEntityPosition(e, isometry) => {
        //             // data.change().set_isometry(e, isometry);
        //         }
        //         UpdateGameData::UpdateEntityIsometry(i, iso) => {
        //             // data.raw.entities.get_mut(i).unwrap().physics_isometry = iso;
        //             // self.render_info.lerp_set.insert(i);
        //         }
        //     }
    }
}
