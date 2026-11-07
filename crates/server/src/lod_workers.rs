//! Native-server LOD build workers: a fixed pool of threads consuming
//! `LodBuildJob`s so a cache-miss (worst case a full region generation)
//! never stalls the manager thread's event routing.

use crossbeam::channel::{unbounded, Sender};
use game::{build_lod_job, LodBuildJob, LodBuilder, LodResult, NoStoredTerrain, StoredTerrain};

/// Floor on the default worker count (and the old fixed value).
pub const LOD_BUILD_THREADS: usize = 2;
/// Ceiling on the default worker count. Past the point where the client's own
/// near meshing takes as long as the LOD fill, more workers buy nothing.
pub const MAX_DEFAULT_LOD_BUILD_THREADS: usize = 6;
/// The default takes one core in this many: LOD fill is background work, and
/// region sim threads, the network edge and (on a dev box) the client keep
/// the rest.
pub const LOD_BUILD_CORE_SHARE: usize = 4;

/// Default worker count for a machine with `cores` logical cores.
///
/// Why it scales: a production-scale join queues ~570 builds (~72 factor-2/4
/// keys at ~0.22 s, ~120 factor-16 keys at ~0.09 s — both dominated by a full
/// region generation — and ~380 cheap tile keys): ~34 worker-seconds. With
/// the old fixed two workers that paced `world_ready` at ~17 s while the
/// server sat at 55% of ONE core on a 20-thread machine (2026-09-20 capture).
/// `LOD_BUILD_THREADS` in the environment overrides it either way.
pub fn default_lod_build_threads(cores: usize) -> usize {
    (cores / LOD_BUILD_CORE_SHARE).clamp(LOD_BUILD_THREADS, MAX_DEFAULT_LOD_BUILD_THREADS)
}

/// Resolve the optional `LOD_BUILD_THREADS` environment value.
pub fn lod_build_threads(value: Option<&str>) -> Result<usize, String> {
    let Some(value) = value else {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        return Ok(default_lod_build_threads(cores));
    };
    value
        .parse::<usize>()
        .ok()
        .filter(|&threads| threads >= 1)
        .ok_or_else(|| format!("invalid LOD_BUILD_THREADS value `{value}` (expected integer >= 1)"))
}

pub struct ThreadLodBuilder {
    jobs: Sender<LodBuildJob>,
    /// Built with a world store: saved regions' LOD is read from it.
    reads_stored: bool,
}

impl ThreadLodBuilder {
    /// `coarse` must be Clone-able across workers — pass seed + materials
    /// and rebuild the closure per thread. `gen_cache` is the same worldgen
    /// disk cache the manager uses for region starts; each worker gets its
    /// own handle and generates pristine LOD sources through it
    /// (`LodBuildJob::Pristine`). `stored` (the world store, if any) serves
    /// `LodBuildJob::Stored` and stored tile overlays.
    pub fn new(
        seed: u64,
        materials: worldgen::material::Materials,
        gen_cache: worldgen::cache::WorldgenCache,
        stored: Option<std::sync::Arc<dyn StoredTerrain>>,
        out: Sender<LodResult>,
        threads: usize,
    ) -> Self {
        let reads_stored = stored.is_some();
        let stored: std::sync::Arc<dyn StoredTerrain> = stored.unwrap_or_else(|| std::sync::Arc::new(NoStoredTerrain));
        let (jobs, job_recv) = unbounded::<LodBuildJob>();
        for i in 0..threads {
            let recv = job_recv.clone();
            let out = out.clone();
            let gen_cache = gen_cache.clone();
            let stored = stored.clone();
            std::thread::Builder::new()
                .name(format!("lod-worker-{i}"))
                .spawn(move || {
                    let coarse =
                        move |key: &game::lod::LodKey| worldgen::coarse_tile(key, seed, materials);
                    // The water layer comes from the recipe on every job
                    // kind (never from chunks): water plan Task 8.
                    let water =
                        move |key: &game::lod::LodKey| worldgen::coarse_water(key, seed, materials);
                    let generate =
                        move |rc: game::RegionCoords| gen_cache.generate_cached(rc, seed, materials);
                    while let Ok(job) = recv.recv() {
                        let started = std::time::Instant::now();
                        // The forged tiles the job reads, pinned for its
                        // length; one that will not load drops the job.
                        let Some(_pins) = prefetch_job(&job) else { continue };
                        let result = build_lod_job(job, &coarse, &water, &generate, stored.as_ref());
                        crate::perf::EDGE.record_lod_build(started.elapsed());
                        let _ = out.send(result);
                    }
                })
                .expect("spawn lod worker");
        }
        Self { jobs, reads_stored }
    }
}

/// Loads and pins the forged-world tiles `job` can read: its key's coarse
/// terrain and water (`worldgen::prefetch_lod`) and, for a job that may
/// generate its region (`Pristine`, or `Stored` falling back to it), that
/// region (`worldgen::prefetch_region`). `None` (logged) when a tile will not
/// load: the job is dropped — never built from missing tiles. The key then
/// stays in the manager's in-flight set, so it is not rebuilt in this process
/// (the client keeps its request outstanding; that key's terrain is missing).
fn prefetch_job(job: &LodBuildJob) -> Option<Vec<worldgen::forged_tiles::TilePins>> {
    prefetch_job_with(job, &worldgen::prefetch_lod, &worldgen::prefetch_region)
}

type Pinned = Result<worldgen::forged_tiles::TilePins, String>;

/// [`prefetch_job`] over explicit prefetches.
fn prefetch_job_with(
    job: &LodBuildJob,
    lod: &dyn Fn(&game::lod::LodKey) -> Pinned,
    region: &dyn Fn(game::RegionCoords) -> Pinned,
) -> Option<Vec<worldgen::forged_tiles::TilePins>> {
    let (key, generates) = match job {
        LodBuildJob::Pristine { key, region } | LodBuildJob::Stored { key, region } => (key, Some(*region)),
        LodBuildJob::Region { key, .. } | LodBuildJob::Tile { key, .. } => (key, None),
    };
    let pinned = std::iter::once(lod(key)).chain(generates.map(region)).collect::<Result<Vec<_>, String>>();
    match pinned {
        Ok(pins) => Some(pins),
        Err(e) => {
            log::warn!("[server] LOD {key:?} dropped: its forged world tiles do not load: {e}");
            None
        }
    }
}

impl LodBuilder for ThreadLodBuilder {
    fn submit(&mut self, job: LodBuildJob) {
        let _ = self.jobs.send(job);
    }

    /// Worldgen for never-run regions runs on the workers, never on the
    /// manager thread.
    fn generates_pristine(&self) -> bool {
        true
    }

    /// With a world store, `Stored` jobs and tile overlays read it on the
    /// workers (pristine when it cannot serve a region).
    fn reads_stored(&self) -> bool {
        self.reads_stored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quarter of the machine, never fewer than the old fixed two, never
    /// more than six: LOD fill is background work next to region sim threads.
    #[test]
    fn default_lod_build_threads_is_a_clamped_quarter_of_the_cores() {
        for (cores, expected) in [(1, 2), (4, 2), (8, 2), (11, 2), (12, 3), (16, 4), (20, 5), (24, 6), (128, 6)] {
            assert_eq!(default_lod_build_threads(cores), expected, "{cores} cores");
        }
    }

    /// Every job pins its key; jobs that may generate their region pin it
    /// too; any failure drops the job.
    #[test]
    fn prefetch_job_pins_what_the_job_reads() {
        use game::lod::LodKey;
        let rc = game::RegionCoords::new(3, -2);
        let key = LodKey::for_region(2, rc);
        let ok_lod = |_: &LodKey| -> Pinned { Ok(Default::default()) };
        let bad_lod = |_: &LodKey| -> Pinned { Err("tile (1, 0)".into()) };
        let ok_region = |_: game::RegionCoords| -> Pinned { Ok(Default::default()) };
        let bad_region = |r: game::RegionCoords| -> Pinned {
            assert_eq!(r, rc);
            Err("tile (0, 0)".into())
        };
        let jobs = || {
            [
                (LodBuildJob::Pristine { key, region: rc }, true),
                (LodBuildJob::Stored { key, region: rc }, true),
                (LodBuildJob::Region { key, chunks: Vec::new() }, false),
                (LodBuildJob::Tile { key, overlays: Vec::new(), stored: vec![rc] }, false),
            ]
        };
        for (job, generates) in jobs() {
            assert_eq!(prefetch_job_with(&job, &ok_lod, &ok_region).map(|p| p.len()), Some(1 + generates as usize));
            assert!(prefetch_job_with(&job, &bad_lod, &ok_region).is_none(), "the key's tiles fail");
            assert_eq!(prefetch_job_with(&job, &ok_lod, &bad_region).is_none(), generates, "the region's tiles fail");
        }
    }

    #[test]
    fn lod_build_threads_defaults_and_validates() {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        assert_eq!(lod_build_threads(None), Ok(default_lod_build_threads(cores)));
        assert_eq!(lod_build_threads(Some("6")), Ok(6));
        for bad in ["0", "-1", "two", ""] {
            let error = lod_build_threads(Some(bad)).unwrap_err();
            assert!(error.contains("LOD_BUILD_THREADS"), "{error}");
        }
    }
}
