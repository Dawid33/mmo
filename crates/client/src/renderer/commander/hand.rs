//! The first-person arm: the body's right arm in the lower right of the view,
//! Minecraft style, animated by what the hands do.
//!
//! Render-only and client-local: one [`Hand`] entity, a child of the [`Eyes`],
//! shown only in first person ([`BodyView::First`]); aim and every action come
//! from the input the sim is sent, never from this pose. Its child cuboids are
//! a skin arm and a sleeve; [`pose_hand`] poses the whole arm each frame with
//! Minecraft's own transform chain ([`minecraft_arm`]).
//!
//! - **Held item**: with an item in the selected hotbar slot the arm's model is
//!   hidden and the item is shown in its place ([`HeldItem`], posed by
//!   [`minecraft_item`]): a block as a small cube turned a corner towards the
//!   view, any other item as its flat picture — the same textures the dropped
//!   piles draw (`item_piles::material_for`). An empty slot shows the bare arm.
//! - **Swing** ([`SWING_SECS`]): a left press (a punch / the start of mining),
//!   a right press with an item in the selected slot (placing), Q (throwing).
//!   While a block is being mined (`HudItems::mining`) with the button held it
//!   swings again each time a swing ends (held right while building, too).
//!   The fist reaches in from the corner to the middle and down, and back.
//! - **Raise**: when the first-person view comes up the arm rises into it from
//!   [`EQUIP_DROP`] below ([`EQUIP_EASE_SECS`]). A change of the selected slot
//!   (or of the item in it) does NOT move the arm: the item just changes
//!   (owner, 2026-11-19 — an equip drop comes back with tools and weapons).
//! - **Bob**: the view's own step bob ([`ViewFeel::bob_pose`]) applied again to
//!   the arm, so it sways on screen as the body strides (none with view bobbing
//!   off).
//! - **Sway**: the arm trails the look a little ([`SWAY_EASE_SECS`],
//!   [`SWAY_GAIN`], at most [`SWAY_MAX_RAD`]) when the view turns.
//!
//! The pose is Minecraft's at its size, scaled about the eye by [`HAND_SCALE`]
//! (same picture, but within the body's collider).

use std::f32::consts::PI;

use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use game::movement::UNITS_PER_METER;

use super::adventure::ClientPlayMode;
use super::first_person::{BodyView, Eyes, FirstPerson};
use super::smooth;
use super::view_feel::{ViewFeel, bob_at};
use crate::renderer::cursor::CursorPolicy;
use crate::renderer::item_piles::{ItemPileAssets, material_for};
use crate::renderer::slint_overlay::HudItems;

/// The whole arm is Minecraft's own pose ([`minecraft_arm`], in blocks)
/// scaled by this about the eye: the same picture on screen (a perspective
/// view does not change under a uniform scale about the eye), but the arm
/// stays within 0.37 m of it — inside the body's collider (`PLAYER_RADIUS`,
/// 0.4 m), so it does not sink into a wall the body stands against.
pub const HAND_SCALE: f32 = 0.2;
/// Minecraft's arm box in model pixels (1/16 block): from, to.
const ARM_FROM: Vec3 = Vec3::new(-3.0, -2.0, -2.0);
const ARM_TO: Vec3 = Vec3::new(1.0, 10.0, 2.0);
/// Its pivot (the shoulder), model pixels.
const ARM_PIVOT: Vec3 = Vec3::new(-5.0, 2.0, 0.0);
/// The arm model's resting outward tilt (`setRotationAngles` at age 0), radians.
const ARM_REST_ROLL: f32 = 0.1;
/// The sleeve covers the arm's top this many pixels, standing this far proud.
const SLEEVE_PX: f32 = 4.0;
const SLEEVE_PROUD_PX: f32 = 0.25;

/// One swing, seconds (Minecraft: 6 ticks).
pub const SWING_SECS: f32 = 0.3;

/// The arm rises into the view from this far below (blocks, before
/// [`HAND_SCALE`]; Minecraft's equip drop).
pub const EQUIP_DROP: f32 = 0.6;
/// Time constant (s) of the arm's rise.
pub const EQUIP_EASE_SECS: f32 = 0.06;

/// Time constant (s) of the look the arm trails.
pub const SWAY_EASE_SECS: f32 = 0.06;
/// The arm turns this share of how far the look ran ahead of its trail
/// (Minecraft's `rotateWithPlayerRotations`).
pub const SWAY_GAIN: f32 = 0.1;
/// The sway's largest turn either way, radians.
pub const SWAY_MAX_RAD: f32 = 0.2;

/// A held block's cube edge, blocks before [`HAND_SCALE`] (Minecraft's
/// `block/block.json` first-person scale).
const HELD_BLOCK_SCALE: f32 = 0.4;
/// A held block's turn about the vertical, degrees (Minecraft's).
const HELD_BLOCK_YAW_DEG: f32 = 45.0;
/// A held flat item (Minecraft's `item/generated.json`, first person, right
/// hand): its picture's edge in blocks, its offset in model pixels, and its
/// turn (degrees about Y, then Z).
const HELD_FLAT_SCALE: f32 = 0.68;
const HELD_FLAT_OFFSET_PX: Vec3 = Vec3::new(1.13, 3.2, 1.13);
const HELD_FLAT_YAW_DEG: f32 = -90.0;
const HELD_FLAT_ROLL_DEG: f32 = 25.0;

const SKIN: Color = Color::srgb(0.86, 0.64, 0.5);
/// The body's colour (`avatar::attach_avatars`' player capsule).
const SLEEVE: Color = Color::srgb(0.8, 0.3, 0.3);

/// The arm's animation state; on the one arm entity under the [`Eyes`].
#[derive(Component, Debug, Default)]
pub struct Hand {
    /// Seconds into the current swing; `None` at rest.
    swing: Option<f32>,
    /// How far the arm is lowered, 0 (up) ..= 1 (down by [`EQUIP_DROP`]).
    lowered: f32,
    /// The look (yaw, pitch) the arm trails.
    trail: Option<(f32, f32)>,
}

/// The arm's own model (skin and sleeve), a child of the [`Hand`]: hidden
/// while an item is held.
#[derive(Component, Debug, Default)]
pub struct ArmModel;

/// The item in the hand, a child of the [`Hand`]: a unit cube (a block) or a
/// unit quad (anything else), sized and turned by its own transform.
#[derive(Component, Debug)]
pub struct HeldItem {
    /// The item drawn now.
    shown: Option<game::IconId>,
    cube: Handle<Mesh>,
    quad: Handle<Mesh>,
}

/// Spawns the arm under the [`Eyes`] when they have none (they are re-made
/// per session; the arm goes with them).
pub fn spawn_hand(
    mut commands: Commands,
    eyes: Query<(Entity, Option<&Children>), With<Eyes>>,
    hands: Query<(), With<Hand>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (eye, children) in &eyes {
        if children.is_some_and(|c| c.iter().any(|e| hands.contains(e))) {
            continue;
        }
        let skin = materials.add(StandardMaterial {
            base_color: SKIN,
            perceptual_roughness: 0.9,
            ..default()
        });
        let sleeve = materials.add(StandardMaterial {
            base_color: SLEEVE,
            perceptual_roughness: 0.9,
            ..default()
        });
        // The parts are in Minecraft's model pixels; the arm's pose scales them.
        let part = |mesh: Handle<Mesh>, material: Handle<StandardMaterial>, centre: Vec3| {
            (
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::from_translation(centre / 16.0),
                NotShadowCaster,
                NotShadowReceiver,
            )
        };
        let arm_size = (ARM_TO - ARM_FROM) / 16.0;
        let arm_centre = (ARM_FROM + ARM_TO) / 2.0;
        let sleeve_from = ARM_FROM - Vec3::splat(SLEEVE_PROUD_PX);
        let sleeve_to =
            Vec3::new(ARM_TO.x, ARM_FROM.y + SLEEVE_PX, ARM_TO.z) + Vec3::splat(SLEEVE_PROUD_PX);
        commands.entity(eye).with_children(|eye| {
            eye.spawn((
                Hand {
                    lowered: 1.0,
                    ..default()
                },
                Transform::default(),
                Visibility::Hidden,
                Name::new("first-person arm"),
            ))
            .with_children(|hand| {
                hand.spawn((ArmModel, Transform::default(), Visibility::Inherited))
                    .with_children(|arm| {
                        arm.spawn(part(
                            meshes.add(Cuboid::from_size(arm_size)),
                            skin,
                            arm_centre,
                        ));
                        let sleeve_size = (sleeve_to - sleeve_from) / 16.0;
                        arm.spawn(part(
                            meshes.add(Cuboid::from_size(sleeve_size)),
                            sleeve,
                            (sleeve_from + sleeve_to) / 2.0,
                        ));
                    });
                let cube = meshes.add(Cuboid::from_size(Vec3::ONE));
                let quad = meshes.add(Rectangle::new(1.0, 1.0));
                hand.spawn((
                    Mesh3d(cube.clone()),
                    MeshMaterial3d(Handle::<StandardMaterial>::default()),
                    HeldItem {
                        shown: None,
                        cube,
                        quad,
                    },
                    Transform::default(),
                    Visibility::Hidden,
                    NotShadowCaster,
                    NotShadowReceiver,
                ));
            });
        });
    }
}

/// Minecraft's first-person arm (`ItemRenderer.func_178095_a`, 1.8; the same
/// in `ItemInHandRenderer.renderPlayerArm` since) at swing progress `p` and
/// equip progress `lowered` (0 = up), then its arm model's pivot and resting
/// tilt: model pixels / 16 → view space, blocks. Each step right-multiplies,
/// as the GL matrix stack does. At rest the fist peeks in at the lower right
/// with the shoulder off screen; a swing reaches it in to the middle and down.
pub fn minecraft_arm(p: f32, lowered: f32) -> Mat4 {
    let p = p.clamp(0.0, 1.0);
    let sp = p.sqrt();
    let t = |x: f32, y: f32, z: f32| Mat4::from_translation(Vec3::new(x, y, z));
    let rx = |deg: f32| Mat4::from_rotation_x(deg.to_radians());
    let ry = |deg: f32| Mat4::from_rotation_y(deg.to_radians());
    let rz = |deg: f32| Mat4::from_rotation_z(deg.to_radians());
    t(
        -0.3 * (sp * PI).sin(),
        0.4 * (sp * 2.0 * PI).sin(),
        -0.4 * (p * PI).sin(),
    ) * t(0.64, -0.6, -0.72)
        * t(0.0, lowered * -EQUIP_DROP, 0.0)
        * ry(45.0)
        * ry((sp * PI).sin() * 70.0)
        * rz((p * p * PI).sin() * -20.0)
        * t(-1.0, 3.6, 3.5)
        * rz(120.0)
        * rx(200.0)
        * ry(-135.0)
        * t(5.6, 0.0, 0.0)
        * Mat4::from_translation(ARM_PIVOT / 16.0)
        * Mat4::from_rotation_z(ARM_REST_ROLL)
}

/// Minecraft's first-person held item (`ItemInHandRenderer`: the swing's
/// offset, `applyItemArmTransform`, `applyItemArmAttackTransform`) at swing
/// progress `p` and equip progress `lowered` (0 = up): the item's own frame
/// (before its model's display transform, [`held_item_local`]) → view space,
/// blocks. At rest the item sits at the lower right; a swing tips it forward
/// and down across the view.
pub fn minecraft_item(p: f32, lowered: f32) -> Mat4 {
    let p = p.clamp(0.0, 1.0);
    let sp = p.sqrt();
    let t = |x: f32, y: f32, z: f32| Mat4::from_translation(Vec3::new(x, y, z));
    let rx = |deg: f32| Mat4::from_rotation_x(deg.to_radians());
    let ry = |deg: f32| Mat4::from_rotation_y(deg.to_radians());
    let rz = |deg: f32| Mat4::from_rotation_z(deg.to_radians());
    let reach = (sp * PI).sin();
    t(
        -0.4 * reach,
        0.2 * (sp * 2.0 * PI).sin(),
        -0.2 * (p * PI).sin(),
    ) * t(0.56, -0.52 + lowered * -EQUIP_DROP, -0.72)
        * ry(45.0 + (p * p * PI).sin() * -20.0)
        * rz(reach * -20.0)
        * rx(reach * -80.0)
        * ry(-45.0)
}

/// A held item's model in the item's frame ([`minecraft_item`]): a block's
/// unit cube, or a flat item's unit quad (Minecraft's display transforms).
fn held_item_local(flat: bool) -> Transform {
    if flat {
        Transform {
            translation: HELD_FLAT_OFFSET_PX / 16.0,
            rotation: Quat::from_rotation_y(HELD_FLAT_YAW_DEG.to_radians())
                * Quat::from_rotation_z(HELD_FLAT_ROLL_DEG.to_radians()),
            scale: Vec3::splat(HELD_FLAT_SCALE),
        }
    } else {
        Transform {
            translation: Vec3::ZERO,
            rotation: Quat::from_rotation_y(HELD_BLOCK_YAW_DEG.to_radians()),
            scale: Vec3::splat(HELD_BLOCK_SCALE),
        }
    }
}

/// Shows the arm in first person and poses it: swing, equip, bob and sway;
/// with an item in the selected slot, the item in the arm's place.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub(crate) fn pose_hand(
    time: Res<Time>,
    fp: Res<FirstPerson>,
    mode: Res<ClientPlayMode>,
    policy: Res<CursorPolicy>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    items: Option<Res<HudItems>>,
    eyes: Query<(&Transform, &ViewFeel), (With<Eyes>, Without<Hand>)>,
    mut hands: Query<(Entity, &ChildOf, &mut Hand, &mut Transform, &mut Visibility)>,
    mut arm_models: Query<
        (&ChildOf, &mut Visibility),
        (With<ArmModel>, Without<Hand>, Without<HeldItem>),
    >,
    mut held_items: Query<
        (
            &ChildOf,
            &mut HeldItem,
            &mut Visibility,
            &mut Transform,
            &mut Mesh3d,
            &mut MeshMaterial3d<StandardMaterial>,
        ),
        (Without<Hand>, Without<Eyes>, Without<ArmModel>),
    >,
    mut pile_assets: Option<ResMut<ItemPileAssets>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let dt = time.delta_secs();
    let shown = mode.adventure() && fp.body_view() == Some(BodyView::First);
    // What the hands are doing, from the input the sim is sent (the same gate).
    let hands_free = shown && policy.look_live();
    let selected = items.as_ref().map(|i| {
        let s = i.hotbar.selected;
        (
            s,
            i.hotbar
                .slots
                .get(usize::from(s))
                .copied()
                .flatten()
                .map(|v| v.icon),
        )
    });
    let mining = items.as_ref().is_some_and(|i| i.mining.is_some());
    let holds_item = selected.is_some_and(|(_, item)| item.is_some());
    let starts_swing = hands_free
        && (buttons.just_pressed(MouseButton::Left)
            || (buttons.just_pressed(MouseButton::Right) && holds_item)
            || keys.just_pressed(KeyCode::KeyQ));
    // Held: mining swings on, and so does building (the sim places again
    // every `game::edit::PLACE_REPEAT_TICKS`).
    let keeps_swinging = hands_free
        && ((mining && buttons.pressed(MouseButton::Left))
            || (holds_item && buttons.pressed(MouseButton::Right)));

    // The item drawn in the hand (none without the piles' materials).
    let item = selected
        .and_then(|(_, item)| item)
        .filter(|_| pile_assets.is_some());

    for (hand_entity, parent, mut hand, mut tf, mut visibility) in &mut hands {
        let want = if shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != want {
            *visibility = want;
        }
        let Ok((eye_tf, feel)) = eyes.get(parent.parent()) else {
            continue;
        };
        if !shown {
            // Raised from below the next time it shows.
            hand.lowered = 1.0;
            hand.swing = None;
            hand.trail = None;
            continue;
        }

        // Swing.
        if let Some(t) = hand.swing.as_mut() {
            *t += dt;
        }
        if hand.swing.is_some_and(|t| t >= SWING_SECS) {
            hand.swing = None;
        }
        if starts_swing || (keeps_swinging && hand.swing.is_none()) {
            // A new press restarts a swing past its half, as Minecraft does.
            if hand.swing.is_none_or(|t| t >= SWING_SECS / 2.0) {
                hand.swing = Some(0.0);
            }
        }

        // The held item replaces the arm's model, at once (no equip drop).
        for (of, mut arm_visibility) in &mut arm_models {
            let want = if item.is_some() {
                Visibility::Hidden
            } else {
                Visibility::Inherited
            };
            if of.parent() == hand_entity && *arm_visibility != want {
                *arm_visibility = want;
            }
        }
        for (of, mut held, mut item_visibility, mut item_tf, mut mesh, mut material) in
            &mut held_items
        {
            if of.parent() != hand_entity {
                continue;
            }
            let want = if item.is_some() {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
            if *item_visibility != want {
                *item_visibility = want;
            }
            if held.shown == item {
                continue;
            }
            held.shown = item;
            if let (Some(icon), Some(assets)) = (item, pile_assets.as_mut()) {
                // `IconId::of` is the item's id.
                let (handle, flat) = material_for(
                    assets,
                    &mut images,
                    &mut materials,
                    game::ItemId(icon.0 as u16),
                );
                mesh.0 = if flat {
                    held.quad.clone()
                } else {
                    held.cube.clone()
                };
                material.0 = handle;
                *item_tf = held_item_local(flat);
            }
        }
        // The rise into the view.
        hand.lowered = smooth(hand.lowered, 0.0, dt, EQUIP_EASE_SECS);
        if hand.lowered < 1e-3 {
            hand.lowered = 0.0;
        }

        // Sway: the look (without the view's own bob tilt, which is tiny).
        let (yaw, pitch, _) = eye_tf.rotation.to_euler(EulerRot::YXZ);
        let (trail_yaw, trail_pitch) = hand.trail.unwrap_or((yaw, pitch));
        let wrap = |a: f32| (a + PI).rem_euclid(2.0 * PI) - PI;
        let trail_yaw = trail_yaw + wrap(yaw - trail_yaw) * (1.0 - (-dt / SWAY_EASE_SECS).exp());
        let trail_pitch = smooth(trail_pitch, pitch, dt, SWAY_EASE_SECS);
        hand.trail = Some((trail_yaw, trail_pitch));
        let lag_yaw = (-wrap(yaw - trail_yaw) * SWAY_GAIN).clamp(-SWAY_MAX_RAD, SWAY_MAX_RAD);
        let lag_pitch = (-(pitch - trail_pitch) * SWAY_GAIN).clamp(-SWAY_MAX_RAD, SWAY_MAX_RAD);
        let sway = Quat::from_rotation_y(lag_yaw) * Quat::from_rotation_x(lag_pitch);

        // Bob: the view's step bob once more, on the arm.
        let (bob_offset, bob_tilt) = feel
            .bob_pose()
            .map_or((Vec3::ZERO, Quat::IDENTITY), |(f, b)| bob_at(f, b));

        // Minecraft's order: the bob, the sway, then the arm — all about the
        // eye, scaled to [`HAND_SCALE`] there.
        let swing = hand.swing.map_or(0.0, |t| t / SWING_SECS);
        let matrix = Mat4::from_scale(Vec3::splat(HAND_SCALE * UNITS_PER_METER))
            * Mat4::from_rotation_translation(bob_tilt, bob_offset)
            * Mat4::from_quat(sway)
            * if item.is_some() {
                minecraft_item(swing, hand.lowered)
            } else {
                minecraft_arm(swing, hand.lowered)
            };
        let pose = Transform::from_matrix(matrix);
        if *tf != pose {
            *tf = pose;
        }
    }
}
