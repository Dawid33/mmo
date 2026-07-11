# Veloren Multi-Threading Architecture — Research & Applicability

**Date:** 2026-07-10
**Method:** two independent tracks — (a) source-level analysis of a fresh clone of
`gitlab.com/veloren/veloren` (file:symbol citations below are from master), and
(b) a web deep-research pass over the developer book, rustdoc, GitLab issues, and
changelogs, with 3-vote adversarial verification per claim (23 confirmed, 2 refuted).
Where the two tracks disagreed (pool topology), the direct source read wins and is noted.

## Headline

Veloren is **server-authoritative with no rollback, no client prediction, and no
cross-machine determinism goal**. Its parallelism story is therefore almost the inverse
of ours: it parallelizes *inside* the single shared-world simulation tick (specs ECS
auto-scheduling + rayon `par_join`), which is exactly what our bit-exact
`hash(before) == hash(after undo)` bar forbids. What *is* directly portable is
everything Veloren does **around** the tick: the low-priority named background job pool
("slowjob"), cooperative cancellation, channel handback with budgeted apply, OS thread
priorities, and per-system CPU instrumentation.

Notably, **Veloren does not partition simulation by region at all** — our
region-per-thread `RegionRunner` design is *ahead* of Veloren on horizontal scaling,
and their measured bottleneck history (network sync, not sim) tells us where our own
next wall is likely to be.

## 1. How Veloren threads its game

### 1.1 ECS and per-tick system parallelism

- ECS is `specs` v0.20 (git-pinned to an amethyst rev; upstream effectively dormant).
  No migration plans documented anywhere. A thin `veloren_common_ecs` wrapper
  (`common/ecs/src/system.rs`, `Job<T>`) adds per-system CPU-time metrics.
- The dispatcher is built **once** at `State::new` (`common/state/src/state.rs`) on a
  shared rayon pool and reused every tick: `dispatcher.dispatch(&self.ecs)` runs **all
  systems in parallel**, constrained only by specs' read/write-set analysis plus
  explicit string-name dependencies (e.g. `phys::Sys` depends on
  `interpolation/controller/mount/stats`; `projectile/beam/melee` depend on `phys`).
  Non-`Send` systems are banned (`try_into_sendable().unwrap_or_else(panic)`).
  (Historical note: issue #63, April 2019 — they originally rebuilt the dispatcher
  *and its thread pool* every tick; construct-once became the pattern.)
- Inside systems, data parallelism via rayon `par_join` is first-class and
  instrumented: `ParMode {None, Single, Rayon, Exact(n)}` records when a system went
  wide, feeding their profiler.

### 1.2 Three thread pools

| Pool | Purpose | Sizing / priority |
|---|---|---|
| ECS rayon pool | `dispatcher.dispatch` + every `par_join` | `max(num_cpus, MIN_RECOMMENDED_RAYON_THREADS)`; on Linux the server tick pool gets **realtime RoundRobin priority 50** |
| SlowJobPool (`common/src/slowjob.rs`) | CPU-heavy multi-tick work (never I/O) | **separate** rayon pool, global limit ≈ `n/2 + n/4` (~75% cores), threads at **low OS priority** (Linux `Batch`, nice-equivalent 15) |
| tokio runtime | networking only | `(num_cpus/4).max(MIN_RECOMMENDED_TOKIO_THREADS)` |

The pool separation is deliberate and battle-scarred: a comment in `slowjob.rs`
(~line 129) explains that rayon work-stealing let an ECS tick task steal a
multi-hundred-ms slowjob when they shared one pool, causing lag spikes bad enough that
**Veloren 0.14 (2023-01) shipped pool isolation as a workaround** rather than wait for
an upstream rayon fix. (The web pass had this 2-1 contested; the direct source read
confirms current master uses a dedicated slowjob pool.)

### 1.3 SlowJobPool mechanics (the most reusable piece)

Jobs are submitted under a **string name**, each name `configure`d with a thread cap:

- Server (`server/src/lib.rs:424`): `CHUNK_GENERATOR |n| n/2+n/4`,
  `CHUNK_SERIALIZER |n| n/2`, `CHUNK_DROP |_| 1`, `RTSIM_SAVE |_| 1`, `WEATHER |_| 1`.
- Voxygen (`voxygen/src/ecs/mod.rs:14`): `TERRAIN_MESHING`, `FIGURE_MESHING`,
  `IMAGE_PROCESSING`, each `|n| n/2`.

The scheduler (`calc_queued_order`) enforces global + per-name limits, guarantees ≥1
running job per queued name (no starvation), spreads remaining capacity proportionally
with round-robin, and workers self-drain the queue on completion. `try_run` gives
start-now-or-error; `cancel` removes queued jobs; per-job timing metrics are collected.

### 1.4 Server tick and chunk generation

`server/src/lib.rs::tick` has 9 phases; the parallel ECS dispatch is phase 4, then
mounts/events, `update_region_map`, `apply_terrain_changes`, and a **sequential**
client-sync phase (`sentinel`, `subscription`, `terrain_sync`, `entity_sync` via
`run_now`).

Chunk generation is the canonical off-tick pattern
(`server/src/chunk_generator.rs`): `slowjob_pool.spawn("CHUNK_GENERATOR", move || {
world.generate_chunk(...); chunk_tx.send(...) })` with the world passed in via `Arc`; a
per-request `Arc<AtomicBool>` **cooperative cancel flag polled inside worldgen**
(abandoned chunks stop burning worker time and increment a `canceled` metric); results
return over an **unbounded crossbeam channel** drained non-blockingly with `try_recv`
inside `sys/terrain.rs`. Chunks integrate in arrival order — fine for them,
non-deterministic by our standards.

Physics is **their own ECS system** (`common/systems/src/phys/mod.rs`), not rapier: a
`SpatialGrid` rebuilt per tick, then rayon `par_join` for entity↔entity pushback and
entity↔terrain resolution. Each entity writes only its own components, so parallel
writes don't conflict — but nothing enforces or verifies bit-exactness.

### 1.5 Regions are interest management, not simulation partitions

`common/src/region.rs::RegionMap` is a single global resource binning entities into
512-block regions (`REGION_LOG2 = 9`) with 16-block hysteresis (`TETHER_LENGTH`),
producing `Left/Entered` events. Its rustdoc: "primarily intended for tracking which
entities need to be synchronized to which clients." Entities excluded from sync may
not appear in it at all; deletion requires a manual, immediate `entity_deleted` call
(doc TODO: "maintain within a system?"). **There is one simulation; regions never tick
on separate threads.**

### 1.6 Client (voxygen) meshing

On chunk change: `slow_jobs.spawn("TERRAIN_MESHING", …)` runs greedy meshing
off-thread (`voxygen/src/scene/terrain/mod.rs::mesh_worker`), returns over a crossbeam
channel, and the main thread **budgets GPU upload**:
`recv_count = dt * CHUNKS_PER_SECOND + mesh_recv_overflow` with fractional carry — a
rate-limited apply queue that trades chunk pop-in latency for frame stability. Figure
(character model) meshing uses the same pool via a cache that owns spawn policy
(`FigureModelCache::get_or_create_model` spawns `FIGURE_MESHING` on miss, with a
synchronous fallback when no pool is available — that fallback is their wasm/headless
story too).

### 1.7 Networking: async edge, sync tick

`veloren-network` is tokio-async and protocol-agnostic (TCP/QUIC/MPSC). The bridge is
channels both ways: tokio tasks deserialize frames into per-stream channels; message
systems drain them non-blockingly inside the tick — notably **`par_join` over all
`Client` components** so per-client message handling is parallelized. Sends go onto a
crossbeam channel the tokio side flushes. tokio owns sockets + framing; the ECS tick
owns message semantics. Same shape as our quinn edge → manager thread → region threads.

### 1.8 Measured bottleneck history

Issue #1356 (0.11 era, Grafana-backed): **EntitySync — the per-client state fan-out —
was the tick bottleneck, not simulation.** Their remedy: `par_iter`/`par_join` over
the sync iterations. The lesson generalizes: sync cost scales with players × entities
and will dominate before sim does.

## 2. Determinism audit — why the headline techniques don't transfer

Confirmed non-deterministic by design (grep for
`rollback|reconcil|prediction|deterministic` finds nothing relevant):

1. **Parallel system dispatch + `par_join`** — intra-tick mutation and float
   accumulation order is rayon-scheduler-dependent. Even where *end state* happens to
   be order-independent, our **undo log is order-sensitive**: entries must replay and
   invert in exact sequence, so any nondeterministic interleaving inside a region tick
   breaks `hash(before) == hash(after undo)` regardless of state convergence.
2. **Unordered job completion** — chunk-gen results applied in arrival order.
3. **Native floats** — no `libm` forcing, no `ordered-float`; dynamic spawns are even
   seeded from thread RNG (`ChaCha8Rng::from_seed(rand::rng().random())`),
   deliberately non-reproducible.
4. **No state hashing or invariant verification** anywhere.

Client-side they don't predict at all — they interpolate authoritative state
(`common/systems/src/interpolation.rs`). Our rollback netcode is solving a strictly
harder problem than anything in their codebase.

## 3. What applies to this project

### Directly portable (recommended)

1. **Pool isolation + OS thread priorities** (`region_threads.rs`, client meshing).
   Veloren's hardest-won lesson: never let a pool that serves latency-sensitive tick
   work also run multi-hundred-ms jobs. We already isolate by architecture (region
   threads ≠ mesher tasks), but we don't set priorities: Veloren elevates server tick
   threads to realtime RoundRobin(50) and demotes background workers to `Batch`/nice-15
   via the `thread_priority` crate. Cheap to adopt: elevate region tick threads,
   demote worldgen/genesis/mesh workers. Directly relevant to the known ~318ms
   first-tick-after-load spike competing with live region ticks for cores.

2. **Named, capped job admission (SlowJobPool shape).** We have ad-hoc async work
   (remesh queue, `worldgen_rings` LOD generation, region genesis). As these multiply,
   a single background pool with per-category caps (`TERRAIN_MESHING |n| n/2`,
   `WORLDGEN_RING |n| n/4`, …), a no-starvation guarantee, and per-category timing
   metrics stops any one workload from flooding cores. Adopt the *shape* — string-named
   configs over one low-priority pool — not the crate.

3. **Cooperative cancellation for worldgen/meshing churn.** Per-request
   `Arc<AtomicBool>` polled inside `generate_region`/meshing, plus a canceled-jobs
   metric. When the player moves and an LOD ring is superseded before its generation
   finishes, we currently burn the work. Matches the known wasm gen-hitching and
   far-ring churn pops. `generate_region` is pure, so a poll-between-columns callback
   is a trivial addition.

4. **Budgeted apply with fractional carry.** Their
   `dt * CHUNKS_PER_SECOND + overflow` accumulator is a nicer formulation than a flat
   per-frame cap for our mesh-apply and ring-apply queues — smooth under variable
   frame time, and the tunable is a named constant (matches our tunables preference).

5. **Per-system/per-phase CPU timeline instrumentation.** Their `ParMode`/CpuTimeline
   per-system profiling fed directly into finding EntitySync as the bottleneck. Our
   analogue: per-phase timings inside `RegionRunner::tick` (input apply / physics /
   boundary scan / snapshot) exported as metrics, so the next hotspot is measured, not
   guessed.

6. **Sync fan-out is the wall to watch.** Their one Grafana-measured bottleneck was
   per-client sync, which lives *outside* our deterministic tick (manager/netcode
   edge) and can therefore be parallelized freely — rayon over clients, exactly their
   #1356 remedy, with zero rollback risk. Budget and profile it before player counts
   make it dominate.

### Validating (no action needed)

- **Engine-agnostic sim under a swappable frontend** — voxygen has "no inherent
  coupling with the internal client library" (they even ship a headless bot frontend);
  same dependency direction as our Bevy-free `game`/`server` under a Bevy client.
- **Async network edge / sync tick over channels** — their tokio↔ECS bridge is our
  quinn↔manager↔region design. Their `par_join` over clients for message decode is a
  safe pattern for our edge (outside the sim).
- **Region-per-thread actors** — no Veloren analogue exists; their RegionMap maps to
  our `WorldManager` subscriptions/interest layer only. Their manual-bookkeeping
  fragility (`entity_deleted` must-call, "TODO maintain within a system?") is an
  argument for keeping that logic centralized in the manager, as we do.

### Not portable (do not copy)

- **Parallel system dispatch / `par_join` inside the sim tick** — breaks undo-log
  ordering and float determinism (§2). Our parallelism unit is the *region*, and we
  already have it. If one region ever gets too hot, Veloren offers no
  determinism-compatible answer; ours is smaller regions or region splitting, not
  intra-tick threads.
- **Unordered integration of async results into sim state.** Veloren applies chunks at
  arrival time. For us, any async product entering `GameData` (region genesis,
  future async terrain edits) must be injected as a **tick-stamped, logged event**
  through `RegionInput`, so re-simulation replays it identically. Arrival order may
  feed a queue; *application* order must be tick-deterministic.
- **specs ECS** — upstream dormant, and ECS auto-scheduling is the very thing we can't
  use in-sim. Our explicit `#[rollback]` state + typed deltas is the right substrate.
- **Their physics model** — hand-rolled, per-entity, no journal; we have deterministic
  rapier with StepJournal.

## 4. Open questions their history raises for us

- When we parallelize sync fan-out at the edge, add per-phase metrics first
  (EntitySync lesson: measure, then parallelize the measured hotspot).
- Thread priority tuning interacts with the tokio runtime and Bevy's own task pools on
  the client — verify no starvation of the render thread before shipping priorities.
- Their `try_run`/synchronous-fallback pattern is the clean way to keep one code path
  across native (pooled) and wasm (inline) — relevant to `InlineSpawner` parity.

## Sources

Primary: Veloren master source (direct clone, 2026-07-10); developer book
(`book.veloren.net` — codebase-structure, ecs, performance-analysis); rustdoc
(`docs.veloren.net` — `slowjob`, `RegionMap`, `common_state::state`); GitLab issues
#63 (dispatcher-per-tick bug, 2019), #1356 (EntitySync bottleneck, 2021); 0.14
changelog (slowjob pool-isolation workaround, 2023). Verification: 25 claims
adversarially checked 3-vote each; 23 confirmed, 2 refuted (both over-specific slowjob
pool-topology claims — resolved here by the direct source read: dedicated low-priority
pool, global limit ≈ 75% of cores).
