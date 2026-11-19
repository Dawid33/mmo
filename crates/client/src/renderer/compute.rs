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
    /// A sim chunk's coarse (block-scale) twin (`meshing::coarse`).
    CoarseMesh = 6,
    /// One static water tile (`water::mesh`).
    WaterMesh = 7,
}

impl JobKind {
    pub fn from_u8(tag: u8) -> Option<Self> {
        Some(match tag {
            1 => Self::NearMesh,
            2 => Self::LodMesh,
            3 => Self::DeriveBricks,
            4 => Self::BrickEdits,
            5 => Self::BodyMesh,
            6 => Self::CoarseMesh,
            7 => Self::WaterMesh,
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
        JobKind::CoarseMesh => go::<super::meshing::coarse::CoarseMeshJob>(input),
        JobKind::WaterMesh => go::<super::water::mesh::WaterMeshJob>(input),
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

