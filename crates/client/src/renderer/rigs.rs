//! Draws sim-animated entities (`game::anim`): the sim owns the animation
//! state, sent as `SetAnimator`; this module only evaluates it. Each rigged
//! entity gets its rig's `.bbmodel` as a child, kept upright (a player
//! body's rotation carries its look pitch) with the feet at
//! `Animator::foot_y`, and its bones are posed every frame with
//! `game::anim::sample` — the same function the sim's bone colliders use.
//!
//! Between sim updates the pose is extrapolated by the wall time since the
//! last one, at most one tick, so clips play smoothly at any frame rate and
//! snap back to the sim's state on every update.

use bevy::prelude::*;
use game::anim::Animator;

use super::model::{BbModel, BbModelPart, BbModelSpawned};

/// The sim's animation state for this entity (from `SetAnimator`).
#[derive(Component, Debug, Clone, Copy)]
pub struct SimAnimator(pub Animator);

/// On a rigged entity: its model child, and when its `SimAnimator` last
/// changed (seconds of `Time::elapsed`).
#[derive(Component, Debug)]
pub struct RigModel {
    model: Entity,
    updated_at: f32,
}

/// Rigged entities get their model child; the capsule placeholder mesh that
/// `attach_avatars` may have given them goes.
pub fn attach_rig_models(
    mut commands: Commands,
    assets: Res<AssetServer>,
    time: Res<Time>,
    added: Query<(Entity, &SimAnimator), Without<RigModel>>,
) {
    for (e, anim) in &added {
        let rig = game::anim::rig(anim.0.rig);
        let model = commands
            .spawn((BbModel(assets.load(rig.asset_path)), Transform::default(), Visibility::default(), ChildOf(e)))
            .id();
        commands
            .entity(e)
            .insert(RigModel { model, updated_at: time.elapsed_secs() })
            .remove::<(Mesh3d, MeshMaterial3d<StandardMaterial>)>();
    }
}

/// An entity that lost its animator loses its model.
pub fn detach_rig_models(mut commands: Commands, orphaned: Query<(Entity, &RigModel), Without<SimAnimator>>) {
    for (e, rig) in &orphaned {
        commands.entity(rig.model).despawn();
        commands.entity(e).remove::<RigModel>();
    }
}

/// Places each model upright under its (interpolated) body and poses its
/// bones from the sim state.
pub fn pose_rig_models(
    time: Res<Time>,
    mut rigged: Query<(&Transform, Ref<SimAnimator>, &mut RigModel), Without<BbModelPart>>,
    mut models: Query<(&mut Transform, Option<&BbModelSpawned>), (With<BbModel>, Without<SimAnimator>)>,
    mut parts: Query<&mut Transform, (With<BbModelPart>, Without<BbModel>, Without<SimAnimator>)>,
) {
    let now = time.elapsed_secs();
    for (body, anim, mut rig_model) in &mut rigged {
        if anim.is_changed() {
            rig_model.updated_at = now;
        }
        let Ok((mut tf, spawned)) = models.get_mut(rig_model.model) else { continue };
        // Body frame -> model frame: keep the body's yaw, drop its pitch.
        let forward = body.rotation * Vec3::NEG_Z;
        let yaw = Quat::from_rotation_y((-forward.x).atan2(-forward.z));
        let upright = body.rotation.inverse() * yaw;
        tf.set_if_neq(Transform {
            translation: upright * Vec3::new(0.0, anim.0.foot_y, 0.0),
            rotation: upright,
            scale: Vec3::ONE,
        });

        let Some(spawned) = spawned else { continue };
        let ahead = (now - rig_model.updated_at).clamp(0.0, game::movement::TICK_DT);
        let rig = game::anim::rig(anim.0.rig);
        let local = rig.model.local_pose(&game::anim::sample(&anim.0, ahead));
        // The asset and the sim's rig are the same file; a hot-reloaded
        // asset with other bones is drawn at rest rather than mis-posed.
        if local.len() != spawned.0.len() {
            continue;
        }
        for ((t, r, s), &e) in local.into_iter().zip(&spawned.0) {
            if let Ok(mut part) = parts.get_mut(e) {
                part.set_if_neq(Transform {
                    translation: Vec3::from(t),
                    rotation: Quat::from_array(r),
                    scale: Vec3::from(s),
                });
            }
        }
    }
}
