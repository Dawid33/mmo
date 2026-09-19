//! Native-server LOD build workers: a fixed pool of threads consuming
//! `LodBuildJob`s so a cache-miss (worst case a full region generation)
//! never stalls the manager thread's event routing.

use crossbeam::channel::{unbounded, Sender};
use game::{build_lod_job, LodBuildJob, LodBuilder, LodResult};

/// Default worker count. Two threads: LOD fill is background work; region sim
/// threads keep priority on cores. `LOD_BUILD_THREADS` in the environment
/// overrides it (a production-scale join queues ~570 builds, ~280 of them full
/// region generations, so this is the knob for join-to-horizon latency).
pub const LOD_BUILD_THREADS: usize = 2;

/// Resolve the optional `LOD_BUILD_THREADS` environment value.
pub fn lod_build_threads(value: Option<&str>) -> Result<usize, String> {
    let Some(value) = value else {
        return Ok(LOD_BUILD_THREADS);
    };
    value
        .parse::<usize>()
        .ok()
        .filter(|&threads| threads >= 1)
        .ok_or_else(|| format!("invalid LOD_BUILD_THREADS value `{value}` (expected integer >= 1)"))
}

pub struct ThreadLodBuilder {
    jobs: Sender<LodBuildJob>,
}

impl ThreadLodBuilder {
    /// `coarse` must be Clone-able across workers — pass seed + materials
    /// and rebuild the closure per thread.
    pub fn new(
        seed: u64,
        materials: worldgen::material::Materials,
        out: Sender<LodResult>,
        threads: usize,
    ) -> Self {
        let (jobs, job_recv) = unbounded::<LodBuildJob>();
        for i in 0..threads {
            let recv = job_recv.clone();
            let out = out.clone();
            std::thread::Builder::new()
                .name(format!("lod-worker-{i}"))
                .spawn(move || {
                    let coarse =
                        move |key: &game::lod::LodKey| worldgen::coarse_tile(key, seed, materials);
                    while let Ok(job) = recv.recv() {
                        let _ = out.send(build_lod_job(job, &coarse));
                    }
                })
                .expect("spawn lod worker");
        }
        Self { jobs }
    }
}

impl LodBuilder for ThreadLodBuilder {
    fn submit(&mut self, job: LodBuildJob) {
        let _ = self.jobs.send(job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lod_build_threads_defaults_and_validates() {
        assert_eq!(lod_build_threads(None), Ok(LOD_BUILD_THREADS));
        assert_eq!(lod_build_threads(Some("6")), Ok(6));
        for bad in ["0", "-1", "two", ""] {
            let error = lod_build_threads(Some(bad)).unwrap_err();
            assert!(error.contains("LOD_BUILD_THREADS"), "{error}");
        }
    }
}
