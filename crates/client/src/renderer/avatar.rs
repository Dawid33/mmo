use bevy::prelude::*;

use super::bridge::{SimKind, SimTarget};
use super::voxel_bodies::VoxelBodyModel;
use game::EntityKind;

/// Lazily-created shared avatar assets (one mesh + material per kind for now).
#[derive(Resource, Default)]
pub struct AvatarAssets {
    player: Option<(Handle<Mesh>, Handle<StandardMaterial>)>,
    slime: Option<SlimeAssets>,
    body: Option<(Handle<Mesh>, Handle<StandardMaterial>)>,
}

/// Marks an `EntityKind::Body` drawn as the placeholder box (a body whose
/// shape the renderer does not know: the dev-dropped cube, or a voxel
/// body's halo mirror before its arrival brings the model). Removed, with
/// the box, once the entity turns out to be a voxel body.
#[derive(Component, Debug)]
pub struct BodyBox;

/// Edge of a body's placeholder box, world units: one block, the dev
/// drop's cube (`game::body::dev_drop_body_safe`).
pub const BODY_BOX_EDGE: f32 = game::BLOCK_VOXELS as f32;

/// A unit cube plus the slime's materials; every part is the cube scaled.
#[derive(Clone)]
struct SlimeAssets {
    cube: Handle<Mesh>,
    gel: Handle<StandardMaterial>,
    core: Handle<StandardMaterial>,
    face: Handle<StandardMaterial>,
}

/// Attaches a renderable body to kind-bearing entities. Entities with an
/// active `Camera3d` are the local player — the first-person camera sits
/// inside the capsule, so the local player gets no mesh. A body that is a
/// voxel body (`VoxelBodyModel`) is drawn by `voxel_bodies`, any other one
/// as a [`BodyBox`] — taken off again if the model comes later.
pub fn attach_avatars(
    mut commands: Commands,
    added: Query<
        (
            Entity,
            &SimKind,
            Has<super::rigs::SimAnimator>,
            Has<VoxelBodyModel>,
        ),
        (Added<SimKind>, Without<Camera3d>, Without<SlimeAvatar>),
    >,
    modelled: Query<Entity, (Added<VoxelBodyModel>, With<BodyBox>)>,
    mut assets: ResMut<AvatarAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for e in &modelled {
        commands
            .entity(e)
            .remove::<(BodyBox, Mesh3d, MeshMaterial3d<StandardMaterial>)>();
    }
    for (e, kind, rigged, modelled) in &added {
        match kind.0 {
            // A rigged player is drawn by `rigs` (its model), not a capsule.
            EntityKind::Player if rigged => {}
            // A person is always drawn by `rigs` (the dwarf), never a capsule.
            EntityKind::Person { .. } => {}
            // A voxel body: its model is its look (`voxel_bodies`).
            EntityKind::Body if modelled => {}
            // Any other body: a one-block box (the shape is not sent; the
            // only such body today is the dev-dropped cube).
            EntityKind::Body => {
                let (mesh, material) = assets
                    .body
                    .get_or_insert_with(|| {
                        (
                            meshes.add(Cuboid::from_length(BODY_BOX_EDGE)),
                            materials.add(StandardMaterial {
                                base_color: Color::srgb(0.62, 0.48, 0.32),
                                ..Default::default()
                            }),
                        )
                    })
                    .clone();
                commands
                    .entity(e)
                    .insert((BodyBox, Mesh3d(mesh), MeshMaterial3d(material)));
            }
            EntityKind::Player => {
                let (mesh, material) = assets
                    .player
                    .get_or_insert_with(|| {
                        (
                            // Mirrors the sim capsule in create_player_safe
                            // (Capsule3d takes the full cylinder length).
                            meshes.add(Capsule3d::new(
                                game::movement::PLAYER_CAPSULE_RADIUS,
                                2.0 * game::movement::PLAYER_CAPSULE_HALF_HEIGHT,
                            )),
                            materials.add(StandardMaterial {
                                base_color: Color::srgb(0.8, 0.3, 0.3),
                                ..Default::default()
                            }),
                        )
                    })
                    .clone();
                commands
                    .entity(e)
                    .insert((Mesh3d(mesh), MeshMaterial3d(material)));
            }
            EntityKind::Slime { size } => {
                let a = assets
                    .slime
                    .get_or_insert_with(|| SlimeAssets {
                        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
                        gel: materials.add(StandardMaterial {
                            base_color: Color::srgba(0.45, 0.85, 0.35, 0.5),
                            alpha_mode: AlphaMode::Blend,
                            perceptual_roughness: 0.25,
                            ..Default::default()
                        }),
                        core: materials.add(StandardMaterial {
                            base_color: Color::srgb(0.3, 0.68, 0.25),
                            ..Default::default()
                        }),
                        face: materials.add(StandardMaterial {
                            base_color: Color::srgb(0.06, 0.12, 0.06),
                            unlit: true,
                            ..Default::default()
                        }),
                    })
                    .clone();
                spawn_slime_body(&mut commands, e, size, &a);
            }
        }
    }
}

/// Marks an entity whose slime body is already built, so a kind re-sent by
/// a rollback does not build a second one.
#[derive(Component)]
pub struct SlimeAvatar;

/// Minecraft's slime model, in fractions of the cube edge, under a pivot at
/// the feet that the squish scales: a translucent 8-px gel cube around an
/// opaque 6-px core, with eyes and a mouth on the core's front (−Z, the
/// sim's forward). Each part is (centre, size, is-face).
const SLIME_PARTS: [([f32; 3], [f32; 3], bool); 5] = [
    ([0.0, 0.5, 0.0], [1.0, 1.0, 1.0], false),       // gel
    ([0.0, 0.5, 0.0], [0.75, 0.75, 0.75], false),    // core
    ([-0.19, 0.6, -0.39], [0.19, 0.19, 0.06], true), // eye
    ([0.19, 0.6, -0.39], [0.19, 0.19, 0.06], true),  // eye
    ([0.1, 0.34, -0.39], [0.1, 0.1, 0.06], true),    // mouth
];

fn spawn_slime_body(commands: &mut Commands, e: Entity, size: u8, a: &SlimeAssets) {
    let edge = game::slime::slime_edge_units(size);
    commands
        .entity(e)
        .insert(SlimeAvatar)
        .with_children(|parent| {
            parent
                .spawn((
                    // The sim pose is the body centre; the pivot sits at the feet.
                    Transform::from_xyz(0.0, -edge / 2.0, 0.0).with_scale(Vec3::splat(edge)),
                    Visibility::default(),
                    SlimeSquish::new(size),
                ))
                .with_children(|pivot| {
                    for (i, (centre, dims, face)) in SLIME_PARTS.iter().enumerate() {
                        let material = match (i, face) {
                            (0, _) => a.gel.clone(),
                            (_, false) => a.core.clone(),
                            (_, true) => a.face.clone(),
                        };
                        pivot.spawn((
                            Mesh3d(a.cube.clone()),
                            MeshMaterial3d(material),
                            Transform::from_translation(Vec3::from(*centre))
                                .with_scale(Vec3::from(*dims)),
                        ));
                    }
                });
        });
}

// Minecraft's squish spring (RenderSlime / EntitySlime), per 50 ms tick —
// the same period as a Minecraft tick — applied per frame by `dt / TICK_S`.
/// Squish target on landing (squat) and on take-off (stretch).
pub const SQUISH_LAND: f32 = -0.5;
pub const SQUISH_TAKE_OFF: f32 = 1.0;
/// Per-tick decay of the target back to rest.
pub const SQUISH_TARGET_DECAY: f32 = 0.6;
/// Per-tick fraction of the gap to the target the squish closes.
pub const SQUISH_FOLLOW: f32 = 0.5;
/// Vertical sim-pose change, in world units per tick, that counts as moving.
pub const SQUISH_MOVE_EPS: f32 = 0.05;
const TICK_S: f32 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HopPhase {
    Ground,
    Rising,
    Falling,
}

/// Render-only squash-and-stretch state of one slime. Take-off and landing
/// are read off the interpolation target's vertical motion, so nothing about
/// it lives in (or rolls back with) the sim.
#[derive(Component, Debug)]
pub struct SlimeSquish {
    size: u8,
    squish: f32,
    target: f32,
    phase: HopPhase,
    last_y: Option<f32>,
}

impl SlimeSquish {
    fn new(size: u8) -> Self {
        Self {
            size,
            squish: 0.0,
            target: 0.0,
            phase: HopPhase::Ground,
            last_y: None,
        }
    }

    /// Feed one sim pose update (once per tick): detects take-off/landing.
    fn observe(&mut self, y: f32) {
        let Some(last) = self.last_y.replace(y) else {
            return;
        };
        let dy = y - last;
        self.phase = match self.phase {
            HopPhase::Ground if dy > SQUISH_MOVE_EPS => {
                self.target = SQUISH_TAKE_OFF;
                HopPhase::Rising
            }
            HopPhase::Rising if dy < -SQUISH_MOVE_EPS => HopPhase::Falling,
            HopPhase::Falling if dy > -SQUISH_MOVE_EPS => {
                self.target = SQUISH_LAND;
                HopPhase::Ground
            }
            p => p,
        };
    }

    /// Advance the spring by `dt` seconds.
    fn advance(&mut self, dt: f32) {
        let k = dt / TICK_S;
        self.target *= SQUISH_TARGET_DECAY.powf(k);
        self.squish += (self.target - self.squish) * (1.0 - (1.0 - SQUISH_FOLLOW).powf(k));
        // Settle exactly, so a resting slime stops rewriting its transform.
        if self.target.abs() < 1e-4 && self.squish.abs() < 1e-4 {
            (self.target, self.squish) = (0.0, 0.0);
        }
    }

    /// Volume-preserving (x, y, z) scale for the current squish; big slimes
    /// wobble less (Minecraft divides by `size / 2 + 1`).
    fn scale(&self) -> Vec3 {
        let f = self.squish / (self.size as f32 * 0.5 + 1.0);
        let s = 1.0 / (f + 1.0);
        Vec3::new(s, 1.0 / s, s)
    }
}

/// Squash and stretch: reads each slime's sim pose updates, runs the spring,
/// scales the pivot. After `interpolate_transforms`, which leaves child
/// transforms alone.
pub fn squish_slimes(
    time: Res<Time>,
    parents: Query<Ref<SimTarget>>,
    mut pivots: Query<(&mut Transform, &mut SlimeSquish, &ChildOf)>,
) {
    let dt = time.delta_secs();
    for (mut tf, mut squish, child_of) in &mut pivots {
        if let Ok(target) = parents.get(child_of.parent()) {
            // The bridge rewrites the target on every pose update, so a
            // change is one sim tick even when the pose itself held still.
            if target.is_changed() {
                squish.observe(target.pos.y);
            }
        }
        squish.advance(dt);
        let edge = game::slime::slime_edge_units(squish.size);
        tf.set_if_neq(Transform {
            scale: squish.scale() * edge,
            ..*tf
        });
    }
}
