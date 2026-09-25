//! One OS thread per running region, each with its own tick timer.
//! Fully independent pacing: a slow region ticks late and catches up by
//! skipping missed deadlines; it never blocks the manager or its neighbours.
use std::collections::BTreeMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use game::{RegionCoords, RegionInput, RegionOutput, RegionRunner, RegionSeed, RegionSpawner};

/// Worldgen shared by every region thread.
pub type SharedRegionGenerator =
    std::sync::Arc<dyn Fn(RegionCoords) -> Vec<(game::ColumnCoords, game::Column)> + Send + Sync>;

#[derive(Default)]
pub struct ThreadRegionSpawner {
    handles: BTreeMap<RegionCoords, JoinHandle<()>>,
    /// `Some` = regions generate their own columns on their own thread, so the
    /// manager hands out `RegionSeed::Deferred` (see `generates_columns`).
    /// `None` (the `Default`, used by tests that pass ready-made seeds) keeps
    /// worldgen with the manager.
    generator: Option<SharedRegionGenerator>,
    /// Non-zero: spawn cell-mode runners with this cell size
    /// (`game::CELL_CHUNKS`).
    cell_chunks: usize,
}

impl ThreadRegionSpawner {
    pub fn with_generator(generator: SharedRegionGenerator) -> Self {
        Self { handles: BTreeMap::new(), generator: Some(generator), cell_chunks: 0 }
    }

    /// Run every spawned region in cell mode (`cell_chunks` = 0: legacy).
    pub fn with_cells(mut self, cell_chunks: usize) -> Self {
        self.cell_chunks = cell_chunks;
        self
    }
}

impl RegionSpawner for ThreadRegionSpawner {
    fn spawn(
        &mut self,
        id: RegionCoords,
        seed: RegionSeed,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Sender<RegionInput> {
        let (send, recv) = unbounded();
        let generator = self.generator.clone();
        let cell_chunks = self.cell_chunks;
        let handle = std::thread::Builder::new()
            .name(format!("region {},{}", id.x, id.z))
            .spawn(move || {
                // Deserialize/generate on the region's own thread so a big
                // region never stalls the manager.
                let genesis_started = Instant::now();
                let parked = matches!(seed, RegionSeed::Parked(..) | RegionSeed::Deferred(Some(_)));
                // Deferred seeds generate here, timed separately from the build.
                let generate_time = std::cell::Cell::new(Duration::ZERO);
                let runner = seed.into_runner(
                    id,
                    out,
                    &|rc| {
                        let started = Instant::now();
                        let columns = match &generator {
                            Some(generate) => generate(rc),
                            None => unreachable!("Deferred seed without a region generator"),
                        };
                        generate_time.set(started.elapsed());
                        columns
                    },
                    cell_chunks,
                );
                let generate_time = generate_time.get();
                let hash_time = runner.baseline_hash_time();
                let build = genesis_started.elapsed() - generate_time - hash_time;
                // `generate_ms` = worldgen on this thread (deferred seeds);
                // `build_ms` = deserialize (parked) or chunk flatten + collider
                // genesis (fresh); `terrain_hash_ms` = the runner's baseline
                // terrain hash (timed inside `RegionRunner::new`).
                game::perf_event!(
                    "region_genesis",
                    "region={},{} ms={} generate_ms={} build_ms={} terrain_hash_ms={} parked={parked}",
                    id.x,
                    id.z,
                    game::perf::ms(genesis_started.elapsed()),
                    game::perf::ms(generate_time),
                    game::perf::ms(build),
                    game::perf::ms(hash_time)
                );
                region_thread_loop(id, runner, recv);
            })
            .expect("failed to spawn region thread");
        self.handles.insert(id, handle);
        send
    }

    fn generates_columns(&self) -> bool {
        self.generator.is_some()
    }

    fn cell_mode(&self) -> bool {
        self.cell_chunks != 0
    }

    fn reap(&mut self, id: RegionCoords) {
        if let Some(handle) = self.handles.remove(&id) {
            // The thread exits right after emitting Stopped (or its channel
            // died); this join is quick.
            if handle.join().is_err() {
                log::error!("region {:?} thread panicked", id);
            }
        }
    }
}

/// recv_deadline is the whole scheduler: handle inputs as they arrive, tick
/// when the deadline fires. Backpressure is inherent — a slow region ticks
/// late; missed deadlines are skipped rather than burst-replayed.
pub fn region_thread_loop(id: RegionCoords, mut runner: RegionRunner, recv: Receiver<RegionInput>) {
    let tick = Duration::from_millis(game::TICK_RATE);
    let mut next = Instant::now() + tick;
    let mut metrics = TickMetrics::new();
    let mut first_tick = true;
    loop {
        match recv.recv_deadline(next) {
            Ok(input) => {
                if !runner.handle_input(input) {
                    return; // Shutdown acknowledged with Stopped
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let compute_start = Instant::now();
                runner.tick();
                metrics.record_tick(compute_start.elapsed());
                if first_tick {
                    // The first step after genesis pays one-off physics setup
                    // (broad-phase build, mass properties) the averages hide.
                    first_tick = false;
                    game::perf_event!(
                        "region_first_tick",
                        "region={},{} ms={}",
                        id.x,
                        id.z,
                        game::perf::ms(compute_start.elapsed())
                    );
                }
                next += tick;
                let now = Instant::now();
                if next < now {
                    // Fell behind (heavy tick / scheduler stall): skip the
                    // missed deadlines instead of spiralling.
                    metrics.overruns += 1;
                    next = now + tick;
                }
                metrics.maybe_report(id, &mut runner);
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Rolling per-region keep-up counters, flushed to one INFO line every
/// `METRICS_REPORT_INTERVAL`. Thread-local to the region — never shared.
struct TickMetrics {
    window_start: Instant,
    ticks: u32,
    compute_sum: Duration,
    compute_max: Duration,
    /// Ticks that finished after their deadline had already passed (the sim
    /// is not keeping up with the 50ms budget).
    overruns: u32,
}

impl TickMetrics {
    fn new() -> Self {
        Self {
            window_start: Instant::now(),
            ticks: 0,
            compute_sum: Duration::ZERO,
            compute_max: Duration::ZERO,
            overruns: 0,
        }
    }

    fn record_tick(&mut self, compute: Duration) {
        self.ticks += 1;
        self.compute_sum += compute;
        if compute > self.compute_max {
            self.compute_max = compute;
        }
    }

    fn maybe_report(&mut self, id: RegionCoords, runner: &mut RegionRunner) {
        if self.window_start.elapsed() < game::METRICS_REPORT_INTERVAL {
            return;
        }
        if self.ticks > 0 {
            let avg = self.compute_sum / self.ticks;
            let avg_ms = avg.as_secs_f32() * 1000.0;
            // Share of the tick period the sim actually spent computing.
            let budget = avg_ms / game::TICK_RATE as f32 * 100.0;
            // The cadence terrain snapshot + hash runs inside a tick; reported
            // beside `max` because it is the prime suspect when they match.
            let (hash_checks, hash_total, hash_max) = runner.take_terrain_hash_cost();
            log::info!(
                "[region ({},{})] ticks={} avg={:.2}ms max={:.2}ms budget={:.1}% overruns={} | terrain_hash n={} sum={:.1}ms max={:.1}ms",
                id.x,
                id.z,
                self.ticks,
                avg_ms,
                self.compute_max.as_secs_f32() * 1000.0,
                budget,
                self.overruns,
                hash_checks,
                game::perf::ms(hash_total),
                game::perf::ms(hash_max),
            );
        } else {
            let _ = runner.take_terrain_hash_cost();
        }
        *self = TickMetrics::new();
    }
}
