//! One OS thread per running region, each with its own tick timer.
//! Fully independent pacing against the shared world clock: a slow region
//! ticks late and catches up with a bounded burst (it never skips tick
//! numbers); it never blocks the manager or its neighbours.
use std::collections::BTreeMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use game::{RegionCoords, RegionInput, RegionOutput, RegionRunner, RegionSeed, RegionSpawner};

/// Loads and pins the forged-world tiles generating a region reads
/// (`worldgen::prefetch_region` on the server). An `Err` (a tile that will
/// not load) stops the region instead.
pub type RegionPrefetch =
    std::sync::Arc<dyn Fn(RegionCoords) -> Result<worldgen::forged_tiles::TilePins, String> + Send + Sync>;

/// What a freshly built region starts from: its stud terrain
/// (`worldgen::stud_gen::generate_stud_region` on the server).
pub type StudGenesis = std::sync::Arc<dyn Fn(RegionCoords) -> game::StudRegionSeed + Send + Sync>;

pub struct ThreadRegionSpawner {
    /// Server start: the base of the world clock's ms (the same base as the
    /// manager's `now_ms`).
    started: Instant,
    /// The world clock (ms since `started` -> world tick).
    clock: game::WorldClock,
    handles: BTreeMap<RegionCoords, JoinHandle<()>>,
    /// The world store: a region that has not run in this process (a
    /// `RegionSeed::Fresh`) is restored from its stored state, if
    /// any, instead of generated — read on the region's own thread. A read
    /// that fails stops the region (`RegionOutput::LoadFailed`): it is never
    /// regenerated over stored data. Runners also seed the halo of stored
    /// neighbours from it (`RegionInput::SeedHaloFromStore`).
    store: Option<crate::claims::HostClaims>,
    /// Every region thread loads and pins the forged tiles of its 3x3
    /// neighbourhood before it is built and holds them for its life (its own
    /// generation and its halo seeds of never-run neighbours read them). A
    /// tile that will not load answers `RegionOutput::LoadFailed`: the region
    /// never generates and never runs with an unseeded seam. `None`: no
    /// forged world to prefetch (tests, tools with their own generators).
    prefetch: Option<RegionPrefetch>,
    /// Worldgen, on the region's own thread: the stud terrain a region starts from
    /// when it is built from nothing (a `RegionSeed::Fresh` the store does not hold),
    /// seeded before the runner's baseline hash. Parked and restored regions never
    /// get it. `None` (the `Default`): such a region starts empty, and the manager
    /// is not told that halos are seeded here (`seeds_halos_on_region_threads`).
    genesis: Option<StudGenesis>,
    /// Stud worldgen of a never-run neighbour's band columns for halo seeds
    /// (`RegionInput::SeedHaloFromWorldgen`), handed to every runner
    /// (`RegionRunner::set_stud_halo_generator`). Set with `genesis` — one that
    /// generates regions always seeds halos too.
    stud_halo: Option<game::StudHaloGenerator>,
}

impl Default for ThreadRegionSpawner {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            clock: game::WorldClock::starting_at(0, 0),
            handles: BTreeMap::new(),
            store: None,
            prefetch: None,
            genesis: None,
            stud_halo: None,
        }
    }
}

impl ThreadRegionSpawner {
    /// Use `clock` (anchored at `started`, the same base as the manager's
    /// `now_ms`) as the world clock.
    pub fn with_clock(mut self, clock: game::WorldClock, started: Instant) -> Self {
        self.clock = clock;
        self.started = started;
        self
    }

    /// Restore never-run regions from the world store.
    /// Loads are fenced through `store` (shared with the cut writer).
    pub fn with_store(mut self, store: crate::claims::HostClaims) -> Self {
        self.store = Some(store);
        self
    }

    /// Regions generate on their own thread: one built from nothing starts from
    /// `genesis`'s stud terrain. The stud halo of a
    /// never-run neighbour is cut from the same function (the neighbour generated
    /// whole, its band columns kept) unless [`Self::with_stud_halo`] gives a
    /// window generator: a stud region always has a floor past its seams.
    pub fn with_genesis(mut self, genesis: StudGenesis) -> Self {
        let whole = genesis.clone();
        self.stud_halo.get_or_insert_with(|| {
            std::sync::Arc::new(move |rc, xs: std::ops::Range<usize>, zs: std::ops::Range<usize>| {
                let seed = whole(rc);
                game::StudRegionSeed {
                    ground: seed.ground.into_iter().filter(|(c, _)| xs.contains(&c.x) && zs.contains(&c.z)).collect(),
                    features: seed.features.into_iter().filter(|(c, _)| xs.contains(&c.x) && zs.contains(&c.z)).collect(),
                }
            })
        });
        self.genesis = Some(genesis);
        self
    }

    /// Stud halo seeds of never-run neighbours generate only the band columns with
    /// `halo` (which must give the same recipes and features as the genesis does
    /// for those columns).
    pub fn with_stud_halo(mut self, halo: game::StudHaloGenerator) -> Self {
        self.stud_halo = Some(halo);
        self
    }

    /// Pin a region's forged tiles with `prefetch` before it generates.
    pub fn with_prefetch(mut self, prefetch: RegionPrefetch) -> Self {
        self.prefetch = Some(prefetch);
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
        let store = self.store.clone();
        let prefetch = self.prefetch.clone();
        let genesis = self.genesis.clone();
        let stud_halo = self.stud_halo.clone();
        let (clock, started) = (self.clock, self.started);
        let handle = std::thread::Builder::new()
            .name(format!("region {},{}", id.x, id.z))
            .spawn(move || {
                // Deserialize/generate on the region's own thread so a big
                // region never stalls the manager.
                let genesis_started = Instant::now();
                let parked = matches!(seed, RegionSeed::Parked(_));
                // Never run in this process: the world store may have it.
                let loaded = match (&seed, &store) {
                    (RegionSeed::Fresh, Some(store)) => store.load_region(id),
                    _ => Ok(None),
                };
                // The region does not run: never regenerated over stored
                // data, never generated without its forged tiles. Its channel
                // stays open until the manager drops it (what it was sent is
                // recovered there).
                let fail = |why: String| {
                    let _ = out.send((id, RegionOutput::LoadFailed(why)));
                    while recv.recv().is_ok() {}
                };
                let stored = match loaded {
                    Ok(stored) => stored,
                    Err(e) => {
                        log::error!("[region {},{}] cannot load from the world store: {e}", id.x, id.z);
                        // A fenced host is replaced for good (the claims set the
                        // process-wide flag); the region just does not run.
                        let why = if matches!(e, world_store::StoreError::Fenced { .. }) { "fenced".to_string() } else { e.to_string() };
                        return fail(why);
                    }
                };
                let from_store = stored.is_some();
                // The forged tiles of its 3x3 neighbourhood, pinned for the
                // runner's life: its own generation (a fresh region) and
                // every halo seed of a
                // never-run neighbour (`SeedHaloFromWorldgen`, also the
                // fallback of `SeedHaloFromStore`, at start or on a reseed)
                // then read only pinned tiles. A tile that will not load
                // stops the region before it runs — never an empty seam.
                let _pins = match &prefetch {
                    Some(prefetch) => {
                        match id.window_3x3().into_iter().map(|rc| prefetch(rc)).collect::<Result<Vec<_>, _>>() {
                            Ok(pins) => pins,
                            Err(e) => {
                                log::error!("[region {},{}] cannot load its forged world tiles: {e}", id.x, id.z);
                                return fail(format!("forged world tiles: {e}"));
                            }
                        }
                    }
                    None => Vec::new(),
                };
                // A fresh region generates here, timed separately from the build.
                let mut generate_time = Duration::ZERO;
                let runner = match stored {
                    Some(stored) => {
                        let (region, terms) = game::restore_portable(id, stored);
                        let mut runner = RegionRunner::new(id, region, out);
                        runner.set_saved_terms(terms);
                        runner
                    }
                    None => {
                        let built = seed.into_region(id, || {
                            let genesis = genesis.as_ref()?;
                            let started = Instant::now();
                            let terrain = genesis(id);
                            generate_time = started.elapsed();
                            Some(terrain)
                        });
                        match built {
                            Ok(region) => RegionRunner::new(id, region, out),
                            // A parked region's blob is its whole state (edits, entities,
                            // piles). One that does not decode is a read error like a
                            // store's: the region does not run (`LoadFailed`, retried by
                            // the manager, which then reads the store's last cut) — it is
                            // never regenerated, or the next cut would save fresh ground
                            // over the stored region.
                            Err(why) => {
                                log::error!("[region {},{}] {why}", id.x, id.z);
                                return fail(why);
                            }
                        }
                    }
                };
                let mut runner = runner;
                if let Some(g) = stud_halo {
                    runner.set_stud_halo_generator(g);
                }
                // Halo seeds of stored neighbours that have not run here
                // (`RegionInput::SeedHaloFromStore`) read the same store.
                if let Some(store) = &store {
                    runner.set_stored_terrain(std::sync::Arc::new(world_store::TerrainFromStore(store.store().clone())));
                }
                let hash_time = runner.baseline_hash_time();
                let build = genesis_started.elapsed() - generate_time - hash_time;
                // `generate_ms` = worldgen on this thread (fresh regions);
                // `build_ms` = deserialize (parked), restore (`stored` = read
                // from the world store) or stud chunk + collider genesis (fresh);
                // `terrain_hash_ms` = the runner's baseline
                // terrain hash (timed inside `RegionRunner::new`).
                game::perf_event!(
                    "region_genesis",
                    "region={},{} ms={} generate_ms={} build_ms={} terrain_hash_ms={} parked={parked} stored={from_store}",
                    id.x,
                    id.z,
                    game::perf::ms(genesis_started.elapsed()),
                    game::perf::ms(generate_time),
                    game::perf::ms(build),
                    game::perf::ms(hash_time)
                );
                region_thread_loop(id, runner, recv, clock, started);
            })
            .expect("failed to spawn region thread");
        self.handles.insert(id, handle);
        send
    }

    fn seeds_halos_on_region_threads(&self) -> bool {
        self.genesis.is_some()
    }

    /// The world clock: one tick per `TICK_RATE` ms of wall time since the
    /// server's start, from the persisted world tick.
    fn world_tick(&self) -> game::Tick {
        self.clock.tick_at(self.started.elapsed().as_millis() as u64)
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
/// when the deadline fires. The deadline is when the region's next tick is due
/// on the world clock (`started` is the clock's ms base); a region behind the
/// world tick runs up to [`game::MAX_CATCH_UP_BURST`] ticks back to back, then
/// services its inputs again (a past deadline returns at once).
pub fn region_thread_loop(
    id: RegionCoords,
    mut runner: RegionRunner,
    recv: Receiver<RegionInput>,
    clock: game::WorldClock,
    started: Instant,
) {
    let now_ms = || started.elapsed().as_millis() as u64;
    let mut metrics = TickMetrics::new();
    let mut first_tick = true;
    loop {
        let next = started + Duration::from_millis(clock.ms_of(runner.current_tick() + 1));
        match recv.recv_deadline(next) {
            Ok(input) => {
                if !runner.handle_input(input) {
                    return; // Shutdown acknowledged with Stopped
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // The deadline fired, so one tick is due even if the ms
                // rounding says otherwise.
                let due = game::ticks_due(runner.current_tick(), clock.tick_at(now_ms())).max(1);
                for _ in 0..due {
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
                }
                metrics.lag_ticks = clock.tick_at(now_ms()).saturating_sub(runner.current_tick());
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
    /// Ticks this region is behind the world tick after its last burst (0 =
    /// keeping up; the sim is not keeping up with the 50ms budget otherwise).
    lag_ticks: u64,
}

impl TickMetrics {
    fn new() -> Self {
        Self {
            window_start: Instant::now(),
            ticks: 0,
            compute_sum: Duration::ZERO,
            compute_max: Duration::ZERO,
            lag_ticks: 0,
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
            // The VOXEL_STATE_HASH_INTERVAL whole-state hash, also in-tick.
            let (state_checks, state_total, state_max) = runner.take_state_hash_cost();
            // Dropped items the pile caps destroyed (`game::item_piles`).
            let items_destroyed = runner.take_items_destroyed();
            log::info!(
                "[region ({},{})] ticks={} avg={:.2}ms max={:.2}ms budget={:.1}% lag_ticks={} | terrain_hash n={} sum={:.1}ms max={:.1}ms | state_hash n={} sum={:.1}ms max={:.1}ms | items_destroyed={}",
                id.x,
                id.z,
                self.ticks,
                avg_ms,
                self.compute_max.as_secs_f32() * 1000.0,
                budget,
                self.lag_ticks,
                hash_checks,
                game::perf::ms(hash_total),
                game::perf::ms(hash_max),
                state_checks,
                game::perf::ms(state_total),
                game::perf::ms(state_max),
                items_destroyed,
            );
        } else {
            let _ = runner.take_terrain_hash_cost();
            let _ = runner.take_state_hash_cost();
            let _ = runner.take_items_destroyed();
        }
        *self = TickMetrics::new();
    }
}
