//! Native-server LOD build workers: a fixed pool of threads consuming
//! `LodBuildJob`s so a cache-miss (worst case a full region generation)
//! never stalls the manager thread's event routing.

use crossbeam::channel::{unbounded, Sender};
use game::{build_lod_job, LodBuildJob, LodBuilder, LodResult};

/// Named constant per the tunables convention. Two threads: LOD fill is
/// background work; region sim threads keep priority on cores.
pub const LOD_BUILD_THREADS: usize = 2;

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
    ) -> Self {
        let (jobs, job_recv) = unbounded::<LodBuildJob>();
        for i in 0..LOD_BUILD_THREADS {
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
