//! Voxel bodies (`game::voxel_body`: terrain detached into one falling,
//! tumbling rigid body — a felled tree's part) drawn on the pulled render
//! path.
//!
//! Shape: the body is an ordinary sim entity (`SimEntity`, `Transform`,
//! `SimTarget::body`), so `interpolate_transforms` glides it between sim
//! poses, rotation included, and `RemoveEntity` despawns it. Its model is
//! fanned out into `BRICK_VOXELS`³ bricks, each a `ChildOf(body)` child with
//! [`VoxelBodyBrick`], `ChunkQuads` and a constant brick `Aabb`, so the body's
//! `Transform` carries them for free. The body's pose is at the model's
//! centre of mass (`AddVoxelBody`'s contract): model brick `b` sits at
//! body-local `b * BRICK_VOXELS - local_com`, with `local_com` from
//! `game::model_mass_properties` — the same pure function the sim placed the
//! body with.
//!
//! A body's bricks are NOT terrain: they hold no `VoxelData`, so
//! `mark_remesh_dirty` never keys them into the `ChunkGrid` (where they would
//! pad their terrain neighbours' meshes) and cave culling never sees them.
//! They are meshed here instead, off the main thread, each brick padded from
//! the model's own neighbouring voxels (air beyond the model), so faces and
//! AO across the body's internal brick seams match one solid piece.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};
use game::{BlockId, Voxel, VoxelModel, BLOCK_VOXELS, BLOCK_VOXEL_COUNT, BRICK_BLOCKS, BRICK_VOXELS};

use super::block_textures::BlockTextureLayers;
use super::meshing::binary::{PaddedVoxels, PADDED};
use super::meshing::mesh_chunk;
use super::meshing::records::QuadRecord;
use super::voxel_pull::ChunkQuads;

/// A body entity whose model still has to be meshed; set by the bridge
/// (`AddVoxelBody`, or a snapshot holding a body). A newer one replaces it.
#[derive(Component, Clone)]
pub struct PendingVoxelBody(pub Arc<VoxelModel>);

/// The body's in-flight mesh build. Replacing it cancels the older build.
#[derive(Component)]
pub struct VoxelBodyMeshing(Task<BodyMesh>);

/// A render brick of a voxel body (a child of the body entity): which brick
/// of the model, in brick units from the model origin.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoxelBodyBrick {
    pub brick: [i32; 3],
}

/// A meshed model: its centre of mass (model units) and each brick's quads.
pub struct BodyMesh {
    pub com: Vec3,
    pub bricks: Vec<([i32; 3], Vec<QuadRecord>)>,
}

/// Half a brick's edge: every brick's `Aabb` is its local 0..BRICK_VOXELS cube.
const BRICK_HALF: f32 = BRICK_VOXELS as f32 / 2.0;

/// One model block expanded to voxels.
enum BlockVoxels {
    Uniform(BlockId),
    /// `BLOCK_VOXEL_COUNT` materials in chisel grid order (x fastest).
    Grid(Vec<BlockId>),
}

/// The model as a voxel lookup over model voxel coordinates.
struct ModelVoxels(BTreeMap<[i32; 3], BlockVoxels>);

impl ModelVoxels {
    fn new(model: &VoxelModel) -> Self {
        let blocks = model
            .blocks
            .iter()
            .map(|(o, (block, chisel))| {
                let voxels = match chisel {
                    Some(c) => BlockVoxels::Grid(c.materials()),
                    None => BlockVoxels::Uniform(block.id),
                };
                (o.map(i32::from), voxels)
            })
            .collect();
        Self(blocks)
    }

    fn at(&self, v: [i32; 3]) -> Voxel {
        let b = BLOCK_VOXELS as i32;
        let Some(block) = self.0.get(&v.map(|c| c.div_euclid(b))) else { return Voxel::default() };
        match block {
            BlockVoxels::Uniform(id) => Voxel::new(*id),
            BlockVoxels::Grid(m) => {
                let [x, y, z] = v.map(|c| c.rem_euclid(b) as usize);
                let i = x + BLOCK_VOXELS * (y + BLOCK_VOXELS * z);
                debug_assert!(i < BLOCK_VOXEL_COUNT);
                Voxel::new(m[i])
            }
        }
    }
}

/// The bricks a model's blocks fall in (brick units from the model origin).
pub fn model_bricks(model: &VoxelModel) -> BTreeSet<[i32; 3]> {
    let n = BRICK_BLOCKS as i32;
    model.blocks.keys().map(|o| o.map(|c| i32::from(c).div_euclid(n))).collect()
}

/// Brick `brick` of the model with a one-voxel shell read from the model
/// itself (air outside it).
fn padded_brick(model: &ModelVoxels, brick: [i32; 3]) -> PaddedVoxels {
    let base = brick.map(|c| c * BRICK_VOXELS as i32 - 1);
    let mut p = vec![Voxel::default(); PADDED * PADDED * PADDED];
    for z in 0..PADDED {
        for y in 0..PADDED {
            for x in 0..PADDED {
                p[PaddedVoxels::index(x, y, z)] = model.at([base[0] + x as i32, base[1] + y as i32, base[2] + z as i32]);
            }
        }
    }
    PaddedVoxels(p)
}

/// Mesh every brick of `model` as pulled quad records (bricks with no
/// visible face are left out). Pure; runs on the compute pool.
pub fn mesh_body(model: &VoxelModel, layers: &BlockTextureLayers) -> BodyMesh {
    let com = game::model_mass_properties(model).local_com;
    let voxels = ModelVoxels::new(model);
    let bricks = model_bricks(model)
        .into_iter()
        .filter_map(|b| {
            let out = mesh_chunk(&padded_brick(&voxels, b), layers, true);
            out.records.map(|r| (b, r))
        })
        .collect();
    BodyMesh { com: Vec3::new(com.x, com.y, com.z), bricks }
}

/// Body-local translation of brick `brick`: its model position less the
/// centre of mass the body's pose sits at.
pub fn brick_translation(brick: [i32; 3], com: Vec3) -> Vec3 {
    Vec3::from_array(brick.map(|c| (c * BRICK_VOXELS as i32) as f32)) - com
}

/// Start meshing every pending body's model on the compute pool.
pub fn spawn_voxel_body_meshes(
    mut commands: Commands,
    pending: Query<(Entity, &PendingVoxelBody)>,
    layers: Res<BlockTextureLayers>,
) {
    for (e, PendingVoxelBody(model)) in &pending {
        let model = Arc::clone(model);
        let layers = BlockTextureLayers(layers.0.clone());
        let task = AsyncComputeTaskPool::get().spawn(async move { mesh_body(&model, &layers) });
        commands.entity(e).remove::<PendingVoxelBody>().insert(VoxelBodyMeshing(task));
    }
}

/// Land finished body meshes: replace the body's brick children.
pub fn apply_voxel_body_meshes(
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut VoxelBodyMeshing, Option<&Children>)>,
    bricks: Query<(), With<VoxelBodyBrick>>,
) {
    for (e, mut task, children) in &mut tasks {
        let Some(mesh) = block_on(future::poll_once(&mut task.0)) else { continue };
        for &child in children.into_iter().flatten() {
            if bricks.contains(child) {
                commands.entity(child).despawn();
            }
        }
        commands.entity(e).remove::<VoxelBodyMeshing>();
        for (brick, records) in mesh.bricks {
            commands.spawn((
                VoxelBodyBrick { brick },
                Transform::from_translation(brick_translation(brick, mesh.com)),
                ChunkQuads(Arc::new(records)),
                Aabb { center: Vec3A::splat(BRICK_HALF), half_extents: Vec3A::splat(BRICK_HALF) },
                NoAutoAabb,
                ChildOf(e),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::bridge::{
        Regions, RegionRoots, RetiredRegionRoots, SimEntity, SimEntityMap, SimTarget, SnapshotVoxelQueue, VoxelData,
    };
    use crate::renderer::{ClientUpdates, GameEvents, LocalPlayer};
    use crate::renderer::meshing::queue::{ChunkGrid, ChunkKey, FrameUploads, RemeshQueue};
    use game::{
        Block, ClientUpdateEvent, EntityKey, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Pose, RegionCoords, Rollback,
    };
    use slotmapd::KeyData;

    fn key(n: u64) -> EntityKey {
        EntityKey::from(KeyData::from_ffi((1 << 32) | n))
    }

    type Updates = crossbeam::channel::Sender<GameDataUpdate>;

    /// Bridge drain + body meshing + the terrain remesh keying, headless.
    /// The client-update sender is returned so the channel stays open.
    fn app() -> (App, Updates, crossbeam::channel::Sender<ClientUpdateEvent>) {
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        let (game_send, _game_recv) = crossbeam::channel::unbounded();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(ClientUpdates(client_recv))
            .insert_resource(GameEvents(game_send))
            .insert_resource(BlockTextureLayers(Default::default()))
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<crate::renderer::hud::HudStatus>()
            .init_resource::<crate::renderer::slint_overlay::UiUpdates>()
            .init_resource::<crate::renderer::collider_debug::DebugColliderLines>()
            .init_resource::<crate::renderer::lod_rings::LodRings>()
            .init_resource::<SnapshotVoxelQueue>()
            .init_resource::<crate::renderer::cursor::CursorPolicy>()
            .init_resource::<RetiredRegionRoots>()
            .init_resource::<ChunkGrid>()
            .init_resource::<RemeshQueue>()
            .init_resource::<FrameUploads>()
            .add_systems(
                PreUpdate,
                (
                    crate::renderer::bridge::drain_client_updates,
                    crate::renderer::bridge::drain_region_updates,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    crate::renderer::meshing::queue::mark_remesh_dirty,
                    spawn_voxel_body_meshes,
                    apply_voxel_body_meshes,
                )
                    .chain(),
            );
        let (update_send, update_recv) = crossbeam::channel::unbounded();
        let rb = Rollback::new(None);
        client_send
            .send(ClientUpdateEvent::NewRegion(RegionCoords::new(0, 0), (*rb.data).clone(), update_recv))
            .unwrap();
        (app, update_send, client_send)
    }

    /// A log-like column: 1 × 3 × 1 blocks — two bricks tall.
    fn column_model() -> VoxelModel {
        let mut model = VoxelModel::default();
        for y in 0..3 {
            model.blocks.insert([0, y, 0], (Block::new(BlockId(1)), None));
        }
        model
    }

    fn body_bricks(app: &mut App, body: Entity) -> Vec<(Entity, VoxelBodyBrick)> {
        let mut q = app.world_mut().query::<(Entity, &VoxelBodyBrick, &ChildOf)>();
        q.iter(app.world()).filter(|(_, _, of)| of.parent() == body).map(|(e, b, _)| (e, *b)).collect()
    }

    fn wait_for(app: &mut App, mut done: impl FnMut(&mut App) -> bool) -> bool {
        for _ in 0..500 {
            app.update();
            if done(app) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        false
    }

    fn spawn_body(app: &mut App, updates: &Updates, k: EntityKey, pose: Pose) -> Entity {
        app.update();
        for kind in [
            GameDataUpdateKind::CreateEntity(k),
            GameDataUpdateKind::AddVoxelBody(k, pose, Box::new(column_model())),
        ] {
            updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, kind)).unwrap();
        }
        app.update();
        let region = RegionCoords::new(0, 0);
        *app.world().resource::<SimEntityMap>().0.get(&(region, k)).expect("body mapped")
    }

    #[test]
    fn voxel_body_spawns_brick_children_outside_chunk_grid() {
        let (mut app, updates, _client) = app();
        let pose = Pose::translation(100.0, 200.0, 300.0);
        let body = spawn_body(&mut app, &updates, key(7), pose);
        assert!(wait_for(&mut app, |app| body_bricks(app, body).len() == 2), "the column's two bricks never landed");

        let entity = app.world().entity(body);
        assert!(entity.contains::<SimEntity>());
        assert!(entity.contains::<SimTarget>(), "the body's pose is interpolated");
        assert_eq!(entity.get::<Transform>().unwrap().translation, Vec3::new(100.0, 200.0, 300.0));

        let com = game::model_mass_properties(&column_model()).local_com;
        let com = Vec3::new(com.x, com.y, com.z);
        assert_eq!(com, Vec3::new(8.0, 24.0, 8.0), "the column's centre: half a block across, 1.5 blocks up");
        let mut bricks = body_bricks(&mut app, body);
        bricks.sort_by_key(|(_, b)| b.brick);
        assert_eq!(bricks.iter().map(|(_, b)| b.brick).collect::<Vec<_>>(), vec![[0, 0, 0], [0, 1, 0]]);
        for (e, b) in bricks {
            let entity = app.world().entity(e);
            assert_eq!(
                entity.get::<Transform>().unwrap().translation,
                Vec3::from_array(b.brick.map(|c| (c * BRICK_VOXELS as i32) as f32)) - com,
            );
            assert!(!entity.get::<ChunkQuads>().expect("quads").0.is_empty());
            assert!(entity.contains::<Aabb>() && entity.contains::<NoAutoAabb>());
            assert!(!entity.contains::<VoxelData>(), "body bricks are not terrain voxels");
            assert!(!entity.contains::<ChunkKey>(), "body bricks are never keyed into the chunk grid");
        }
        assert!(app.world().resource::<ChunkGrid>().0.is_empty(), "nothing entered the chunk grid");
    }

    #[test]
    fn removing_the_body_despawns_its_bricks() {
        let (mut app, updates, _client) = app();
        let k = key(9);
        let body = spawn_body(&mut app, &updates, k, Pose::identity());
        assert!(wait_for(&mut app, |app| !body_bricks(app, body).is_empty()));
        let bricks: Vec<Entity> = body_bricks(&mut app, body).into_iter().map(|(e, _)| e).collect();

        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::RemoveEntity(k))).unwrap();
        app.update();
        assert!(app.world().get_entity(body).is_err(), "body despawned");
        for e in bricks {
            assert!(app.world().get_entity(e).is_err(), "brick despawned with its body");
        }
    }

    /// The internal seam between the column's two bricks draws no faces:
    /// the bricks are padded from each other, not from air.
    #[test]
    fn bricks_are_padded_from_the_model() {
        let mesh = mesh_body(&column_model(), &BlockTextureLayers(Default::default()));
        let quads: usize = mesh.bricks.iter().map(|(_, r)| r.len()).sum();
        // A 1×3×1 column greedy-merges to 6 faces, split by the brick seam
        // into the lower brick's 4 sides + bottom and the upper's 4 + top.
        assert_eq!(quads, 10);
    }

    #[test]
    fn rotated_aabb_contains_all_corners() {
        let aabb = Aabb { center: Vec3A::splat(BRICK_HALF), half_extents: Vec3A::splat(BRICK_HALF) };
        let tf = GlobalTransform::from(Transform {
            translation: Vec3::new(5.0, -7.0, 11.0),
            rotation: Quat::from_euler(EulerRot::YXZ, 0.7, -0.4, 1.1),
            scale: Vec3::ONE,
        });
        let (center, half) = crate::renderer::voxel_pull::world_aabb(&tf, &aabb);
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { 0.0 } else { 32.0 },
                if i & 2 == 0 { 0.0 } else { 32.0 },
                if i & 4 == 0 { 0.0 } else { 32.0 },
            );
            let p = tf.transform_point(corner);
            assert!((p - center).abs().cmple(half + 1e-3).all(), "corner {p} outside {center} ± {half}");
            lo = lo.min(p);
            hi = hi.max(p);
        }
        // Tight: exactly the corners' box.
        assert!((center - (lo + hi) / 2.0).abs().max_element() < 1e-3);
        assert!((half - (hi - lo) / 2.0).abs().max_element() < 1e-3);

        // Unrotated terrain keeps exactly `scale * half` around the moved centre.
        let lod = GlobalTransform::from(Transform::from_xyz(64.0, -2.0, 0.0).with_scale(Vec3::splat(2.0)));
        let (c, h) = crate::renderer::voxel_pull::world_aabb(&lod, &aabb);
        assert_eq!(c, Vec3::new(64.0 + 32.0, -2.0 + 32.0, 32.0));
        assert_eq!(h, Vec3::splat(32.0));
    }
}
