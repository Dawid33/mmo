//! Simulation-owned skeletal animation: the sim decides which clip an entity
//! plays and how far into it it is, and its colliders follow the animated
//! bones. The renderer draws the same pose from the same state.
//!
//! The state is tiny ([`Animator`]: rig, clip, clip time, playback speed and
//! a crossfade). The pose is a pure function of it ([`sample`]): the rig's
//! Blockbench clips evaluated by `bbmodel` with `libm` trig and plain
//! arithmetic, so the server, the client's prediction and the wasm build all
//! get bit-identical bones. Nothing per bone is stored or sent; the client
//! evaluates [`sample`] at its render time.
//!
//! Each bone with geometry gets one cuboid collider (its cubes' box in bone
//! space) on the entity's body, re-posed every tick from the pose
//! ([`Rollback::tick_animators_safe`]). The colliders have zero density, so
//! the body's mass properties never sum two non-zero parts (that path goes
//! through glamx `SymmetricEigen3`, whose std trig differs across targets).
//! The entity's own movement keeps using its first collider (the capsule):
//! the character controller excludes its own body, so its bones never block
//! it, while every other mover's sweep hits them.
//!
//! Rigs are compiled in (`include_bytes!`), so server and client always
//! evaluate the same file.

use std::sync::OnceLock;

use bbmodel::{BonePose, Model};
use rapier3d::math::Vector;
use rapier3d::prelude::{ColliderBuilder, ColliderHandle};
use slotmapd::Key as _;
use state_hash::StateHash;

use crate::math::Quat;
use crate::movement::{TICK_DT, UNITS_PER_METER};
use crate::{
    EntityKey, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Pose, Rollback,
};

// --- Tunables -----------------------------------------------------------

/// Below this horizontal speed (m/s) a grounded walker plays its idle clip.
pub const ANIM_IDLE_MAX_SPEED: f32 = 0.25;
/// At or above this horizontal speed (m/s) it runs instead of walking.
pub const ANIM_RUN_MIN_SPEED: f32 = 1.5;
/// Seconds a clip change crossfades over.
pub const ANIM_CROSSFADE_SECS: f32 = 0.2;
/// Gait clips scale their playback to the ground speed (so feet don't
/// slide), clamped to this range.
pub const ANIM_MIN_PLAYBACK: f32 = 0.25;
pub const ANIM_MAX_PLAYBACK: f32 = 3.0;
/// No clip: the rest pose.
pub const REST_CLIP: u16 = u16::MAX;

// --- Rigs ---------------------------------------------------------------

/// The compiled-in rigs. Append only: the discriminant is serialized.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub enum RigId {
    #[default]
    Dwarf,
}

/// What the sim needs to know about a rig beyond the model file: the gait
/// clips' ground speeds (world units per second at playback speed 1, from
/// the creature manifest's per-gait stride / length).
struct RigDef {
    bytes: &'static [u8],
    asset_path: &'static str,
    walk_speed: f32,
    run_speed: f32,
}

fn def(id: RigId) -> RigDef {
    match id {
        RigId::Dwarf => RigDef {
            bytes: include_bytes!("../../../assets/models/creatures/dwarf.bbmodel"),
            asset_path: "models/creatures/dwarf.bbmodel",
            // assets/models/creatures/creatures.json: dwarf walk / run speed.
            walk_speed: 9.3894,
            run_speed: 37.6058,
        },
    }
}

/// One bone's collision box, in that bone's space (world units).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hitbox {
    pub node: usize,
    pub center: [f32; 3],
    pub half_extents: [f32; 3],
}

/// A parsed rig: the model (no textures) plus what the sim derives from it.
pub struct Rig {
    pub id: RigId,
    pub model: Model,
    /// The `.bbmodel` path under `assets/` the renderer loads for this rig.
    pub asset_path: &'static str,
    pub hitboxes: Vec<Hitbox>,
    pub idle: u16,
    pub walk: u16,
    pub run: u16,
    walk_speed: f32,
    run_speed: f32,
}

impl Rig {
    fn load(id: RigId) -> Rig {
        let d = def(id);
        let model = bbmodel::parse_rig(d.bytes).expect("compiled-in rig parses");
        let clip = |name: &str| {
            model
                .animations
                .iter()
                .position(|a| a.name == name)
                .map_or(REST_CLIP, |i| i as u16)
        };
        let hitboxes = model
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(node, n)| {
                let mut it = n.parts.iter().flat_map(|p| p.positions.iter());
                let first = *it.next()?;
                let (mut lo, mut hi) = (first, first);
                for p in it {
                    for i in 0..3 {
                        lo[i] = lo[i].min(p[i]);
                        hi[i] = hi[i].max(p[i]);
                    }
                }
                let center = [0, 1, 2].map(|i| (lo[i] + hi[i]) * 0.5);
                let half_extents = [0, 1, 2].map(|i| (hi[i] - lo[i]) * 0.5);
                // A flat decal plane has no volume to collide with.
                if half_extents.iter().any(|h| *h <= 0.0) {
                    return None;
                }
                Some(Hitbox { node, center, half_extents })
            })
            .collect();
        Rig {
            id,
            asset_path: d.asset_path,
            idle: clip("idle"),
            walk: clip("walk"),
            run: clip("run"),
            walk_speed: d.walk_speed,
            run_speed: d.run_speed,
            hitboxes,
            model,
        }
    }
}

/// The rig, parsed once per process.
pub fn rig(id: RigId) -> &'static Rig {
    static DWARF: OnceLock<Rig> = OnceLock::new();
    match id {
        RigId::Dwarf => DWARF.get_or_init(|| Rig::load(RigId::Dwarf)),
    }
}

// --- State --------------------------------------------------------------

/// One clip playing: which, how far in (seconds of clip time, unwrapped:
/// [`bbmodel::Animation::local_time`] loops it), and how fast.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct ClipPlay {
    pub clip: u16,
    pub time: f32,
    pub speed: f32,
}

impl Default for ClipPlay {
    fn default() -> Self {
        Self { clip: REST_CLIP, time: 0.0, speed: 1.0 }
    }
}

/// A clip fading out under the current one.
#[derive(Debug, Default, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct Fade {
    pub from: ClipPlay,
    /// Seconds since the fade began, of `length`.
    pub elapsed: f32,
    pub length: f32,
}

/// An entity's animation state. Rollback state (`Ecs::animator`), sent to the
/// renderer as `SetAnimator` whenever it changes (every tick while a clip
/// plays).
#[derive(Debug, Default, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct Animator {
    pub rig: RigId,
    /// Model y = 0 (the feet) in body-local units: a body's origin is its
    /// capsule centre, above the feet. Fixed per entity; carried here so the
    /// renderer places the model from the one update it already gets.
    pub foot_y: f32,
    pub current: ClipPlay,
    pub fade: Option<Fade>,
}

impl Animator {
    pub fn idle(rig_id: RigId) -> Self {
        let clip = rig(rig_id).idle;
        Self { rig: rig_id, foot_y: 0.0, current: ClipPlay { clip, ..ClipPlay::default() }, fade: None }
    }

    /// Switch to `clip` (crossfading from the current one) unless it already
    /// plays, then set its playback speed.
    pub fn play(&mut self, clip: u16, speed: f32) {
        if clip != self.current.clip {
            self.fade = Some(Fade { from: self.current, elapsed: 0.0, length: ANIM_CROSSFADE_SECS });
            self.current = ClipPlay { clip, time: 0.0, speed };
        } else {
            self.current.speed = speed;
        }
    }

    /// Advance `dt` seconds of wall time.
    pub fn advance(&mut self, dt: f32) {
        self.current.time += dt * self.current.speed;
        if let Some(f) = self.fade.as_mut() {
            f.from.time += dt * f.from.speed;
            f.elapsed += dt;
            if f.elapsed >= f.length {
                self.fade = None;
            }
        }
    }
}

/// The bone collider handles of an entity (`Ecs::bones`): one per rig
/// hitbox, in [`Rig::hitboxes`] order. Changes only when the colliders are
/// built.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct BoneColliders {
    pub colliders: Vec<ColliderHandle>,
}

// --- Pose ---------------------------------------------------------------

fn sample_clip(rig: &Rig, play: &ClipPlay, ahead: f32) -> Vec<(usize, BonePose)> {
    let Some(anim) = rig.model.animations.get(play.clip as usize) else { return Vec::new() };
    let t = anim.local_time(play.time + ahead * play.speed);
    anim.sample(t, &Default::default())
}

/// Every animated bone's pose, `ahead` seconds of wall time past the state
/// (the renderer's sub-tick extrapolation; the sim passes 0). Deterministic.
pub fn sample(animator: &Animator, ahead: f32) -> Vec<(usize, BonePose)> {
    let rig = rig(animator.rig);
    let poses = sample_clip(rig, &animator.current, ahead);
    match animator.fade {
        Some(f) => {
            let w = ((f.elapsed + ahead) / f.length).clamp(0.0, 1.0);
            if w >= 1.0 {
                poses
            } else {
                bbmodel::animation::blend(&sample_clip(rig, &f.from, ahead), &poses, w)
            }
        }
        None => poses,
    }
}

/// Model-space transform (translation, rotation) of every node for `poses`.
/// Scale keys are not applied: colliders keep their rest size.
pub fn model_space(rig: &Rig, poses: &[(usize, BonePose)]) -> Vec<(Vector, Quat)> {
    let local = rig.model.local_pose(poses);
    let mut out: Vec<(Vector, Quat)> = Vec::with_capacity(local.len());
    for (node, (t, r, _)) in rig.model.nodes.iter().zip(local) {
        let t = Vector::from(t);
        let r = Quat::from_array(r);
        out.push(match node.parent {
            None => (t, r),
            Some(p) => {
                let (pt, pr) = out[p];
                (pt + pr * t, pr * r)
            }
        });
    }
    out
}

/// Each hitbox's pose relative to the body: `upright` (body frame →
/// model frame rotation; undoes a player's look pitch), the feet at
/// `foot_y`, then the bone transform and the box centre.
fn hitbox_poses(rig: &Rig, animator: &Animator, foot_y: f32, upright: Quat) -> Vec<Pose> {
    let bones = model_space(rig, &sample(animator, 0.0));
    rig.hitboxes
        .iter()
        .map(|h| {
            let (t, r) = bones[h.node];
            let model = t + r * Vector::from(h.center);
            let translation = upright * (Vector::new(0.0, foot_y, 0.0) + model);
            Pose::from_parts(translation, upright * r)
        })
        .collect()
}

/// Which clip a body-moved entity plays, and how fast, from its horizontal
/// speed (world units/s) and whether it stands on something.
pub fn gait(rig: &Rig, horizontal_speed: f32, grounded: bool) -> (u16, f32) {
    let m = horizontal_speed / UNITS_PER_METER;
    if !grounded || m < ANIM_IDLE_MAX_SPEED {
        return (rig.idle, 1.0);
    }
    let (clip, clip_speed) = if m < ANIM_RUN_MIN_SPEED {
        (rig.walk, rig.walk_speed)
    } else {
        (rig.run, rig.run_speed)
    };
    let speed = (horizontal_speed / clip_speed).clamp(ANIM_MIN_PLAYBACK, ANIM_MAX_PLAYBACK);
    (clip, speed)
}

// --- Sim ----------------------------------------------------------------

impl Rollback {
    /// Give `e` (which already has its body and movement collider) an
    /// animator in `animator`'s state and one collider per rig hitbox,
    /// undo-safely. `foot_y`: where the model's feet sit in the body frame
    /// (stored as [`Animator::foot_y`]).
    pub fn attach_rig_safe(&mut self, e: EntityKey, mut animator: Animator, foot_y: f32) {
        animator.foot_y = foot_y;
        let Some(body) = *self.data.ecs.rigidbody.try_get(e) else { return };
        let upright = self.upright_of(e);
        let rig = rig(animator.rig);
        let mut colliders = Vec::with_capacity(rig.hitboxes.len());
        for (h, pose) in rig.hitboxes.iter().zip(hitbox_poses(rig, &animator, foot_y, upright)) {
            let c = ColliderBuilder::cuboid(h.half_extents[0], h.half_extents[1], h.half_extents[2])
                .position(pose)
                .density(0.0)
                .user_data(e.data().as_ffi() as u128)
                .build();
            colliders.push(self.attach_collider_safe(body, c));
        }
        self.data.ecs.bones.set_safe(e, Some(BoneColliders { colliders }));
        self.set_animator_safe(e, animator);
    }

    /// Set the animator and tell the renderer, both directions.
    pub(crate) fn set_animator_safe(&mut self, e: EntityKey, animator: Animator) {
        let old = *self.data.ecs.animator.try_get(e);
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetAnimator(e, Some(animator)),
        ));
        self.data.ecs.animator.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetAnimator(e, old),
        ));
        self.data.ecs.animator.set_safe(e, Some(animator));
    }

    /// Body frame → model frame rotation. A camera-carrying body's rotation
    /// includes the look pitch, which must not tilt the model.
    fn upright_of(&self, e: EntityKey) -> Quat {
        match self.data.ecs.camera.try_get(e) {
            Some(c) => crate::look_quat(c.yaw, c.pitch).inverse() * crate::yaw_quat(c.yaw),
            None => Quat::IDENTITY,
        }
    }

    /// Once per region tick, after the physics step: pick each animated
    /// entity's clip from how its body moved, advance it one tick, and move
    /// its bone colliders to the new pose (used by the next step). Entities
    /// in key order, so client and server log the same entries.
    pub fn tick_animators_safe(&mut self) {
        let keys: Vec<EntityKey> = self
            .data
            .ecs
            .animator
            .iter()
            .filter_map(|(k, a)| a.map(|_| k))
            .collect();
        for e in keys {
            let Some(mut animator) = *self.data.ecs.animator.try_get(e) else { continue };
            let Some(body) = *self.data.ecs.rigidbody.try_get(e) else { continue };
            let Some(b) = self.data.physics.bodies.get(body) else { continue };
            let v = b.linvel();
            let horizontal = libm::sqrtf(v.x * v.x + v.z * v.z);
            let grounded = self.data.ecs.motion.try_get(e).map_or(true, |m| m.grounded && !m.flying);
            let rig = rig(animator.rig);
            let (clip, speed) = gait(rig, horizontal, grounded);
            animator.play(clip, speed);
            animator.advance(TICK_DT);
            self.set_animator_safe(e, animator);

            let Some(bones) = self.data.ecs.bones.try_get(e).clone() else { continue };
            let upright = self.upright_of(e);
            let poses = hitbox_poses(rig, &animator, animator.foot_y, upright);
            for (handle, pose) in bones.colliders.iter().zip(poses) {
                let Some(c) = self.data.physics.colliders.get(*handle) else { continue };
                if c.position_wrt_parent() == Some(&pose) {
                    continue;
                }
                self.data
                    .physics
                    .colliders
                    .mutate_collider_safe(*handle, move |c| c.set_position_wrt_parent(pose));
            }
        }
    }
}
