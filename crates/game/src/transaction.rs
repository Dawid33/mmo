use std::collections::VecDeque;

use crate::{
    common::{EntityId, GameEvent},
    region::text_layout,
    Camera, Entity, EntityType, RegionData,
};

pub struct Transaction<'a> {
    event: GameEvent,
    pub event_log: &'a mut VecDeque<(GameEvent, Box<dyn Fn(&mut RegionData)>)>,
    r: &'a mut RegionData,
}

macro_rules! undo {
    ($s:ident, $x:expr) => {
        $s.undo(Box::new($x));
    };
}

impl<'a> Transaction<'a> {
    pub fn new(
        event: GameEvent,
        event_log: &'a mut VecDeque<(GameEvent, Box<dyn Fn(&mut RegionData)>)>,
        r: &'a mut RegionData,
    ) -> Self {
        Self {
            event,
            event_log,
            r,
        }
    }

    fn undo(&mut self, undo: Box<dyn Fn(&mut RegionData)>) {
        self.event_log.push_back((self.event, Box::new(undo)));
    }

    pub fn get_camera(&self, e: EntityId) -> &Camera {
        let camera = match &self.r.data.raw().entities.get(e).unwrap().kind {
            EntityType::Camera(ref camera) => camera,
            _ => panic!("Tried to get camera but entity isn't a camera"),
        };
        camera
    }

    pub fn get_camera_id(&self) -> Option<usize> {
        let mut cam = None;
        for (i, item) in self.r.data.raw().entities.iter().enumerate() {
            match &item.kind {
                EntityType::Camera(_) => {
                    cam = Some(i);
                    break;
                }
                _ => (),
            }
        }
        cam
    }

    pub fn update_camera(&mut self) {
        if let Some(cam_id) = self.get_camera_id() {
            self.r.data.update_camera(cam_id);
        }
    }

    pub fn set_camera_speed(&mut self, x: f32, y: f32, z: f32) {
        let id = self.get_camera_id().unwrap();
        let old = self.get_camera(id).velocity;
        self.r.data.set_camera_velocity(id, x, y, z);
        undo!(self, move |r| {
            r.data.set_camera_velocity(id, old.x, old.y, old.z);
        });
    }

    pub fn tick(&mut self) {
        self.r.data.tick();
        undo!(self, |d| {
            d.data.untick();
        });
    }

    pub fn create_camera(&mut self) {
        let mut e = Entity::default();
        e.kind = EntityType::Camera(Camera::new());
        let index = self.r.data.create_entity(e);
        undo!(self, move |d| {
            d.data.remove_entity(index);
        });
    }

    pub fn create_text_entity(&mut self, text: &str) {
        let layout = text_layout(&mut self.r.font_context, text);
        let mut e = Entity::default();
        e.kind = EntityType::Text {
            content: text.to_string(),
        };
        let index = self.r.data.create_entity(e);
        self.r.text_layouts.insert(index, layout);
        undo!(self, move |d| {
            d.text_layouts.remove(&index).unwrap();
            d.data.remove_entity(index);
        });
    }

    pub fn _add_entity(&mut self, _e: Entity) {
        // let index = self.r.data.entities.len();
        // self.r.data.entities.insert(index, e);
        // self.event_log.push_back((
        //     self.event,
        //     Box::new(move |r| {
        //         r.data.entities.remove(index);
        //     }),
        // ));
    }
}
