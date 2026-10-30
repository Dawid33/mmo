//! LOD build execution. `WorldManager` never blocks on a build: it submits
//! `LodBuildJob`s to a `LodBuilder` and consumes results from a channel.
//! `InlineLodBuilder` computes synchronously (wasm/tests); the native
//! server wraps a small thread pool (crates/server/src/lod_workers.rs).

use std::collections::BTreeMap;

use crossbeam::channel::Sender;

use crate::lod::{
    copy_region_cells_into_tile, downsample_chunks, LodCell, LodKey, PackedLodGrid,
};
use crate::{Chunk, ChunkCoords, Column, ColumnCoords, LodPayload, RegionCoords, TerrainSnapshot, Voxel};

pub type LodResult = (LodKey, LodPayload);
pub type CoarseGenerator = Box<dyn Fn(&LodKey) -> Vec<LodCell> + Send>;
/// Full-resolution worldgen for one region, run by a builder that
/// [`generates_pristine`](LodBuilder::generates_pristine) terrain itself.
pub type PristineGenerator = Box<dyn Fn(RegionCoords) -> Vec<(ColumnCoords, Column)> + Send>;

#[derive(Debug)]
pub enum LodBuildJob {
    /// Sub-block factors (below `COARSE_MIN_FACTOR`): downsample this region's chunks
    /// (authoritative when the region has state, generated otherwise —
    /// the manager resolves the source, the builder just computes).
    Region {
        key: LodKey,
        chunks: Vec<(ChunkCoords, Chunk)>,
    },
    /// Sub-block factors for a region with NO authoritative state (never
    /// run, not parked): the builder generates the terrain itself, then
    /// downsamples it. Only submitted to builders that
    /// [`generates_pristine`](LodBuilder::generates_pristine) — worldgen costs
    /// hundreds of ms per region, and on the manager thread that is routing
    /// latency for every client (measured 24 calls / 8.7 s for one LOD ring,
    /// tripping the client's keep-up warning).
    Pristine { key: LodKey, region: RegionCoords },
    /// Block-scale and coarser factors (`COARSE_MIN_FACTOR` and up):
    /// coarse-generate the key, then overlay each region that has
    /// authoritative state. Pristine regions never pay for full generation
    /// here, which is what keeps the wide outer rings affordable.
    Tile {
        key: LodKey,
        overlays: Vec<(RegionCoords, Vec<(ChunkCoords, Chunk)>)>,
    },
}

pub trait LodBuilder: Send {
    fn submit(&mut self, job: LodBuildJob);

    /// Whether this builder accepts [`LodBuildJob::Pristine`], i.e. runs
    /// worldgen off the manager thread. `false` keeps the manager generating
    /// (and LRU-caching) pristine terrain itself — right for a synchronous
    /// builder, which has no other thread to offload to.
    fn generates_pristine(&self) -> bool {
        false
    }
}

/// `generate` argument for builders that never receive a `Pristine` job.
pub fn no_pristine_generator(region: RegionCoords) -> Vec<(ColumnCoords, Column)> {
    unreachable!("Pristine LOD job for {region:?} reached a builder that does not generate pristine terrain")
}

/// The pure build. Both builder impls call this.
pub fn build_lod_job(
    job: LodBuildJob,
    coarse: &dyn Fn(&LodKey) -> Vec<LodCell>,
    generate: &dyn Fn(RegionCoords) -> Vec<(ColumnCoords, Column)>,
) -> LodResult {
    match job {
        LodBuildJob::Pristine { key, region } => {
            // Same materialization the manager used to do before submitting a
            // `Region` job, so the payload is bit-identical.
            let chunks = TerrainSnapshot::from_columns(generate(region)).chunks;
            let cells = downsample_chunks(chunks, key.factor);
            (key, encode_cells(cells))
        }
        LodBuildJob::Region { key, chunks } => {
            let cells = downsample_chunks(chunks, key.factor);
            (key, encode_cells(cells))
        }
        LodBuildJob::Tile { key, overlays } if key.region_span() == 1 => {
            // One region, possibly several grids per band (factor 16). The
            // overlay is authoritative for every grid COLUMN it has chunks in;
            // columns it lacks keep the pristine coarse terrain. A region
            // covers every column (same as replacing the key).
            let cells = match overlays.into_iter().next() {
                Some((_, chunks)) => {
                    let authoritative = downsample_chunks(chunks, key.factor);
                    let covered: std::collections::BTreeSet<(i32, i32)> =
                        authoritative.iter().map(|c| (c.group[0], c.group[2])).collect();
                    let mut cells: Vec<LodCell> = coarse(&key)
                        .into_iter()
                        .filter(|c| !covered.contains(&(c.group[0], c.group[2])))
                        .collect();
                    cells.extend(authoritative);
                    cells
                }
                None => coarse(&key),
            };
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
        let _ = self.out.send(build_lod_job(job, &self.coarse, &no_pristine_generator));
    }
}
