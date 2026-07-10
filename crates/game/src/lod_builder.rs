//! LOD build execution. `WorldManager` never blocks on a build: it submits
//! `LodBuildJob`s to a `LodBuilder` and consumes results from a channel.
//! `InlineLodBuilder` computes synchronously (wasm/tests); the native
//! server wraps a small thread pool (crates/server/src/lod_workers.rs).

use std::collections::BTreeMap;

use crossbeam::channel::Sender;

use crate::lod::{
    copy_region_cells_into_tile, downsample_chunks, LodCell, LodKey, PackedLodGrid,
};
use crate::{Chunk, ChunkCoords, LodPayload, RegionCoords, Voxel};

pub type LodResult = (LodKey, LodPayload);
pub type CoarseGenerator = Box<dyn Fn(&LodKey) -> Vec<LodCell> + Send>;

#[derive(Debug)]
pub enum LodBuildJob {
    /// Region-keyed factors (2/4/16): downsample this region's chunks
    /// (authoritative when the region has state, generated otherwise —
    /// the manager resolves the source, the builder just computes).
    Region {
        key: LodKey,
        chunks: Vec<(ChunkCoords, Chunk)>,
    },
    /// Tile-keyed factors (64/256): coarse-generate the tile, then overlay
    /// each region that has authoritative state.
    Tile {
        key: LodKey,
        overlays: Vec<(RegionCoords, Vec<(ChunkCoords, Chunk)>)>,
    },
}

pub trait LodBuilder: Send {
    fn submit(&mut self, job: LodBuildJob);
}

/// The pure build. Both builder impls call this.
pub fn build_lod_job(job: LodBuildJob, coarse: &dyn Fn(&LodKey) -> Vec<LodCell>) -> LodResult {
    match job {
        LodBuildJob::Region { key, chunks } => {
            let cells = downsample_chunks(chunks, key.factor);
            (key, encode_cells(cells))
        }
        LodBuildJob::Tile { key, overlays } => {
            let mut tile: BTreeMap<[i32; 3], Vec<Voxel>> =
                coarse(&key).into_iter().map(|c| (c.group, c.grid)).collect();
            for (rc, chunks) in overlays {
                let cells = downsample_chunks(chunks, key.factor);
                copy_region_cells_into_tile(&mut tile, &cells, rc, &key);
            }
            let cells = tile
                .into_iter()
                .map(|(group, grid)| LodCell { group, grid })
                .collect();
            (key, encode_cells(cells))
        }
    }
}

fn encode_cells(cells: Vec<LodCell>) -> LodPayload {
    cells
        .into_iter()
        // All-air grids render nothing; don't ship them.
        .filter(|c| c.grid.iter().any(|v| !v.is_air()))
        .map(|c| (c.group, PackedLodGrid::encode(&c.grid)))
        .collect()
}

/// Synchronous builder: computes on submit and pushes straight to `out`.
pub struct InlineLodBuilder {
    pub coarse: CoarseGenerator,
    pub out: Sender<LodResult>,
}

impl LodBuilder for InlineLodBuilder {
    fn submit(&mut self, job: LodBuildJob) {
        let _ = self.out.send(build_lod_job(job, &self.coarse));
    }
}
