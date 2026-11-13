//! Native-server LOD build workers: a fixed pool of threads consuming
//! `LodBuildJob`s so a cache-miss (worst case a full region generation)
//! never stalls the manager thread's event routing.

use crossbeam::channel::{unbounded, Receiver, RecvTimeoutError, Sender};
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

/// First delay before a LOD job whose forged tiles did not load is tried
/// again; it doubles per failed attempt up to [`LOD_RETRY_MAX_MS`].
pub const LOD_RETRY_BASE_MS: u64 = 250;
/// Longest delay between attempts: a permanently bad tile is retried at this
/// pace for as long as the server runs (one prefetch per key per period).
pub const LOD_RETRY_MAX_MS: u64 = 30_000;

/// Capped exponential backoff of LOD jobs whose tiles failed to load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    pub base: std::time::Duration,
    pub max: std::time::Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            base: std::time::Duration::from_millis(LOD_RETRY_BASE_MS),
            max: std::time::Duration::from_millis(LOD_RETRY_MAX_MS),
        }
    }
}

impl Backoff {
    /// The wait after failed attempt number `attempt` (0 = the first).
    pub fn delay(&self, attempt: u32) -> std::time::Duration {
        let factor = 1u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
        self.base.saturating_mul(factor).min(self.max)
    }
}

/// A job on the worker queue, with how many times its tiles failed to load.
struct Queued {
    job: LodBuildJob,
    attempt: u32,
}

/// What the retry thread is sent.
enum Delayed {
    /// Put `Queued` back on the worker queue at the instant.
    Job(std::time::Instant, Queued),
    /// The builder is gone: drop what waits and exit.
    Stop,
}

pub struct ThreadLodBuilder {
    jobs: Sender<Queued>,
    /// The retry thread (stopped on drop: it holds a sender of `jobs`, which
    /// would otherwise keep the workers alive).
    retry: Sender<Delayed>,
    /// Built with a world store: saved regions' LOD is read from it.
    reads_stored: bool,
}

/// Loads and pins what a job reads (see [`prefetch_job`]).
type JobPrefetch = dyn Fn(&LodBuildJob) -> Result<Vec<worldgen::forged_tiles::TilePins>, String>;

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
        let (jobs, job_recv) = unbounded::<Queued>();
        let retry = spawn_retry_thread(jobs.clone());
        for i in 0..threads {
            let recv = job_recv.clone();
            let out = out.clone();
            let retry = retry.clone();
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
                    worker_loop(&recv, &retry, Backoff::default(), &prefetch_job, &mut |job| {
                        let started = std::time::Instant::now();
                        let result = build_lod_job(job, &coarse, &water, &generate, stored.as_ref());
                        crate::perf::EDGE.record_lod_build(started.elapsed());
                        result
                    }, &out);
                })
                .expect("spawn lod worker");
        }
        Self { jobs, retry, reads_stored }
    }
}

impl Drop for ThreadLodBuilder {
    fn drop(&mut self) {
        let _ = self.retry.send(Delayed::Stop);
    }
}

/// One worker: pins the job's forged tiles, builds it with the pins held and
/// sends the result. A job whose tiles do not load is never built from
/// missing tiles: it goes to the retry thread and comes back after
/// `backoff.delay(attempt)` (capped), so a store hiccup costs latency, not
/// the key (the manager keeps a key in flight until its result lands). The
/// first failure of a job is a warn, later ones debug.
fn worker_loop(
    recv: &Receiver<Queued>,
    retry: &Sender<Delayed>,
    backoff: Backoff,
    prefetch: &JobPrefetch,
    build: &mut dyn FnMut(LodBuildJob) -> LodResult,
    out: &Sender<LodResult>,
) {
    while let Ok(Queued { job, attempt }) = recv.recv() {
        let pins = match prefetch(&job) {
            Ok(pins) => pins,
            Err(e) => {
                let delay = backoff.delay(attempt);
                let key = job_key(&job);
                if attempt == 0 {
                    log::warn!("[server] LOD {key:?}: its forged world tiles do not load ({e}); retrying in {delay:?}, then backing off to {:?}", backoff.max);
                } else {
                    log::debug!("[server] LOD {key:?}: attempt {} failed ({e}); retrying in {delay:?}", attempt + 1);
                }
                let queued = Queued { job, attempt: attempt.saturating_add(1) };
                let _ = retry.send(Delayed::Job(std::time::Instant::now() + delay, queued));
                continue;
            }
        };
        let result = build(job);
        drop(pins);
        let _ = out.send(result);
    }
}

/// Holds delayed jobs and puts each back on `jobs` when it is due; exits on
/// [`Delayed::Stop`] or when every sender is gone.
fn spawn_retry_thread(jobs: Sender<Queued>) -> Sender<Delayed> {
    let (send, recv) = unbounded::<Delayed>();
    std::thread::Builder::new()
        .name("lod-retry".into())
        .spawn(move || {
            let mut waiting: std::collections::BTreeMap<(std::time::Instant, u64), Queued> = Default::default();
            let mut seq = 0u64;
            loop {
                let next = match waiting.first_key_value() {
                    Some(((due, _), _)) => recv.recv_deadline(*due),
                    None => recv.recv().map_err(|_| RecvTimeoutError::Disconnected),
                };
                match next {
                    Ok(Delayed::Job(due, queued)) => {
                        seq += 1;
                        waiting.insert((due, seq), queued);
                    }
                    Ok(Delayed::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => {
                        let now = std::time::Instant::now();
                        while let Some(entry) = waiting.first_entry() {
                            if entry.key().0 > now {
                                break;
                            }
                            let _ = jobs.send(entry.remove());
                        }
                    }
                }
            }
        })
        .expect("spawn lod retry thread");
    send
}

fn job_key(job: &LodBuildJob) -> game::lod::LodKey {
    match job {
        LodBuildJob::Pristine { key, .. }
        | LodBuildJob::Stored { key, .. }
        | LodBuildJob::Region { key, .. }
        | LodBuildJob::Tile { key, .. } => *key,
    }
}

/// Loads and pins the forged-world tiles `job` can read: its key's coarse
/// terrain and water (`worldgen::prefetch_lod`) and, for a job that may
/// generate its region (`Pristine`, or `Stored` falling back to it), that
/// region (`worldgen::prefetch_region`). An `Err` names the tile that did
/// not load.
fn prefetch_job(job: &LodBuildJob) -> Result<Vec<worldgen::forged_tiles::TilePins>, String> {
    prefetch_job_with(job, &worldgen::prefetch_lod, &worldgen::prefetch_region)
}

type Pinned = Result<worldgen::forged_tiles::TilePins, String>;

/// [`prefetch_job`] over explicit prefetches.
fn prefetch_job_with(
    job: &LodBuildJob,
    lod: &dyn Fn(&game::lod::LodKey) -> Pinned,
    region: &dyn Fn(game::RegionCoords) -> Pinned,
) -> Result<Vec<worldgen::forged_tiles::TilePins>, String> {
    let generates = match job {
        LodBuildJob::Pristine { region, .. } | LodBuildJob::Stored { region, .. } => Some(*region),
        LodBuildJob::Region { .. } | LodBuildJob::Tile { .. } => None,
    };
    std::iter::once(lod(&job_key(job))).chain(generates.map(region)).collect()
}

impl LodBuilder for ThreadLodBuilder {
    fn submit(&mut self, job: LodBuildJob) {
        let _ = self.jobs.send(Queued { job, attempt: 0 });
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
            assert_eq!(prefetch_job_with(&job, &ok_lod, &ok_region).map(|p| p.len()), Ok(1 + generates as usize));
            assert!(prefetch_job_with(&job, &bad_lod, &ok_region).is_err(), "the key's tiles fail");
            assert_eq!(prefetch_job_with(&job, &ok_lod, &bad_region).is_err(), generates, "the region's tiles fail");
        }
    }

    /// Doubling from the base, never past the cap, no overflow however many
    /// attempts a permanently bad tile racks up.
    #[test]
    fn lod_retry_backoff_is_capped() {
        let b = Backoff::default();
        assert_eq!(b.delay(0), std::time::Duration::from_millis(LOD_RETRY_BASE_MS));
        assert_eq!(b.delay(1), std::time::Duration::from_millis(2 * LOD_RETRY_BASE_MS));
        let delays: Vec<_> = (0..200).chain([u32::MAX - 1, u32::MAX]).map(|a| b.delay(a)).collect();
        assert!(delays.windows(2).all(|w| w[0] <= w[1]), "never shrinks");
        assert!(delays.iter().all(|d| *d <= b.max), "never past the cap");
        assert_eq!(*delays.last().unwrap(), std::time::Duration::from_millis(LOD_RETRY_MAX_MS), "reaches it");
    }

    /// A job whose tiles fail to load a few times (a store hiccup) is retried
    /// after the backoff and then built and delivered — never dropped, never
    /// built without its pins.
    #[test]
    fn lod_job_retries_until_its_tiles_load() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        let key = game::lod::LodKey::for_region(16, game::RegionCoords::new(1, 2));
        let (jobs, job_recv) = unbounded::<Queued>();
        let retry = spawn_retry_thread(jobs.clone());
        let (out, results) = unbounded();
        let calls = Arc::new(AtomicU32::new(0));
        let built = Arc::new(AtomicU32::new(0));
        let (c, b) = (calls.clone(), built.clone());
        let backoff = Backoff { base: std::time::Duration::from_millis(2), max: std::time::Duration::from_millis(8) };
        let worker = {
            let retry = retry.clone();
            std::thread::spawn(move || {
                let prefetch = move |_: &LodBuildJob| -> Result<Vec<worldgen::forged_tiles::TilePins>, String> {
                    // The first three loads fail, as on a dropped connection.
                    if c.fetch_add(1, Ordering::SeqCst) < 3 {
                        Err("forged tile (1, 0): down".into())
                    } else {
                        Ok(Vec::new())
                    }
                };
                worker_loop(&job_recv, &retry, backoff, &prefetch, &mut |job| {
                    b.fetch_add(1, Ordering::SeqCst);
                    build_lod_job(job, &|_| Vec::new(), &game::lod::LodWater::dry, &|_| Vec::new(), &NoStoredTerrain)
                }, &out);
            })
        };
        let started = std::time::Instant::now();
        jobs.send(Queued { job: LodBuildJob::Tile { key, overlays: Vec::new(), stored: Vec::new() }, attempt: 0 }).unwrap();
        let (got, _, _) = results.recv_timeout(std::time::Duration::from_secs(10)).expect("built after the retries");
        assert_eq!(got, key);
        assert_eq!(calls.load(Ordering::SeqCst), 4, "three failed prefetches, then the one that pinned");
        assert_eq!(built.load(Ordering::SeqCst), 1);
        assert!(started.elapsed() >= std::time::Duration::from_millis(2 + 4 + 8), "it waited out the backoff");
        let _ = retry.send(Delayed::Stop);
        drop(jobs);
        worker.join().unwrap();
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
