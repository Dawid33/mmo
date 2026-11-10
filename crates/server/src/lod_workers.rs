//! Native-server LOD build workers: a fixed pool of threads building the far-terrain
//! tiles of LOD keys (`game::stud_lod`), so a cache miss never stalls the manager
//! thread's event routing.

use crossbeam::channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use game::lod::LodKey;
use game::{LodBuilder, StudLodResult};

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
/// Why it scales: a join queues hundreds of builds at once, and a fixed two workers
/// paced `world_ready` while the server sat on a fraction of one core (measured on
/// the voxel LOD pipeline, 2026-09-20 capture; not re-measured on stud tiles).
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

/// A key on the worker queue, with how many times its tiles failed to load.
struct Queued {
    key: LodKey,
    attempt: u32,
}

/// What the retry thread is sent.
enum Delayed {
    /// Put `Queued` back on the worker queue at the instant.
    Job(std::time::Instant, Queued),
    /// The builder is gone: drop what waits and exit.
    Stop,
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

/// The server's LOD builder: a pool of threads building each key's far-terrain tile
/// from the ground recipe (`worldgen::stud_gen::stud_lod_tile`). A worker pins the
/// forged tiles its key reads first, and a key whose tiles do not load is never built
/// from missing tiles and never dropped: it goes to the retry thread and comes back
/// after `backoff.delay(attempt)` (capped), so a store hiccup costs latency, not the
/// key (the manager keeps a key in flight until its result lands). The retry thread
/// is stopped on drop: it holds a sender of `jobs`, which would otherwise keep the
/// workers alive.
pub struct ThreadStudLodBuilder {
    jobs: Sender<Queued>,
    retry: Sender<Delayed>,
    results: Receiver<StudLodResult>,
}

impl ThreadStudLodBuilder {
    pub fn new(seed: u64, threads: usize) -> Self {
        Self::with(threads, Backoff::default(), std::sync::Arc::new(move |key: &LodKey| {
            let _pins = worldgen::prefetch_lod(key)?;
            Ok(worldgen::stud_gen::stud_lod_tile(key, seed))
        }))
    }

    /// A pool whose workers answer a key with `build` (`Err` = its tiles did not
    /// load: retried after `backoff`).
    fn with(
        threads: usize,
        backoff: Backoff,
        build: std::sync::Arc<dyn Fn(&LodKey) -> Result<game::StudLodTile, String> + Send + Sync>,
    ) -> Self {
        let (jobs, job_recv) = unbounded::<Queued>();
        let (out, results) = unbounded();
        let retry = spawn_retry_thread(jobs.clone());
        for i in 0..threads {
            let (recv, out, retry, build) = (job_recv.clone(), out.clone(), retry.clone(), build.clone());
            std::thread::Builder::new()
                .name(format!("stud-lod-worker-{i}"))
                .spawn(move || stud_worker_loop(&recv, &retry, backoff, build.as_ref(), &out))
                .expect("spawn stud lod worker");
        }
        Self { jobs, retry, results }
    }
}

impl Drop for ThreadStudLodBuilder {
    fn drop(&mut self) {
        let _ = self.retry.send(Delayed::Stop);
    }
}

fn stud_worker_loop(
    recv: &Receiver<Queued>,
    retry: &Sender<Delayed>,
    backoff: Backoff,
    build: &(dyn Fn(&LodKey) -> Result<game::StudLodTile, String> + Send + Sync),
    out: &Sender<StudLodResult>,
) {
    while let Ok(Queued { key, attempt }) = recv.recv() {
        let started = std::time::Instant::now();
        match build(&key) {
            Ok(tile) => {
                crate::perf::EDGE.record_lod_build(started.elapsed());
                let _ = out.send((key, tile));
            }
            Err(e) => {
                let delay = backoff.delay(attempt);
                if attempt == 0 {
                    log::warn!("[server] stud LOD {key:?}: its forged world tiles do not load ({e}); retrying in {delay:?}, then backing off to {:?}", backoff.max);
                } else {
                    log::debug!("[server] stud LOD {key:?}: attempt {} failed ({e}); retrying in {delay:?}", attempt + 1);
                }
                let queued = Queued { key, attempt: attempt.saturating_add(1) };
                let _ = retry.send(Delayed::Job(std::time::Instant::now() + delay, queued));
            }
        }
    }
}

impl LodBuilder for ThreadStudLodBuilder {
    fn submit(&mut self, key: LodKey) {
        let _ = self.jobs.send(Queued { key, attempt: 0 });
    }

    fn poll(&mut self) -> Option<StudLodResult> {
        self.results.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The builder's threads answer every submitted key with its tile, once; a key
    /// whose tiles fail to load is retried until they do.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn stud_builder_threads_build_every_key_and_retry_failed_loads() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        let tile_of = |key: &LodKey| {
            let side = game::StudLodTile::side_for(key);
            let sample = game::StudLodSample::of_column(key.coords[0] * 10 + key.coords[1], game::MAT_GRASS, None);
            game::StudLodTile::encode(side, &vec![sample; game::StudLodTile::sample_count(side)])
        };
        let flaky = LodKey { factor: 64, coords: [9, 9] };
        let failures = Arc::new(AtomicU32::new(0));
        let f = failures.clone();
        let backoff = Backoff { base: std::time::Duration::from_millis(2), max: std::time::Duration::from_millis(8) };
        let mut builder = ThreadStudLodBuilder::with(3, backoff, Arc::new(move |key: &LodKey| {
            // The flaky key's tiles fail to load twice.
            if *key == flaky && f.fetch_add(1, Ordering::SeqCst) < 2 {
                return Err("forged tile (1, 0): down".into());
            }
            Ok(tile_of(key))
        }));
        let mut keys: Vec<LodKey> = (0..20).map(|i| LodKey { factor: 64, coords: [i % 5, i / 5] }).collect();
        keys.push(flaky);
        for key in &keys {
            builder.submit(*key);
        }
        let mut got = std::collections::BTreeMap::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while got.len() < keys.len() && std::time::Instant::now() < deadline {
            match builder.poll() {
                Some((key, tile)) => assert!(got.insert(key, tile).is_none(), "{key:?} answered twice"),
                None => std::thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        assert_eq!(got.len(), keys.len(), "every key is answered");
        for key in &keys {
            assert_eq!(got[key], tile_of(key), "{key:?}");
        }
        assert_eq!(failures.load(Ordering::SeqCst), 3, "two failed loads of the flaky key, then the one that built");
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(builder.poll().is_none(), "nothing else arrives");
    }

    /// The real stud builder builds the procedural world's tile: what
    /// `worldgen::stud_gen::stud_lod_tile` gives the key.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn stud_builder_builds_the_recipe_tile() {
        let seed = 7;
        let key = LodKey::for_region(game::stud_ring_table(game::lod::LOD_RING_TABLE)[0].1, game::RegionCoords::new(2, -1));
        let mut builder = ThreadStudLodBuilder::new(seed, 2);
        builder.submit(key);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let (got, tile) = loop {
            if let Some(result) = builder.poll() {
                break result;
            }
            assert!(std::time::Instant::now() < deadline, "the tile never arrived");
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        assert_eq!(got, key);
        assert_eq!(tile, worldgen::stud_gen::stud_lod_tile(&key, seed));
        assert!(tile.decode().is_some());
    }

    /// A quarter of the machine, never fewer than the old fixed two, never
    /// more than six: LOD fill is background work next to region sim threads.
    #[test]
    fn default_lod_build_threads_is_a_clamped_quarter_of_the_cores() {
        for (cores, expected) in [(1, 2), (4, 2), (8, 2), (11, 2), (12, 3), (16, 4), (20, 5), (24, 6), (128, 6)] {
            assert_eq!(default_lod_build_threads(cores), expected, "{cores} cores");
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
