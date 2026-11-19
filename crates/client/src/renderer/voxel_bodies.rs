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
use bevy::tasks::{block_on, futures_lite::future, Task};
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

/// Marks a body entity drawn by its voxel model (set with every
/// [`PendingVoxelBody`], never removed): `avatar` draws such an
/// `EntityKind::Body` no placeholder box.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct VoxelBodyModel;

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

/// A detached body's model to mesh (a compute job, `renderer::compute`);
/// answers the centre of mass and each brick's records ([`BodyMesh`]).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct BodyMeshJob {
    pub model: VoxelModel,
    pub layers: BlockTextureLayers,
}

impl super::compute::ComputeJob for BodyMeshJob {
    type Output = ([f32; 3], Vec<([i32; 3], Vec<QuadRecord>)>);
    const KIND: super::compute::JobKind = super::compute::JobKind::BodyMesh;

    fn run(self) -> Self::Output {
        let BodyMesh { com, bricks } = mesh_body(&self.model, &self.layers);
        (com.to_array(), bricks)
    }
}

/// Start meshing every pending body's model on the compute pool.
pub fn spawn_voxel_body_meshes(
    mut commands: Commands,
    pending: Query<(Entity, &PendingVoxelBody)>,
    layers: Res<BlockTextureLayers>,
) {
    for (e, PendingVoxelBody(model)) in &pending {
        let job = BodyMeshJob { model: VoxelModel::clone(model), layers: layers.clone() };
        let task = super::compute::spawn(job, |(com, bricks)| BodyMesh { com: Vec3::from_array(com), bricks });
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

