//! Compute jobs: the CPU work the renderer hands off the main thread
//! (meshing, brick derivation). Each kind is a [`ComputeJob`] — a serializable
//! input whose [`ComputeJob::run`] is a pure function to a serializable
//! output — so the same job can run on any executor:
//!
//! - native: Bevy's `AsyncComputeTaskPool` threads, the job value moved in as
//!   is (nothing is serialized);
//! - wasm: a Web Worker of the compute pool (`web_worker::pool`) — Bevy's
//!   pools are single-threaded there, so the bincode-encoded job is posted to
//!   another wasm instance, which runs [`run_encoded`] and posts the output
//!   back; with workers off it runs on the page as before.
//!
//! [`spawn`] returns an ordinary Bevy `Task`, so the polling systems
//! (`block_on(poll_once(..))`) are the same on every target. A new kind of
//! work goes in [`JobKind`] and [`run_encoded`]; its input must carry
//! everything it reads (a worker has none of the page's ECS or resources —
//! only the process-global block properties, which every worker installs).

use bevy::tasks::{AsyncComputeTaskPool, Task};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Every kind of compute job (the tag a worker dispatches on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JobKind {
    /// One near brick: gather its padded voxels and mesh them (`meshing::queue`).
    NearMesh = 1,
    /// One LOD cell grid (`meshing::lod`).
    LodMesh = 2,
    /// A region snapshot batch: derive each chunk's render bricks (`bridge`).
    DeriveBricks = 3,
    /// An edited chunk's dirty bricks (`bricks`).
    BrickEdits = 4,
    /// A detached voxel body's bricks (`voxel_bodies`).
    BodyMesh = 5,
}

impl JobKind {
    pub fn from_u8(tag: u8) -> Option<Self> {
        Some(match tag {
            1 => Self::NearMesh,
            2 => Self::LodMesh,
            3 => Self::DeriveBricks,
            4 => Self::BrickEdits,
            5 => Self::BodyMesh,
            _ => return None,
        })
    }
}

/// A unit of off-main-thread work (see the module doc).
pub trait ComputeJob: Serialize + DeserializeOwned + Send + 'static {
    type Output: Serialize + DeserializeOwned + Send + 'static;
    const KIND: JobKind;
    fn run(self) -> Self::Output;
}

/// Run `job` off the main thread; `then` turns its output into the task's
/// value (on native in the task, on wasm on the page when the worker's answer
/// lands — keep it cheap, e.g. wrapping vectors into a `Mesh`). Dropping the
/// task cancels it where the executor can (a worker finishes the job and its
/// answer is dropped).
pub fn spawn<J, T>(job: J, then: impl FnOnce(J::Output) -> T + Send + 'static) -> Task<T>
where
    J: ComputeJob,
    T: Send + 'static,
{
    #[cfg(target_arch = "wasm32")]
    let job = match crate::web_worker::pool::submit(job) {
        Ok(reply) => return AsyncComputeTaskPool::get().spawn(async move { then(reply.await) }),
        Err(job) => job,
    };
    AsyncComputeTaskPool::get().spawn(async move { then(job.run()) })
}

/// Whether [`spawn`] sends jobs to another wasm instance (so a job should
/// carry only what it reads rather than share a big `Arc`): wasm with the
/// compute pool on.
pub fn serializes() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        crate::web_worker::enabled() && crate::web_worker::compute_worker_count() > 0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

/// Decode a `kind` job from `input`, run it, encode its output — what a
/// compute worker does with each message.
pub fn run_encoded(kind: JobKind, input: &[u8]) -> Vec<u8> {
    fn go<J: ComputeJob>(input: &[u8]) -> Vec<u8> {
        let job: J = bincode::deserialize(input).expect("compute job decodes");
        bincode::serialize(&job.run()).expect("compute output encodes")
    }
    match kind {
        JobKind::NearMesh => go::<super::meshing::queue::NearMeshJob>(input),
        JobKind::LodMesh => go::<super::meshing::lod::LodMeshJob>(input),
        JobKind::DeriveBricks => go::<super::bridge::DeriveBricksJob>(input),
        JobKind::BrickEdits => go::<super::bricks::BrickEditsJob>(input),
        JobKind::BodyMesh => go::<super::voxel_bodies::BodyMeshJob>(input),
    }
}

/// Encode a job for [`run_encoded`] (the wasm pool's input; tests).
pub fn encode_job<J: ComputeJob>(job: &J) -> Vec<u8> {
    bincode::serialize(job).expect("compute job encodes")
}

/// Decode a [`run_encoded`] output.
pub fn decode_output<J: ComputeJob>(output: &[u8]) -> J::Output {
    bincode::deserialize(output).expect("compute output decodes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::block_textures::BlockTextureLayers;
    use crate::renderer::bridge::{DeriveBricksJob, VoxelData};
    use crate::renderer::meshing::queue::NearMeshJob;
    use game::{BlockId, ChunkCoords};

    /// A job answers the same through the worker path (encode, `run_encoded`,
    /// decode) as run in place.
    fn round_trip<J: ComputeJob>(job: J) -> J::Output {
        decode_output::<J>(&run_encoded(J::KIND, &encode_job(&job)))
    }

    fn layers() -> BlockTextureLayers {
        BlockTextureLayers([(BlockId(1), [3, 4, 5])].into_iter().collect())
    }

    fn floor_brick() -> VoxelData {
        VoxelData::new(game::Chunk::flat_floor(8).brick([0; 3]).voxels())
    }

    fn near_job(pull: bool) -> NearMeshJob {
        NearMeshJob {
            center: floor_brick(),
            neighbors: vec![([1, 0, 0], VoxelData::uniform(BlockId(1)))],
            layers: layers(),
            pull,
            interior: None,
        }
    }

    #[test]
    fn near_mesh_job_round_trips_both_render_modes() {
        for pull in [true, false] {
            let direct = near_job(pull).run();
            assert!(direct.quad_count > 0);
            assert_eq!(round_trip(near_job(pull)), direct, "pull={pull}");
        }
    }

    /// A snapshot with a 2x2x2 block of floor chunks: every chunk has
    /// snapshot neighbours to bury its face bricks against.
    fn snapshot() -> (game::GameData, Vec<ChunkCoords>) {
        let mut rb = game::Rollback::new(None);
        rb.new_transaction();
        let mut coords = Vec::new();
        for x in 0..2 {
            for y in 0..2 {
                for z in 0..2 {
                    let at = ChunkCoords::new(x, y, z);
                    let chunk = if y == 0 { game::Chunk::flat_floor(16) } else { game::Chunk::flat_floor(8) };
                    rb.create_mesh(at, chunk);
                    coords.push(at);
                }
            }
        }
        ((*rb.data).clone(), coords)
    }

    #[test]
    fn derive_bricks_from_a_gathered_part_matches_the_shared_snapshot() {
        let (data, coords) = snapshot();
        let data = std::sync::Arc::new(data);
        // One chunk of the batch: the part carries its neighbours outside it.
        let items = vec![coords[0], coords[3]];
        let shared = DeriveBricksJob::shared(data.clone(), items.clone()).run();
        assert!(shared.iter().all(Option::is_some));
        let part = crate::renderer::bricks::SnapshotPart::gather(&data, items.iter().copied());
        assert!(part.chunks.len() > items.len(), "face neighbours are gathered");
        assert_eq!(round_trip(DeriveBricksJob::part(part, items)), shared);
    }

    #[test]
    fn brick_edit_and_lod_jobs_round_trip() {
        let edits = crate::renderer::bricks::BrickEditsJob {
            source: game::Chunk::flat_floor(8),
            dirty: [[0, 0, 0], [1, 0, 1]].into_iter().collect(),
        };
        let direct = crate::renderer::bricks::BrickEditsJob { source: edits.source.clone(), dirty: edits.dirty.clone() }.run();
        assert_eq!(round_trip(edits), direct);

        let grid = game::lod::PackedLodGrid(vec![(4096, BlockId(1)), (28768, BlockId::AIR)]);
        let lod = || crate::renderer::meshing::lod::LodMeshJob {
            packed: grid.clone(),
            neighbors: Default::default(),
            layers: layers(),
            pull: true,
        };
        assert_eq!(round_trip(lod()), lod().run());
    }
}
