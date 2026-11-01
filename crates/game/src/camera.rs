#[allow(unused)]
use log::info;
use glamx::{Mat4, Quat, Vec4};
use rapier3d::math::Vector;
use rapier3d::prelude::RigidBodyHandle;
use std::f32::consts::FRAC_PI_2;

use crate::input::Key;
use crate::movement::{self, MoveKeys};
use crate::{Controller, GameData, GameDataUpdate, Undo};

pub const ASPECT: f32 = 16.0 / 9.0;
/// Vertical field of view in radians (Perspective3 expects radians, not degrees).
pub const FOV_Y: f32 = std::f32::consts::FRAC_PI_3; // 60°

/// Max pitch magnitude (radians), a hair under straight up/down to dodge
/// the gimbal singularity at the poles.
pub const PITCH_LIMIT: f32 = FRAC_PI_2 - 0.001;
/// Minecraft-style look sensitivity in [0, 1]; tune to taste.
pub const LOOK_SENSITIVITY: f32 = 0.5;
/// Radians of yaw per pixel of mouse motion, before the sensitivity curve.
pub const LOOK_BASE_X: f32 = 0.0026;
/// Radians of pitch per pixel of mouse motion, before the sensitivity curve.
pub const LOOK_BASE_Y: f32 = 0.0026;

/// Minecraft cubic sensitivity: fine control near center, fast at the ends.
/// `f = sens*0.6 + 0.2; mult = f^3 * 8`, then `px * base * mult`.
pub fn look_delta_rad(px: f32, base: f32) -> f32 {
    let f = LOOK_SENSITIVITY * 0.6 + 0.2;
    let mult = f * f * f * 8.0;
    px * base * mult
}

/// Accumulate a mouse delta into (yaw, pitch). Yaw wraps freely; pitch is
/// clamped to +/-PITCH_LIMIT. Non-finite deltas are ignored so a stray NaN
/// cannot corrupt the look. Yaw: rightward mouse decreases the angle
/// (matches the previous controller's convention). Pitch: negative `dy`
/// (mouse up) increases the angle toward `+PITCH_LIMIT`, matching the
/// same non-inverted, subtract-the-delta convention as yaw.
pub fn apply_look(yaw: f32, pitch: f32, dx: f32, dy: f32) -> (f32, f32) {
    let dx = if dx.is_finite() { dx } else { 0.0 };
    let dy = if dy.is_finite() { dy } else { 0.0 };
    let new_yaw = yaw - look_delta_rad(dx, LOOK_BASE_X);
    // Compare-and-select: a NaN pitch passes through unchanged (unreachable:
    // the deltas are sanitised above and the stored pitch was clamped).
    let p =pitch - look_delta_rad(dy, LOOK_BASE_Y);
    let new_pitch = if p < -PITCH_LIMIT {
        -PITCH_LIMIT
    } else if p > PITCH_LIMIT {
        PITCH_LIMIT
    } else {
        p
    };
    (new_yaw, new_pitch)
}

/// Rotation by `angle` about a unit `axis`, with the half-angle trig through
/// `libm` so it is bit-identical on native and wasm32 (std's `sin`/`cos` are
/// the platform's; glam's `from_axis_angle` calls them).
fn axis_angle(axis: Vector, angle: f32) -> Quat {
    let half = angle * 0.5;
    let s = libm::sinf(half);
    Quat::from_xyzw(axis.x * s, axis.y * s, axis.z * s, libm::cosf(half))
}

/// World-Y yaw only — the horizontal facing used for movement direction.
pub fn yaw_quat(yaw: f32) -> Quat {
    axis_angle(Vector::Y, yaw)
}

/// Full look orientation: yaw about world-Y, then pitch about local-X.
/// Roll is structurally impossible (the X axis is invariant under its own
/// rotation, so the camera-right vector stays horizontal).
pub fn look_quat(yaw: f32, pitch: f32) -> Quat {
    yaw_quat(yaw) * axis_angle(Vector::X, pitch)
}

/// A perspective projection stored as its matrix: nalgebra's `Perspective3`,
/// ported to glam with the same formulas (the camera state keeps its exact
/// values across the switch). `fovy`/`aspect`/`znear`/`zfar` are read back
/// from the matrix, as nalgebra's accessors do. Near/far describe `m[2][2]`
/// and `m[3][2]` (row 2), so for a matrix built by `Camera::new` (projection
/// times the OpenGL→wgpu depth remap) they are those of the remapped matrix.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub struct Perspective3 {
    matrix: Mat4,
}

impl Perspective3 {
    /// nalgebra `Perspective3::new`: fovy in radians.
    pub fn new(aspect: f32, fovy: f32, znear: f32, zfar: f32) -> Self {
        assert!(zfar != znear, "The near-plane and far-plane must not be superimposed.");
        assert!(aspect != 0.0, "The aspect ratio must not be zero.");
        let mut res = Self::from_matrix_unchecked(Mat4::IDENTITY);
        res.set_fovy(fovy);
        res.set_aspect(aspect);
        res.set_znear_and_zfar(znear, zfar);
        res.matrix.w_axis.w = 0.0; // (3, 3)
        res.matrix.z_axis.w = -1.0; // (3, 2)
        res
    }

    pub fn from_matrix_unchecked(matrix: Mat4) -> Self {
        Self { matrix }
    }

    pub fn as_matrix(&self) -> &Mat4 {
        &self.matrix
    }

    // (row, col) → glam's column-major `col(c)[r]`.
    fn get(&self, r: usize, c: usize) -> f32 {
        self.matrix.col(c)[r]
    }

    fn set(&mut self, r: usize, c: usize, v: f32) {
        self.matrix.col_mut(c)[r] = v;
    }

    pub fn aspect(&self) -> f32 {
        self.get(1, 1) / self.get(0, 0)
    }

    pub fn fovy(&self) -> f32 {
        libm::atanf(1.0 / self.get(1, 1)) * 2.0
    }

    pub fn znear(&self) -> f32 {
        let ratio = (-self.get(2, 2) + 1.0) / (-self.get(2, 2) - 1.0);
        self.get(2, 3) / (ratio * 2.0) - self.get(2, 3) / 2.0
    }

    pub fn zfar(&self) -> f32 {
        let ratio = (-self.get(2, 2) + 1.0) / (-self.get(2, 2) - 1.0);
        (self.get(2, 3) - ratio * self.get(2, 3)) / 2.0
    }

    pub fn set_aspect(&mut self, aspect: f32) {
        assert!(aspect != 0.0, "The aspect ratio must not be zero.");
        self.set(0, 0, self.get(1, 1) / aspect);
    }

    pub fn set_fovy(&mut self, fovy: f32) {
        let old_m22 = self.get(1, 1);
        let new_m22 = 1.0 / libm::tanf(fovy / 2.0);
        self.set(1, 1, new_m22);
        self.set(0, 0, self.get(0, 0) * (new_m22 / old_m22));
    }

    pub fn set_znear_and_zfar(&mut self, znear: f32, zfar: f32) {
        self.set(2, 2, (zfar + znear) / (znear - zfar));
        self.set(2, 3, zfar * znear * 2.0 / (znear - zfar));
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, state_hash::StateHash)]
#[module(crate)]
pub struct Camera {
    pub opengl_to_wgpu_matrix: Mat4,
    pub proj_matrix: Perspective3,
    pub view_matrix: Option<RigidBodyHandle>,
    pub yaw: f32,
    pub pitch: f32,
}

impl Camera {
    pub fn new(handle: RigidBodyHandle) -> Self {
        let m = Mat4::from_cols(
            Vec4::new(1.0, 0.0, 0.0, 0.0),
            Vec4::new(0.0, 1.0, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 0.5, 0.0),
            Vec4::new(0.0, 0.0, 0.5, 1.0),
        );
        Camera {
            proj_matrix: Perspective3::from_matrix_unchecked(
                *Perspective3::new(
                    ASPECT,
                    FOV_Y,
                    // 1 unit = 1/16 m: near 6 cm, far 125 m.
                    1.0,
                    2000.0,
                )
                .as_matrix()
                    * m,
            ),
            opengl_to_wgpu_matrix: m,
            view_matrix: Some(handle),
            yaw: 0.0,
            pitch: 0.0,
        }
    }
}

impl Camera {
    pub fn build_projection(&self) -> Mat4 {
        *self.proj_matrix.as_matrix()
    }
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            // nalgebra's `Matrix4::default()` is all zeros (glam's is identity).
            opengl_to_wgpu_matrix: Mat4::ZERO,
            proj_matrix: Perspective3::new(
                ASPECT,
                FOV_Y,
                // 1 unit = 1/16 m: near 6 cm, far 125 m.
                1.0,
                2000.0,
            ),
            view_matrix: Default::default(),
            yaw: 0.0,
            pitch: 0.0,
        }
    }
}

pub struct CameraController {}

impl Controller for CameraController {
    fn on_tick<'a>(&mut self, data: &mut Undo<GameData>) {
        let data = data.as_refs_mut();
        for (client_id, e_id) in data.player_entites.iter() {
            let e_id = *e_id;
            let ecs = data.ecs.as_refs_mut();
            let client = data.clients.get_mut(client_id).unwrap();
            if let Some((width, height)) = *client.input.window_resized() {
                let old = ecs.camera.get(e_id).proj_matrix.clone();
                ecs.camera.emit_on_undo(GameDataUpdate::new(
                    crate::GameDataTransactionKind::Undo,
                    crate::GameDataUpdateKind::UpdateCameraViewProj(e_id, old.clone()),
                ));
                let mut scope = ecs.camera.undo_scope();
                scope
                    .get_mut(e_id)
                    .proj_matrix
                    .set_aspect(width as f32 / height as f32);
                let m = scope.get_mut(e_id).proj_matrix.clone();
                scope.register(move |d, _| d.get_mut(e_id).proj_matrix = old);
                ecs.camera.send(GameDataUpdate::new(
                    crate::GameDataTransactionKind::Do,
                    crate::GameDataUpdateKind::UpdateCameraViewProj(e_id, m),
                ));
            }

            // The body obeys gravity whether or not the client is driving it;
            // a released cursor (menus, startup) only silences the input.
            let active = client.fps_cam_mode;
            let handle = ecs.rigidbody.get(e_id).clone();
            // Look scalars are authoritative; the body quaternion is derived.
            let (yaw0, pitch0) = {
                let cam = ecs.camera.get(e_id);
                (cam.yaw, cam.pitch)
            };
            let b = data.physics.bodies.get(handle).unwrap();

            let motion0 = (*ecs.motion.try_get(e_id)).unwrap_or_default();
            let mut motion = motion0;
            // An adventurer on a walk is steered by it, not the keyboard.
            let walk = match ecs.walker.try_get(e_id) {
                Some(w) if client.mode == crate::PlayMode::Adventure => {
                    Some(crate::steer(w, crate::feet_of(b.translation()), motion0.grounded))
                }
                _ => None,
            };
            let (keys, yaw) = match walk {
                Some((keys, yaw)) => (keys, yaw),
                None => {
                    let held = |key: Key| active && client.input.key_held(&key);
                    let keys = MoveKeys {
                        forward: held(Key::KeyW),
                        back: held(Key::KeyS),
                        left: held(Key::KeyA),
                        right: held(Key::KeyD),
                        jump_pressed: active && client.input.key_pressed(&Key::Space),
                        jump_held: held(Key::Space),
                        descend: held(Key::ShiftLeft),
                        sprint: held(Key::ControlLeft),
                    };
                    (keys, yaw0)
                }
            };
            let t = b.translation().clone();
            // Collision-corrected movement: slide along / stop at terrain
            // instead of teleporting through it. Queries are read-only; the
            // body write below is the footprint-sized mutate_body_safe entry.
            let corrected = movement::walk_body(
                &mut motion,
                &keys,
                yaw,
                **data.tick as u64,
                &data.physics.broad_phase,
                &data.physics.narrow_phase,
                &data.physics.bodies,
                &data.physics.colliders,
                handle,
            );
            if motion != motion0 {
                // set_safe, not an undo_scope on the value: a body that
                // arrived without movement state has an empty slot.
                ecs.motion.set_safe(e_id, Some(motion));
            }
            if corrected != Vector::ZERO {
                // Footprint-sized entry: set_next_kinematic_* also wakes
                // the body and marks it modified, but all of that lives in
                // the body slot + modified-mark that mutate_body_safe
                // restores — no whole-set snapshot.
                data.physics.bodies.mutate_body_safe(handle, move |b| {
                    b.set_next_kinematic_translation(t + corrected)
                });
            }
            if walk.is_some() {
                // Face the way the walk heads, level — the same undo-tracked
                // look scalars and body rotation as mouse look below.
                if yaw != yaw0 || pitch0 != 0.0 {
                    let mut scope = ecs.camera.undo_scope();
                    {
                        let c = scope.get_mut(e_id);
                        c.yaw = yaw;
                        c.pitch = 0.0;
                    }
                    scope.register(move |d, _| {
                        let c = d.get_mut(e_id);
                        c.yaw = yaw0;
                        c.pitch = pitch0;
                    });
                    let r = crate::look_quat(yaw, 0.0);
                    data.physics.bodies.mutate_body_safe(handle, move |b| b.set_next_kinematic_rotation(r));
                }
                continue;
            }
            if !active {
                continue;
            }

            // --- Look: accumulate mouse delta into yaw/pitch scalars ---
            let diff = client.input.mouse_diff();
            let (new_yaw, new_pitch) = crate::apply_look(yaw0, pitch0, diff.0, diff.1);
            if new_yaw != yaw0 || new_pitch != pitch0 {
                // Undo-tracked scalar update on the Camera component (same
                // pattern as the proj_matrix mutation above).
                let mut scope = ecs.camera.undo_scope();
                {
                    let c = scope.get_mut(e_id);
                    c.yaw = new_yaw;
                    c.pitch = new_pitch;
                }
                scope.register(move |d, _| {
                    let c = d.get_mut(e_id);
                    c.yaw = yaw0;
                    c.pitch = pitch0;
                });
                // Rebuild the body's full-look rotation. Same footprint-sized
                // entry as the translation site above (set_next_kinematic_
                // rotation wakes + marks the body — all inside the footprint).
                let r = crate::look_quat(new_yaw, new_pitch);
                data.physics.bodies.mutate_body_safe(handle, move |b| {
                    b.set_next_kinematic_rotation(r)
                });
            }
        }
    }
}

impl CameraController {
    pub fn new() -> Box<dyn Controller> {
        Box::new(Self {})
    }
}
